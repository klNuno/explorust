//! The Windows 11 menu window: one resident popup per level (the root takes the keyboard, the
//! submenus never activate), a modal loop like `TrackPopupMenu`'s, mouse and keyboard input.

use super::paint::{self, Fonts, View};
use super::{log, read};
use crate::model::{self, Entry, Hit, Kind, Layout};
use render::Surface;
use std::cell::RefCell;
use std::time::Instant;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;
use windows::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::IContextMenu;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::w;

const WM_MOUSELEAVE: u32 = 0x02A3;
const CLASS: windows_core::PCWSTR = w!("explorust.menu");
const TIMER_SUB: usize = 1;
const HOVER_MS: u32 = 300;
/// Acrylic tint of the dark flyout (ABGR).
const TINT: u32 = 0xE62C_2C2C;

/// What the user chose.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pick {
    Command(u32),
    More,
}

struct Win {
    hwnd: HWND,
    surface: Surface,
    size: (u32, u32),
}

struct Level {
    win: usize,
    entries: Vec<Entry>,
    top: Vec<(String, u32)>,
    layout: Layout,
    hover: Option<Hit>,
    open: Option<usize>,
    bitmaps: Vec<Option<ID2D1Bitmap1>>,
    /// Screen position of the window.
    origin: (i32, i32),
}

struct Ui {
    fonts: Fonts,
    pool: Vec<Win>,
    levels: Vec<Level>,
    scale: f32,
    cm: Option<IContextMenu>,
    pick: Option<Pick>,
    done: bool,
    pressed: bool,
    tracking: Vec<bool>,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
    static HOOK: RefCell<Option<HHOOK>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    UI.with(|u| u.try_borrow_mut().ok().and_then(|mut b| b.as_mut().map(f)))
}

fn new_window(root: bool) -> windows_core::Result<Win> {
    // SAFETY: a window of this thread's class with DWM attributes on it.
    unsafe {
        let inst = GetModuleHandleW(None)?;
        let ex = WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | if root { WINDOW_EX_STYLE(0) } else { WS_EX_NOACTIVATE };
        let hwnd = CreateWindowExW(ex, CLASS, w!(""), WS_POPUP, 0, 0, 64, 64, None, None, Some(inst.into()), None)?;
        render::backdrop::apply(hwnd, render::backdrop::Backdrop::Acrylic { tint_abgr: TINT });
        let round = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &round as *const _ as _, 4);
        let border = 0x0040_4040u32;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &border as *const _ as _, 4);
        let dark = 1u32;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, 4);
        let surface = Surface::new(hwnd, 64, 64)?;
        Ok(Win { hwnd, surface, size: (64, 64) })
    }
}

/// Makes this thread's fonts and root window ahead of the first right-click.
pub fn prewarm() {
    UI.with(|u| {
        if u.borrow().is_some() {
            return;
        }
        let t = Instant::now();
        // SAFETY: registers this thread's window class once; a second call fails harmlessly.
        unsafe {
            let inst = GetModuleHandleW(None).unwrap_or_default();
            let cursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
            let wc = WNDCLASSW {
                style: CS_DROPSHADOW,
                lpfnWndProc: Some(proc_),
                hInstance: inst.into(),
                lpszClassName: CLASS,
                hCursor: cursor,
                ..Default::default()
            };
            RegisterClassW(&wc);
        }
        let ui = Fonts::new().and_then(|fonts| {
            let root = new_window(true)?;
            Ok(Ui {
                fonts,
                pool: vec![root],
                levels: Vec::new(),
                scale: 1.0,
                cm: None,
                pick: None,
                done: false,
                pressed: false,
                tracking: vec![false],
            })
        });
        match ui {
            Ok(ui) => {
                *u.borrow_mut() = Some(ui);
                log::line(&format!("prewarm: fonts and root window in {:.1} ms", t.elapsed().as_secs_f64() * 1000.0));
            }
            Err(e) => log::line(&format!("prewarm failed: {e}")),
        }
    });
}

fn work_area(x: i32, y: i32) -> (i32, i32, i32, i32) {
    // SAFETY: monitor queries with an out struct.
    unsafe {
        let m = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(m, &mut mi);
        let r = mi.rcWork;
        (r.left, r.top, r.right, r.bottom)
    }
}

