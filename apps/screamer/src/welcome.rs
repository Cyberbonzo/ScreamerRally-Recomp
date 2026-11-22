//! The first-run window: the game's logo, what the program needs, and a button that opens
//! Windows' folder picker. The folder the player picks is checked here (`firstrun::validate`
//! through `check`): a problem is shown in the window, the game found is shown briefly and
//! the window closes. Drawn with Direct2D and DirectWrite (sharp at any DPI), the logo
//! decoded by WIC from the program itself.
//!
//! Enter: choose a folder; Esc or closing the window: quit (no game, nothing to run).

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT, UpdateWindow};
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForSystem, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, VK_ESCAPE, VK_RETURN};
use windows::Win32::UI::Shell::{FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::firstrun::Found;

/// The logo (the game's own, `assets/logo.png`).
const LOGO: &[u8] = include_bytes!("../../../assets/logo.png");

/// The client area in device-independent pixels.
const W: f32 = 600.0;
const H: f32 = 520.0;

/// Colours (sRGB, 0..1): the background's gradient, text, the logo's orange for the main
/// button, and the status line's kinds.
const BG_TOP: [f32; 3] = [0.075, 0.082, 0.110];
const BG_BOTTOM: [f32; 3] = [0.118, 0.129, 0.176];
const TEXT: [f32; 3] = [0.94, 0.95, 0.97];
const MUTED: [f32; 3] = [0.62, 0.65, 0.72];
const FAINT: [f32; 3] = [0.42, 0.45, 0.52];
const ORANGE: [f32; 3] = [0.969, 0.635, 0.110];
const ORANGE_HOT: [f32; 3] = [1.0, 0.725, 0.255];
const BUTTON_TEXT: [f32; 3] = [0.10, 0.08, 0.06];
const OUTLINE: [f32; 3] = [0.25, 0.27, 0.34];
const OUTLINE_HOT: [f32; 3] = [0.17, 0.19, 0.25];
const RED: [f32; 3] = [1.0, 0.45, 0.42];
const GREEN: [f32; 3] = [0.45, 0.86, 0.55];
const AMBER: [f32; 3] = [1.0, 0.78, 0.36];

/// What the status line says.
#[derive(Clone, PartialEq)]
enum Status {
    None,
    Note(String),
    Checking(String),
    Error(String),
    Found(String),
}

#[derive(Clone, Copy, PartialEq)]
enum Button {
    Choose,
    Quit,
}

/// What the window asks of `run`'s loop (handled outside the window procedure, where the
/// check is).
#[derive(Clone, Copy, PartialEq)]
enum Request {
    None,
    Choose,
    Quit,
    Done,
    /// `SR_WELCOME_PICK` (tests).
    TestPick,
}

struct Ui {
    hwnd: HWND,
    dpi: u32,
    target: Option<ID2D1HwndRenderTarget>,
    logo: Option<ID2D1Bitmap>,
    d2d: ID2D1Factory,
    dwrite: IDWriteFactory,
    status: Status,
    hot: Option<Button>,
    pressed: Option<Button>,
    request: Request,
    busy: bool,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

fn with_ui<R>(f: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    UI.with(|u| u.borrow_mut().as_mut().map(f))
}

fn color(c: [f32; 3], a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c[0], g: c[1], b: c[2], a }
}

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

/// The buttons' rectangles (DIPs).
fn button_rect(b: Button) -> D2D_RECT_F {
    let (wc, wq, gap, h) = (250.0, 120.0, 14.0, 46.0);
    let x0 = (W - (wc + gap + wq)) / 2.0;
    let y = 398.0;
    match b {
        Button::Choose => rect(x0, y, x0 + wc, y + h),
        Button::Quit => rect(x0 + wc + gap, y, x0 + wc + gap + wq, y + h),
    }
}

fn hit(x: f32, y: f32) -> Option<Button> {
    [Button::Choose, Button::Quit].into_iter().find(|&b| {
        let r = button_rect(b);
        x >= r.left && x < r.right && y >= r.top && y < r.bottom
    })
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// The logo as a Direct2D bitmap (premultiplied BGRA from the PNG's straight RGBA).
unsafe fn load_logo(rt: &ID2D1HwndRenderTarget) -> Option<ID2D1Bitmap> {
    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let stream = wic.CreateStream().ok()?;
        stream.InitializeFromMemory(LOGO).ok()?;
        let dec = wic.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = dec.GetFrame(0).ok()?;
        let conv = wic.CreateFormatConverter().ok()?;
        conv.Initialize(&frame, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).ok()?;
        rt.CreateBitmapFromWicBitmap(&conv, None).ok()
    }
}

impl Ui {
    fn ensure_target(&mut self) {
        if self.target.is_some() {
            return;
        }
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: self.dpi as f32,
            dpiY: self.dpi as f32,
            ..Default::default()
        };
        let hprops = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd: self.hwnd,
            pixelSize: D2D_SIZE_U { width: (rc.right - rc.left) as u32, height: (rc.bottom - rc.top) as u32 },
            presentOptions: D2D1_PRESENT_OPTIONS_NONE,
        };
        if let Ok(rt) = unsafe { self.d2d.CreateHwndRenderTarget(&props, &hprops) } {
            self.logo = unsafe { load_logo(&rt) };
            self.target = Some(rt);
        }
    }

    fn text_format(&self, size: f32, weight: DWRITE_FONT_WEIGHT, align: DWRITE_TEXT_ALIGNMENT) -> Option<IDWriteTextFormat> {
        unsafe {
            let f = self.dwrite.CreateTextFormat(w!("Segoe UI"), None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("en-us")).ok()?;
            let _ = f.SetTextAlignment(align);
            let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);
            let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
            Some(f)
        }
    }

    fn paint(&mut self) {
        self.ensure_target();
        let Some(rt) = self.target.clone() else { return };
        unsafe {
            rt.BeginDraw();
            // the background: a dark vertical gradient, a soft glow of the logo's orange
            let stops = [D2D1_GRADIENT_STOP { position: 0.0, color: color(BG_TOP, 1.0) }, D2D1_GRADIENT_STOP { position: 1.0, color: color(BG_BOTTOM, 1.0) }];
            if let Ok(sc) = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) {
                let p = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: windows_numerics::Vector2 { X: 0.0, Y: 0.0 }, endPoint: windows_numerics::Vector2 { X: 0.0, Y: H } };
                if let Ok(b) = rt.CreateLinearGradientBrush(&p, None, &sc) {
                    rt.FillRectangle(&rect(0.0, 0.0, W, H), &b);
                }
            }
            let glow = [D2D1_GRADIENT_STOP { position: 0.0, color: color(ORANGE, 0.20) }, D2D1_GRADIENT_STOP { position: 1.0, color: color(ORANGE, 0.0) }];
            if let Ok(sc) = rt.CreateGradientStopCollection(&glow, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) {
                let p = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES {
                    center: windows_numerics::Vector2 { X: W / 2.0, Y: 118.0 },
                    gradientOriginOffset: windows_numerics::Vector2 { X: 0.0, Y: 0.0 },
                    radiusX: 330.0,
                    radiusY: 150.0,
                };
                if let Ok(b) = rt.CreateRadialGradientBrush(&p, None, &sc) {
                    rt.FillRectangle(&rect(0.0, 0.0, W, 300.0), &b);
                }
            }
            // the logo
            if let Some(logo) = &self.logo {
                let size = logo.GetSize();
                let lw = 430.0;
                let lh = lw * size.height / size.width.max(1.0);
                let x = (W - lw) / 2.0;
                rt.DrawBitmap(logo, Some(&rect(x, 34.0, x + lw, 34.0 + lh)), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
            }
            let brush = |c: [f32; 3], a: f32| rt.CreateSolidColorBrush(&color(c, a), None).ok();
            let draw = |text: &str, f: &Option<IDWriteTextFormat>, r: D2D_RECT_F, c: [f32; 3]| {
                if let (Some(f), Some(b)) = (f, brush(c, 1.0)) {
                    rt.DrawText(&wide(text), f, &r, &b, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                }
            };
            let center = DWRITE_TEXT_ALIGNMENT_CENTER;
            draw("Where is your game?", &self.text_format(23.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, center), rect(0.0, 222.0, W, 256.0), TEXT);
            draw(
                "Screamer Rally runs from your own copy of the original game. Choose the folder that holds it: the CD image (.mds, .cue or .iso, with the CD music) or the DOS game installed on disk.",
                &self.text_format(14.5, DWRITE_FONT_WEIGHT_NORMAL, center),
                rect(56.0, 262.0, W - 56.0, 330.0),
                MUTED,
            );
            // the status line
            let (text, c) = match &self.status {
                Status::None => (String::new(), MUTED),
                Status::Note(t) => (t.clone(), AMBER),
                Status::Checking(t) => (t.clone(), MUTED),
                Status::Error(t) => (t.clone(), RED),
                Status::Found(t) => (t.clone(), GREEN),
            };
            if !text.is_empty() {
                draw(&text, &self.text_format(13.0, DWRITE_FONT_WEIGHT_NORMAL, center), rect(40.0, 336.0, W - 40.0, 392.0), c);
            }
            // the buttons
            for b in [Button::Choose, Button::Quit] {
                let r = button_rect(b);
                let rr = D2D1_ROUNDED_RECT { rect: r, radiusX: 9.0, radiusY: 9.0 };
                let hot = self.hot == Some(b) && !self.busy;
                let down = hot && self.pressed == Some(b);
                match b {
                    Button::Choose => {
                        let fill = if self.busy { color(ORANGE, 0.45) } else if hot { color(ORANGE_HOT, 1.0) } else { color(ORANGE, 1.0) };
                        if let Ok(f) = rt.CreateSolidColorBrush(&fill, None) {
                            let rr = if down { D2D1_ROUNDED_RECT { rect: rect(r.left + 1.0, r.top + 1.0, r.right - 1.0, r.bottom - 1.0), ..rr } } else { rr };
                            rt.FillRoundedRectangle(&rr, &f);
                        }
                        draw("Choose game folder\u{2026}", &self.text_format(15.5, DWRITE_FONT_WEIGHT_SEMI_BOLD, center), rect(r.left, r.top + 11.0, r.right, r.bottom), BUTTON_TEXT);
                    }
                    Button::Quit => {
                        if hot {
                            if let Some(f) = brush(OUTLINE_HOT, 1.0) {
                                rt.FillRoundedRectangle(&rr, &f);
                            }
                        }
                        if let Some(o) = brush(OUTLINE, 1.0) {
                            rt.DrawRoundedRectangle(&rr, &o, 1.5, None);
                        }
                        draw("Quit", &self.text_format(15.5, DWRITE_FONT_WEIGHT_NORMAL, center), rect(r.left, r.top + 11.0, r.right, r.bottom), TEXT);
                    }
                }
            }
            draw(
                "Your game files are only read, never changed.",
                &self.text_format(12.0, DWRITE_FONT_WEIGHT_NORMAL, center),
                rect(0.0, H - 40.0, W, H - 16.0),
                FAINT,
            );
            if rt.EndDraw(None, None).is_err() {
                // the device was lost: made again at the next paint
                self.target = None;
                self.logo = None;
            }
        }
    }

    fn redraw(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }
}

