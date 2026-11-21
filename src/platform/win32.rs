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
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::Mutex;
use windows::core::{s, w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, LRESULT, RECT, TRUE, WPARAM};
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
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
    // Before GetWindowTextW: for our own windows it sends WM_GETTEXT to the UI thread and
    // waits, and this runs on the hook thread.
    let mut pid = 0u32;
    GetWindowThreadProcessId(h, Some(&mut pid));
    if pid == skip_pid {
        return None;
    }
    let mut buf = [0u16; 512];
    let n = GetWindowTextW(h, &mut buf);
    if n <= 0 {
        return None;
    }
    let title = String::from_utf16_lossy(&buf[..n as usize]);
    // UWP apps all live in ApplicationFrameHost; their title is the better name.
    let app = match exe_path(pid) {
        Some(p) if !p.to_lowercase().ends_with("\\applicationframehost.exe") => web_app_name(h).unwrap_or_else(|| app_name(&p)),
        _ => title.clone(),
    };
    Some(Win::new(h.0 as WinId, app, title))
}

/// An installed web app (PWA) runs in its browser's process, so the exe would call it "Vivaldi"
/// and it would share Vivaldi's app hint. Chromium browsers give PWA windows their own
/// AppUserModelID ("Vivaldi._crx_<id>"); Start's Apps folder knows its name ("Claude Duet").
unsafe fn web_app_name(h: HWND) -> Option<String> {
    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
    use windows::Win32::UI::Shell::{FOLDERID_AppsFolder, IShellItem, SHCreateItemInKnownFolder, KF_FLAG_DEFAULT, SIGDN_NORMALDISPLAY};
    const PKEY_APP_USER_MODEL_ID: PROPERTYKEY = PROPERTYKEY { fmtid: windows::core::GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3), pid: 5 };
    static CACHE: Mutex<BTreeMap<String, Option<String>>> = Mutex::new(BTreeMap::new());

    let store: IPropertyStore = SHGetPropertyStoreForWindow(h).ok()?;
    let id = store.GetValue(&PKEY_APP_USER_MODEL_ID).ok()?.to_string();
    if !id.contains("._crx_") {
        return None;
    }
    if let Some(name) = CACHE.lock().unwrap().get(&id) {
        return name.clone();
    }
    // Already-initialized (either apartment) is fine; the shell item just needs COM.
    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    let wide: Vec<u16> = id.encode_utf16().chain([0]).collect();
    let name = SHCreateItemInKnownFolder::<_, IShellItem>(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, PCWSTR(wide.as_ptr()))
        .and_then(|item| item.GetDisplayName(SIGDN_NORMALDISPLAY))
        .ok()
        .map(|p| {
            let s = p.to_string().unwrap_or_default();
            CoTaskMemFree(Some(p.0 as _));
            s
        })
        .filter(|s| !s.is_empty() && *s != id);
    CACHE.lock().unwrap().insert(id, name.clone());
    name
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
    OVERLAY.store(h.0 as isize, Ordering::Relaxed);
    unsafe {
        // Clicks (rows, footer keys) must not activate the overlay: the window you were in
        // keeps focus until you pick another, exactly as with the keyboard.
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
        SetWindowLongPtrW(h, GWL_EXSTYLE, ex | WS_EX_NOACTIVATE.0 as isize);
        create_frame(h);
    }
    glass(h);
    Ok(())
}

/// Rounded corners and the acrylic blur, for the overlay and the settings window. Either
/// window's page paints its tint (`--card`) over the blur.
fn glass(h: HWND) {
    unsafe {
        let round = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(h, DWMWA_WINDOW_CORNER_PREFERENCE, &round as *const _ as _, size_of_val(&round) as u32);
        let Ok(user32) = GetModuleHandleW(w!("user32.dll")) else { return };
        let Some(f) = GetProcAddress(user32, s!("SetWindowCompositionAttribute")) else { return };
        let f: unsafe extern "system" fn(HWND, *mut CompositionData) -> BOOL = std::mem::transmute(f);
        // ACCENT_ENABLE_ACRYLICBLURBEHIND, tint 0xAABBGGRR; the CSS card adds the real tint.
        let mut accent = AccentPolicy { state: 4, flags: 0, color: 0x20141210, animation: 0 };
        let mut data = CompositionData { attribute: 19, data: &mut accent as *mut _ as _, size: size_of::<AccentPolicy>() };
        let _ = f(h, &mut data);
    }
}

/// The settings window: the overlay's glass, no title bar (the page draws its own).
pub fn init_settings(w: &WebviewWindow) -> tauri::Result<()> {
    glass(HWND(w.hwnd()?.0 as _));
    Ok(())
}

/// The overlay's HWND, cached by `init`. Everything below runs on the hook thread or other
/// background threads, so it must never call Tauri window getters (`hwnd()`,
/// `scale_factor()`, ...): those post to the UI thread and wait for the answer. A hook thread
/// waiting on a busy or deadlocked UI thread stalls every keypress and mouse move on the PC,
/// and Windows then kills the hung app. Plain Win32 calls are safe from any thread.
static OVERLAY: AtomicIsize = AtomicIsize::new(0);