impl Ui {
    fn px(&self, v: f32) -> i32 {
        (v * self.scale).round() as i32
    }

    /// Builds level `depth` from entries and shows it; `at` is the anchor: a point for the root,
    /// the parent row's screen rect for a submenu.
    fn open(&mut self, entries: Vec<Entry>, top: Vec<(String, u32)>, at: RECT, sub: bool) -> windows_core::Result<()> {
        let depth = self.levels.len();
        while self.pool.len() <= depth {
            self.pool.push(new_window(false)?);
            self.tracking.push(false);
        }
        let widths = self.fonts.measure(&entries);
        let layout = model::layout(top.len(), &entries, &widths);
        let (w, h) = (self.px(layout.width), self.px(layout.height));
        let area = work_area(at.left, at.top);
        let (x, y) = if sub {
            let (x, y) = model::place_sub((at.left, at.top, at.right, at.bottom), w, h, area, self.px(4.0));
            (x, y - self.px(model::metrics::PAD))
        } else {
            model::place(at.left, at.top, w, h, area)
        };
        let win = &mut self.pool[depth];
        if win.size != (w as u32, h as u32) {
            win.surface.resize(w as u32, h as u32)?;
            win.size = (w as u32, h as u32);
        }
        let bitmaps = entries
            .iter()
            .map(|e| e.bitmap.as_ref().and_then(|(bw, bh, px)| win.surface.bitmap(*bw, *bh, px).ok()))
            .collect();
        let level = Level { win: depth, entries, top, layout, hover: None, open: None, bitmaps, origin: (x, y) };
        self.levels.push(level);
        self.draw(depth);
        let hwnd = self.pool[depth].hwnd;
        // SAFETY: shows our own popup; the root takes the foreground for the keyboard.
        unsafe {
            let flags = SWP_SHOWWINDOW | if sub || depth > 0 { SWP_NOACTIVATE } else { SET_WINDOW_POS_FLAGS(0) };
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, flags);
            if depth == 0 {
                let _ = SetForegroundWindow(hwnd);
                let _ = SetFocus(Some(hwnd));
            }
        }
        Ok(())
    }

    fn draw(&self, depth: usize) {
        let Some(l) = self.levels.get(depth) else { return };
        let v = View {
            entries: &l.entries,
            top: &l.top,
            layout: &l.layout,
            hover: l.hover,
            open: l.open,
            pressed: self.pressed && depth + 1 == self.levels.len(),
            bitmaps: &l.bitmaps,
        };
        if let Err(e) = paint::draw(&self.pool[l.win].surface, &self.fonts, &v, self.scale) {
            log::line(&format!("draw level {depth}: {e}"));
        }
    }

    /// Closes the levels deeper than `depth`.
    fn close_from(&mut self, depth: usize) {
        while self.levels.len() > depth {
            let l = self.levels.pop().unwrap();
            // SAFETY: hides our own popup.
            unsafe {
                let _ = ShowWindow(self.pool[l.win].hwnd, SW_HIDE);
            }
        }
        if let Some(parent) = depth.checked_sub(1).and_then(|d| self.levels.get_mut(d)) {
            parent.open = None;
        }
        if depth > 0 {
            self.draw(depth - 1);
        }
    }

    fn level_of(&self, hwnd: HWND) -> Option<usize> {
        self.levels.iter().position(|l| self.pool[l.win].hwnd == hwnd)
    }

    fn row_rect(&self, depth: usize, row: usize) -> RECT {
        let l = &self.levels[depth];
        let (y, h) = l.layout.rows[row];
        let (ox, oy) = l.origin;
        RECT { left: ox, top: oy + self.px(y), right: ox + self.px(l.layout.width), bottom: oy + self.px(y + h) }
    }

    /// Opens the submenu of `row` at `depth`, filling it through the handler first.
    fn open_sub(&mut self, depth: usize, row: usize, select_first: bool) {
        if self.levels.get(depth).and_then(|l| l.open) == Some(row) && self.levels.len() > depth + 1 {
            return;
        }
        self.close_from(depth + 1);
        let Some(cm) = self.cm.clone() else { return };
        let e = &self.levels[depth].entries[row];
        if e.kind != Kind::Submenu || !e.enabled {
            return;
        }
        let sub = HMENU(e.submenu as *mut _);
        let t = Instant::now();
        read::init_popup(&cm, sub, row as u32);
        let entries = model::tidy(read::entries(&cm, sub, super::FIRST));
        log::line(&format!("submenu '{}': {} entries in {:.1} ms", e.label, entries.len(), t.elapsed().as_secs_f64() * 1000.0));
        let entries: Vec<Entry> = entries.into_iter().filter(|e| e.kind == Kind::Separator || !e.label.is_empty()).collect();
        if entries.is_empty() {
            return;
        }
        let at = self.row_rect(depth, row);
        self.levels[depth].open = Some(row);
        if let Err(err) = self.open(entries, Vec::new(), at, true) {
            log::line(&format!("submenu window: {err}"));
            return;
        }
        if select_first {
            let d = depth + 1;
            self.levels[d].hover = model::next_row(&self.levels[d].entries, None, 1).map(Hit::Row);
            self.draw(d);
        }
    }

    fn activate(&mut self, depth: usize, hit: Hit, keyboard: bool) {
        match hit {
            Hit::Top(i) => {
                if let Some(&(_, id)) = self.levels[depth].top.get(i) {
                    self.finish(Some(Pick::Command(id)));
                }
            }
            Hit::Row(i) => {
                let e = &self.levels[depth].entries[i];
                if !e.enabled {
                    return;
                }
                match e.kind {
                    Kind::Command => {
                        let id = e.id;
                        self.finish(Some(Pick::Command(id)));
                    }
                    Kind::More => self.finish(Some(Pick::More)),
                    Kind::Submenu => self.open_sub(depth, i, keyboard),
                    Kind::Separator => {}
                }
            }
        }
    }

    fn finish(&mut self, pick: Option<Pick>) {
        self.pick = pick;
        self.done = true;
    }

    fn key(&mut self, vk: u16) {
        let Some(depth) = self.levels.len().checked_sub(1) else { return };
        let l = &self.levels[depth];
        let cur = l.hover;
        let row = match cur {
            Some(Hit::Row(r)) => Some(r),
            _ => None,
        };
        let n_top = l.top.len();
        let set = |ui: &mut Ui, h: Option<Hit>| {
            ui.levels[depth].hover = h;
            ui.draw(depth);
        };
        match VIRTUAL_KEY(vk) {
            VK_DOWN => match cur {
                Some(Hit::Top(_)) => set(self, model::next_row(&l.entries, None, 1).map(Hit::Row)),
                _ => {
                    let next = model::next_row(&l.entries, row, 1);
                    let wrapped = matches!((row, next), (Some(a), Some(b)) if b <= a);
                    let h = if (wrapped || row.is_none() && next.is_none()) && n_top > 0 { Some(Hit::Top(0)) } else { next.map(Hit::Row) };
                    set(self, if row.is_none() && n_top > 0 { Some(Hit::Top(0)) } else { h });
                }
            },
            VK_UP => {
                let prev = model::next_row(&l.entries, row, -1);
                let wrapped = matches!((row, prev), (Some(a), Some(b)) if b >= a);
                match cur {
                    Some(Hit::Top(_)) => set(self, model::next_row(&l.entries, None, -1).map(Hit::Row)),
                    _ if wrapped && n_top > 0 => set(self, Some(Hit::Top(0))),
                    _ => set(self, prev.map(Hit::Row)),
                }
            }
            VK_HOME => set(self, if n_top > 0 { Some(Hit::Top(0)) } else { model::next_row(&l.entries, None, 1).map(Hit::Row) }),
            VK_END => set(self, model::next_row(&l.entries, None, -1).map(Hit::Row)),
            VK_RIGHT => match cur {
                Some(Hit::Top(i)) => set(self, Some(Hit::Top((i + 1) % n_top))),
                Some(Hit::Row(r)) if l.entries[r].kind == Kind::Submenu => self.open_sub(depth, r, true),
                _ => {}
            },
            VK_LEFT => match cur {
                Some(Hit::Top(i)) => set(self, Some(Hit::Top((i + n_top - 1) % n_top))),
                _ if depth > 0 => self.close_from(depth),
                _ => {}
            },
            VK_RETURN | VK_SPACE => {
                if let Some(h) = cur {
                    self.activate(depth, h, true);
                }
            }
            VK_ESCAPE => {
                if depth > 0 {
                    self.close_from(depth);
                } else {
                    self.finish(None);
                }
            }
            VK_F10 if unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0 => self.finish(Some(Pick::More)),
            VK_APPS => self.finish(None),
            _ => {}
        }
    }

    fn char(&mut self, c: char) {
        let Some(depth) = self.levels.len().checked_sub(1) else { return };
        let hits = model::by_mnemonic(&self.levels[depth].entries, c);
        match hits.as_slice() {
            [] => {}
            [one] => self.activate(depth, Hit::Row(*one), true),
            many => {
                let cur = match self.levels[depth].hover {
                    Some(Hit::Row(r)) => Some(r),
                    _ => None,
                };
                let next = many.iter().find(|&&i| Some(i) > cur).or(many.first()).copied();
                self.levels[depth].hover = next.map(Hit::Row);
                self.draw(depth);
            }
        }
    }

    fn mouse_move(&mut self, depth: usize, x: f32, y: f32) {
        let l = &self.levels[depth];
        let h = model::hit(&l.layout, &l.entries, x, y);
        if h != l.hover {
            self.levels[depth].hover = h;
            self.draw(depth);
            // Hovering decides about submenus after a delay, as Windows does.
            let root = self.pool[0].hwnd;
            // SAFETY: a one-shot timer on our root window, killed when it fires.
            unsafe {
                SetTimer(Some(root), TIMER_SUB, HOVER_MS, None);
            }
        }
        // A level deeper than one being hovered keeps its parent row lit.
    }

    /// The hover timer: open the hovered submenu, or close the ones the pointer left.
    fn hover_timer(&mut self) {
        let Some(depth) = (0..self.levels.len()).rev().find(|&d| self.levels[d].hover.is_some()) else { return };
        match self.levels[depth].hover {
            Some(Hit::Row(r)) if self.levels[depth].entries[r].kind == Kind::Submenu => self.open_sub(depth, r, false),
            Some(_) if self.levels.len() > depth + 1 => self.close_from(depth + 1),
            _ => {}
        }
    }
}

