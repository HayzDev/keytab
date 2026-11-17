# keytab

A keyboard-first Alt-Tab replacement (Rust + Tauri 2). Open it, type a window's letter(s), and that window gets focus.
Inspired by [Adam Basis](https://www.youtube.com/@AdamBasis)'s alt-tab videos.

## Keys

| Key | Does |
|---|---|
| **Right Alt** (Windows) / your niri bind | open / close |
| hold Right Alt + **Right Shift** / `keytab --search` | open in fuzzy search |
| letters | focus the window with that hint (two letters when first letters clash) |
| **Right Shift** (while open) | switch to search mode; press again to go back to hints |
| hold **Left Shift** | preview the selected window; release to focus a unique match |
| Backspace | undo a typed letter / query character |
| ↑ ↓ or **Alt+K / Alt+J** | move the selection in either mode |
| Enter | focus the selected window in either mode |
| Delete | close the selected window; in the hint dialog: reset to default |
| `\` | set a hint for this one window (lives as long as the window) |
| `\|` | set a hint for every window of this app (saved to disk) |
| Esc / CapsLock | close or go back |

Hints are prefix-free. Auto hints use the app's own letters (chrome + code → `ch`, `co`). User hints win, and the auto hints rearrange around them.
If several windows of one app share an app hint `x`, they become `x1`, `x2`, …

App hints are saved in `%APPDATA%\keytab\app-hints.json` on Windows and `~/.config/keytab/app-hints.json` on Linux. The file maps lowercase app name to hint.

## Build

```sh
cargo run              # dev
cargo build --release  # target/release/keytab(.exe)
cargo test
```

No Node/npm needed. Linux uses native GTK widgets; Windows uses `ui/index.html`.

### Fedora deps

```sh
sudo dnf install webkit2gtk4.1-devel openssl-devel librsvg2-devel gtk3-devel gtk-layer-shell-devel wayland-devel wayland-protocols-devel
sudo dnf install python3-gobject gstreamer1-plugins-base pipewire-gstreamer
sudo dnf group install c-development
```

## Linux / niri

Wayland has no global keyboard hook, so keytab works differently there:

1. A Niri keybind runs the lightweight `keytabctl --toggle` or `keytabctl --search`.
2. The helper sends the command to the resident process over D-Bus.
3. An already mapped, transparent GTK layer surface gains keyboard focus and renders
   the shared switcher state. Opening does not create another window or webview.

Install the binary and the supplied Niri configuration:

```sh
cargo build --release --locked
install -Dm755 target/release/keytab ~/.local/bin/keytab
install -Dm755 scripts/keytabctl ~/.local/bin/keytabctl
install -Dm644 niri.kdl ~/.config/niri/keytab.kdl
```

Add this line once to `~/.config/niri/config.kdl`:

```kdl
include "keytab.kdl"
```

Then reload and start Keytab for the current session:

```sh
niri validate
niri msg action load-config-file
~/.local/bin/keytab
```

**Win+Tab** opens hints; **Win+Shift+Tab** opens fuzzy search. Keytab starts hidden
on future Niri logins. The overlay uses the `keytab` layer namespace and does not
appear in Niri's normal window list or trigger window tiling/open animations.
The panel has a translucent background, a thin lighter outline, and compositor
blur clipped to its rounded bounds. Blur requires compositor support for
`ext-background-effect-v1` (available on this device); GTK layer shell 0.9 or newer
is required. The supplied layer rule disables wallpaper-only (xray) blur for Keytab.
Escape, selecting a window, or clicking the transparent area dismisses it.

Previews are snapshots loaded asynchronously through Niri’s PipeWire interface.
They stay in memory and leave the clipboard untouched. Recent snapshots are reused
immediately; snapshots older than five seconds refresh in the background on the
next Shift hold. Captures share one temporary session and scale on the GPU.
In normal mode, hints wait for Shift release while previews are shown; a unique
prefix is enough to focus. Shifted number-row symbols (`!` through `)`) count as
their digits in hints, so Shift+2 still enters `2`. Backspace widens the matches again. In search mode,
Shift only shows thumbnails; typing and Enter keep their normal behavior.

Quit with `keytabctl --quit` (there's no tray on Linux).

Window list, focus and close go through `niri msg --json windows` and `niri msg action focus-window|close-window --id N`.

### Status of the niri port

- Built and tested on Fedora 44 with Niri. The running compositor supplies
  `focus_timestamp`; older versions without it use focused-first, then Niri's order.
- Known gap vs Windows: the Windows version handles keys from the moment you press the hotkey. On Wayland, a key typed before the overlay has focus is lost.
- Known gap vs Windows: CapsLock still toggles caps on Wayland, because a focused app can't stop the compositor from seeing it.

## Layout / porting

### Windows look

- The overlay window is sized to the card (`layout` command) and gets an acrylic blur
  plus Windows 11 rounded corners.
- Apps show their real icon (`http://icon.localhost/<window id>`, from `WM_GETICON`)
  and a friendly name from the exe's version info ("Phone Link", not "PhoneExperienceHost").
- A trailing " - App" in titles is hidden, since the app name is already shown.
- Clicking outside the card closes it (low-level mouse hook).
- Debug builds take `--keys=^ab` to press keys after opening (`^` holds Shift), for screenshots.

### Windows previews

Holding Left Shift (Right Shift toggles search mode)
shows one large live preview of the selected window under the list; arrow keys change
it. It's a DWM thumbnail (`DwmRegisterThumbnail`) drawn over a box the webview lays out
and reports through the `layout` command, sized to the window's shape. A minimized
window shows its icon instead.
Shift is never swallowed by the hook, so the app behind just sees a bare Shift.

```
src/
  main.rs           Tauri wiring, `key`/`activate`/`hide` commands, --toggle/--search/--quit
  switcher.rs       platform-neutral state machine (abstract `Key`s), fuzzy search, hint dialogs, saved hints
  keys.rs           `Win` model + prefix-free hint assignment (pure, tested)
  platform/mod.rs   the backend contract
  platform/win32.rs Windows: EnumWindows, low-level keyboard hook, no-activate overlay
  platform/niri.rs  Niri IPC and persistent layer surface
  platform/gtk_ui.rs native Linux UI and keyboard events
  platform/material.c Wayland compositor blur region
scripts/keytabctl    lightweight Linux D-Bus command client
scripts/thumbnail.py embedded one-shot PipeWire capture helper (Python/GStreamer)
ui/index.html       Windows UI
```

To port to another platform or compositor:

1. Add `src/platform/<name>.rs` implementing the functions listed in `platform/mod.rs`:
   - `GLOBAL_INPUT`
   - `list`, `focus`, `close`, `alive`
   - `show`, `hide`
   - `start_input`
2. Add a `cfg` line for it in `platform/mod.rs`.

Translate input to `switcher::Key` and call `switcher::on_key`. Platforms without
a global hook set `GLOBAL_INPUT = false` and deliver keys from the focused overlay.
Render the shared view using the platform's UI backend.
