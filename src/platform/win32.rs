//! Windows backend: EnumWindows for the list, a low-level keyboard hook for input.
//!
//! All keys are handled in the hook against a snapshot taken the instant the overlay
//! opens, so typing before it paints never drops keys. The overlay is shown without
//! activation; the webview only renders.
use crate::keys::{capitalize, Win, WinId};
use crate::switcher::{on_key, Key};
use tauri::WebviewWindow;
use windows::core::{BOOL, PWSTR};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use windows::core::{s, w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, LRESULT, RECT, TRUE, WPARAM};
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
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
    let app = match exe_path(pid) {
        Some(p) if !p.to_lowercase().ends_with("\\applicationframehost.exe") => app_name(&p),
        _ => title.clone(),
    };
    Some(Win::new(h.0 as WinId, app, title))
}

unsafe fn exe_path(pid: u32) -> Option<String> {
    let p = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let r = QueryFullProcessImageNameW(p, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
    let _ = CloseHandle(p);
    r.ok()?;
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

/// "Phone Link", not "PhoneExperienceHost"; "Explorer", not "Windows Explorer": the exe's ProductName or FileDescription when
/// it reads like a name, else the capitalized file stem. Cached per exe path.
fn app_name(path: &str) -> String {
    static CACHE: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());
    if let Some(n) = CACHE.lock().unwrap().get(path) {
        return n.clone();
    }
    let stem = std::path::Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let name = version_strings(path)
        .into_iter()
        .map(|s| s.replace(['®', '™'], "").replace("Microsoft ", "").trim().trim_start_matches("Windows ").to_string())
        .find(|s| !s.is_empty() && s.chars().count() <= 28 && !s.contains("Operating System"))
        .unwrap_or_else(|| capitalize(&stem));
    CACHE.lock().unwrap().insert(path.into(), name.clone());
    name
}

/// ProductName and FileDescription from the exe's version resource.
fn version_strings(path: &str) -> Vec<String> {
    unsafe {
        let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
        let file = PCWSTR(wide.as_ptr());
        let size = GetFileVersionInfoSizeW(file, None);
        if size == 0 {
            return vec![];
        }
        let mut data = vec![0u8; size as usize];
        if GetFileVersionInfoW(file, None, size, data.as_mut_ptr() as _).is_err() {
            return vec![];
        }
        let query = |sub: &str| -> Option<(*const u16, u32)> {
            let sub: Vec<u16> = sub.encode_utf16().chain([0]).collect();
            let (mut ptr, mut len) = (std::ptr::null_mut(), 0u32);
            VerQueryValueW(data.as_ptr() as _, PCWSTR(sub.as_ptr()), &mut ptr, &mut len)
                .as_bool()
                .then_some((ptr as *const u16, len))
        };
        let Some((t, _)) = query("\\VarFileInfo\\Translation").filter(|&(_, n)| n >= 4) else { return vec![] };
        let (lang, cp) = (*t, *t.add(1));
        ["ProductName", "FileDescription"]
            .iter()
            .filter_map(|k| {
                let (p, n) = query(&format!("\\StringFileInfo\\{lang:04x}{cp:04x}\\{k}"))?;
                let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n as usize));
                Some(s.trim_end_matches('\0').to_string())
            })
            .collect()
    }
}

/// The window's icon as PNG, for the `icon` URI scheme. None for windows without one
/// (most UWP frames); the UI shows a monogram then.
pub fn icon_png(id: WinId) -> Option<Vec<u8>> {
    unsafe {
        let h = hwnd(id);
        let hicon = [ICON_BIG, ICON_SMALL2]
            .into_iter()
            .find_map(|kind| {
                let mut r = 0usize;
                SendMessageTimeoutW(h, WM_GETICON, WPARAM(kind as usize), LPARAM(0), SMTO_ABORTIFHUNG, 100, Some(&mut r));
                (r != 0).then(|| HICON(r as _))
            })
            .or_else(|| {
                let c = GetClassLongPtrW(h, GCLP_HICON);
                (c != 0).then(|| HICON(c as _))
            })?;
        let mut ii = ICONINFO::default();
        GetIconInfo(hicon, &mut ii).ok()?;
        let png = bitmap_png(ii.hbmColor);
        let _ = DeleteObject(ii.hbmColor.into());
        let _ = DeleteObject(ii.hbmMask.into());
        png
    }
}

