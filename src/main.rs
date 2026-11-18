#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! keytab: open the switcher, then type a window's letter(s) to focus it.
//! Search mode fuzzy-matches windows. In search: Delete closes the selected window,
//! `\` sets a hint for that window, `|` a saved hint for every window of that app.
//!
//! Layout: `switcher` (state machine, platform-neutral), `keys` (hint assignment),
//! `platform/*` (one file per OS). See README for porting notes.
mod keys;
mod platform;
mod settings;
mod switcher;
mod updater;
#[cfg(target_os = "linux")]
mod window_hints;

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

// ---- mouse (Windows overlay; the Linux GTK overlay calls `switcher` directly) -------------

#[tauri::command]
fn select(id: keys::WinId) {
    switcher::select(id);
}

#[tauri::command]
fn close_window(id: keys::WinId) {
    switcher::close_window(id);
}

#[tauri::command]
fn edit_hint(id: keys::WinId, app: bool) {
    switcher::edit_hint(id, app);
}

/// A row's hide button (Shift+click: `app`, every window of the app).
#[tauri::command]
fn hide_window(id: keys::WinId, app: bool) {
    switcher::hide_window(id, app);
}

/// A click on a footer key.
#[tauri::command]
fn press(name: String) {
    switcher::press(&name);
}

// ---- settings window (ui/settings.html, on both platforms) ----------------------------------

#[derive(serde::Serialize)]
struct SettingsView {
    settings: settings::Settings,
    autostart: bool,
    version: String,
    config_dir: String,
    /// "windows" or "linux": the page shows each platform's own controls.
    platform: &'static str,
    /// The desktop's light/dark preference, where the webview can't see it (Linux portal).
    system_scheme: Option<&'static str>,
    /// The compositor blurs behind the window; without it the page uses an opaque card.
    blur: bool,
    /// Startup updates are possible for this build (not debug, not built from source on Linux).
    auto_update_allowed: bool,
    /// Linux: the niri keybinds that run keytab (read-only; keytab never edits niri's config).
    #[cfg(target_os = "linux")]
    binds: Vec<platform::Bind>,
    /// Linux: the niri config file whose `spawn-at-startup` starts keytab.
    #[cfg(target_os = "linux")]
    autostart_source: Option<String>,
}

#[tauri::command]
fn get_settings() -> SettingsView {
    #[cfg(target_os = "linux")]
    let autostart_source = platform::autostart_source();
    SettingsView {
        settings: settings::get(),
        #[cfg(windows)]
        autostart: platform::autostart(),
        #[cfg(target_os = "linux")]
        autostart: autostart_source.is_some(),
        version: updater::current(),
        config_dir: settings::config_dir().display().to_string(),
        platform: if cfg!(windows) { "windows" } else { "linux" },
        #[cfg(windows)]
        system_scheme: None,
        #[cfg(target_os = "linux")]
        system_scheme: platform::system_scheme(),
        #[cfg(windows)]
        blur: true,
        #[cfg(target_os = "linux")]
        blur: platform::blur_available(),
        auto_update_allowed: updater::auto_allowed(),
        #[cfg(target_os = "linux")]
        binds: platform::binds(),
        #[cfg(target_os = "linux")]
        autostart_source,
    }
}

#[tauri::command]
fn save_settings(app: tauri::AppHandle, value: settings::Settings) {
    use tauri::Emitter;
    let before = settings::get();
    #[cfg(windows)]
    platform::set_hotkey(&value.hotkey);
    if before.previews_on != value.previews_on {
        switcher::set_previews_pinned(value.previews_on);
    }
    settings::set(value.clone());
    // The GTK switcher re-themes itself (GTK calls belong on the main thread).
    #[cfg(target_os = "linux")]
    let _ = app.run_on_main_thread(platform::apply_settings);
    let _ = app.emit("settings", value);
}

#[tauri::command]
fn set_login(on: bool) -> bool {
    #[cfg(windows)]
    {
        platform::set_autostart(on);
        let now = platform::autostart();
        if let Some(item) = LOGIN_ITEM.get() {
            let _ = item.set_checked(now);
        }
        now
    }
    // niri starts keytab with `spawn-at-startup` in the user's config, which keytab
    // only reads: the page shows where, and the switch is read-only.
    #[cfg(target_os = "linux")]
    {
        let _ = on;
        platform::autostart()
    }
}

