//! Task View (Win+Tab): a full-screen window over the monitor of the foreground window with an
//! acrylic backdrop, the current desktop's windows as large live DWM thumbnails, and the desktops
//! strip at the bottom (Windows' virtual desktops through `desktops`, the wallpaper as each
//! desktop's picture from a thumbnail of the desktop window). Click a window to switch to it, its
//! close button to close it, a desktop to switch to it, New desktop to add one, a desktop's close
//! button to remove it; drag a window onto a desktop to move it there. Esc, Win+Tab again, a click
//! on the background or losing the focus closes it.

use crate::layout::{Layout, Rect};
use crate::tv_layout::{self as TL, Strip};
use crate::tv_scene::{Hover, Painter, TvView};
use crate::windows_list::{self as wl, Entry};
use crate::{activate, desktops, log};
use shelldata::Image;
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Instant;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION, DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE,
    DwmQueryThumbnailSourceSize, DwmRegisterThumbnail, DwmUnregisterThumbnail, DwmUpdateThumbnailProperties,
};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromWindow, ValidateRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{GUID, PCWSTR, w};

const CLASS: PCWSTR = w!("explorust.switcher.taskview");
pub const WM_ICONS: u32 = WM_APP + 1;
const T_REBUILD: usize = 11;
/// Acrylic tint, ABGR: a dark veil over the blurred desktop.
const TINT: u32 = 0xB01C_1C1C;
const DRAG_START: i32 = 6;

struct Drag {
    index: usize,
    from: POINT,
    active: bool,
}

struct Tv {
    hwnd: HWND,
    shown: bool,
    orig: HWND,
    mon: RECT,
    scale: f32,
    entries: Vec<Entry>,
    layout: Layout,
    thumbs: Vec<isize>,
    desk_names: Vec<String>,
    desk_ids: Vec<GUID>,
    current: usize,
    service: bool,
    strip: Strip,
    desk_thumbs: Vec<isize>,
    hover: Hover,
    drag: Option<Drag>,
    drop_on: Option<usize>,
    painter: Option<Painter>,
    icons: HashMap<String, Option<Image>>,
    /// Set while Task View itself moves the focus, so the deactivation does not close it twice.
    acting: bool,
}

thread_local! {
    static TV: RefCell<Option<Tv>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Tv) -> R) -> Option<R> {
    TV.with(|c| {
        let mut b = c.try_borrow_mut().ok()?;
        b.as_mut().map(f)
    })
}

pub fn create() -> windows_core::Result<HWND> {
    // SAFETY: a window class and a window owned by this thread.
    unsafe {
        let inst = GetModuleHandleW(None)?;
        let cursor = LoadCursorW(None, IDC_ARROW)?;
        let wc = WNDCLASSW { lpfnWndProc: Some(proc_), hInstance: inst.into(), lpszClassName: CLASS, hCursor: cursor, ..Default::default() };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            CLASS,
            w!("Task View"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(inst.into()),
            None,
        )?;
        render::backdrop::apply(hwnd, render::backdrop::Backdrop::Acrylic { tint_abgr: TINT });
        TV.with(|c| {
            *c.borrow_mut() = Some(Tv {
                hwnd,
                shown: false,
                orig: HWND::default(),
                mon: RECT::default(),
                scale: 1.0,
                entries: Vec::new(),
                layout: Layout { w: 0.0, h: 0.0, tiles: Vec::new() },
                thumbs: Vec::new(),
                desk_names: Vec::new(),
                desk_ids: Vec::new(),
                current: 0,
                service: false,
                strip: Strip::default(),
                desk_thumbs: Vec::new(),
                hover: Hover::None,
                drag: None,
                drop_on: None,
                painter: None,
                icons: HashMap::new(),
                acting: false,
            })
        });
        Ok(hwnd)
    }
}

pub fn destroy() {
    if let Some(Some(h)) = with(|t| {
        t.unregister();
        t.hwnd
    })
    .map(Some)
    {
        // SAFETY: our own window.
        unsafe {
            if IsWindow(Some(h)).as_bool() {
                let _ = DestroyWindow(h);
            }
        }
    }
}

fn px(v: f32, s: f32) -> i32 {
    (v * s).round() as i32
}