unsafe fn bitmap_png(bmp: HBITMAP) -> Option<Vec<u8>> {
    if bmp.is_invalid() {
        return None;
    }
    let mut bm = BITMAP::default();
    if GetObjectW(bmp.into(), size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as _)) == 0 {
        return None;
    }
    let (w, h) = (bm.bmWidth, bm.bmHeight);
    let mut bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut px = vec![0u8; (w * h * 4) as usize];
    let dc = GetDC(None);
    let n = GetDIBits(dc, bmp, 0, h as u32, Some(px.as_mut_ptr() as _), &mut bi, DIB_RGB_COLORS);
    ReleaseDC(None, dc);
    if n == 0 {
        return None;
    }
    // ponytail: icons without alpha are shown fully opaque (mask ignored); rare today.
    let no_alpha = px.chunks(4).all(|p| p[3] == 0);
    for p in px.chunks_mut(4) {
        p.swap(0, 2);
        if no_alpha {
            p[3] = 255;
        }
    }
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w as u32, h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().ok()?.write_image_data(&px).ok()?;
    Some(out)
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

/// Is the overlay showing, and where it is on screen (for the mouse hook).
static OPEN: AtomicBool = AtomicBool::new(false);
static CARD: Mutex<RECT> = Mutex::new(RECT { left: 0, top: 0, right: 0, bottom: 0 });

#[repr(C)]
struct AccentPolicy {
    state: u32,
    flags: u32,
    color: u32,
    animation: u32,
}

#[repr(C)]
struct CompositionData {
    attribute: u32,
    data: *mut std::ffi::c_void,
    size: usize,
}

/// Acrylic blur behind the transparent webview. It's the accent-policy kind (as used by
/// the taskbar), which keeps blurring while the window is inactive; DWM system backdrops
/// fall back to a flat color, and the overlay is never active. The blur ignores window
/// regions, so `layout` sizes the window to the card and Windows 11 rounds its corners.
pub fn init(overlay: &WebviewWindow) -> tauri::Result<()> {
    let h = HWND(overlay.hwnd()?.0 as _);
    unsafe {
        let round = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(h, DWMWA_WINDOW_CORNER_PREFERENCE, &round as *const _ as _, size_of_val(&round) as u32);
        let Ok(user32) = GetModuleHandleW(w!("user32.dll")) else { return Ok(()) };
        let Some(f) = GetProcAddress(user32, s!("SetWindowCompositionAttribute")) else { return Ok(()) };
        let f: unsafe extern "system" fn(HWND, *mut CompositionData) -> BOOL = std::mem::transmute(f);
        // ACCENT_ENABLE_ACRYLICBLURBEHIND, tint 0xAABBGGRR; the CSS card adds the real tint.
        let mut accent = AccentPolicy { state: 4, flags: 0, color: 0x20141210, animation: 0 };
        let mut data = CompositionData { attribute: 19, data: &mut accent as *mut _ as _, size: size_of::<AccentPolicy>() };
        let _ = f(h, &mut data);
    }
    Ok(())
}