fn overlay() -> HWND {
    HWND(OVERLAY.load(Ordering::Relaxed) as *mut _)
}

/// Topmost, without taking focus: the hook already gets the keys. Async positioning and
/// showing, because this runs inside the hook callback and the window belongs to the UI thread.
pub fn show(_overlay: &WebviewWindow) {
    let h = overlay();
    OPEN.store(true, Ordering::Relaxed);
    unsafe {
        // Centered sideways on the active window's monitor, top edge at 22% of its height.
        // The height follows the card (`layout`), so it grows downward from there.
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let mut win = RECT::default();
        let _ = GetMonitorInfoW(MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY), &mut mi);
        let _ = GetWindowRect(h, &mut win);
        let w = mi.rcWork;
        let x = w.left + (w.right - w.left - (win.right - win.left)) / 2;
        let y = w.top + (w.bottom - w.top) * 22 / 100;
        let _ = SetWindowPos(h, Some(HWND_TOPMOST), x, y, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS);
        let _ = ShowWindowAsync(h, SW_SHOWNOACTIVATE);
    }
}

pub fn hide(_overlay: &WebviewWindow) {
    OPEN.store(false, Ordering::Relaxed);
    layout(None, &[]);
    unsafe {
        let _ = ShowWindowAsync(overlay(), SW_HIDE);
    }
}

/// The preview's own window. A DWM thumbnail is a plain rectangle that nothing in the webview
/// can clip, so it's drawn into this borderless popup (owned by the overlay, so it stays above
/// it) and Windows 11 rounds the popup's corners. Created on the UI thread by `init`.
static FRAME: AtomicIsize = AtomicIsize::new(0);

fn frame() -> HWND {
    HWND(FRAME.load(Ordering::Relaxed) as *mut _)
}

unsafe extern "system" fn frame_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(h, msg, wp, lp)
}

unsafe fn create_frame(owner: HWND) {
    let Ok(inst) = GetModuleHandleW(None) else { return };
    let class = w!("keytab-preview");
    RegisterClassW(&WNDCLASSW {
        lpfnWndProc: Some(frame_proc),
        hInstance: inst.into(),
        lpszClassName: class,
        hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
        ..Default::default()
    });
    let ex = WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST;
    let Ok(f) = CreateWindowExW(ex, class, w!(""), WS_POPUP, 0, 0, 1, 1, Some(owner), None, Some(inst.into()), None) else { return };
    let small = DWMWCP_ROUNDSMALL;
    let _ = DwmSetWindowAttribute(f, DWMWA_WINDOW_CORNER_PREFERENCE, &small as *const _ as _, size_of_val(&small) as u32);
    // No system border: the CSS ring around the preview box is the frame.
    let none = 0xFFFFFFFEu32; // DWMWA_COLOR_NONE
    let _ = DwmSetWindowAttribute(f, DWMWA_BORDER_COLOR, &none as *const _ as _, 4);
    FRAME.store(f.0 as isize, Ordering::Relaxed);
}

/// Live DWM thumbnails, by window id. One per preview box.
static THUMBS: Mutex<Vec<(WinId, isize)>> = Mutex::new(Vec::new());

