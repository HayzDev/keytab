//! niri (Wayland) backend, via the `niri msg` CLI. See README "Linux / niri".
//!
//! Wayland has no global keyboard hook, so input works differently from Windows:
//! a niri keybind runs `keytab --toggle` (or `--search`), the single-instance plugin
//! forwards that to the running keytab, the overlay takes keyboard focus, and the
//! webview forwards key events to `switcher::on_key` (GLOBAL_INPUT = false).
use crate::keys::{capitalize, Win, WinId};
use serde::Deserialize;
use std::cmp::Reverse;
use std::process::Command;
use tauri::WebviewWindow;

pub const GLOBAL_INPUT: bool = false;

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
pub fn list() -> Vec<Win> {
    let Some(json) = niri(&["--json", "windows"]) else { return Vec::new() };
    let mut wins: Vec<NiriWindow> = serde_json::from_str(&json).unwrap_or_else(|e| {
        eprintln!("keytab: unexpected `niri msg --json windows` output: {e}");
        Vec::new()
    });
    let me = std::process::id() as i32;
    wins.retain(|w| w.pid != Some(me));
    wins.sort_by_key(|w| Reverse((w.is_focused, timestamp(&w.focus_timestamp))));
    wins.into_iter()
        .map(|w| {
            let title = w.title.unwrap_or_default();
            // "org.gnome.Nautilus" -> "Nautilus", "firefox" -> "Firefox"
            let app = match w.app_id.as_deref() {
                Some(id) if !id.is_empty() => capitalize(id.rsplit('.').next().unwrap_or(id)),
                _ => title.clone(),
            };
            Win::new(w.id as WinId, app, title)
        })
        .collect()
}

pub fn focus(id: WinId) {
    niri(&["action", "focus-window", "--id", &id.to_string()]);
}

/// Like clicking X: the app may still ask to save.
pub fn close(id: WinId) {
    niri(&["action", "close-window", "--id", &id.to_string()]);
}

/// ponytail: niri never reuses window ids, so a stale per-window hint is harmless (it
/// just never matches again). Prune against list() if the map ever grows large.
pub fn alive(_id: WinId) -> bool {
    true
}

/// The overlay needs real keyboard focus here (no global hook). Needs a niri window
/// rule to float it; see README.
pub fn show(overlay: &WebviewWindow) {
    let _ = overlay.show();
    let _ = overlay.set_focus();
}

pub fn hide(overlay: &WebviewWindow) {
    let _ = overlay.hide();
}

/// Nothing to install: keys come from the webview via the `key` command.
pub fn start_input() {}
