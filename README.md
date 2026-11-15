# keytab

A keyboard-first Alt-Tab replacement (Rust + Tauri 2). Open it, type a window's letter(s), and that window gets focus.
Inspired by [Adam Basis](https://www.youtube.com/@AdamBasis)'s alt-tab videos.

## Keys

| Key | Does |
|---|---|
| **Right Alt** (Windows) / your niri bind | open / close |
| hold Right Alt + **Right Shift** / `keytab --search` | open in fuzzy search |
| letters | focus the window with that hint (two letters when first letters clash) |
| Backspace | undo a typed letter / query character |
| ↑ ↓ or **Alt+K / Alt+J** | move the selection (search) |
| Enter | focus the selected window (search) |
| Delete | close the selected window (search); in the hint dialog: reset to default |
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

No Node/npm needed: the UI is a single static `ui/index.html`.

### Fedora deps

```sh
sudo dnf install webkit2gtk4.1-devel openssl-devel librsvg2-devel gtk3-devel
sudo dnf group install c-development
```

## Linux / niri

Wayland has no global keyboard hook, so keytab works differently there:

1. A niri keybind runs `keytab --toggle` or `keytab --search`.
2. The single-instance plugin passes that to the already-running keytab.
3. The overlay takes real keyboard focus, and the webview forwards keys into the same state machine.

Add to `~/.config/niri/config.kdl`:

```kdl
spawn-at-startup "keytab"

binds {
    Mod+Tab       { spawn "keytab" "--toggle"; }
    Mod+Shift+Tab { spawn "keytab" "--search"; }
}

window-rule {
    // Check the real app-id with `niri msg windows` while keytab is open.
    match app-id="keytab"
    open-floating true
    border { off; }
    focus-ring { off; }
    shadow { off; }
}
```

Quit with `keytab --quit` (there's no tray on Linux).

Window list, focus and close go through `niri msg --json windows` and `niri msg action focus-window|close-window --id N`.

### Status of the niri port

- The Linux code compiles. The niri backend has **not been run on a real niri session yet**.
- Things to verify first:
  1. The app-id in the window rule.
  2. That transparency looks right.
  3. That `focus_timestamp` exists in your niri version. Without it the list is focused-first, then niri's order instead of most-recently-used.
- Known gap vs Windows: the Windows version handles keys from the moment you press the hotkey. On Wayland, a key typed before the overlay has focus is lost.
- Known gap vs Windows: CapsLock still toggles caps on Wayland, because a focused app can't stop the compositor from seeing it.

## Layout / porting

```
src/
  main.rs           Tauri wiring, `key`/`activate`/`hide` commands, --toggle/--search/--quit
  switcher.rs       platform-neutral state machine (abstract `Key`s), fuzzy search, hint dialogs, saved hints
  keys.rs           `Win` model + prefix-free hint assignment (pure, tested)
  platform/mod.rs   the backend contract
  platform/win32.rs Windows: EnumWindows, low-level keyboard hook, no-activate overlay
  platform/niri.rs  niri: `niri msg` IPC, overlay takes focus, keys come from the webview
ui/index.html       the whole UI
```

To port to another platform or compositor:

1. Add `src/platform/<name>.rs` implementing the functions listed in `platform/mod.rs`:
   - `GLOBAL_INPUT`
   - `list`, `focus`, `close`, `alive`
   - `show`, `hide`
   - `start_input`
2. Add a `cfg` line for it in `platform/mod.rs`.

Nothing else should need to change.
- If the platform can see keys globally (like Windows), translate them to `switcher::Key` and call `switcher::on_key` from `start_input`.
- If it can't (Wayland), set `GLOBAL_INPUT = false`; the webview forwards keys automatically.