/// The current windows; on Linux an IPC failure reads as no windows here.
fn open_windows() -> Vec<keys::Win> {
    #[cfg(target_os = "linux")]
    return platform::list().unwrap_or_default();
    #[cfg(windows)]
    platform::list()
}

/// The Hints page: saved app hints and every open app's current hint.
#[tauri::command]
fn hint_apps() -> Vec<switcher::HintApp> {
    switcher::hint_apps(switcher::without_hidden(open_windows()))
}

/// One entry on the Switcher page's "Hidden windows" list.
#[derive(serde::Serialize)]
struct HiddenView {
    #[serde(flatten)]
    rule: settings::Hidden,
    /// Open windows it hides right now.
    windows: usize,
    /// An open window of the app, for its icon.
    icon: Option<keys::WinId>,
}

#[tauri::command]
fn hidden_windows() -> Vec<HiddenView> {
    let open = open_windows();
    settings::get().hidden.into_iter().map(|rule| {
        let windows = open.iter().filter(|w| rule.matches(&w.app, &w.title)).count();
        let icon = open.iter().find(|w| w.app.to_lowercase() == rule.app.to_lowercase()).map(|w| w.id);
        HiddenView { rule, windows, icon }
    }).collect()
}

/// Show a hidden window (or app) in the switcher again.
#[tauri::command]
fn unhide_window(app: tauri::AppHandle, rule: settings::Hidden) -> Vec<HiddenView> {
    use tauri::Emitter;
    let mut s = settings::get();
    s.hidden.retain(|h| *h != rule);
    settings::set(s.clone());
    let _ = app.emit("settings", s);
    hidden_windows()
}

/// Why this hint can't be saved, while typing it (None if it can).
#[tauri::command]
fn check_app_hint(app: String, hint: String) -> Option<String> {
    switcher::check_app_hint(&app, &hint)
}

#[tauri::command]
fn set_app_hint(app: String, hint: String) -> Result<Vec<switcher::HintApp>, String> {
    switcher::set_app_hint(&app, &hint)?;
    Ok(hint_apps())
}

/// Forget one saved app hint, or all of them (`app` = null).
#[tauri::command]
fn remove_app_hint(app: Option<String>) -> Vec<switcher::HintApp> {
    switcher::remove_app_hint(app.as_deref());
    hint_apps()
}

/// Opens the config folder, or a keytab page on GitHub, outside keytab.
#[tauri::command]
fn open_external(target: String) {
    let arg: std::ffi::OsString = if target == "config" {
        let dir = settings::config_dir();
        let _ = std::fs::create_dir_all(&dir);
        dir.into()
    } else if target.starts_with("https://github.com/HayzDev/keytab") {
        target.into()
    } else {
        return;
    };
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer.exe").arg(arg).spawn();
    #[cfg(target_os = "linux")]
    platform::open_external(arg);
}

#[tauri::command]
async fn open_settings(app: tauri::AppHandle) -> Result<(), String> {
    settings_window(&app).map_err(|e| e.to_string())
}

#[tauri::command]
async fn check_update(channel: settings::Channel) -> Result<updater::Info, String> {
    tauri::async_runtime::spawn_blocking(move || updater::check(channel)).await.map_err(|e| e.to_string())?
}

/// Downloads and starts the installer, then quits so it can replace keytab.
#[tauri::command]
async fn install_update(app: tauri::AppHandle, update: updater::Update) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || updater::install(&update)).await.map_err(|e| e.to_string())??;
    app.exit(0);
    Ok(())
}

/// Windows callers use an async command or task to avoid WebView2 deadlocks.
/// On Linux the native overlay and settings GTK widgets belong to the main thread.
fn settings_window(app: &tauri::AppHandle) -> tauri::Result<()> {
    #[cfg(target_os = "linux")]
    {
        let handle = app.clone();
        app.run_on_main_thread(move || {
            if let Err(e) = build_settings_window(&handle) {
                eprintln!("keytab: couldn't open settings: {e}");
            }
        })
    }
    #[cfg(windows)]
    build_settings_window(app)
}

fn build_settings_window(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::Manager;
    switcher::hide();
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.unminimize();
        w.show()?;
        return w.set_focus();
    }
    // Frameless glass like the switcher: the page draws its own top bar (drag, tabs, close).
    let w = tauri::WebviewWindowBuilder::new(app, "settings", tauri::WebviewUrl::App("settings.html".into()))
        .title("keytab settings")
        .inner_size(760.0, 560.0)
        .resizable(false)
        .maximizable(false)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .center()
        .visible(false)
        .build()?;
    platform::init_settings(&w)?;
    w.show()?;
    w.set_focus()
}

