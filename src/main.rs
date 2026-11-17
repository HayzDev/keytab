#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! keytab: open the switcher, then type a window's letter(s) to focus it.
//! Search mode fuzzy-matches windows. In search: Delete closes the selected window,
//! `\` sets a hint for that window, `|` a saved hint for every window of that app.
//!
//! Layout: `switcher` (state machine, platform-neutral), `keys` (hint assignment),
//! `platform/*` (one file per OS). See README for porting notes.
mod keys;
mod platform;
mod switcher;

#[tauri::command]
fn activate(id: keys::WinId) {
    switcher::activate(id);
}

#[tauri::command]
fn hide() {
    switcher::hide();
}

/// Key events from the webview; only sent when the overlay has focus (no global hook).
#[tauri::command]
fn key(key: String, down: bool) {
    switcher::on_key(switcher::Key::from_dom(&key), down);
}

#[tauri::command]
fn ready() {
    switcher::refresh();
}

/// Where the webview put the card and the Shift preview boxes, in CSS px.
#[cfg(windows)]
#[tauri::command]
fn layout(card: Option<(f64, f64, f64, f64)>, thumbs: Vec<(keys::WinId, f64, f64, f64, f64)>) -> Vec<(keys::WinId, f64)> {
    platform::layout(card, &thumbs)
}

/// `keytab --toggle` / `keytab --search`, for compositor keybinds.
fn handle_args<S: AsRef<str>>(args: &[S]) {
    if args.iter().any(|a| a.as_ref() == "--search") {
        switcher::search();
    } else if args.iter().any(|a| a.as_ref() == "--toggle") {
        switcher::toggle();
    }
    // Debug builds: `--keys=^ab` presses keys after opening (^ = hold Shift, ~ = Del), for
    // screenshots, since the Windows hook ignores synthetic input.
    #[cfg(debug_assertions)]
    for a in args.iter().filter_map(|a| a.as_ref().strip_prefix("--keys=")) {
        for c in a.chars() {
            let key = match c {
                '^' => switcher::Key::Preview,
                '~' => switcher::Key::Delete,
                _ => switcher::Key::from_dom(&c.to_string()),
            };
            switcher::on_key(key, true);
        }
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    if platform::forward_args() {
        return;
    }
    // A quit request must not start a hidden instance when none is running.
    #[cfg(target_os = "linux")]
    let quit = std::env::args().any(|a| a == "--quit");
    let builder = tauri::Builder::default();

    // A second `keytab --toggle` hands its args to the running instance and exits.
    #[cfg(target_os = "linux")]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
        if args.iter().any(|a| a == "--quit") {
            app.exit(0);
            return;
        }
        // Queue early invocations until setup has initialized the overlay.
        if let Err(e) = app.run_on_main_thread(move || handle_args(&args)) {
            eprintln!("keytab: couldn't handle launch arguments: {e}");
        }
    }));

    #[cfg(windows)]
    let builder = builder.invoke_handler(tauri::generate_handler![activate, hide, key, ready, layout]);
    #[cfg(not(windows))]
    let builder = builder.invoke_handler(tauri::generate_handler![activate, hide, key, ready]);
    // App icons for the webview: http://icon.localhost/<window id>.
    #[cfg(windows)]
    // Off the UI thread: WM_GETICON waits (up to 100ms) on each app, and a busy app must
    // never stall the overlay.
    let builder = builder.register_asynchronous_uri_scheme_protocol("icon", |_ctx, req, responder| {
        let id = req.uri().path().trim_start_matches('/').parse().ok();
        std::thread::spawn(move || {
            let res = tauri::http::Response::builder();
            responder.respond(match id.and_then(platform::icon_png) {
                Some(png) => res.header("Content-Type", "image/png").body(png).unwrap(),
                None => res.status(404).body(Vec::new()).unwrap(),
            });
        });
    });

    builder
        .setup(move |app| {
            #[cfg(target_os = "linux")]
            if quit {
                app.handle().exit(0);
                return Ok(());
            }
            #[cfg(target_os = "linux")]
            let overlay = tauri::window::WindowBuilder::from_config(app, &app.config().app.windows[0])?.build()?;
            #[cfg(windows)]
            let overlay = tauri::WebviewWindowBuilder::from_config(app, &app.config().app.windows[0])?.build()?;
            platform::init(&overlay)?;
            switcher::init(app.handle().clone(), overlay);
            #[cfg(windows)]
            tray(app)?;
            platform::start_input();
            handle_args(&std::env::args().collect::<Vec<_>>());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running keytab");
}

/// Windows-only tray with Quit. (On niri: `keytab --quit`.)
#[cfg(windows)]
fn tray(app: &mut tauri::App) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::TrayIconBuilder;
    let quit = MenuItem::with_id(app, "quit", "Quit keytab", true, None::<&str>)?;
    TrayIconBuilder::new()
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("keytab — Right Alt to switch, + Right Shift to search")
        .menu(&Menu::with_items(app, &[&quit])?)
        .on_menu_event(|app, e| if e.id == "quit" { app.exit(0) })
        .build(app)?;
    Ok(())
}