fn thumb(id: isize, r: &Rect, s: f32, client_only: bool, opacity: u8) {
    if id == 0 {
        return;
    }
    let props = DWM_THUMBNAIL_PROPERTIES {
        dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE | DWM_TNP_OPACITY | DWM_TNP_SOURCECLIENTAREAONLY,
        rcDestination: RECT { left: px(r.x, s), top: px(r.y, s), right: px(r.right(), s), bottom: px(r.bottom(), s) },
        opacity,
        fVisible: true.into(),
        fSourceClientAreaOnly: client_only.into(),
        ..Default::default()
    };
    // SAFETY: a thumbnail id we registered.
    let _ = unsafe { DwmUpdateThumbnailProperties(id, &props) };
}

/// The window that draws the wallpaper: the desktop part's Progman (or Windows').
fn wallpaper_window() -> Option<HWND> {
    // SAFETY: plain lookup.
    unsafe { FindWindowW(w!("Progman"), None).ok().filter(|h| !h.is_invalid()) }
}

impl Tv {
    fn unregister(&mut self) {
        for t in self.thumbs.drain(..).chain(self.desk_thumbs.drain(..)) {
            if t != 0 {
                // SAFETY: a thumbnail id we registered.
                let _ = unsafe { DwmUnregisterThumbnail(t) };
            }
        }
    }

    /// Reads the desktops; one unnamed desktop when the service is not reached.
    fn read_desktops(&mut self) {
        let r = desktops::with_vd(|vd| {
            let desks = vd.desktops()?;
            let cur = vd.current_index(&desks).unwrap_or(0);
            Ok((desks.iter().map(|d| (d.name.clone(), d.id)).collect::<Vec<_>>(), cur))
        });
        match r {
            Ok((d, cur)) => {
                self.desk_names = d.iter().map(|x| x.0.clone()).collect();
                self.desk_ids = d.iter().map(|x| x.1).collect();
                self.current = cur;
                self.service = true;
            }
            Err(e) => {
                log::line(&format!("task view: no desktop service ({e}), one desktop"));
                self.desk_names = vec!["Desktop 1".into()];
                self.desk_ids = vec![GUID::zeroed()];
                self.current = 0;
                self.service = false;
            }
        }
    }

    /// Lists the windows, reads the desktops, lays out and registers every thumbnail.
    fn build(&mut self) {
        self.unregister();
        self.entries = wl::snapshot(self.hwnd);
        self.read_desktops();
        let mut aspects = Vec::with_capacity(self.entries.len());
        for e in &self.entries {
            // SAFETY: DWM thumbnail of a top-level window onto our window.
            let (id, size) = unsafe {
                match DwmRegisterThumbnail(self.hwnd, e.hwnd) {
                    Ok(id) => (id, DwmQueryThumbnailSourceSize(id).unwrap_or_default()),
                    Err(_) => (0, Default::default()),
                }
            };
            self.thumbs.push(id);
            aspects.push(if size.cy > 0 { size.cx as f32 / size.cy as f32 } else { 16.0 / 10.0 });
        }
        let s = self.scale;
        let mw = (self.mon.right - self.mon.left) as f32 / s;
        let mh = (self.mon.bottom - self.mon.top) as f32 / s;
        self.layout = TL::windows(&aspects, mw, mh);
        self.strip = TL::strip(self.desk_names.len(), mw, mh);
        for (t, id) in self.layout.tiles.iter().zip(&self.thumbs) {
            thumb(*id, &t.thumb, s, false, 255);
        }
        if let Some(wall) = wallpaper_window() {
            for d in &self.strip.desks {
                // SAFETY: DWM thumbnail of the desktop window onto ours.
                let id = unsafe { DwmRegisterThumbnail(self.hwnd, wall) }.unwrap_or(0);
                thumb(id, &d.thumb, s, true, 255);
                self.desk_thumbs.push(id);
            }
        }
    }

