//! The game window. Runs its own message loop on a dedicated thread.
//!
//! Keyboard input is forwarded as PC scancodes (set 1, with `E0` prefixes for extended
//! keys), exactly what the game's own IRQ1 handler reads from port 60h, so the original
//! keyboard code runs unchanged.
//!
//! Window modes: windowed (resizable), borderless fullscreen on the window's monitor (with
//! the flip-model swap chain it gets direct scan-out) and exclusive fullscreen (the same
//! borderless window, the swap chain then takes the display in the chosen display mode, by
//! default the desktop's; the renderer does that, see `Window::exclusive`). The settings
//! overlay sets the windowed size (`set_client_size`) and lists the monitor's modes
//! (`monitor`). Alt+Enter toggles between windowed and
//! the last fullscreen kind; that key combination is not passed to the game.

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BLACK_BRUSH, GetMonitorInfoW, GetStockObject, HBRUSH, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

pub type KeySink = Box<dyn Fn(u8) + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowMode {
    Windowed,
    /// Borderless window covering the whole monitor.
    Borderless,
    /// Exclusive fullscreen (DXGI), in the chosen display mode (default the desktop's).
    Exclusive,
}

impl WindowMode {
    fn code(self) -> usize {
        match self {
            WindowMode::Windowed => 0,
            WindowMode::Borderless => 1,
            WindowMode::Exclusive => 2,
        }
    }
}

const WM_SET_MODE: u32 = WM_APP + 1;
const WM_SET_SIZE: u32 = WM_APP + 2;

/// Switch a window created by `spawn` to another mode (any thread).
pub fn set_mode(hwnd: isize, mode: WindowMode) {
    unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_SET_MODE, WPARAM(mode.code()), LPARAM(0));
    }
}

/// Give a window created by `spawn` this client size when windowed (any thread; at once if
/// it is windowed now, else when it returns to windowed).
pub fn set_client_size(hwnd: isize, width: u32, height: u32) {
    unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_SET_SIZE, WPARAM(width as usize), LPARAM(height as isize));
    }
}

/// The window's current client size.
pub fn client_size(hwnd: isize) -> (u32, u32) {
    let mut r = RECT::default();
    let _ = unsafe { GetClientRect(HWND(hwnd as *mut _), &mut r) };
    ((r.right - r.left).max(0) as u32, (r.bottom - r.top).max(0) as u32)
}

/// What the monitor a window is on offers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Monitor {
    /// Its size in pixels (the desktop's resolution).
    pub size: (u32, u32),
    /// The largest client area a window fits in on it (the work area, without the taskbar,
    /// less the window frame).
    pub max_client: (u32, u32),
    /// The desktop's display mode: width, height, refresh rate (Hz).
    pub desktop: (u32, u32, u32),
    /// The display modes it offers (32-bit colour, progressive): width, height, refresh rate,
    /// largest first, each refresh rate from the highest.
    pub modes: Vec<(u32, u32, u32)>,
}

