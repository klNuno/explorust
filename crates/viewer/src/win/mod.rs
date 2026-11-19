//! The viewer window: Mica behind a composition swap chain, the image drawn with Direct2D, a
//! command bar of Fluent glyphs, keys and mouse for browsing, zoom, rotation and fullscreen.

mod actions;
mod decode;
mod folder;
pub mod log;
mod paint;
mod theme;

use crate::view::{View, step};
use decode::{Decoded, factory};
use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::IWICImagingFactory;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Input::Touch::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{HSTRING, w};

const CLASS: windows_core::PCWSTR = w!("explorust.Viewer");
const WM_DECODED: u32 = WM_APP + 1;
const TIMER_ANIM: usize = 1;
pub const BAR_H: f32 = 48.0;

/// A full decode handed from the worker thread to the UI thread.
type Slot = Arc<Mutex<Option<(u64, Decoded)>>>;

pub struct App {
    pub hwnd: HWND,
    pub surface: render::Surface,
    pub text: render::text::Text,
    pub ui: windows::Win32::Graphics::DirectWrite::IDWriteTextFormat,
    pub glyphs: windows::Win32::Graphics::DirectWrite::IDWriteTextFormat,
    wic: IWICImagingFactory,
    pub light: bool,
    pub path: String,
    list: Vec<String>,
    at: usize,
    /// Full image size and the frames (one, or a composed animation) as D2D bitmaps.
    pub img: (f32, f32),
    pub frames: Vec<(ID2D1Bitmap1, u32)>,
    pub frame: usize,
    pub view: View,
    pub hover: Option<paint::Button>,
    pub pressed: Option<paint::Button>,
    pub error: Option<String>,
    full: Option<(WINDOWPLACEMENT, i32)>,
    drag: Option<(i32, i32, f32, f32)>,
    epoch: u64,
    slot: Slot,
    pinch: f64,
    first_frame: bool,
    pub size: (u32, u32),
    pub k: f32,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.as_mut().map(f)))
}

/// Entry of `explorust-viewer.exe`; returns the exit code.
pub fn run(args: &[String]) -> i32 {
    unsafe {
        let _ = OleInitialize(None);
    }
    let code = match args.first().map(String::as_str) {
        Some("--register") => (!actions::register()) as i32,
        Some("--unregister") => (!actions::unregister()) as i32,
        Some("--defaults") => (!actions::defaults()) as i32,
        p => window(p.map(|p| std::path::absolute(p).map(|a| a.display().to_string()).unwrap_or_else(|_| p.into()))),
    };
    unsafe { OleUninitialize() };
    code
}

fn client(hwnd: HWND) -> (u32, u32) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut r);
    }
    ((r.right - r.left).max(1) as u32, (r.bottom - r.top).max(1) as u32)
}