    fn draw(&mut self) {
        if !self.shown {
            return;
        }
        let Some(mut p) = self.painter.take() else { return };
        let view = TvView {
            scale: self.scale,
            windows: &self.layout,
            titles: self.entries.iter().map(|e| e.title.as_str()).collect(),
            keys: self.entries.iter().map(|e| e.key.as_str()).collect(),
            icons: &self.icons,
            strip: &self.strip,
            desks: self.desk_names.iter().map(|s| s.as_str()).collect(),
            current: self.current,
            hover: self.hover,
            dragging: self.drag.as_ref().filter(|d| d.active).map(|d| d.index),
            drop_on: self.drop_on,
            empty: "No open windows on this desktop",
        };
        if let Err(e) = p.task_view(&view) {
            log::line(&format!("task view draw: {e}"));
        }
        self.painter = Some(p);
    }

    fn hit(&self, x: f32, y: f32) -> Hover {
        if let Some(i) = self.layout.hit(x, y) {
            return if self.layout.tiles[i].close().contains(x, y) { Hover::WindowClose(i) } else { Hover::Window(i) };
        }
        if let Some(i) = self.strip.hit(x, y) {
            return if self.desk_names.len() > 1 && self.strip.desks[i].close.contains(x, y) {
                Hover::DeskClose(i)
            } else {
                Hover::Desk(i)
            };
        }
        if self.strip.new_desk.contains(x, y) && self.service {
            return Hover::NewDesk;
        }
        Hover::None
    }
}

/// Win+Tab: opens Task View, or closes it when it is open.
pub fn toggle() {
    let shown = with(|t| t.shown).unwrap_or(false);
    if shown {
        close(None);
        return;
    }
    let t0 = Instant::now();
    let r = with(|t| {
        // SAFETY: plain queries, then placing and showing our own window.
        unsafe {
            t.orig = GetForegroundWindow();
            let mon = MonitorFromWindow(if t.orig.is_invalid() { t.hwnd } else { t.orig }, MONITOR_DEFAULTTOPRIMARY);
            let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(mon, &mut mi);
            t.mon = mi.rcMonitor;
            let rc = t.mon;
            let _ = SetWindowPos(t.hwnd, Some(HWND_TOPMOST), rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top, SWP_NOACTIVATE);
            t.scale = GetDpiForWindow(t.hwnd).max(96) as f32 / 96.0;
            if t.painter.is_none() {
                match Painter::new(t.hwnd, (rc.right - rc.left) as u32, (rc.bottom - rc.top) as u32) {
                    Ok(p) => t.painter = Some(p),
                    Err(e) => log::line(&format!("task view painter: {e}")),
                }
            }
        }
        t.build();
        t.shown = true;
        t.hover = Hover::None;
        t.drag = None;
        t.drop_on = None;
        t.draw();
        // SAFETY: shows our window.
        unsafe {
            let _ = ShowWindow(t.hwnd, SW_SHOW);
        }
        let tiles: Vec<String> =
            t.layout.tiles.iter().map(|r| format!("{},{} {}x{}", r.rect.x, r.rect.y, r.rect.w, r.rect.h)).collect();
        let desks: Vec<String> =
            t.strip.desks.iter().map(|d| format!("{},{} {}x{}", d.rect.x, d.rect.y, d.rect.w, d.rect.h)).collect();
        let n = &t.strip.new_desk;
        log::line(&format!("task view: tiles {}; desks {}; new {},{} {}x{}", tiles.join("; "), desks.join("; "), n.x, n.y, n.w, n.h));
        log::line(&format!(
            "task view: {} windows ({}), desktops {:?} current {}, service {}, shown in {:.1} ms",
            t.entries.len(),
            t.entries.iter().map(|e| format!("{:?}", e.title)).collect::<Vec<_>>().join(", "),
            t.desk_names,
            t.current,
            t.service,
            t0.elapsed().as_secs_f32() * 1000.0
        ));
        // SAFETY: plain DPI query of our own window.
        let px = (16.0 * unsafe { GetDpiForWindow(t.hwnd) }.max(96) as f32 / 96.0).round() as i32;
        let wanted: Vec<_> = t
            .entries
            .iter()
            .filter(|e| !t.icons.contains_key(&e.key))
            .map(|e| (e.key.clone(), e.aumid.clone(), e.exe.clone()))
            .collect();
        t.acting = true;
        (t.hwnd, wanted, px)
    });
    if let Some((hwnd, wanted, px)) = r {
        let how = activate::activate(hwnd);
        with(|t| t.acting = false);
        log::line(&format!("task view: focus via {how}"));
        crate::window::spawn_icons(hwnd, wanted, px);
    }
}

