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

use tauri::Manager;

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

/// `keytab --toggle` / `keytab --search`, for compositor keybinds.
fn handle_args<S: AsRef<str>>(args: &[S]) {
    if args.iter().any(|a| a.as_ref() == "--search") {
        switcher::search();
    } else if args.iter().any(|a| a.as_ref() == "--toggle") {
        switcher::toggle();
    }
}

fn main() {
    let builder = tauri::Builder::default();

    // A second `keytab --toggle` hands its args to the running instance and exits.
    #[cfg(target_os = "linux")]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
        if args.iter().any(|a| a == "--quit") {
            app.exit(0);
        }
        handle_args(&args);
    }));

    builder
        .invoke_handler(tauri::generate_handler![activate, hide, key])
        .setup(|app| {
            let overlay = app.get_webview_window("main").expect("main window");
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
