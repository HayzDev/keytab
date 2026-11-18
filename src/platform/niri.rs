//! niri (Wayland) backend, via the `niri msg` CLI. See README "Linux / niri".
//!
//! Wayland has no global keyboard hook, so input works differently from Windows:
//! a niri keybind runs `keytab --toggle` (or `--search`), the single-instance plugin
//! forwards that to the running keytab; native GTK widgets render the shared state
//! and send keyboard events to `switcher::on_key` (GLOBAL_INPUT = false).
use crate::keys::{capitalize, Win, WinId};
use serde::Deserialize;
use std::cmp::Reverse;
use std::process::Command;
use super::Overlay;
use gtk_layer_shell::{KeyboardMode, Layer, LayerShell};
use gtk::prelude::*;

pub const GLOBAL_INPUT: bool = false;

extern "C" {
    fn keytab_material(window: *mut gtk::ffi::GtkWindow, x: i32, y: i32, width: i32, height: i32);
    fn keytab_commit(window: *mut gtk::ffi::GtkWindow);
    fn keytab_blur_available() -> gtk::glib::ffi::gboolean;
    fn keytab_window_blur(window: *mut gtk::ffi::GtkWindow, width: i32, height: i32, radius: i32);
    fn keytab_window_blur_release(window: *mut gtk::ffi::GtkWindow);
}

/// The compositor blurs behind surfaces (checked once, at startup).
static BLUR: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn blur_available() -> bool {
    BLUR.load(std::sync::atomic::Ordering::Relaxed)
}

/// Reuse the plugin's D-Bus endpoint before initializing GTK/WebKit in the client.
pub fn forward_args() -> bool {
    let Ok(bus) = zbus::blocking::Connection::session() else { return false };
    bus.call_method(
        Some("dev.keytab.app.SingleInstance"),
        "/dev/keytab/app/SingleInstance",
        Some("org.SingleInstance.DBus"),
        "ExecuteCallback",
        &(std::env::args().collect::<Vec<_>>(), String::new()),
    ).is_ok()
}