/// Hides Task View; `then` is the window to bring to the front, else the original foreground.
fn close(then: Option<HWND>) {
    let r = with(|t| {
        if !t.shown {
            return None;
        }
        t.shown = false;
        t.acting = true;
        t.unregister();
        // SAFETY: our own window.
        unsafe {
            let _ = ReleaseCapture();
            let _ = KillTimer(Some(t.hwnd), T_REBUILD);
            let _ = ShowWindow(t.hwnd, SW_HIDE);
        }
        Some(then.unwrap_or(t.orig))
    })
    .flatten();
    if let Some(h) = r {
        let how = if h.is_invalid() { "nothing" } else { activate::activate(h) };
        log::line(&format!("task view: closed, {:?} in front via {how}", wl::title(h)));
    }
    with(|t| t.acting = false);
}

fn rebuild_soon() {
    with(|t| {
        // SAFETY: a one-shot timer on our window.
        unsafe { SetTimer(Some(t.hwnd), T_REBUILD, 250, None) };
    });
}

fn dips(lp: LPARAM, s: f32) -> (f32, f32) {
    ((lp.0 & 0xFFFF) as i16 as f32 / s, ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / s)
}

fn pt(lp: LPARAM) -> POINT {
    POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 }
}

fn on_move(hwnd: HWND, lp: LPARAM) {
    with(|t| {
        if !t.shown {
            return;
        }
        let (x, y) = dips(lp, t.scale);
        if let Some(d) = t.drag.as_mut() {
            let p = pt(lp);
            if !d.active && ((p.x - d.from.x).abs() > DRAG_START || (p.y - d.from.y).abs() > DRAG_START) && t.service {
                d.active = true;
                // SAFETY: capture on our own window while the drag runs.
                unsafe { SetCapture(hwnd) };
                log::line(&format!("task view: drag {:?}", t.entries.get(d.index).map(|e| e.title.as_str()).unwrap_or("")));
            }
            if d.active {
                let i = d.index;
                // The thumbnail follows the cursor at half size, centered on it.
                if let (Some(tile), Some(id)) = (t.layout.tiles.get(i), t.thumbs.get(i)) {
                    let (w, h) = (tile.thumb.w * 0.5, tile.thumb.h * 0.5);
                    thumb(*id, &Rect::new(x - w / 2.0, y - h / 2.0, w, h), t.scale, false, 220);
                }
                let over = t.strip.hit(x, y).filter(|&k| k != t.current);
                if over != t.drop_on {
                    t.drop_on = over;
                    t.draw();
                } else if t.drop_on.is_none() {
                    t.draw();
                }
                return;
            }
        }
        let h = t.hit(x, y);
        if h != t.hover {
            t.hover = h;
            t.draw();
        }
    });
}

fn on_down(lp: LPARAM) {
    with(|t| {
        let (x, y) = dips(lp, t.scale);
        if let Hover::Window(i) = t.hit(x, y) {
            t.drag = Some(Drag { index: i, from: pt(lp), active: false });
        }
    });
}

enum Act {
    None,
    Close(Option<HWND>),
    CloseWindow(HWND, String),
    Switch(usize),
    New,
    RemoveDesk(usize),
    Move(HWND, String, usize),
}

fn on_up(lp: LPARAM) {
    let act = with(|t| {
        let (x, y) = dips(lp, t.scale);
        // SAFETY: ends a capture we may hold.
        let _ = unsafe { ReleaseCapture() };
        if let Some(d) = t.drag.take().filter(|d| d.active) {
            let target = t.drop_on.take();
            let e = &t.entries[d.index];
            return match target {
                Some(k) => Act::Move(e.hwnd, e.title.clone(), k),
                None => {
                    // Dropped elsewhere: back to its tile.
                    if let (Some(tile), Some(id)) = (t.layout.tiles.get(d.index), t.thumbs.get(d.index)) {
                        thumb(*id, &tile.thumb, t.scale, false, 255);
                    }
                    t.draw();
                    Act::None
                }
            };
        }
        match t.hit(x, y) {
            Hover::Window(i) => Act::Close(Some(t.entries[i].hwnd)),
            Hover::WindowClose(i) => Act::CloseWindow(t.entries[i].hwnd, t.entries[i].title.clone()),
            Hover::Desk(i) => Act::Switch(i),
            Hover::DeskClose(i) => Act::RemoveDesk(i),
            Hover::NewDesk => Act::New,
            Hover::None => Act::Close(None),
        }
    })
    .unwrap_or(Act::None);
    run(act);
}

