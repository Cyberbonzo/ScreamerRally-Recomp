//! The editor's Win32 window: a resizable top-level window whose mouse and keyboard input
//! becomes [`Event`]s (never passed to the game), placed next to the game's window.

use std::cell::{Cell, RefCell};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow, ScreenToClient, ValidateRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_CONTROL, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::editor::{Btn, Event, Pointer};

const STYLE: WINDOW_STYLE = WS_OVERLAPPEDWINDOW;
const MK_CONTROL: usize = 0x0008;
const WM_MOUSELEAVE: u32 = 0x02a3;
const MK_BUTTONS: usize = 0x0001 | 0x0002 | 0x0010;

struct Data {
    events: RefCell<Vec<Event>>,
    close: Cell<bool>,
    tracking: Cell<bool>,
    pointer: Cell<Pointer>,
    /// The window must be drawn again (WM_PAINT).
    paint: Cell<bool>,
}

pub struct EditorWindow {
    pub hwnd: HWND,
    data: *mut Data,
}

fn utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn xy(lp: LPARAM) -> (f64, f64) {
    ((lp.0 & 0xffff) as u16 as i16 as f64, ((lp.0 >> 16) & 0xffff) as u16 as i16 as f64)
}

fn key_down(vk: u16) -> bool {
    unsafe { GetKeyState(vk as i32) < 0 }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let data = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as usize as *const Data;
    if data.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    }
    let d = unsafe { &*data };
    let push = |e: Event| d.events.borrow_mut().push(e);
    match msg {
        WM_MOUSEMOVE => {
            if !d.tracking.get() {
                let mut t = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                unsafe {
                    let _ = TrackMouseEvent(&mut t);
                }
                d.tracking.set(true);
            }
            let (x, y) = xy(lp);
            push(Event::Move(x, y));
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            d.tracking.set(false);
            push(Event::Leave);
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            let btn = match msg {
                WM_LBUTTONDOWN => Btn::Left,
                WM_RBUTTONDOWN => Btn::Right,
                _ => Btn::Middle,
            };
            unsafe {
                SetCapture(hwnd);
            }
            let (x, y) = xy(lp);
            push(Event::Down { btn, x, y, ctrl: wp.0 & MK_CONTROL != 0 });
            LRESULT(0)
        }
        WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP => {
            let btn = match msg {
                WM_LBUTTONUP => Btn::Left,
                WM_RBUTTONUP => Btn::Right,
                _ => Btn::Middle,
            };
            let (x, y) = xy(lp);
            push(Event::Up { btn, x, y });
            if wp.0 & MK_BUTTONS == 0 {
                unsafe {
                    let _ = ReleaseCapture();
                }
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xffff) as u16 as i16 as f64 / 120.0;
            let (x, y) = xy(lp);
            let mut p = POINT { x: x as i32, y: y as i32 };
            unsafe {
                let _ = ScreenToClient(hwnd, &mut p);
            }
            push(Event::Wheel { notches: delta, x: p.x as f64, y: p.y as f64 });
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let repeat = (lp.0 >> 30) & 1 != 0;
            push(Event::Key { vk: wp.0 as u32, ctrl: key_down(VK_CONTROL.0), shift: key_down(VK_SHIFT.0), repeat });
            LRESULT(0)
        }
        WM_CHAR => {
            if let Some(c) = char::from_u32(wp.0 as u32) {
                if !c.is_control() {
                    push(Event::Char(c));
                }
            }
            LRESULT(0)
        }
        WM_SIZE => {
            if wp.0 as u32 != SIZE_MINIMIZED {
                push(Event::Resize((lp.0 & 0xffff) as u32, ((lp.0 >> 16) & 0xffff) as u32));
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let r = unsafe { &*(lp.0 as *const RECT) };
            unsafe {
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            push(Event::Dpi(((wp.0 >> 16) & 0xffff) as u32));
            LRESULT(0)
        }
        WM_SETCURSOR if (lp.0 & 0xffff) as u32 == HTCLIENT => {
            let id = match d.pointer.get() {
                Pointer::Arrow => IDC_ARROW,
                Pointer::Cross => IDC_CROSS,
                Pointer::Hand => IDC_HAND,
                Pointer::Move => IDC_SIZEALL,
            };
            unsafe {
                if let Ok(c) = LoadCursorW(None, id) {
                    SetCursor(Some(c));
                }
            }
            LRESULT(1)
        }
        WM_GETMINMAXINFO => {
            let mm = unsafe { &mut *(lp.0 as *mut MINMAXINFO) };
            mm.ptMinTrackSize = POINT { x: 640, y: 420 };
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            unsafe {
                let _ = ValidateRect(Some(hwnd), None);
            }
            d.paint.set(true);
            LRESULT(0)
        }
        WM_CLOSE => {
            // the editor saves first, then destroys the window
            d.close.set(true);
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// The game's window in this process (class `ScreamerRallyWindow`), if it is there yet.
pub fn game_window() -> Option<HWND> {
    use windows::Win32::System::Threading::GetCurrentProcessId;
    unsafe {
        let mut after: Option<HWND> = None;
        loop {
            let h = FindWindowExW(None, after, w!("ScreamerRallyWindow"), None).ok()?;
            if h.0.is_null() {
                return None;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, Some(&mut pid));
            if pid == GetCurrentProcessId() {
                return Some(h);
            }
            after = Some(h);
        }
    }
}

/// Where the editor opens (outer rectangle: x, y, w, h): to the right of the game's window
/// where the monitor has room (a windowed game is first moved to the left of the screen and
/// made narrower if needed), else on another monitor (the game fullscreen), else over the
/// right part of the game's monitor.
pub fn placement(game: Option<HWND>) -> Option<(i32, i32, i32, i32)> {
    let game = game?;
    unsafe {
        let mut g = RECT::default();
        GetWindowRect(game, &mut g).ok()?;
        let mon = MonitorFromWindow(game, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(mon, &mut mi).as_bool() {
            return None;
        }
        let wk = mi.rcWork;
        // fullscreen: covering the monitor without a caption (a windowed game as big as the
        // monitor, as the tool makes a fullscreen setting on one monitor, is moved and sized)
        let captioned = (GetWindowLongW(game, GWL_STYLE) as u32 & WS_CAPTION.0) == WS_CAPTION.0;
        let full = !captioned && g.left <= mi.rcMonitor.left && g.top <= mi.rcMonitor.top && g.right >= mi.rcMonitor.right && g.bottom >= mi.rcMonitor.bottom;
        // sizes for 100 %, scaled to the monitor's scale
        let k = GetDpiForWindow(game).max(96) as i32;
        let px = |v: i32| v * k / 96;
        let mut room = wk.right - g.right;
        if !full && room < px(760) {
            // a windowed game without room on its right (one monitor): the game's window to the
            // left of the work area, made narrower (same shape) if the editor still lacks room
            let (gw, gh) = (g.right - g.left, g.bottom - g.top);
            // the editor gets two fifths of the screen (at least its minimum)
            let max_w = (wk.right - wk.left) - px(760).max((wk.right - wk.left) * 2 / 5);
            if max_w >= px(480) {
                let (nw, nh) = if gw > max_w { (max_w, gh * max_w / gw.max(1)) } else { (gw, gh) };
                let ny = g.top.min(wk.bottom - nh).max(wk.top);
                let _ = SetWindowPos(game, None, wk.left, ny, nw, nh, SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS);
                g = RECT { left: wk.left, top: ny, right: wk.left + nw, bottom: ny + nh };
                room = wk.right - g.right;
            }
        }
        let height = |area: &RECT| ((g.bottom - g.top).max(px(900))).min(area.bottom - area.top);
        if !full && room >= px(760) {
            let h = height(&wk);
            let y = g.top.max(wk.top).min(wk.bottom - h);
            return Some((g.right, y, room.min(px(1200)), h));
        }
        // another monitor: its work area, a margin round it
        if let Some(other) = other_monitor(mon) {
            let (w, h) = ((other.right - other.left) * 9 / 10, (other.bottom - other.top) * 9 / 10);
            return Some((other.left + (other.right - other.left - w) / 2, other.top + (other.bottom - other.top - h) / 2, w, h));
        }
        let w = px(1000).min(wk.right - wk.left);
        let h = height(&wk);
        Some((wk.right - w, wk.top, w, h))
    }
}

fn other_monitor(not: windows::Win32::Graphics::Gdi::HMONITOR) -> Option<RECT> {
    use windows::Win32::Foundation::LPARAM;
    use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR};
    struct Search {
        not: HMONITOR,
        found: Option<RECT>,
    }
    unsafe extern "system" fn each(m: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> windows::core::BOOL {
        let s = unsafe { &mut *(lp.0 as *mut Search) };
        if m != s.not && s.found.is_none() {
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if unsafe { GetMonitorInfoW(m, &mut mi) }.as_bool() {
                s.found = Some(mi.rcWork);
            }
        }
        true.into()
    }
    let mut s = Search { not, found: None };
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(each), LPARAM(&mut s as *mut Search as isize));
    }
    s.found
}

impl EditorWindow {
    /// Create and show the window (outer rectangle `at`, else a default place) on this
    /// thread; its messages are handled by [`EditorWindow::pump`] on the same thread.
    pub fn create(title: &str, at: Option<(i32, i32, i32, i32)>) -> Result<EditorWindow, String> {
        unsafe {
            let inst = GetModuleHandleW(None).map_err(|e| e.to_string())?;
            let class = w!("ScreamerRallyVisibilityEditor");
            let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: inst.into(), lpszClassName: class, hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(), ..Default::default() };
            RegisterClassW(&wc);
            let t = utf16(title);
            let (x, y, w, h) = at.unwrap_or((CW_USEDEFAULT, CW_USEDEFAULT, 1200, 900));
            let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), class, PCWSTR(t.as_ptr()), STYLE, x, y, w, h, None, None, Some(inst.into()), None).map_err(|e| format!("CreateWindowExW failed: {e}"))?;
            let data = Box::into_raw(Box::new(Data { events: RefCell::new(Vec::new()), close: Cell::new(false), tracking: Cell::new(false), pointer: Cell::new(Pointer::Arrow), paint: Cell::new(true) }));
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, data as usize as _);
            // shown without taking the focus from the game
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            Ok(EditorWindow { hwnd, data })
        }
    }

    fn data(&self) -> &Data {
        unsafe { &*self.data }
    }

    /// Handle the waiting messages; false when the window is gone (WM_QUIT).
    pub fn pump(&self) -> bool {
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    return false;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        true
    }

    /// Wait for a message or `ms` milliseconds.
    pub fn wait(&self, ms: u32) {
        use windows::Win32::UI::WindowsAndMessaging::{MsgWaitForMultipleObjects, QS_ALLINPUT};
        unsafe {
            let _ = MsgWaitForMultipleObjects(None, false, ms, QS_ALLINPUT);
        }
    }

    pub fn events(&self) -> Vec<Event> {
        std::mem::take(&mut *self.data().events.borrow_mut())
    }

    pub fn take_paint(&self) -> bool {
        self.data().paint.replace(false)
    }

    pub fn close_requested(&self) -> bool {
        self.data().close.get()
    }

    pub fn cancel_close(&self) {
        self.data().close.set(false);
    }

    /// The mouse pointer over the client area (`inside`: the mouse is there now).
    pub fn set_pointer(&self, p: Pointer, inside: bool) {
        if self.data().pointer.replace(p) != p && inside {
            let id = match p {
                Pointer::Arrow => IDC_ARROW,
                Pointer::Cross => IDC_CROSS,
                Pointer::Hand => IDC_HAND,
                Pointer::Move => IDC_SIZEALL,
            };
            unsafe {
                if let Ok(c) = LoadCursorW(None, id) {
                    SetCursor(Some(c));
                }
            }
        }
    }

    pub fn set_title(&self, s: &str) {
        let t = utf16(s);
        unsafe {
            let _ = SetWindowTextW(self.hwnd, PCWSTR(t.as_ptr()));
        }
    }

    pub fn client_size(&self) -> (u32, u32) {
        let mut r = RECT::default();
        let _ = unsafe { GetClientRect(self.hwnd, &mut r) };
        ((r.right - r.left).max(0) as u32, (r.bottom - r.top).max(0) as u32)
    }

    pub fn dpi(&self) -> u32 {
        unsafe { GetDpiForWindow(self.hwnd) }.max(96)
    }

    /// Give the window this client size (the test script).
    pub fn set_client_size(&self, w: u32, h: u32) {
        unsafe {
            let mut r = RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 };
            let _ = AdjustWindowRectExForDpi(&mut r, STYLE, false, WINDOW_EX_STYLE(0), self.dpi());
            let _ = SetWindowPos(self.hwnd, None, 0, 0, r.right - r.left, r.bottom - r.top, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }

    /// A question with Yes / No (the window closing with unsaved changes it cannot save).
    pub fn ask(&self, text: &str) -> bool {
        let t = utf16(text);
        let c = utf16("Screamer Rally - Visibility Editor");
        unsafe { MessageBoxW(Some(self.hwnd), PCWSTR(t.as_ptr()), PCWSTR(c.as_ptr()), MB_YESNO | MB_ICONWARNING) == IDYES }
    }

    /// Ask the window to close (as its close button does).
    pub fn post_close(&self) {
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }

    pub fn destroy(&self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

impl Drop for EditorWindow {
    fn drop(&mut self) {
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            drop(Box::from_raw(self.data));
        }
    }
}