fn window(path: Option<String>) -> i32 {
    unsafe {
        let Ok(inst) = GetModuleHandleW(None) else { return 1 };
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(proc),
            hInstance: inst.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: CLASS,
            hIcon: LoadIconW(Some(inst.into()), PCWSTR_ICON).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        // Photos opens near 70% of the work area, centered.
        let mut wa = RECT::default();
        let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut wa as *mut _ as _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
        let (ww, wh) = ((wa.right - wa.left) * 7 / 10, (wa.bottom - wa.top) * 8 / 10);
        let title = path.as_deref().map(|p| p.rsplit('\\').next().unwrap_or(p).to_string()).unwrap_or_else(|| "Photos".into());
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            &HSTRING::from(title),
            WS_OVERLAPPEDWINDOW,
            wa.left + (wa.right - wa.left - ww) / 2,
            wa.top + (wa.bottom - wa.top - wh) / 2,
            ww,
            wh,
            None,
            None,
            Some(inst.into()),
            None,
        ) else {
            return 1;
        };
        let light = theme::light();
        let dark: i32 = (!light) as i32;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, 4);
        let kind = DWMSBT_MAINWINDOW;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &kind as *const _ as _, 4);
        let m = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &m);
        let k = GetDpiForWindow(hwnd) as f32 / 96.0;
        let size = client(hwnd);
        let (Ok(surface), Ok(text), Ok(wic)) = (render::Surface::new(hwnd, size.0, size.1), render::text::Text::new(), factory()) else {
            log::line("surface, text or WIC failed");
            return 1;
        };
        use windows::Win32::Graphics::DirectWrite::*;
        let ui = text.format("Segoe UI Variable Text", 14.0 * k, DWRITE_FONT_WEIGHT_NORMAL, render::text::Align::Leading).unwrap();
        let glyphs = text.format("Segoe Fluent Icons", 16.0 * k, DWRITE_FONT_WEIGHT_NORMAL, render::text::Align::Center).unwrap();
        let _ = ui.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
        let app = App {
            hwnd,
            surface,
            text,
            ui,
            glyphs,
            wic,
            light,
            path: path.clone().unwrap_or_default(),
            list: Vec::new(),
            at: 0,
            img: (0.0, 0.0),
            frames: Vec::new(),
            frame: 0,
            view: View { cx: 0.0, cy: 0.0, scale: 1.0, turns: 0, fit: true },
            hover: None,
            pressed: None,
            error: path.is_none().then(|| tr("Open an image with explorust-viewer.exe <file>", "Ouvrez une image avec explorust-viewer.exe <fichier>").into()),
            full: None,
            drag: None,
            epoch: 0,
            slot: Arc::new(Mutex::new(None)),
            pinch: 0.0,
            first_frame: true,
            size,
            k,
        };
        APP.with(|a| *a.borrow_mut() = Some(app));
        let gc = GESTURECONFIG { dwID: GID_ZOOM, dwWant: 1 /* GC_ZOOM */, dwBlock: 0 };
        let _ = SetGestureConfig(hwnd, 0, &[gc], size_of::<GESTURECONFIG>() as u32);
        with(|a| a.load(true));
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        with(|a| a.paint());
        with(|a| {
            let dir = a.path.rsplit_once('\\').map(|(d, _)| d.to_string()).unwrap_or_default();
            if !dir.is_empty() {
                a.list = folder::images(a.hwnd, &dir);
                a.at = a.list.iter().position(|p| p.eq_ignore_ascii_case(&a.path)).unwrap_or(0);
                log::line(&format!("folder {dir}: {} images, at {}, {:.1} ms since start", a.list.len(), a.at, log::since_start()));
            }
        });
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        APP.with(|a| a.borrow_mut().take());
        0
    }
}

/// French text when the user's UI language is French, English otherwise.
pub fn tr(en: &'static str, fr: &'static str) -> &'static str {
    let lang = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() };
    if lang & 0x3ff == 0x0c { fr } else { en }
}

const PCWSTR_ICON: windows_core::PCWSTR = windows_core::PCWSTR(1 as _);

impl App {
    pub fn area(&self) -> (f32, f32, f32, f32) {
        let (w, h) = (self.size.0 as f32, self.size.1 as f32);
        if self.full.is_some() { (0.0, 0.0, w, h) } else { (0.0, BAR_H * self.k, w, h - BAR_H * self.k) }
    }

    pub fn fullscreen(&self) -> bool {
        self.full.is_some()
    }