/// The monitor the window `hwnd` is on (`None`: no window or monitor information).
pub fn monitor(hwnd: isize) -> Option<Monitor> {
    use windows::Win32::Graphics::Gdi::{DEVMODEW, DM_INTERLACED, ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_FLAGS, ENUM_DISPLAY_SETTINGS_MODE, EnumDisplaySettingsExW, EnumDisplaySettingsW, MONITORINFOEXW};
    use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
    unsafe {
        let hwnd = HWND(hwnd as *mut _);
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if !GetMonitorInfoW(mon, &mut mi as *mut _ as *mut _).as_bool() {
            return None;
        }
        let (r, wk) = (mi.monitorInfo.rcMonitor, mi.monitorInfo.rcWork);
        let size = ((r.right - r.left) as u32, (r.bottom - r.top) as u32);
        let mut frame = RECT::default();
        let dpi = GetDpiForWindow(hwnd).max(96);
        let _ = AdjustWindowRectExForDpi(&mut frame, WINDOWED_STYLE, false, WINDOW_EX_STYLE(0), dpi);
        let max_client = (
            ((wk.right - wk.left) - (frame.right - frame.left)).max(1) as u32,
            ((wk.bottom - wk.top) - (frame.bottom - frame.top)).max(1) as u32,
        );
        let device = windows::core::PCWSTR(mi.szDevice.as_ptr());
        let mut dm = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
        let hz = if EnumDisplaySettingsW(device, ENUM_CURRENT_SETTINGS, &mut dm).as_bool() { dm.dmDisplayFrequency } else { 60 };
        let mut modes = Vec::new();
        for i in 0..4096 {
            let mut dm = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
            if !EnumDisplaySettingsExW(device, ENUM_DISPLAY_SETTINGS_MODE(i), &mut dm, ENUM_DISPLAY_SETTINGS_FLAGS(0)).as_bool() {
                break;
            }
            let interlaced = dm.Anonymous2.dmDisplayFlags & DM_INTERLACED.0 != 0;
            if dm.dmBitsPerPel == 32 && !interlaced && dm.dmDisplayFrequency > 1 {
                modes.push((dm.dmPelsWidth, dm.dmPelsHeight, dm.dmDisplayFrequency));
            }
        }
        modes.sort_by(|a, b| (b.0 as u64 * b.1 as u64, b.0, b.2).cmp(&(a.0 as u64 * a.1 as u64, a.0, a.2)));
        modes.dedup();
        Some(Monitor { size, max_client, desktop: (size.0, size.1, hz), modes })
    }
}

pub struct Window {
    pub hwnd: isize,
    /// Client size, packed as `width << 32 | height`.
    pub size: Arc<AtomicU64>,
    pub closed: Arc<AtomicBool>,
    /// The swap chain should hold the display exclusively now (exclusive mode and the
    /// application active); the renderer follows it.
    pub exclusive: Arc<AtomicBool>,
}

impl Window {
    pub fn client_size(&self) -> (u32, u32) {
        let v = self.size.load(Ordering::Relaxed);
        ((v >> 32) as u32, v as u32)
    }
}

struct WindowData {
    keys: KeySink,
    size: Arc<AtomicU64>,
    closed: Arc<AtomicBool>,
    borderless: Cell<bool>,
    /// Windowed placement to return to from borderless.
    placement: RefCell<Option<WINDOWPLACEMENT>>,
    /// Fullscreen means exclusive (else borderless); kept for Alt+Enter.
    exclusive_mode: Cell<bool>,
    exclusive: Arc<AtomicBool>,
}

const WINDOWED_STYLE: WINDOW_STYLE = WS_OVERLAPPEDWINDOW;
const VK_RETURN: u32 = 0x0d;

/// Switch between windowed and borderless fullscreen.
unsafe fn set_borderless(hwnd: HWND, data: &WindowData, on: bool) {
    unsafe {
        if on {
            let mut p = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
            if GetWindowPlacement(hwnd, &mut p).is_ok() {
                *data.placement.borrow_mut() = Some(p);
            }
            let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if !GetMonitorInfoW(mon, &mut mi).as_bool() {
                return;
            }
            let r = mi.rcMonitor;
            SetWindowLongW(hwnd, GWL_STYLE, (WS_POPUP | WS_VISIBLE).0 as i32);
            let _ = SetWindowPos(hwnd, Some(HWND_TOP), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_FRAMECHANGED | SWP_NOOWNERZORDER);
        } else {
            SetWindowLongW(hwnd, GWL_STYLE, (WINDOWED_STYLE | WS_VISIBLE).0 as i32);
            if let Some(p) = *data.placement.borrow() {
                let _ = SetWindowPlacement(hwnd, &p);
            }
            let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER);
        }
        data.borderless.set(on);
        data.exclusive.store(on && data.exclusive_mode.get(), Ordering::Relaxed);
        store_client_size(hwnd, data);
    }
}