/// Configure the hidden GTK window before its first map.
pub fn init(overlay: &Overlay) -> tauri::Result<()> {
    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    window.set_decorated(false);
    window.set_default_size(640, 720);
    window.set_focus_on_map(false);
    if let Some(visual) = WidgetExt::screen(&window).and_then(|s| s.rgba_visual()) {
        window.set_visual(Some(&visual));
    }
    window.set_app_paintable(false);
    let css = gtk::CssProvider::new();
    css.load_from_data(b"window { background-color: transparent; }").expect("overlay CSS");
    window.style_context().add_provider(&css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    window.init_layer_shell();
    window.set_namespace("keytab");
    window.set_layer(Layer::Overlay);
    window.set_keyboard_mode(KeyboardMode::None);
    window.set_accept_focus(true);
    super::gtk_ui::init(&window);
    assert!(window.is_layer_window(), "keytab: layer-shell initialization failed");
    // Keep the renderer and Wayland surface warm; the empty panel is transparent.
    window.show_all();
    // GTK replaces a pre-realization input shape while mapping the window.
    window.input_shape_combine_region(Some(&gtk::cairo::Region::create()));
    material(&window, 0, 0, 0, 0);
    BLUR.store(unsafe { keytab_blur_available() } != 0, std::sync::atomic::Ordering::Relaxed);
    overlay.gtk_window()?.connect_destroy(move |_| window.close());
    watch_color_scheme();
    Ok(())
}

fn material_rect(x: i32, y: i32, width: i32, height: i32) -> [i32; 4] {
    let (x, y) = (x.clamp(0, 640), y.clamp(0, 720));
    [x, y, width.clamp(0, 640 - x), height.clamp(0, 720 - y)]
}

pub fn material(window: &gtk::Window, x: i32, y: i32, width: i32, height: i32) {
    let [x, y, width, height] = material_rect(x, y, width, height);
    unsafe { keytab_material(window.as_ptr() as _, x, y, width, height) };
}

/// Raw niri app ids by window, from the last `list()`, for app icons.
static APP_IDS: std::sync::Mutex<Vec<(WinId, String)>> = std::sync::Mutex::new(Vec::new());

pub fn app_id(id: WinId) -> Option<String> {
    APP_IDS.lock().unwrap().iter().find(|(w, _)| *w == id).map(|(_, a)| a.clone())
}

#[derive(Deserialize)]
struct NiriWindow {
    id: u64,
    title: Option<String>,
    app_id: Option<String>,
    pid: Option<i32>,
    #[serde(default)]
    is_focused: bool,
    /// Only in newer niri releases. Kept loose (any JSON) so a format change can't break
    /// parsing of the whole list; used for most-recently-used order when it's there.
    #[serde(default)]
    focus_timestamp: serde_json::Value,
}

/// `{"secs": s, "nanos": n}` or a plain number -> sortable; anything else -> 0.
fn timestamp(v: &serde_json::Value) -> (u64, u64) {
    let num = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    v.as_u64().map_or((num("secs"), num("nanos")), |n| (n, 0))
}

fn niri(args: &[&str]) -> Option<String> {
    let out = Command::new("niri").arg("msg").args(args).output().ok()?;
    if !out.status.success() {
        eprintln!("keytab: niri msg {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// All windows, focused / most recently focused first. Keys unassigned.
/// None means IPC failed; it must not be mistaken for all windows having closed.
pub fn list() -> Option<Vec<Win>> {
    let json = niri(&["--json", "windows"])?;
    let mut wins: Vec<NiriWindow> = serde_json::from_str(&json).map_err(|e| {
        eprintln!("keytab: unexpected `niri msg --json windows` output: {e}");
    }).ok()?;
    let me = std::process::id() as i32;
    wins.retain(|w| w.pid != Some(me));
    wins.sort_by_key(|w| Reverse((w.is_focused, timestamp(&w.focus_timestamp))));
    *APP_IDS.lock().unwrap() = wins.iter()
        .filter_map(|w| Some((w.id as WinId, w.app_id.clone().filter(|a| !a.is_empty())?)))
        .collect();
    Some(wins.into_iter()
        .map(|w| {
            let title = w.title.unwrap_or_default();
            // "org.gnome.Nautilus" -> "Nautilus", "firefox" -> "Firefox"
            let app = match w.app_id.as_deref() {
                Some(id) if !id.is_empty() => capitalize(id.rsplit('.').next().unwrap_or(id)),
                _ => title.clone(),
            };
            Win::new(w.id as WinId, app, title)
        })
        .collect())
}

pub fn window_hint_session() -> Option<&'static str> {
    static SESSION: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    SESSION.get_or_init(|| {
        let socket = std::env::var("NIRI_SOCKET").ok()?;
        let pid: u32 = std::path::Path::new(&socket).file_stem()?.to_str()?.rsplit('.').next()?.parse().ok()?;
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // Field 22, after the parenthesized command and fields 3 through 21.
        let start: u64 = stat.rsplit_once(')')?.1.split_whitespace().nth(19)?.parse().ok()?;
        Some(format!("{}:{pid}:{start}:{socket}", boot.trim()))
    }).as_deref()
}

pub fn focus(id: WinId) {
    niri(&["action", "focus-window", "--id", &id.to_string()]);
}

/// Like clicking X: the app may still ask to save.
pub fn close(id: WinId) {
    niri(&["action", "close-window", "--id", &id.to_string()]);
}

/// Layer-shell grants keyboard focus without creating a managed app window.
pub fn show(_overlay: &Overlay) {
    let window = super::gtk_ui::window();
    window.input_shape_combine_region(None);
    window.set_keyboard_mode(KeyboardMode::Exclusive);
    unsafe { keytab_commit(window.as_ptr() as _) };
}

pub fn hide(_overlay: &Overlay) {
    let window = super::gtk_ui::window();
    window.set_keyboard_mode(KeyboardMode::None);
    window.input_shape_combine_region(Some(&gtk::cairo::Region::create()));
    material(&window, 0, 0, 0, 0);
    unsafe { keytab_commit(window.as_ptr() as _) };
}

/// Native GTK key handlers are installed by `init`.
pub fn start_input() {}

/// Settings changed: re-theme the switcher. Main thread.
pub fn apply_settings() {
    super::gtk_ui::apply_settings();
}

// ---- settings window ------------------------------------------------------------------

/// The settings window: compositor blur behind its rounded card, like the switcher.
pub fn init_settings(w: &tauri::WebviewWindow) -> tauri::Result<()> {
    if !blur_available() { return Ok(()); }
    let window = w.gtk_window()?;
    // Window rules may size it differently from what it asked for: follow every allocation,
    // but only while mapped, when its wl_surface exists.
    let blur = |window: &gtk::Window| unsafe {
        keytab_window_blur(window.as_ptr() as _, window.allocated_width(), window.allocated_height(), 8)
    };
    window.connect_map_event(move |window, _| {
        blur(window.upcast_ref());
        gtk::glib::Propagation::Proceed
    });
    window.connect_size_allocate(move |window, _| {
        if window.is_mapped() { blur(window.upcast_ref()); }
    });
    window.connect_unmap(|window| unsafe { keytab_window_blur_release(window.upcast_ref::<gtk::Window>().as_ptr() as _) });
    Ok(())
}

/// The niri config folder: `$XDG_CONFIG_HOME/niri` or `~/.config/niri`.
fn niri_dir() -> std::path::PathBuf {
    let env = |k| std::env::var_os(k).filter(|v| !v.is_empty()).map(std::path::PathBuf::from);
    env("XDG_CONFIG_HOME").or_else(|| env("HOME").map(|h| h.join(".config"))).unwrap_or_default().join("niri")
}

/// The niri config files keytab reads (never writes): `$NIRI_CONFIG` or `config.kdl`, and
/// every `.kdl` file in the niri folder and its direct subfolders (`include`d files).
fn niri_configs() -> Vec<std::path::PathBuf> {
    let dir = niri_dir();
    let mut files: Vec<std::path::PathBuf> = std::env::var_os("NIRI_CONFIG").map(Into::into).into_iter().collect();
    let kdl = |d: &std::path::Path| -> Vec<std::path::PathBuf> {
        let mut v: Vec<_> = std::fs::read_dir(d).into_iter().flatten().flatten().map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "kdl") && p.is_file()).collect();
        v.sort();
        v
    };
    // keytab.kdl first: it's where the supplied binds live.
    files.extend(kdl(&dir).into_iter().filter(|p| p.ends_with("keytab.kdl")));
    files.extend(kdl(&dir).into_iter().filter(|p| !p.ends_with("keytab.kdl")));
    for sub in std::fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
        files.extend(kdl(&sub));
    }
    files.dedup();
    files
}

