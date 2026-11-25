#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
mod api;
mod assets;
mod display;
mod perf;
mod recommend;
mod downloads;
mod feed;
mod i18n;
mod import;
mod links;
mod locale;
#[cfg(target_os = "macos")]
mod macos;
mod player;
mod store;
mod ui;
mod updater;

use gpui_kit::*;

/// Window placement from the last session (size, position, maximized), or a
/// centered default sized to the screen.
fn saved_window_bounds(cx: &App) -> WindowBounds {
    if let Some(v) = store::kv_get("window").and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()) {
        let f = |k: &str| v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
        let (w, h) = (f("w"), f("h"));
        if w >= 640.0 && h >= 400.0 {
            let b = Bounds::new(point(px(f("x")), px(f("y"))), size(px(w), px(h)));
            // ignore positions that are no longer on any display
            let visible = cx.displays().iter().any(|d| d.bounds().intersects(&b));
            if visible {
                return if v.get("maximized").and_then(|m| m.as_bool()).unwrap_or(false) {
                    WindowBounds::Maximized(b)
                } else {
                    WindowBounds::Windowed(b)
                };
            }
        }
    }
    let display = cx.primary_display().map(|d| d.bounds().size).unwrap_or(size(px(1600.), px(1000.)));
    let sz = size((display.width * 0.82).min(px(1600.)), (display.height * 0.85).min(px(1000.)));
    WindowBounds::Windowed(Bounds::centered(None, sz, cx))
}

pub fn save_window_bounds(window: &Window) {
    let (b, maximized) = match window.window_bounds() {
        WindowBounds::Windowed(b) => (b, false),
        WindowBounds::Maximized(b) => (b, true),
        WindowBounds::Fullscreen(b) => (b, false),
    };
    let v = serde_json::json!({
        "x": f32::from(b.origin.x), "y": f32::from(b.origin.y),
        "w": f32::from(b.size.width), "h": f32::from(b.size.height),
        "maximized": maximized,
    });
    store::kv_set("window", &v.to_string());
}

fn main() {
    perf::mark("main");
    // the local stream proxy must never be reached through the user's proxy
    // SAFETY: set before any other thread starts
    unsafe {
        std::env::set_var("no_proxy", "127.0.0.1,localhost,::1");
        std::env::set_var("NO_PROXY", "127.0.0.1,localhost,::1");
    }
    #[cfg(target_os = "macos")]
    if let Some(path) = macos::path_with_homebrew() {
        // SAFETY: this still runs before any other thread starts.
        unsafe { std::env::set_var("PATH", path) };
    }
    perf::init();
    store::init();
    perf::mark("store");
    display::choose();
    perf::mark("display");
    // one running app: a second launch (e.g. a clicked streamtube: link) hands over and exits
    let arg = std::env::args().nth(1).unwrap_or_default();
    if !display::relaunched() && links::forward_to_running(&arg) {
        return;
    }
    let (incoming_tx, mut incoming_rx) = futures::channel::mpsc::unbounded();
    let _ = links::listen(incoming_tx.clone());
    perf::mark("links");
    let application = gpui_kit::application()
        .with_assets(assets::Assets)
        .with_http_client(ui::UreqHttp::new());
    #[cfg(target_os = "macos")]
    {
        let urls_tx = incoming_tx.clone();
        application.on_open_urls(move |urls| {
            for url in urls {
                let _ = urls_tx.unbounded_send(url);
            }
        });
        let reopen_tx = incoming_tx.clone();
        application.on_reopen(move |_| {
            let _ = reopen_tx.unbounded_send(String::new());
        });
    }
    drop(incoming_tx);
    application.run(move |cx: &mut App| {
        perf::mark("gpui running");
        gpui_kit::init(cx);
        i18n::apply_component_locale();
        perf::mark("gpui_kit init");
        player::preview::init(cx);
        perf::mark("preview init");
        cx.set_global(ui::Pal::new(true, store::settings_snapshot().accent));
        ui::apply_theme(None, cx);
        player::controller::init(cx);
        perf::mark("player init");
        let opts = WindowOptions {
            app_id: Some("streamtube".into()),
            window_bounds: Some(saved_window_bounds(cx)),
            titlebar: Some(TitlebarOptions {
                title: Some("Stream Tube".into()),
                appears_transparent: !cfg!(target_os = "macos"),
                ..Default::default()
            }),
            window_min_size: Some(size(px(720.), px(480.))),
            ..Default::default()
        };
        let (main_window, _) = gpui_kit::open_window(opts, cx, |window, cx| {
            ui::apply_theme(Some(window), cx);
            cx.new(|cx| ui::app::AppView::new(window, cx))
        })
        .expect("failed to open window");
        #[cfg(target_os = "macos")]
        macos::init_menu(main_window, cx);
        perf::mark("window open");
        ui::app_icon::apply(cx);
        links::register_scheme();
        perf::mark("icon");
        cx.spawn(async move |cx| {
            use futures::StreamExt;
            while let Some(arg) = incoming_rx.next().await {
                let _ = cx.update(|cx| {
                    let _ = main_window.update(cx, |_, window, cx| {
                        ui::app::restore_window(window);
                        window.activate_window();
                        if let Some(route) = ui::app::route_from_link(&arg) {
                            ui::app::navigate(route, window, cx);
                        }
                    });
                });
            }
        })
        .detach();
        cx.on_window_closed(|cx, id| {
            ui::pip::window_closed(id, cx);
            // the picture-in-picture window alone doesn't keep the app running
            let pip = ui::pip::handle(cx);
            if cx.windows().iter().all(|w| Some(*w) == pip) {
                player::controller::player(cx).update(cx, |p, _| p.save_progress(true));
                cx.quit();
            }
        })
        .detach();
    });
}