/// Topmost, without taking focus: the hook already gets the keys. Async because this
/// runs inside the hook callback, which must not block on the UI thread.
pub fn show(overlay: &WebviewWindow) {
    let Ok(h) = overlay.hwnd() else { return };
    let h = HWND(h.0 as *mut _);
    OPEN.store(true, Ordering::Relaxed);
    unsafe {
        let _ = SetWindowPos(h, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        let _ = ShowWindowAsync(h, SW_SHOWNOACTIVATE);
    }
}

pub fn hide(overlay: &WebviewWindow) {
    OPEN.store(false, Ordering::Relaxed);
    layout(overlay, None, &[]);
    let Ok(h) = overlay.hwnd() else { return };
    unsafe {
        let _ = ShowWindowAsync(HWND(h.0 as *mut _), SW_HIDE);
    }
}

/// Live DWM thumbnails, by window id. One per preview box.
static THUMBS: Mutex<Vec<(WinId, isize)>> = Mutex::new(Vec::new());

/// The webview's layout, in CSS px: the card (the window is sized to it, keeping its top
/// left corner) and the preview boxes, each filled with a cropped live thumbnail. Returns each
/// preview's aspect ratio (0 if it has nothing to show, e.g. minimized).
pub fn layout(overlay: &WebviewWindow, card: Option<(f64, f64, f64, f64)>, rects: &[(WinId, f64, f64, f64, f64)]) -> Vec<(WinId, f64)> {
    let mut thumbs = THUMBS.lock().unwrap();
    thumbs.retain(|&(id, t)| {
        let keep = rects.iter().any(|r| r.0 == id);
        if !keep {
            let _ = unsafe { DwmUnregisterThumbnail(t) };
        }
        keep
    });
    let Ok(dest) = overlay.hwnd() else { return vec![] };
    let dest = HWND(dest.0 as *mut _);
    let scale = overlay.scale_factor().unwrap_or(1.0);
    let px = |v: f64| (v * scale).round() as i32;

    if let Some((_, _, w, h)) = card {
        let mut win = RECT::default();
        unsafe {
            let _ = SetWindowPos(dest, None, 0, 0, px(w), px(h), SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
            let _ = GetWindowRect(dest, &mut win);
        }
        *CARD.lock().unwrap() = win;
    }

    let mut out = Vec::new();
    for &(id, x, y, w, h) in rects {
        let src = hwnd(id);
        if unsafe { IsIconic(src).as_bool() } {
            out.push((id, 0.0));
            continue;
        }
        let t = match thumbs.iter().find(|t| t.0 == id) {
            Some(t) => t.1,
            None => match unsafe { DwmRegisterThumbnail(dest, src) } {
                Ok(t) => {
                    thumbs.push((id, t));
                    t
                }
                Err(_) => continue,
            },
        };
        // Visible bounds, without the invisible resize borders, in window coordinates.
        let (mut win, mut vis) = (RECT::default(), RECT::default());
        unsafe {
            let _ = GetWindowRect(src, &mut win);
            if DwmGetWindowAttribute(src, DWMWA_EXTENDED_FRAME_BOUNDS, &mut vis as *mut _ as _, size_of::<RECT>() as u32).is_err() {
                vis = win;
            }
        }
        let (sx, sy) = ((vis.left - win.left) as f64, (vis.top - win.top) as f64);
        let (sw, sh) = ((vis.right - vis.left) as f64, (vis.bottom - vis.top) as f64);
        if sw < 1.0 || sh < 1.0 {
            continue;
        }
        // Cover: scale to fill the box, crop the overflow (centered sideways, kept at the top).
        let (bw, bh) = (w * scale, h * scale);
        let fit = (bw / sw).max(bh / sh);
        let (cw, ch) = (bw / fit, bh / fit);
        let cx = sx + (sw - cw) / 2.0;
        let props = DWM_THUMBNAIL_PROPERTIES {
            dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_RECTSOURCE | DWM_TNP_VISIBLE,
            rcDestination: RECT { left: px(x), top: px(y), right: px(x + w), bottom: px(y + h) },
            rcSource: RECT { left: cx as i32, top: sy as i32, right: (cx + cw) as i32, bottom: (sy + ch) as i32 },
            fVisible: true.into(),
            ..Default::default()
        };
        let _ = unsafe { DwmUpdateThumbnailProperties(t, &props) };
        out.push((id, sw / sh));
    }
    out
}

// ---- input -----------------------------------------------------------------------

/// Virtual-key code -> Key. Right Alt activates, Right Shift (while held) searches.
fn translate(vk: u32) -> Key {
    match VIRTUAL_KEY(vk as u16) {
        VK_RMENU => Key::Activate,
        VK_RSHIFT => Key::Search,
        VK_LSHIFT => Key::Preview,
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
        if kb.flags.0 & LLKHF_INJECTED.0 == 0 {
            let key = translate(kb.vkCode);
            // Shift always passes through: a bare Shift is harmless to the app behind, and
            // swallowing it would hide it from GetAsyncKeyState (`|` detection).
            if on_key(key, down) && !matches!(key, Key::Preview | Key::Search) {
                return LRESULT(1);
            }
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

/// A click outside the card closes the overlay (the click still goes through).
unsafe extern "system" fn mouse(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32
        && OPEN.load(Ordering::Relaxed)
        && matches!(wp.0 as u32, WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN)
    {
        let pt = (*(lp.0 as *const MSLLHOOKSTRUCT)).pt;
        let r = *CARD.lock().unwrap();
        if !(pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom) {
            std::thread::spawn(crate::switcher::hide);
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

/// Install the low-level keyboard and mouse hooks on their own thread with a message loop.
pub fn start_input() {
    std::thread::spawn(|| unsafe {
        let module = GetModuleHandleW(None).expect("module handle");
        let _hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), Some(module.into()), 0)
            .expect("install keyboard hook");
        let _mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse), Some(module.into()), 0)
            .expect("install mouse hook");
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
    });
}