/// A niri keybind that runs keytab, for the settings window (read-only).
#[derive(serde::Serialize, Clone, Debug, PartialEq)]
pub struct Bind {
    /// "F13", "Mod+Tab".
    pub keys: String,
    /// "open", "search" or "settings".
    pub action: &'static str,
    /// The config file it's in.
    pub file: String,
}

/// What a KDL config says about keytab: its keybinds, and the file that starts it at login.
fn scan_config(text: &str, file: &str, binds: &mut Vec<Bind>, startup: &mut Option<String>) {
    let mut block_key: Option<String> = None;
    let mut in_comment = false;
    for raw in text.lines() {
        // Block comments (/* */) and line comments; good enough for niri configs.
        let mut line = raw.to_string();
        if in_comment {
            match line.find("*/") { Some(i) => { line = line[i + 2..].to_string(); in_comment = false; } None => continue }
        }
        while let Some(i) = line.find("/*") {
            match line[i..].find("*/") {
                Some(j) => line.replace_range(i..i + j + 2, ""),
                None => { line.truncate(i); in_comment = true; }
            }
        }
        if let Some(i) = line.find("//") { line.truncate(i); }
        let line = line.trim();
        if line.is_empty() || line.starts_with("/-") { continue; }
        let first = line.split_whitespace().next().unwrap_or("");
        let runs_keytab = line.contains("keytab\"") || line.contains("keytabctl") || line.contains("keytab ");
        if first.starts_with("spawn") && first.ends_with("-at-startup") {
            if runs_keytab && startup.is_none() && !line.contains("--") { *startup = Some(file.into()); }
            continue;
        }
        if line.ends_with('{') && !first.starts_with("spawn") {
            block_key = Some(first.into());
        }
        if !runs_keytab { continue; }
        let action = if line.contains("--search") { "search" } else if line.contains("--settings") { "settings" }
            else if line.contains("--toggle") { "open" } else { continue };
        let keys = if first.starts_with("spawn") { block_key.clone() } else { Some(first.to_string()) };
        if let Some(keys) = keys.filter(|k| !k.contains('"') && k != "binds") {
            if !binds.iter().any(|b| b.keys == keys) {
                binds.push(Bind { keys, action, file: file.into() });
            }
        }
        if line.contains('}') && !line.ends_with('{') { block_key = None; }
    }
}