fn lparam_dips(lp: LPARAM, dpi: u32) -> (f32, f32) {
    let (x, y) = ((lp.0 & 0xffff) as i16 as f32, ((lp.0 >> 16) & 0xffff) as i16 as f32);
    let s = 96.0 / dpi.max(1) as f32;
    (x * s, y * s)
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            unsafe {
                BeginPaint(hwnd, &mut ps);
            }
            with_ui(|u| u.paint());
            unsafe {
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEMOVE => {
            with_ui(|u| {
                let (x, y) = lparam_dips(lp, u.dpi);
                let hot = hit(x, y);
                if hot != u.hot {
                    u.hot = hot;
                    u.redraw();
                }
            });
            LRESULT(0)
        }
        WM_SETCURSOR => {
            let hand = with_ui(|u| u.hot.is_some() && !u.busy).unwrap_or(false);
            if hand && (lp.0 & 0xffff) as u32 == HTCLIENT {
                unsafe {
                    let _ = SetCursor(LoadCursorW(None, IDC_HAND).ok());
                }
                return LRESULT(1);
            }
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        WM_LBUTTONDOWN => {
            with_ui(|u| {
                let (x, y) = lparam_dips(lp, u.dpi);
                u.pressed = hit(x, y);
                u.redraw();
            });
            unsafe {
                SetCapture(hwnd);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            unsafe {
                let _ = ReleaseCapture();
            }
            with_ui(|u| {
                let (x, y) = lparam_dips(lp, u.dpi);
                let b = hit(x, y);
                if b.is_some() && b == u.pressed && !u.busy {
                    u.request = if b == Some(Button::Choose) { Request::Choose } else { Request::Quit };
                }
                u.pressed = None;
                u.redraw();
            });
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let key = wp.0 as u16;
            with_ui(|u| {
                if !u.busy {
                    if key == VK_RETURN.0 {
                        u.request = Request::Choose;
                    } else if key == VK_ESCAPE.0 {
                        u.request = Request::Quit;
                    }
                }
            });
            LRESULT(0)
        }
        WM_TIMER => {
            unsafe {
                let _ = KillTimer(Some(hwnd), wp.0);
            }
            with_ui(|u| u.request = if wp.0 == 2 { Request::TestPick } else { Request::Done });
            LRESULT(0)
        }
        WM_SIZE => {
            with_ui(|u| {
                if let Some(rt) = &u.target {
                    let (w, h) = ((lp.0 & 0xffff) as u32, ((lp.0 >> 16) & 0xffff) as u32);
                    unsafe {
                        let _ = rt.Resize(&D2D_SIZE_U { width: w, height: h });
                    }
                }
            });
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let dpi = (wp.0 & 0xffff) as u32;
            with_ui(|u| {
                u.dpi = dpi;
                if let Some(rt) = &u.target {
                    unsafe { rt.SetDpi(dpi as f32, dpi as f32) };
                }
            });
            let r = unsafe { &*(lp.0 as *const RECT) };
            unsafe {
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            with_ui(|u| {
                if !u.busy {
                    u.request = Request::Quit;
                }
            });
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// Windows' folder picker; `None`: cancelled.
fn pick_folder(owner: HWND) -> Option<PathBuf> {
    unsafe {
        let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let opts = dlg.GetOptions().ok()?;
        dlg.SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST).ok()?;
        let _ = dlg.SetTitle(w!("Select the folder with your Screamer Rally game"));
        let _ = dlg.SetOkButtonLabel(w!("Use this folder"));
        dlg.Show(Some(owner)).ok()?;
        let item = dlg.GetResult().ok()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = name.to_string().ok();
        CoTaskMemFree(Some(name.0 as *const _));
        path.map(PathBuf::from)
    }
}

/// The window, until the player picks a game that `check` accepts (`Some`) or quits
/// (`None`). `note`: shown first (a remembered game that cannot be used any more).
pub fn run(note: Option<&str>, check: &mut dyn FnMut(&Path) -> Result<Found, String>) -> Option<Found> {
    sr_render::window::init_dpi();
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let result = unsafe { window(note, check) };
    if com.is_ok() {
        unsafe { CoUninitialize() };
    }
    match result {
        Ok(r) => r,
        Err(e) => {
            // no window: say what is needed in a message box instead
            crate::log_info(&format!("first-run window: {e}"));
            crate::firstrun::message(
                "Screamer Rally runs from your own copy of the game: start it with --disc and the CD image or the game's folder, or set [game] path in data\\settings.ini.",
                true,
            );
            None
        }
    }
}

unsafe fn window(note: Option<&str>, check: &mut dyn FnMut(&Path) -> Result<Found, String>) -> windows::core::Result<Option<Found>> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = w!("ScreamerRallyWelcome");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: class,
            hIcon: LoadIconW(Some(instance.into()), PCWSTR(1 as _)).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let dpi = GetDpiForSystem();
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let mut r = RECT { left: 0, top: 0, right: (W * dpi as f32 / 96.0) as i32, bottom: (H * dpi as f32 / 96.0) as i32 };
        let _ = AdjustWindowRectExForDpi(&mut r, style, false, WINDOW_EX_STYLE(0), dpi);
        let (ww, wh) = (r.right - r.left, r.bottom - r.top);
        let (sw, sh) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), class, w!("Screamer Rally"), style, (sw - ww) / 2, (sh - wh) / 2, ww, wh, None, None, Some(instance.into()), None)?;
        // a dark title bar to match
        let dark: i32 = 1;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const i32 as *const _, 4);
        let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        let dpi = GetDpiForWindow(hwnd).max(96);
        UI.with(|u| {
            *u.borrow_mut() = Some(Ui {
                hwnd,
                dpi,
                target: None,
                logo: None,
                d2d,
                dwrite,
                status: note.map_or(Status::None, |n| Status::Note(n.into())),
                hot: None,
                pressed: None,
                request: Request::None,
                busy: false,
            })
        });
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = UpdateWindow(hwnd);
        // SR_WELCOME_PICK=FOLDER (tests): as if the player picked that folder, once, after a
        // second
        let mut test_pick = std::env::var_os("SR_WELCOME_PICK").map(PathBuf::from);
        if test_pick.is_some() {
            SetTimer(Some(hwnd), 2, 1000, None);
        }
        let mut found = None;
        let mut msg = MSG::default();
        let result = loop {
            if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                break None;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
            let request = with_ui(|u| std::mem::replace(&mut u.request, Request::None)).unwrap_or(Request::Quit);
            match request {
                Request::None => {}
                Request::Quit => break None,
                Request::Done => break found.take(),
                Request::Choose | Request::TestPick => {
                    let dir = if request == Request::TestPick { test_pick.take() } else { pick_folder(hwnd) };
                    let Some(dir) = dir else { continue };
                    with_ui(|u| {
                        u.busy = true;
                        u.status = Status::Checking(format!("Checking {}\u{2026}", dir.display()));
                        u.redraw();
                    });
                    let _ = UpdateWindow(hwnd);
                    let r = check(&dir);
                    with_ui(|u| {
                        u.busy = false;
                        u.hot = None;
                        u.status = match &r {
                            Ok(f) => Status::Found(format!("Found: {}. Starting\u{2026}", f.description)),
                            Err(e) => Status::Error(e.clone()),
                        };
                        u.redraw();
                    });
                    if let Ok(f) = r {
                        // the game found stays on screen for a moment, then the window closes
                        with_ui(|u| u.busy = true);
                        found = Some(f);
                        SetTimer(Some(hwnd), 1, 1200, None);
                    }
                }
            }
        };
        let _ = DestroyWindow(hwnd);
        UI.with(|u| *u.borrow_mut() = None);
        let _ = UnregisterClassW(class, Some(instance.into()));
        Ok(result)
    }
}
