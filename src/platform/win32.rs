//! Windows backend: EnumWindows for the list, a low-level keyboard hook for input.
//!
//! All keys are handled in the hook against a snapshot taken the instant the overlay
//! opens, so typing before it paints never drops keys. The overlay is shown without
//! activation; the webview only renders.
use crate::keys::{capitalize, Win, WinId};
use crate::switcher::{on_key, Key};
use tauri::WebviewWindow;
use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, LRESULT, TRUE, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// We see every key system-wide, so the overlay never needs focus.
pub const GLOBAL_INPUT: bool = true;

fn hwnd(id: WinId) -> HWND {
    HWND(id as isize as *mut _)
}

// ---- windows ---------------------------------------------------------------------

/// Alt-Tab-eligible windows in z-order (most recently used first). Keys unassigned.
pub fn list() -> Vec<Win> {
    let mut hwnds: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut hwnds as *mut _ as isize));
    }
    let me = std::process::id();
    hwnds.into_iter().filter_map(|h| unsafe { info(h, me) }).collect()
}

unsafe extern "system" fn collect(h: HWND, lp: LPARAM) -> BOOL {
    (*(lp.0 as *mut Vec<HWND>)).push(h);
    TRUE
}

/// Same heuristic Alt-Tab uses: visible, unowned (or app-window), not a tool window, not cloaked.
unsafe fn info(h: HWND, skip_pid: u32) -> Option<Win> {
    if !IsWindowVisible(h).as_bool() {
        return None;
    }
    let ex = GetWindowLongPtrW(h, GWL_EXSTYLE) as u32;
    let app_window = ex & WS_EX_APPWINDOW.0 != 0;
    if !app_window {
        if ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) != 0 {
            return None;
        }
        if GetWindow(h, GW_OWNER).is_ok_and(|o| !o.is_invalid()) {
            return None;
        }
    }
    // Cloaked = suspended UWP frames, windows on other virtual desktops, etc.
    let mut cloaked = 0u32;
    let _ = DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, 4);
    if cloaked != 0 {
        return None;
    }
    let mut buf = [0u16; 512];
    let n = GetWindowTextW(h, &mut buf);
    if n <= 0 {
        return None;
    }
    let title = String::from_utf16_lossy(&buf[..n as usize]);
    let mut pid = 0u32;
    GetWindowThreadProcessId(h, Some(&mut pid));
    if pid == skip_pid {
        return None;
    }
    // UWP apps all live in ApplicationFrameHost; their title is the better name.
    let app = match exe_name(pid) {
        Some(e) if !e.eq_ignore_ascii_case("ApplicationFrameHost") => capitalize(&e),
        _ => title.clone(),
    };
    Some(Win::new(h.0 as WinId, app, title))
}

unsafe fn exe_name(pid: u32) -> Option<String> {
    let p = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let r = QueryFullProcessImageNameW(p, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
    let _ = CloseHandle(p);
    r.ok()?;
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    Some(std::path::Path::new(&path).file_stem()?.to_string_lossy().into_owned())
}

pub fn focus(id: WinId) {
    unsafe {
        let h = hwnd(id);
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        // Windows only lets the process that sent the last input steal focus.
        // An empty mouse input makes that us (same trick PowerToys uses).
        let input = INPUT { r#type: INPUT_MOUSE, ..Default::default() };
        SendInput(&[input], size_of::<INPUT>() as i32);
        let _ = SetForegroundWindow(h);
    }
}

/// Politely close (WM_CLOSE), like clicking X: the app may still ask to save.
pub fn close(id: WinId) {
    unsafe {
        let _ = PostMessageW(Some(hwnd(id)), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
}

pub fn alive(id: WinId) -> bool {
    unsafe { IsWindow(Some(hwnd(id))).as_bool() }
}

// ---- overlay ---------------------------------------------------------------------

/// Topmost, without taking focus: the hook already gets the keys. Async because this
/// runs inside the hook callback, which must not block on the UI thread.
pub fn show(overlay: &WebviewWindow) {
    let Ok(h) = overlay.hwnd() else { return };
    let h = HWND(h.0 as *mut _);
    unsafe {
        let _ = SetWindowPos(h, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        let _ = ShowWindowAsync(h, SW_SHOWNOACTIVATE);
    }
}

pub fn hide(overlay: &WebviewWindow) {
    let Ok(h) = overlay.hwnd() else { return };
    unsafe {
        let _ = ShowWindowAsync(HWND(h.0 as *mut _), SW_HIDE);
    }
}

// ---- input -----------------------------------------------------------------------

/// Virtual-key code -> Key. Right Alt activates, Right Shift (while held) searches.
fn translate(vk: u32) -> Key {
    match VIRTUAL_KEY(vk as u16) {
        VK_RMENU => Key::Activate,
        VK_RSHIFT => Key::Search,
        VK_ESCAPE | VK_CAPITAL => Key::Esc,
        VK_BACK => Key::Back,
        VK_RETURN => Key::Enter,
        VK_UP => Key::Up,
        VK_DOWN => Key::Down,
        VK_DELETE => Key::Delete,
        VK_LMENU => Key::Alt,
        VK_SPACE => Key::Char(' '),
        VK_OEM_MINUS => Key::Char('-'),
        VK_OEM_PERIOD => Key::Char('.'),
        _ if matches!(vk, 0x30..=0x39 | 0x41..=0x5A) => Key::Char((vk as u8 as char).to_ascii_lowercase()),
        _ if types_char(vk, '|') => Key::HintApp,
        _ if types_char(vk, '\\') => Key::HintWindow,
        _ => Key::Other,
    }
}

/// Does this key, with the current Shift state, type `ch` in the foreground window's
/// keyboard layout? By character, not position (`\` is VK_OEM_5 on US, VK_OEM_102 on ISO).
fn types_char(vk: u32, ch: char) -> bool {
    unsafe {
        let thread = GetWindowThreadProcessId(GetForegroundWindow(), None);
        let scan = VkKeyScanExW(ch as u16, GetKeyboardLayout(thread)) as u16;
        let shift = GetAsyncKeyState(VK_SHIFT.0 as i32) < 0;
        // Low byte = virtual key; high byte = modifiers (1 shift, 2 ctrl, 4 alt). Shift only.
        scan != 0xFFFF && (scan & 0xFF) as u32 == vk && scan & 0x600 == 0 && (scan & 0x100 != 0) == shift
    }
}

unsafe extern "system" fn hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let kb = &*(lp.0 as *const KBDLLHOOKSTRUCT);
        let msg = wp.0 as u32;
        let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        if kb.flags.0 & LLKHF_INJECTED.0 == 0 && on_key(translate(kb.vkCode), down) {
            return LRESULT(1);
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

/// Install the low-level keyboard hook on its own thread with a message loop.
pub fn start_input() {
    std::thread::spawn(|| unsafe {
        let module = GetModuleHandleW(None).expect("module handle");
        let _hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), Some(module.into()), 0)
            .expect("install keyboard hook");
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
    });
}