fn config_summary() -> (Vec<Bind>, Option<String>) {
    let (mut binds, mut startup) = (Vec::new(), None);
    for path in niri_configs() {
        if let Ok(text) = std::fs::read_to_string(&path) {
            scan_config(&text, &path.display().to_string(), &mut binds, &mut startup);
        }
    }
    (binds, startup)
}

/// The niri keybinds that run keytab. keytab never edits them.
pub fn binds() -> Vec<Bind> {
    config_summary().0
}

/// The niri config file whose `spawn-at-startup` starts keytab, if any.
pub fn autostart_source() -> Option<String> {
    config_summary().1
}

pub fn autostart() -> bool {
    autostart_source().is_some()
}

/// The config folder or a keytab page on GitHub, in the user's file manager or browser.
pub fn open_external(target: std::ffi::OsString) {
    match Command::new("xdg-open").arg(target).spawn() {
        Ok(mut child) => { std::thread::spawn(move || child.wait()); }
        Err(e) => eprintln!("keytab: couldn't run xdg-open: {e}"),
    }
}

/// The app's icon as PNG, for the settings window's `icon://localhost/<window id>`.
/// Must run on the GTK main thread.
pub fn icon_png(id: WinId) -> Option<Vec<u8>> {
    let icon = super::gtk_ui::icon_for(app_id(id).as_deref(), "")?;
    let theme = gtk::IconTheme::default()?;
    let info = theme.lookup_by_gicon(&icon, 40, gtk::IconLookupFlags::FORCE_SIZE)?;
    info.load_icon().ok()?.save_to_bufferv("png", &[]).ok()
}

// ---- theme ----------------------------------------------------------------------------

/// The desktop's color scheme from the XDG settings portal: 1 dark, 2 light, else unknown.
static SCHEME: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// "light" or "dark" when the desktop prefers one.
pub fn system_scheme() -> Option<&'static str> {
    match SCHEME.load(std::sync::atomic::Ordering::Relaxed) { 1 => Some("dark"), 2 => Some("light"), _ => None }
}

fn scheme_value(v: &zbus::zvariant::Value<'_>) -> Option<u32> {
    match v {
        zbus::zvariant::Value::U32(n) => Some(*n),
        zbus::zvariant::Value::Value(inner) => scheme_value(inner),
        _ => None,
    }
}