/// Open the settings window from anywhere (the switcher's gear, `--settings`).
pub(crate) fn show_settings() {
    if let Some(app) = APP.get().cloned() {
        tauri::async_runtime::spawn(async move {
            if let Err(e) = settings_window(&app) {
                eprintln!("keytab: couldn't open settings: {e}");
            }
        });
    }
}

/// At startup (release builds, when enabled): install an update for the chosen channel.
/// Waits a little for the network after login, and never interrupts an open switcher.
fn auto_update(app: tauri::AppHandle) {
    let s = settings::get();
    if !updater::auto_allowed() || !s.auto_update {
        return;
    }
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(10));
        let Ok(updater::Info { available: Some(update), .. }) = updater::check(s.channel) else { return };
        while switcher::is_open() {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        match updater::install(&update) {
            Ok(()) => app.exit(0),
            Err(e) => eprintln!("keytab: auto-update failed: {e}"),
        }
    });
}

static APP: std::sync::OnceLock<tauri::AppHandle> = std::sync::OnceLock::new();

#[cfg(windows)]
static LOGIN_ITEM: std::sync::OnceLock<tauri::menu::CheckMenuItem<tauri::Wry>> = std::sync::OnceLock::new();

/// `keytab --toggle` / `keytab --search` / `keytab --settings`, for compositor keybinds.
fn handle_args<S: AsRef<str>>(args: &[S]) {
    if args.iter().any(|a| a.as_ref() == "--search") {
        switcher::search();
    } else if args.iter().any(|a| a.as_ref() == "--toggle") {
        switcher::toggle();
    }
    if args.iter().any(|a| a.as_ref() == "--settings") {
        show_settings();
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
    let builder = builder.invoke_handler(tauri::generate_handler![
        activate, hide, key, ready, layout, select, close_window, edit_hint, hide_window, press, get_settings, save_settings,
        set_login, hint_apps, hidden_windows, unhide_window, check_app_hint, set_app_hint, remove_app_hint, open_external,
        open_settings, check_update, install_update
    ]);
    #[cfg(not(windows))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        activate, hide, key, ready, select, close_window, edit_hint, hide_window, press, get_settings, save_settings,
        set_login, hint_apps, hidden_windows, unhide_window, check_app_hint, set_app_hint, remove_app_hint, open_external,
        open_settings, check_update, install_update
    ]);
    // App icons for the settings page: icon://localhost/<window id> (http://icon.localhost/
    // on Windows). Linux looks them up through GTK, on its main thread.
    #[cfg(target_os = "linux")]
    let builder = builder.register_asynchronous_uri_scheme_protocol("icon", |_ctx, req, responder| {
        let id = req.uri().path().trim_start_matches('/').parse().ok();
        gtk::glib::MainContext::default().invoke(move || {
            let res = tauri::http::Response::builder();
            responder.respond(match id.and_then(platform::icon_png) {
                Some(png) => res.header("Content-Type", "image/png").body(png).unwrap(),
                None => res.status(404).body(Vec::new()).unwrap(),
            });
        });
    });
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
            #[cfg(windows)]
            platform::set_hotkey(&settings::get().hotkey);
            platform::init(&overlay)?;
            switcher::init(app.handle().clone(), overlay);
            #[cfg(windows)]
            tray(app)?;
            let _ = APP.set(app.handle().clone());
            platform::start_input();
            handle_args(&std::env::args().collect::<Vec<_>>());
            auto_update(app.handle().clone());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running keytab");
}

/// Windows-only tray: Settings (also a left click on the icon), Start at login and Quit.
/// (On niri: `keytab --quit`.)
#[cfg(windows)]
fn tray(app: &mut tauri::App) -> tauri::Result<()> {
    use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    fn open_settings(app: &tauri::AppHandle) {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = settings_window(&app) {
                eprintln!("keytab: couldn't open settings: {e}");
            }
        });
    }
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let login = CheckMenuItem::with_id(app, "login", "Start at login", true, platform::autostart(), None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit keytab", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&settings, &login, &sep, &quit])?;
    let _ = LOGIN_ITEM.set(login);
    TrayIconBuilder::new()
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("keytab — click for settings")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, e| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                open_settings(tray.app_handle());
            }
        })
        .on_menu_event(|app, e| match e.id.as_ref() {
            "settings" => open_settings(app),
            "login" => {
                set_login(!platform::autostart());
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;
    Ok(())
}