    /// Decodes the current file scaled to the window on this thread (the first frame), then the
    /// full image on a worker that posts WM_DECODED.
    fn load(&mut self, first: bool) {
        self.epoch += 1;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_ANIM);
        }
        self.frames.clear();
        self.frame = 0;
        self.error = None;
        if self.path.is_empty() {
            return;
        }
        let t = std::time::Instant::now();
        let area = self.area();
        match decode::decode(&self.wic, &self.path, Some((area.2 as u32, area.3 as u32))) {
            Ok(d) => {
                let scaled = d.frames.len() == 1 && d.frames[0].0.w < d.full_w;
                self.take(d);
                log::line(&format!(
                    "first frame {} {}x{} decoded in {:.1} ms{}",
                    self.path,
                    self.img.0,
                    self.img.1,
                    t.elapsed().as_secs_f64() * 1000.0,
                    if scaled { " (scaled)" } else { "" }
                ));
                if scaled {
                    let (slot, path, epoch, hwnd) = (self.slot.clone(), self.path.clone(), self.epoch, self.hwnd.0 as usize);
                    std::thread::spawn(move || {
                        use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
                        unsafe {
                            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                            let t = std::time::Instant::now();
                            if let Ok(f) = factory() {
                                if let Ok(d) = decode::decode(&f, &path, None) {
                                    log::line(&format!("full decode {path} in {:.1} ms", t.elapsed().as_secs_f64() * 1000.0));
                                    *slot.lock().unwrap() = Some((epoch, d));
                                    let _ = PostMessageW(Some(HWND(hwnd as _)), WM_DECODED, WPARAM(epoch as usize), LPARAM(0));
                                }
                            }
                            CoUninitialize();
                        }
                    });
                }
            }
            Err(e) => {
                let ok = decode::can_open(&self.wic, &self.path);
                self.error = Some(format!("{} ({:#x}{})", tr("Cannot open this file", "Impossible d'ouvrir ce fichier"), e.code().0, if ok { "" } else { tr(", no codec", ", aucun codec") }));
                log::line(&format!("decode {} failed: {e:?}", self.path));
            }
        }
        if !first {
            self.title();
        }
    }

    /// Takes a decode: its bitmaps replace the shown ones, the view fits unless zoomed already.
    fn take(&mut self, d: Decoded) {
        let keep = !self.frames.is_empty() && !self.view.fit;
        self.frames = d.frames.iter().filter_map(|(img, delay)| self.surface.bitmap(img.w, img.h, &img.px).ok().map(|b| (b, *delay))).collect();
        self.img = (d.full_w as f32, d.full_h as f32);
        if !keep {
            self.view = View::fitted(self.img.0, self.img.1, d.turns, self.area());
        }
        if self.frames.len() > 1 {
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_ANIM, self.frames[0].1, None);
            }
        }
    }

    fn title(&self) {
        let name = self.path.rsplit('\\').next().unwrap_or(&self.path);
        unsafe {
            let _ = SetWindowTextW(self.hwnd, &HSTRING::from(name));
        }
    }

    fn browse(&mut self, forward: bool) {
        if self.list.len() < 2 {
            return;
        }
        self.at = step(self.list.len(), self.at, forward);
        self.path = self.list[self.at].clone();
        self.view.fit = true;
        let t = std::time::Instant::now();
        self.load(false);
        self.paint();
        log::line(&format!("browse to {} ({}/{}) shown in {:.1} ms", self.path, self.at + 1, self.list.len(), t.elapsed().as_secs_f64() * 1000.0));
    }

    fn refit(&mut self) {
        self.view = View::fitted(self.img.0, self.img.1, self.view.turns, self.area());
    }

    fn zoom(&mut self, factor: f32, px: f32, py: f32) {
        self.view = self.view.zoom_at(factor, px, py);
        self.paint();
    }

    fn center(&self) -> (f32, f32) {
        let a = self.area();
        (a.0 + a.2 / 2.0, a.1 + a.3 / 2.0)
    }

    fn toggle_full(&mut self) {
        unsafe {
            if let Some((wp, style)) = self.full.take() {
                SetWindowLongW(self.hwnd, GWL_STYLE, style);
                let _ = SetWindowPlacement(self.hwnd, &wp);
                let _ = SetWindowPos(self.hwnd, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED);
            } else {
                let mut wp = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
                let _ = GetWindowPlacement(self.hwnd, &mut wp);
                let style = GetWindowLongW(self.hwnd, GWL_STYLE);
                let mon = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST);
                let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
                let _ = GetMonitorInfoW(mon, &mut mi);
                self.full = Some((wp, style));
                SetWindowLongW(self.hwnd, GWL_STYLE, style & !(WS_OVERLAPPEDWINDOW.0 as i32) | WS_POPUP.0 as i32);
                let r = mi.rcMonitor;
                let _ = SetWindowPos(self.hwnd, Some(HWND_TOP), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_FRAMECHANGED);
            }
        }
        // WM_SIZE came while this App was borrowed and was dropped: take the new size here.
        self.size = client(self.hwnd);
        let _ = self.surface.resize(self.size.0, self.size.1);
        if self.view.fit {
            self.refit();
        }
        self.paint();
        log::line(&format!("fullscreen {}", self.full.is_some()));
    }

    fn rotate(&mut self, clockwise: bool) {
        self.view = self.view.rotate(self.img.0, self.img.1, self.area(), clockwise);
        self.paint();
    }

    fn delete(&mut self) {
        if self.path.is_empty() || !actions::recycle(self.hwnd, &self.path) {
            return;
        }
        if self.list.len() > 1 {
            self.list.remove(self.at);
            self.at = self.at.min(self.list.len() - 1);
            self.path = self.list[self.at].clone();
            self.view.fit = true;
            self.load(false);
            self.paint();
        } else {
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }

    pub fn act(&mut self, b: paint::Button) {
        use paint::Button::*;
        let (cx, cy) = self.center();
        match b {
            ZoomIn => self.zoom(1.25, cx, cy),
            ZoomOut => self.zoom(0.8, cx, cy),
            Rotate => self.rotate(true),
            Delete => self.delete(),
            Copy => {
                actions::copy(&self.path);
            }
            OpenWith => actions::open_with(self.hwnd, &self.path),
            Full => self.toggle_full(),
            Prev => self.browse(false),
            Next => self.browse(true),
        }
    }
}