/// Apply a mode from `WindowMode::code`.
unsafe fn apply_mode(hwnd: HWND, data: &WindowData, code: usize) {
    if code != 0 {
        data.exclusive_mode.set(code == 2);
    }
    // leaving exclusive for borderless: the renderer gives the display back first
    data.exclusive.store(false, Ordering::Relaxed);
    unsafe { set_borderless(hwnd, data, code != 0) };
}

/// Windowed client size `w` x `h`: now (moved as needed to stay inside the monitor's work
/// area), or while fullscreen for the return to windowed.
unsafe fn set_size(hwnd: HWND, data: &WindowData, w: i32, h: i32) {
    use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
    unsafe {
        let mut r = RECT { left: 0, top: 0, right: w, bottom: h };
        let _ = AdjustWindowRectExForDpi(&mut r, WINDOWED_STYLE, false, WINDOW_EX_STYLE(0), GetDpiForWindow(hwnd).max(96));
        let (fw, fh) = (r.right - r.left, r.bottom - r.top);
        if data.borderless.get() {
            if let Some(p) = data.placement.borrow_mut().as_mut() {
                let n = &mut p.rcNormalPosition;
                n.right = n.left + fw;
                n.bottom = n.top + fh;
                p.showCmd = SW_SHOWNORMAL.0 as u32;
            }
            return;
        }
        if IsZoomed(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let mut cur = RECT::default();
        let _ = GetWindowRect(hwnd, &mut cur);
        let (mut x, mut y) = (cur.left, cur.top);
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(mon, &mut mi).as_bool() {
            let wk = mi.rcWork;
            x = x.min(wk.right - fw).max(wk.left);
            y = y.min(wk.bottom - fh).max(wk.top);
        }
        let _ = SetWindowPos(hwnd, None, x, y, fw, fh, SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER);
        store_client_size(hwnd, data);
    }
}

/// Publish the current client size (WM_SIZE does too; this covers style changes).
unsafe fn store_client_size(hwnd: HWND, data: &WindowData) {
    let mut r = RECT::default();
    if unsafe { GetClientRect(hwnd, &mut r) }.is_ok() {
        let (w, h) = ((r.right - r.left).max(0) as u64, (r.bottom - r.top).max(0) as u64);
        data.size.store(w << 32 | h, Ordering::Relaxed);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let data = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as usize as *const WindowData;
    if data.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    }
    let data = unsafe { &*data };
    match msg {
        WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
            let vk = wp.0 as u32;
            // let Windows handle Alt+F4
            if msg == WM_SYSKEYDOWN && vk == 0x73 {
                return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
            }
            // Alt+Enter: toggle borderless fullscreen (Enter is not sent to the game)
            if vk == VK_RETURN && (msg == WM_SYSKEYDOWN || msg == WM_SYSKEYUP) {
                let repeat = (lp.0 >> 30) & 1 != 0;
                if msg == WM_SYSKEYDOWN && !repeat {
                    let code = if data.borderless.get() { 0 } else if data.exclusive_mode.get() { 2 } else { 1 };
                    unsafe { apply_mode(hwnd, data, code) };
                }
                return LRESULT(0);
            }
            let scan = ((lp.0 >> 16) & 0xff) as u8;
            let extended = (lp.0 >> 24) & 1 != 0;
            let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
            if scan != 0 {
                if extended {
                    (data.keys)(0xe0);
                }
                (data.keys)(if up { scan | 0x80 } else { scan });
            }
            LRESULT(0)
        }
        WM_SIZE => {
            let (w, h) = ((lp.0 & 0xffff) as u64, ((lp.0 >> 16) & 0xffff) as u64);
            data.size.store(w << 32 | h, Ordering::Relaxed);
            LRESULT(0)
        }
        // no mouse in the game: hide the pointer over the fullscreen picture
        WM_SETCURSOR if data.borderless.get() && (lp.0 & 0xffff) as u32 == HTCLIENT => {
            unsafe { SetCursor(None) };
            LRESULT(1)
        }
        // display mode or scale changed while fullscreen: cover the monitor again
        WM_DISPLAYCHANGE | WM_DPICHANGED if data.borderless.get() => {
            unsafe { set_borderless(hwnd, data, true) };
            LRESULT(0)
        }
        // moved to a monitor with another scale: take the size Windows suggests
        WM_DPICHANGED => {
            let r = unsafe { &*(lp.0 as *const RECT) };
            unsafe {
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            LRESULT(0)
        }
        // the settings overlay switches the window mode (wp: WindowMode::code)
        WM_SET_MODE => {
            unsafe { apply_mode(hwnd, data, wp.0) };
            LRESULT(0)
        }
        // the settings overlay sets the windowed client size (wp x lp)
        WM_SET_SIZE => {
            unsafe { set_size(hwnd, data, wp.0 as i32, lp.0 as i32) };
            LRESULT(0)
        }
        // exclusive fullscreen: give the display back while another application is active
        // (minimised), take it again when the game is activated
        WM_ACTIVATEAPP if data.borderless.get() && data.exclusive_mode.get() => {
            let active = wp.0 != 0;
            sr_runtime::debug!("display: the game {} (exclusive fullscreen)", if active { "is active again" } else { "is no longer active" });
            data.exclusive.store(active, Ordering::Relaxed);
            unsafe {
                if active {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                } else {
                    let _ = ShowWindow(hwnd, SW_MINIMIZE);
                }
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            data.closed.store(true, Ordering::Relaxed);
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// Opt in to per-monitor DPI awareness so the window is sized in real pixels.
pub fn init_dpi() {
    use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// Create the window (client area `width` x `height` when windowed) on its own thread.
pub fn spawn(title: &str, width: u32, height: u32, mode: WindowMode, keys: KeySink) -> Result<Window, String> {
    let size = Arc::new(AtomicU64::new((width as u64) << 32 | height as u64));
    let closed = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let exclusive = Arc::new(AtomicBool::new(false));
    let data = WindowData {
        keys,
        size: size.clone(),
        closed: closed.clone(),
        borderless: Cell::new(false),
        placement: RefCell::new(None),
        exclusive_mode: Cell::new(false),
        exclusive: exclusive.clone(),
    };
    std::thread::Builder::new()
        .name("window".into())
        .spawn(move || unsafe {
            let inst = GetModuleHandleW(None).unwrap_or_default();
            let class = w!("ScreamerRallyWindow");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: inst.into(),
                lpszClassName: class,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
                ..Default::default()
            };
            RegisterClassW(&wc);
            let style = WINDOWED_STYLE;
            let mut r = RECT { left: 0, top: 0, right: width as i32, bottom: height as i32 };
            let _ = AdjustWindowRectEx(&mut r, style, false, WINDOW_EX_STYLE(0));
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                PCWSTR(title.as_ptr()),
                style,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                r.right - r.left,
                r.bottom - r.top,
                None,
                None,
                Some(inst.into()),
                None,
            );
            let hwnd = match hwnd {
                Ok(h) => h,
                Err(e) => {
                    let _ = tx.send(Err(format!("CreateWindowExW failed: {e}")));
                    return;
                }
            };
            let data = Box::into_raw(Box::new(data));
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, data as usize as _);
            let _ = ShowWindow(hwnd, SW_SHOW);
            if mode != WindowMode::Windowed {
                apply_mode(hwnd, &*data, mode.code());
                if mode == WindowMode::Exclusive {
                    (*data).exclusive.store(true, Ordering::Relaxed);
                }
            }
            let _ = tx.send(Ok(hwnd.0 as isize));
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        })
        .map_err(|e| e.to_string())?;
    let hwnd = rx.recv().map_err(|e| e.to_string())??;
    Ok(Window { hwnd, size, closed, exclusive })
}