fn run(act: Act) {
    let t0 = Instant::now();
    match act {
        Act::None => {}
        Act::Close(h) => close(h),
        Act::CloseWindow(h, title) => {
            // SAFETY: asks the window to close, as its own close button does.
            let _ = unsafe { PostMessageW(Some(h), WM_SYSCOMMAND, WPARAM(SC_CLOSE as usize), LPARAM(0)) };
            log::line(&format!("task view: close {title:?}"));
            rebuild_soon();
        }
        Act::Switch(i) => {
            close(Some(HWND::default()));
            match desktops::switch_to(i) {
                Ok(how) => log::line(&format!("task view: switched to desktop {} via {how} in {:.1} ms", i + 1, ms(t0))),
                Err(e) => log::line(&format!("task view: switch to desktop {}: {e}", i + 1)),
            }
        }
        Act::New => {
            let r = desktops::with_vd(|vd| vd.create().map(|d| d.name));
            log::line(&format!("task view: new desktop {:?} in {:.1} ms", r.as_ref().map_err(|e| e.to_string()), ms(t0)));
            rebuild_now();
        }
        Act::RemoveDesk(i) => {
            let r = desktops::close(i);
            log::line(&format!("task view: remove desktop {}: {:?} in {:.1} ms", i + 1, r.map_err(|e| e.to_string()), ms(t0)));
            rebuild_now();
        }
        Act::Move(h, title, k) => {
            let r = desktops::with_vd(|vd| {
                let desks = vd.desktops()?;
                let d = desks.get(k).ok_or(windows_core::Error::from_hresult(windows_core::HRESULT(0x80070057u32 as i32)))?;
                vd.move_window(h, d)?;
                Ok(vd.window_desktop(h).map(|g| g == d.id))
            });
            log::line(&format!(
                "task view: moved {title:?} to desktop {}: {:?} (window now on it: {:?}) in {:.1} ms",
                k + 1,
                r.as_ref().map(|_| "ok").map_err(|e| e.to_string()),
                r.as_ref().ok().copied().flatten(),
                ms(t0)
            ));
            rebuild_now();
        }
    }
}

fn rebuild_now() {
    with(|t| {
        if t.shown {
            t.hover = Hover::None;
            t.build();
            t.draw();
        }
    });
}

fn ms(t: Instant) -> f32 {
    t.elapsed().as_secs_f32() * 1000.0
}

pub fn icons_arrived(got: Vec<(String, Option<Image>)>) {
    with(|t| {
        t.icons.extend(got);
        t.draw();
    });
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEMOVE => {
            on_move(hwnd, lp);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            on_down(lp);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            on_up(lp);
            LRESULT(0)
        }
        WM_KEYDOWN if wp.0 == VK_ESCAPE.0 as usize => {
            close(None);
            LRESULT(0)
        }
        WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            let leave = with(|t| t.shown && !t.acting).unwrap_or(false);
            if leave {
                log::line("task view: lost the focus");
                // Another window took the focus: leave it there.
                close(Some(HWND::default()));
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == T_REBUILD => {
            // SAFETY: a one-shot use of our own timer.
            let _ = unsafe { KillTimer(Some(hwnd), T_REBUILD) };
            rebuild_now();
            LRESULT(0)
        }
        WM_ICONS => {
            // SAFETY: the box was made by `spawn_icons` and is ours now.
            let got = unsafe { Box::from_raw(lp.0 as *mut Vec<(String, Option<Image>)>) };
            icons_arrived(*got);
            LRESULT(0)
        }
        WM_PAINT => {
            // SAFETY: nothing to paint by GDI; the frame comes from the swap chain.
            let _ = unsafe { ValidateRect(Some(hwnd), None) };
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}
