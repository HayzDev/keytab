//! Everything OS-specific lives behind this module. A backend provides:
//!
//! - `GLOBAL_INPUT: bool`: true if `start_input` sees keys system-wide (Windows hook).
//!   false means the overlay takes focus and the webview forwards keys instead.
//! - `list() -> Vec<Win>`: switchable windows, most recently used first, excluding keytab.
//! - `focus(WinId)`, `close(WinId)`, `alive(WinId) -> bool`
//! - `show(&WebviewWindow)`, `hide(&WebviewWindow)`: the overlay.
//! - `start_input()`: begin delivering keys to `switcher::on_key` (may do nothing).
//!
//! To port to a new platform, add a file implementing these and a `cfg` line below.

#[cfg(windows)]
mod win32;
#[cfg(windows)]
pub use win32::*;

#[cfg(target_os = "linux")]
mod niri;
#[cfg(target_os = "linux")]
pub use niri::*;