/// The webview's layout, in CSS px: the card (the window is sized to it, keeping its top
/// left corner) and the preview boxes, each filled with a cropped live thumbnail. Returns each
/// preview's aspect ratio (0 if it has nothing to show, e.g. minimized).
pub fn layout(card: Option<(f64, f64, f64, f64)>, rects: &[(WinId, f64, f64, f64, f64)]) -> Vec<(WinId, f64)> {
    let mut thumbs = THUMBS.lock().unwrap();
    thumbs.retain(|&(id, t)| {
        let keep = rects.iter().any(|r| r.0 == id);
        if !keep {
            let _ = unsafe { DwmUnregisterThumbnail(t) };
        }
        keep
    });
    let dest = overlay();
    let scale = match unsafe { GetDpiForWindow(dest) } {
        0 => 1.0,
        dpi => dpi as f64 / 96.0,
    };
    let px = |v: f64| (v * scale).round() as i32;

    if let Some((_, _, w, h)) = card {
        let mut win = RECT::default();
        unsafe {
            let _ = SetWindowPos(dest, None, 0, 0, px(w), px(h), SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
            let _ = GetWindowRect(dest, &mut win);
        }
        *CARD.lock().unwrap() = win;
    }

    let fr = frame();
    let mut origin = RECT::default();
    unsafe {
        let _ = GetWindowRect(dest, &mut origin);
    }
    let mut out = Vec::new();
    let mut shown = false;
    for &(id, x, y, w, h) in rects {
        let src = hwnd(id);
        if unsafe { IsIconic(src).as_bool() } {
            out.push((id, 0.0));
            continue;
        }
        let t = match thumbs.iter().find(|t| t.0 == id) {
            Some(t) => t.1,
            None => match unsafe { DwmRegisterThumbnail(fr, src) } {
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
            rcDestination: RECT { left: 0, top: 0, right: px(w), bottom: px(h) },
            rcSource: RECT { left: cx as i32, top: sy as i32, right: (cx + cw) as i32, bottom: (sy + ch) as i32 },
            fVisible: true.into(),
            ..Default::default()
        };
        unsafe {
            let _ = DwmUpdateThumbnailProperties(t, &props);
            let flags = SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_ASYNCWINDOWPOS;
            let _ = SetWindowPos(fr, Some(HWND_TOPMOST), origin.left + px(x), origin.top + px(y), px(w), px(h), flags);
        }
        shown = true;
        out.push((id, sw / sh));
    }
    if !shown {
        unsafe {
            let _ = ShowWindowAsync(fr, SW_HIDE);
        }
    }
    out
}

// ---- input -----------------------------------------------------------------------

/// The key that opens the switcher (settings `hotkey`); Right Alt by default.
static ACTIVATE_VK: AtomicU32 = AtomicU32::new(VK_RMENU.0 as u32);

/// Apply the `hotkey` setting. Unknown names fall back to Right Alt.
pub fn set_hotkey(name: &str) {
    let vk = match name {
        "RightCtrl" => VK_RCONTROL,
        "RightWin" => VK_RWIN,
        "Apps" => VK_APPS,
        "CapsLock" => VK_CAPITAL,
        _ => VK_RMENU,
    };
    ACTIVATE_VK.store(vk.0 as u32, Ordering::Relaxed);
}

/// Virtual-key code -> Key. The hotkey activates, Right Shift (while held) searches.
fn translate(vk: u32) -> Key {
    if vk == ACTIVATE_VK.load(Ordering::Relaxed) {
        return Key::Activate;
    }
    match VIRTUAL_KEY(vk as u16) {
        VK_RSHIFT => Key::Search,
        VK_LSHIFT => Key::Preview,
        VK_ESCAPE | VK_CAPITAL => Key::Esc,
        VK_BACK => Key::Back,
        VK_RETURN => Key::Enter,
        VK_UP => Key::Up,
        VK_DOWN => Key::Down,
        VK_DELETE => Key::Delete,
        VK_LMENU => Key::Alt,
        VK_LCONTROL | VK_RCONTROL => Key::Ctrl,
        VK_SPACE => Key::Char(' '),
        VK_OEM_MINUS => Key::Char('-'),
        VK_OEM_PERIOD => Key::Char('.'),
        _ if matches!(vk, 0x30..=0x39 | 0x41..=0x5A) => Key::Char((vk as u8 as char).to_ascii_lowercase()),
        _ if types_char(vk, '|') => Key::HintApp,
        _ if types_char(vk, '/') => Key::Slash,
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
        // AltGr layouts send a fake Left Ctrl (scan code 0x21D) with Right Alt; it must not
        // count as Ctrl (which selects the current window).
        let altgr_ctrl = kb.vkCode == VK_LCONTROL.0 as u32 && kb.scanCode & 0x200 != 0;
        // The `demo` feature (for recording the README demo) accepts scripted keystrokes.
        let injected = !cfg!(feature = "demo") && kb.flags.0 & LLKHF_INJECTED.0 != 0;
        if !injected && !altgr_ctrl {
            let key = translate(kb.vkCode);
            // Shift always passes through: a bare Shift is harmless to the app behind, and
            // swallowing it would hide it from GetAsyncKeyState (`|` detection).
            // If Windows saw the hotkey go down without us (pressed while keytab was starting,
            // or let through by a hook timeout), let its release through too, or the key stays
            // stuck down for every app and every later press of it is swallowed as well.
            // The hook runs before the key state updates, so this is the state before this event.
            let leaked = key == Key::Activate && !down && GetAsyncKeyState(kb.vkCode as i32) < 0;
            if on_key(key, down) && !leaked && !matches!(key, Key::Preview | Key::Search) {
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

// ---- start at login ---------------------------------------------------------------

/// `HKCU\...\Run` value that starts keytab at login. The installer sets it on first install.
const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const RUN_VALUE: PCWSTR = w!("keytab");

/// Where the installer puts keytab (per-user NSIS install), for restarting after an update.
pub fn installed_exe() -> std::path::PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(|d| std::path::PathBuf::from(d).join("keytab").join("keytab.exe"))
        .filter(|p| p.exists())
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_default()
}

pub fn autostart() -> bool {
    use windows::Win32::System::Registry::*;
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ, None, None, None).is_ok() }
}

pub fn set_autostart(on: bool) {
    use windows::Win32::System::Registry::*;
    let mut key = HKEY::default();
    unsafe {
        if RegCreateKeyW(HKEY_CURRENT_USER, RUN_KEY, &mut key).is_err() {
            return;
        }
        if on {
            let Ok(exe) = std::env::current_exe() else { return };
            let cmd: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain([0]).collect();
            let bytes = std::slice::from_raw_parts(cmd.as_ptr() as *const u8, cmd.len() * 2);
            let _ = RegSetValueExW(key, RUN_VALUE, None, REG_SZ, Some(bytes));
        } else {
            let _ = RegDeleteValueW(key, RUN_VALUE);
        }
        let _ = RegCloseKey(key);
    }
}