fn dips(lp: LPARAM, scale: f32) -> (f32, f32) {
    let x = (lp.0 & 0xFFFF) as i16 as f32;
    let y = ((lp.0 >> 16) & 0xFFFF) as i16 as f32;
    (x / scale, y / scale)
}

unsafe extern "system" fn proc_(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let handled = match msg {
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_MOUSEMOVE => with(|ui| {
            if let Some(d) = ui.level_of(h) {
                if !ui.tracking[d] {
                    ui.tracking[d] = true;
                    let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: h, dwHoverTime: 0 };
                    // SAFETY: asks for WM_MOUSELEAVE on our window.
                    let _ = unsafe { TrackMouseEvent(&mut tme) };
                }
                let (x, y) = dips(lp, ui.scale);
                ui.mouse_move(d, x, y);
            }
        }),
        WM_MOUSELEAVE => with(|ui| {
            if let Some(d) = ui.level_of(h) {
                ui.tracking[d] = false;
                if ui.levels[d].hover.is_some() {
                    ui.levels[d].hover = None;
                    ui.draw(d);
                }
            }
        }),
        WM_LBUTTONDOWN | WM_RBUTTONDOWN => with(|ui| {
            ui.pressed = true;
            if let Some(d) = ui.level_of(h) {
                ui.draw(d);
            }
        }),
        WM_LBUTTONUP | WM_RBUTTONUP => with(|ui| {
            ui.pressed = false;
            if let Some(d) = ui.level_of(h) {
                let (x, y) = dips(lp, ui.scale);
                let l = &ui.levels[d];
                match model::hit(&l.layout, &l.entries, x, y) {
                    Some(hit) => ui.activate(d, hit, false),
                    None => ui.draw(d),
                }
            }
        }),
        WM_KEYDOWN | WM_SYSKEYDOWN => with(|ui| {
            if wp.0 as u16 == VK_MENU.0 || wp.0 as u16 == VK_F4.0 && msg == WM_SYSKEYDOWN {
                ui.finish(None);
            } else {
                ui.key(wp.0 as u16);
            }
        }),
        WM_CHAR => with(|ui| {
            if let Some(c) = char::from_u32(wp.0 as u32).filter(|c| !c.is_control()) {
                ui.char(c);
            }
        }),
        WM_TIMER if wp.0 == TIMER_SUB => {
            // SAFETY: our own timer.
            unsafe {
                let _ = KillTimer(Some(h), TIMER_SUB);
            }
            with(|ui| ui.hover_timer())
        }
        WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => with(|ui| {
            let to = HWND(lp.0 as _);
            if !ui.done && !ui.levels.is_empty() && ui.level_of(to).is_none() {
                ui.finish(None);
            }
        }),
        _ => None,
    };
    if handled.is_some() && msg != WM_ACTIVATE {
        return LRESULT(0);
    }
    // SAFETY: default handling.
    unsafe { DefWindowProcW(h, msg, wp, lp) }
}