/// Follow the portal's `color-scheme` in the background; the switcher re-themes on change.
fn watch_color_scheme() {
    std::thread::spawn(|| {
        use zbus::zvariant::OwnedValue;
        let Ok(bus) = zbus::blocking::Connection::session() else { return };
        let set = |n: u32| {
            if SCHEME.swap(n, std::sync::atomic::Ordering::Relaxed) != n {
                gtk::glib::idle_add_once(super::gtk_ui::apply_settings);
            }
        };
        let rule = zbus::MatchRule::builder().msg_type(zbus::message::Type::Signal)
            .interface("org.freedesktop.portal.Settings").and_then(|r| r.member("SettingChanged")).map(|r| r.build());
        // Subscribe before reading, so a change in between isn't missed.
        let changes = rule.ok().and_then(|r| zbus::blocking::MessageIterator::for_match_rule(r, &bus, Some(8)).ok());
        let read = |method: &str| bus.call_method(Some("org.freedesktop.portal.Desktop"), "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.portal.Settings"), method, &("org.freedesktop.appearance", "color-scheme")).ok()
            .and_then(|m| m.body().deserialize::<OwnedValue>().ok())
            .and_then(|v| scheme_value(&v));
        if let Some(n) = read("ReadOne").or_else(|| read("Read")) { set(n); }
        for msg in changes.into_iter().flatten().flatten() {
            if let Ok((ns, key, value)) = msg.body().deserialize::<(String, String, OwnedValue)>() {
                if ns == "org.freedesktop.appearance" && key == "color-scheme" {
                    if let Some(n) = scheme_value(&value) { set(n); }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clips_material_to_surface() {
        assert_eq!(material_rect(24, 250, 592, 220), [24, 250, 592, 220]);
        assert_eq!(material_rect(-10, -10, i32::MAX, i32::MAX), [0, 0, 640, 720]);
        assert_eq!(material_rect(700, 900, 100, 100), [640, 720, 0, 0]);
        assert_eq!(material_rect(0, 0, -1, -1), [0, 0, 0, 0]);
    }

    #[test]
    fn finds_keytab_binds_and_autostart_without_editing_the_config() {
        let config = r#"
spawn-at-startup "~/.local/bin/keytab"
// spawn-at-startup "something-else"
binds {
    F13 repeat=false hotkey-overlay-title="Keytab" { spawn "~/.local/bin/keytabctl" "--toggle"; }
    // Alt+Tab { spawn "~/.local/bin/keytabctl" "--toggle"; }
    Mod+Shift+Tab repeat=false { spawn "~/.local/bin/keytabctl" "--search"; }
    Mod+Comma {
        spawn "keytabctl" "--settings";
    }
    /* Mod+X { spawn "keytabctl" "--toggle"; } */
    Mod+T { spawn "alacritty"; }
}
"#;
        let (mut binds, mut startup) = (Vec::new(), None);
        scan_config(config, "keytab.kdl", &mut binds, &mut startup);
        let found: Vec<_> = binds.iter().map(|b| (b.keys.as_str(), b.action)).collect();
        assert_eq!(found, [("F13", "open"), ("Mod+Shift+Tab", "search"), ("Mod+Comma", "settings")]);
        assert_eq!(startup.as_deref(), Some("keytab.kdl"));
        let (mut binds, mut startup) = (Vec::new(), None);
        scan_config("// spawn-at-startup \"keytab\"\nspawn-at-startup \"waybar\"", "config.kdl", &mut binds, &mut startup);
        assert!(binds.is_empty() && startup.is_none());
    }

    #[test]
    fn reads_portal_color_schemes() {
        use zbus::zvariant::Value;
        assert_eq!(scheme_value(&Value::U32(2)), Some(2));
        assert_eq!(scheme_value(&Value::Value(Box::new(Value::U32(1)))), Some(1));
        assert_eq!(scheme_value(&Value::Str("dark".into())), None);
    }

    #[test]
    fn parses_niri_focus_timestamps() {
        let w: NiriWindow = serde_json::from_str(r#"{
            "id":29,"title":"Keytab","app_id":"dev.keytab.app","pid":123,
            "is_focused":true,"focus_timestamp":{"secs":10266,"nanos":232048883}
        }"#).unwrap();
        assert!(w.is_focused);
        assert_eq!(timestamp(&w.focus_timestamp), (10266, 232048883));
        assert_eq!(timestamp(&serde_json::json!(42)), (42, 0));
        assert_eq!(timestamp(&serde_json::Value::Null), (0, 0));
    }
}
