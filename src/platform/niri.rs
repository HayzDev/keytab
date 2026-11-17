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
    overlay.gtk_window()?.connect_destroy(move |_| window.close());
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