/// Dismisses the menu on a press outside its windows (the press goes on to its window).
unsafe extern "system" fn mouse_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0
        && matches!(wp.0 as u32, WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_NCLBUTTONDOWN | WM_NCRBUTTONDOWN)
    {
        // SAFETY: lParam is a MOUSEHOOKSTRUCT for WH_MOUSE.
        let target = unsafe { (*(lp.0 as *const MOUSEHOOKSTRUCT)).hwnd };
        with(|ui| {
            if ui.level_of(target).is_none() {
                ui.finish(None);
            }
        });
    }
    // SAFETY: passes the event on.
    unsafe { CallNextHookEx(None, code, wp, lp) }
}

/// Shows the Windows 11 menu and runs its modal loop; returns what was picked.
pub fn run(cm: &IContextMenu, entries: Vec<Entry>, top: Vec<(String, u32)>, owner: HWND, at: POINT, t0: Instant) -> Option<Pick> {
    prewarm();
    let started = with(|ui| {
        // SAFETY: DPI of the owner's monitor.
        ui.scale = unsafe { GetDpiForWindow(owner) } as f32 / 96.0;
        if ui.scale <= 0.0 {
            ui.scale = 1.0;
        }
        ui.cm = Some(cm.clone());
        ui.pick = None;
        ui.done = false;
        ui.pressed = false;
        let n = entries.len();
        let r = ui.open(entries, top, RECT { left: at.x, top: at.y, right: at.x, bottom: at.y }, false);
        if let Err(e) = &r {
            log::line(&format!("open: {e}"));
        }
        log::line(&format!("first frame presented {:.1} ms after the right-click ({n} rows)", t0.elapsed().as_secs_f64() * 1000.0));
        r.is_ok()
    });
    if started != Some(true) {
        return None;
    }
    // SAFETY: a thread mouse hook for the modal loop, removed below.
    unsafe {
        let hook = SetWindowsHookExW(WH_MOUSE, Some(mouse_hook), None, windows::Win32::System::Threading::GetCurrentThreadId()).ok();
        HOOK.with(|h| *h.borrow_mut() = hook);
    }
    let mut msg = MSG::default();
    loop {
        if with(|ui| ui.done) != Some(false) {
            break;
        }
        // SAFETY: the thread's message loop while the menu shows.
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if r.0 <= 0 {
            // WM_QUIT: hand it back to the thread's own loop.
            unsafe { PostQuitMessage(msg.wParam.0 as i32) };
            with(|ui| ui.finish(None));
            break;
        }
        // Keys typed while the menu shows are the menu's.
        if matches!(msg.message, WM_KEYDOWN | WM_KEYUP | WM_CHAR | WM_SYSKEYDOWN | WM_SYSKEYUP) {
            if let Some(root) = with(|ui| ui.pool[0].hwnd) {
                msg.hwnd = root;
            }
        }
        // SAFETY: standard translation and dispatch.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    // SAFETY: removes the hook set above.
    unsafe {
        if let Some(h) = HOOK.with(|h| h.borrow_mut().take()) {
            let _ = UnhookWindowsHookEx(h);
        }
    }
    with(|ui| {
        ui.close_from(0);
        ui.cm = None;
        ui.pick
    })
    .flatten()
}