fn xy(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xffff) as i16 as i32, ((lp.0 >> 16) & 0xffff) as i16 as i32)
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_ERASEBKGND => {
                // Black is transparent in the extended frame: Mica shows through.
                let mut r = RECT::default();
                let _ = GetClientRect(hwnd, &mut r);
                FillRect(HDC(wp.0 as _), &r, HBRUSH(GetStockObject(BLACK_BRUSH).0));
                LRESULT(1)
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
                with(|a| a.paint());
                LRESULT(0)
            }
            WM_SIZE => {
                with(|a| {
                    a.size = client(hwnd);
                    let _ = a.surface.resize(a.size.0, a.size.1);
                    if a.view.fit {
                        a.refit();
                    }
                    a.paint();
                });
                LRESULT(0)
            }
            WM_DPICHANGED => {
                let r = &*(lp.0 as *const RECT);
                with(|a| a.k = (wp.0 & 0xffff) as f32 / 96.0);
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
                LRESULT(0)
            }
            WM_DECODED => {
                with(|a| {
                    let got = a.slot.lock().unwrap().take();
                    if let Some((epoch, d)) = got {
                        if epoch == a.epoch {
                            let fit = a.view.fit;
                            let v = a.view;
                            a.take(d);
                            if !fit {
                                a.view = v;
                            }
                            a.paint();
                        }
                    }
                });
                LRESULT(0)
            }
            WM_TIMER if wp.0 == TIMER_ANIM => {
                with(|a| {
                    if a.frames.len() > 1 {
                        a.frame = (a.frame + 1) % a.frames.len();
                        let _ = SetTimer(Some(hwnd), TIMER_ANIM, a.frames[a.frame].1, None);
                        a.paint();
                    }
                });
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let delta = ((wp.0 >> 16) & 0xffff) as i16 as f32 / 120.0;
                let mut p = POINT { x: xy(lp).0, y: xy(lp).1 };
                let _ = ScreenToClient(hwnd, &mut p);
                with(|a| a.zoom(1.2f32.powf(delta), p.x as f32, p.y as f32));
                LRESULT(0)
            }
            WM_GESTURE => {
                let mut gi = GESTUREINFO { cbSize: size_of::<GESTUREINFO>() as u32, ..Default::default() };
                let h = HGESTUREINFO(lp.0 as _);
                if GetGestureInfo(h, &mut gi).is_ok() && gi.dwID == GID_ZOOM.0 {
                    let dist = gi.ullArguments as f64;
                    let mut p = POINT { x: gi.ptsLocation.x as i32, y: gi.ptsLocation.y as i32 };
                    let _ = ScreenToClient(hwnd, &mut p);
                    with(|a| {
                        if gi.dwFlags & GF_BEGIN != 0 || a.pinch <= 0.0 {
                            a.pinch = dist;
                        } else if dist > 0.0 {
                            let f = (dist / a.pinch) as f32;
                            a.pinch = dist;
                            a.zoom(f, p.x as f32, p.y as f32);
                        }
                    });
                    let _ = CloseGestureInfoHandle(h);
                    return LRESULT(0);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
                let key = VIRTUAL_KEY(wp.0 as u16);
                let done = with(|a| {
                    let (cx, cy) = a.center();
                    match key {
                        VK_LEFT => a.browse(false),
                        VK_RIGHT => a.browse(true),
                        VK_DELETE => a.delete(),
                        VK_F11 => a.toggle_full(),
                        VK_ESCAPE if a.fullscreen() => a.toggle_full(),
                        VK_ADD | VK_OEM_PLUS => a.zoom(1.25, cx, cy),
                        VK_SUBTRACT | VK_OEM_MINUS => a.zoom(0.8, cx, cy),
                        VK_0 | VK_NUMPAD0 if ctrl => {
                            a.refit();
                            a.paint();
                        }
                        VK_R if ctrl => a.rotate(GetKeyState(VK_SHIFT.0 as i32) >= 0),
                        VK_C if ctrl => {
                            actions::copy(&a.path);
                        }
                        _ => return false,
                    }
                    true
                });
                if done == Some(true) { LRESULT(0) } else { DefWindowProcW(hwnd, msg, wp, lp) }
            }
            WM_MOUSEMOVE => {
                let (x, y) = xy(lp);
                with(|a| {
                    if let Some((sx, sy, cx, cy)) = a.drag {
                        a.view.cx = cx + (x - sx) as f32;
                        a.view.cy = cy + (y - sy) as f32;
                        a.view.fit = false;
                        a.paint();
                        return;
                    }
                    let h = a.hit(x as f32, y as f32);
                    if h != a.hover {
                        a.hover = h;
                        a.paint();
                    }
                    let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                    let _ = TrackMouseEvent(&mut tme);
                });
                LRESULT(0)
            }
            0x02A3 => {
                // WM_MOUSELEAVE
                with(|a| {
                    if a.hover.take().is_some() {
                        a.paint();
                    }
                });
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let (x, y) = xy(lp);
                with(|a| {
                    a.pressed = a.hit(x as f32, y as f32);
                    if a.pressed.is_none() {
                        a.drag = Some((x, y, a.view.cx, a.view.cy));
                        SetCapture(hwnd);
                    }
                    a.paint();
                });
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let (x, y) = xy(lp);
                let _ = ReleaseCapture();
                let b = with(|a| {
                    a.drag = None;
                    let p = a.pressed.take();
                    if p.is_some() && p == a.hit(x as f32, y as f32) { p } else { None }
                })
                .flatten();
                if let Some(b) = b {
                    with(|a| a.act(b));
                }
                with(|a| a.paint());
                LRESULT(0)
            }
            WM_LBUTTONDBLCLK => {
                with(|a| a.toggle_full());
                LRESULT(0)
            }
            WM_SETTINGCHANGE => {
                with(|a| {
                    let l = theme::light();
                    if l != a.light {
                        a.light = l;
                        let dark: i32 = (!l) as i32;
                        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, 4);
                        a.paint();
                    }
                });
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
