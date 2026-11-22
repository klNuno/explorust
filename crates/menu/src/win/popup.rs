//! The Windows 11 menu window: one resident popup per level (the root takes the keyboard, the
//! submenus never activate), a modal loop like `TrackPopupMenu`'s, mouse and keyboard input.

use super::paint::{self, Fonts, View};
use super::{log, read, tip::Tip, worker};
use crate::model::{self, CommandItem, Entry, Hit, Kind, Layout, Recent};
use render::Surface;
use render::style::{self, Native, Part, WindowStyle};
use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;
use std::time::Instant;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;
use windows::Win32::Graphics::Dwm::{
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWCP_ROUND, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    CreateRoundRectRgn, DeleteObject, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromPoint, SetWindowRgn,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::IContextMenu;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::w;

const WM_MOUSELEAVE: u32 = 0x02A3;
const CLASS: windows_core::PCWSTR = w!("explorust.menu");
const TIMER_SUB: usize = 1;
const TIMER_TIP: usize = 2;
const TIP_MS: u32 = 500;
const HOVER_MS: u32 = 300;
/// Acrylic tint of the dark flyout (ABGR).
const TINT: u32 = 0xE62C_2C2C;

/// What the user chose: a command id with its verb, or the classic menu.
#[derive(Clone, Debug, PartialEq)]
pub enum Pick {
    Command(u32, String),
    More,
    /// The worker could not build the selection's menu: the caller shows the view's own.
    Fallback,
}

/// Where the menu's rows come from.
#[derive(Clone)]
pub enum Src {
    /// A menu this thread fills (the view's background menu).
    Local(IContextMenu),
    /// The worker's menu of this generation.
    Worker(u64),
    /// Static commands owned by an explorust surface.
    Commands(Rc<Commands>),
}

pub struct Commands {
    root: Vec<Entry>,
    submenus: Vec<Vec<Entry>>,
    centered: bool,
}

struct Win {
    hwnd: HWND,
    surface: Surface,
    /// Handler icons as D2D bitmaps of this window's device, by hash of their pixels: made once,
    /// kept across opens.
    icons: HashMap<u64, ID2D1Bitmap1>,
}

/// Icon cache use of one level: (bitmaps reused, bitmaps made).
type IconUse = (u32, u32);

impl Win {
    /// The D2D bitmap of a handler icon, from the cache when the same pixels were seen before.
    fn icon(&mut self, b: &Option<(u32, u32, Vec<u8>)>, used: &mut IconUse) -> Option<ID2D1Bitmap1> {
        let (w, h, px) = b.as_ref()?;
        let mut hs = DefaultHasher::new();
        (w, h, px).hash(&mut hs);
        let k = hs.finish();
        if let Some(b) = self.icons.get(&k) {
            used.0 += 1;
            return Some(b.clone());
        }
        let b = self.surface.bitmap(*w, *h, px).ok()?;
        if self.icons.len() >= 256 {
            self.icons.clear();
        }
        self.icons.insert(k, b.clone());
        used.1 += 1;
        Some(b)
    }

    fn icons_of(&mut self, entries: &[Entry]) -> (Vec<Option<ID2D1Bitmap1>>, IconUse) {
        let mut used = (0, 0);
        let v = entries.iter().map(|e| self.icon(&e.bitmap, &mut used)).collect();
        (v, used)
    }
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
    src: Option<Src>,
    /// The handlers' rows are in (the menu shows what the worker read, not the static part).
    ready: bool,
    /// A row picked before the handlers' rows arrived: its verb and label.
    pending: Option<(String, String)>,
    /// A submenu asked from the worker: (depth, row).
    sub_wait: Option<(usize, usize)>,
    /// What kind of menu shows (`model::Recent` key), whether it is a selection's, its label.
    key: String,
    items: bool,
    more: &'static str,
    /// Prewarm fills in flight: generation and the key they fill.
    prewarm: Vec<(u64, String)>,
    recent: Recent,
    at: POINT,
    t0: Instant,
    pick: Option<Pick>,
    done: bool,
    pressed: bool,
    tracking: Vec<bool>,
    tip: Option<Tip>,
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
        let appearance = WindowStyle {
            native: Native::Solid(TINT),
            border: 0x0040_4040,
            corner: DWMWCP_ROUND,
            popup: true,
        };
        style::bind_with_repaint(hwnd, Part::Menus, appearance, Some(|hwnd| {
            with(|ui| {
                if let Some(depth) = ui.level_of(hwnd) { ui.draw(depth); }
            });
        }));
        let dark = 1u32;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, 4);
        let surface = Surface::new(hwnd, 64, 64)?;
        Ok(Win { hwnd, surface, icons: HashMap::new() })
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
            let tip = Tip::new(root.hwnd);
            Ok(Ui {
                tip,
                fonts,
                pool: vec![root],
                levels: Vec::new(),
                scale: 1.0,
                src: None,
                ready: true,
                pending: None,
                sub_wait: None,
                key: String::new(),
                items: false,
                more: "",
                prewarm: Vec::new(),
                recent: Recent::default(),
                at: POINT::default(),
                t0: Instant::now(),
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

fn round_window(hwnd: HWND, width: i32, height: i32, diameter: i32) {
    // The region also clips the backdrop when DWM disables its own corner treatment.
    // SAFETY: Windows owns the region after SetWindowRgn succeeds.
    unsafe {
        let region = CreateRoundRectRgn(0, 0, width, height, diameter, diameter);
        if !region.is_invalid() && SetWindowRgn(hwnd, Some(region), true) == 0 {
            let _ = DeleteObject(region.into());
        }
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
        } else if let Some(Src::Commands(commands)) = &self.src {
            let x = at.left - if commands.centered { w / 2 } else { 0 };
            let y = at.top - h;
            (x.clamp(area.0, (area.2 - w).max(area.0)), y.clamp(area.1, (area.3 - h).max(area.1)))
        } else {
            model::place(at.left, at.top, w, h, area)
        };
        let win = &mut self.pool[depth];
        win.surface.resize(w as u32, h as u32)?;
        let (bitmaps, _) = win.icons_of(&entries);
        let level = Level { win: depth, entries, top, layout, hover: None, open: None, bitmaps, origin: (x, y) };
        self.levels.push(level);
        self.draw(depth);
        let hwnd = self.pool[depth].hwnd;
        round_window(hwnd, w, h, self.px(2.0 * model::metrics::CORNER_RADIUS));
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
        if depth == 0 {
            if let Some(t) = self.tip.as_mut() {
                t.hide();
            }
        }
        while self.levels.len() > depth {
            let l = self.levels.pop().unwrap();
            let win = &mut self.pool[l.win];
            // SAFETY: hides our own popup.
            unsafe {
                let _ = ShowWindow(win.hwnd, SW_HIDE);
            }
            if let Err(e) = win.surface.resize(64, 64) {
                log::line(&format!("hidden menu resize: {e}"));
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
        if !self.ready {
            return;
        }
        let Some(src) = self.src.clone() else { return };
        let e = &self.levels[depth].entries[row];
        if e.kind != Kind::Submenu || !e.enabled {
            return;
        }
        let entries = match src {
            Src::Local(cm) => {
                let sub = HMENU(e.submenu as *mut _);
                let t = Instant::now();
                read::init_popup(&cm, sub, row as u32);
                let entries = read::entries(&cm, sub, super::FIRST);
                log::line(&format!("submenu '{}': {} entries in {:.1} ms", e.label, entries.len(), t.elapsed().as_secs_f64() * 1000.0));
                entries
            }
            Src::Worker(gn) => {
                self.sub_wait = Some((depth, row));
                let reply = self.pool[0].hwnd.0 as isize;
                worker::send(worker::Job::Sub { gn, menu: e.submenu, index: row as u32, depth, row, reply });
                return;
            }
            Src::Commands(commands) => commands.submenus[e.submenu as usize].clone(),
        };
        self.show_sub(depth, row, entries, select_first);
    }

    /// Shows the entries of the submenu of `row` at `depth`.
    fn show_sub(&mut self, depth: usize, row: usize, entries: Vec<Entry>, select_first: bool) {
        let entries: Vec<Entry> = model::tidy(entries).into_iter().filter(|e| e.kind == Kind::Separator || !e.label.is_empty()).collect();
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
                if let Some((verb, id)) = self.levels[depth].top.get(i).cloned() {
                    if !self.ready {
                        self.pending = Some((verb, String::new()));
                        return;
                    }
                    self.finish(Some(Pick::Command(id, verb)));
                }
            }
            Hit::Row(i) => {
                let e = &self.levels[depth].entries[i];
                if !e.enabled {
                    return;
                }
                match e.kind {
                    Kind::Command if !self.ready => {
                        self.pending = Some((e.verb.clone(), e.label.clone()));
                    }
                    Kind::Command => {
                        let (id, verb) = (e.id, e.verb.clone());
                        self.finish(Some(Pick::Command(id, verb)));
                    }
                    Kind::More => self.finish(Some(Pick::More)),
                    Kind::Submenu => self.open_sub(depth, i, keyboard),
                    Kind::Separator => {}
                }
            }
        }
    }

    /// The handlers' rows arrived: the root level shows them in place of the static part, the
    /// window and its swap chain take the new size, and a row picked meanwhile runs.
    fn merge(&mut self, entries: &[Entry]) {
        let (top, list) = model::arrange(entries, self.items, self.more);
        self.recent.store(&self.key, &top, &list);
        self.ready = true;
        if self.done || self.levels.is_empty() {
            return;
        }
        let t = Instant::now();
        let n = list.len();
        self.close_from(1);
        let hover = self.levels[0].hover;
        let hover_verb = match hover {
            Some(Hit::Row(r)) => self.levels[0].entries.get(r).map(|e| (e.verb.clone(), e.label.clone())),
            _ => None,
        };
        let widths = self.fonts.measure(&list);
        let layout = model::layout(top.len(), &list, &widths);
        let (w, h) = (self.px(layout.width), self.px(layout.height));
        let area = work_area(self.at.x, self.at.y);
        let (x, y) = model::place(self.at.x, self.at.y, w, h, area);
        let win = &mut self.pool[0];
        if let Err(e) = win.surface.resize(w as u32, h as u32) {
            log::line(&format!("merge resize: {e}"));
        }
        let (bitmaps, icons) = win.icons_of(&list);
        let hover = match (hover, hover_verb) {
            (Some(Hit::Top(i)), _) if i < top.len() => Some(Hit::Top(i)),
            (_, Some((v, l))) => list.iter().position(|e| e.kind != Kind::Separator && e.verb == v && e.label == l).map(Hit::Row),
            _ => None,
        };
        let hwnd = win.hwnd;
        self.levels[0] = Level { win: 0, entries: list, top, layout, hover, open: None, bitmaps, origin: (x, y) };
        round_window(hwnd, w, h, self.px(2.0 * model::metrics::CORNER_RADIUS));
        // SAFETY: moves and sizes our own popup; the swap chain already has the new size.
        unsafe {
            let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
        }
        self.draw(0);
        log::line(&format!(
            "handlers' rows merged {:.1} ms after the right-click ({n} rows, merge {:.1} ms, icons {} cached {} made)",
            self.t0.elapsed().as_secs_f64() * 1000.0,
            t.elapsed().as_secs_f64() * 1000.0,
            icons.0,
            icons.1
        ));
        if let Some((verb, label)) = self.pending.take() {
            let l = &self.levels[0];
            let top = l.top.iter().position(|(v, _)| !verb.is_empty() && *v == verb).map(Hit::Top);
            let row = l
                .entries
                .iter()
                .position(|e| e.kind == Kind::Command && e.enabled && ((!verb.is_empty() && e.verb == verb) || (!label.is_empty() && e.label == label)))
                .map(Hit::Row);
            match top.or(row) {
                Some(hit) => self.activate(0, hit, false),
                None => {
                    log::line(&format!("picked '{label}' [{verb}] before the fill; the filled menu has no such row"));
                    self.finish(None);
                }
            }
        } else if let Some(Hit::Row(r)) = self.levels[0].hover {
            if self.levels[0].entries[r].kind == Kind::Submenu {
                // SAFETY: the hover timer on our root window.
                unsafe {
                    SetTimer(Some(hwnd), TIMER_SUB, HOVER_MS, None);
                }
            }
        }
    }

    fn on_reply(&mut self, r: worker::Reply) {
        let cur = match &self.src {
            Some(Src::Worker(g)) => Some(*g),
            _ => None,
        };
        match r {
            worker::Reply::Filled { gn, entries, fill_ms, ok } => {
                if let Some(i) = self.prewarm.iter().position(|(g, _)| *g == gn) {
                    let (_, key) = self.prewarm.remove(i);
                    if ok {
                        let (top, list) = model::arrange(&entries, key.starts_with("items"), super::more_label());
                        self.recent.store(&key, &top, &list);
                        log::line(&format!("prewarm {key}: {} rows in {fill_ms:.1} ms", list.len()));
                    }
                    return;
                }
                if cur != Some(gn) {
                    return;
                }
                if !ok {
                    log::line("worker fill failed: the view's own menu replaces it");
                    self.ready = true;
                    self.pending = None;
                    self.finish(Some(Pick::Fallback));
                    return;
                }
                log::line(&format!("worker filled {gn} in {fill_ms:.1} ms ({} entries)", entries.len()));
                self.merge(&entries);
            }
            worker::Reply::Sub { gn, depth, row, entries } => {
                if cur != Some(gn) || self.done || self.sub_wait != Some((depth, row)) {
                    return;
                }
                self.sub_wait = None;
                log::line(&format!("submenu row {row}: {} entries from the worker", entries.len()));
                let still = self.levels.get(depth).is_some_and(|l| l.hover == Some(Hit::Row(row)) || l.open == Some(row));
                if still && self.levels.len() == depth + 1 {
                    self.show_sub(depth, row, entries, true);
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
            VK_F10 if !matches!(self.src, Some(Src::Commands(_))) && unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0 => {
                self.finish(Some(Pick::More));
            }
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
            if let Some(t) = self.tip.as_mut() {
                t.hide();
            }
            if matches!(h, Some(Hit::Top(_))) {
                let root = self.pool[0].hwnd;
                // SAFETY: a one-shot timer on our root window.
                unsafe {
                    SetTimer(Some(root), TIMER_TIP, TIP_MS, None);
                }
            }
            // Hovering decides about submenus after a delay, as Windows does.
            let root = self.pool[0].hwnd;
            // SAFETY: a one-shot timer on our root window, killed when it fires.
            unsafe {
                SetTimer(Some(root), TIMER_SUB, HOVER_MS, None);
            }
        }
        // A level deeper than one being hovered keeps its parent row lit.
    }

    /// The tooltip timer: name the hovered top-row button under it.
    fn tip_timer(&mut self) {
        let Some(l) = self.levels.first() else { return };
        let Some(Hit::Top(i)) = l.hover else { return };
        let Some((verb, _)) = l.top.get(i) else { return };
        // SAFETY: plain query of the UI language.
        let text = model::top_tip(verb, unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() });
        if text.is_empty() {
            return;
        }
        let (bx, by, bw, bh) = l.layout.top[i];
        let (x, y) = (l.origin.0 + self.px(bx + bw / 2.0), l.origin.1 + self.px(by + bh) + self.px(6.0));
        if let Some(t) = self.tip.as_mut() {
            t.show(text, x, y);
        }
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
    if msg == worker::reply_msg() {
        // SAFETY: posted by the worker as a Box<Reply>.
        let mut r = Some(*unsafe { Box::from_raw(lp.0 as *mut worker::Reply) });
        with(|ui| {
            if let Some(r) = r.take() {
                ui.on_reply(r);
            }
        });
        if let Some(r) = r {
            LATE.with(|l| l.borrow_mut().push(r));
        }
        return LRESULT(0);
    }
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
                if let Some(t) = ui.tip.as_mut() {
                    t.hide();
                }
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
        WM_TIMER if wp.0 == TIMER_TIP => {
            // SAFETY: our own timer.
            unsafe {
                let _ = KillTimer(Some(h), TIMER_TIP);
            }
            with(|ui| ui.tip_timer())
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

/// One menu to show: where its rows come from and where it opens.
pub struct Request<'a> {
    pub src: Src,
    /// The `model::Recent` key of its kind.
    pub key: String,
    pub items: bool,
    pub more: &'static str,
    pub owner: HWND,
    pub at: POINT,
    pub t0: Instant,
    /// Fills a local menu after the first frame and returns its entries.
    pub fill: Option<&'a mut dyn FnMut() -> Vec<Entry>>,
}

thread_local! {
    /// Worker replies that came while the UI state was borrowed (a handler pumping messages).
    static LATE: RefCell<Vec<worker::Reply>> = const { RefCell::new(Vec::new()) };
}

/// A prewarm fill of generation `gn` is on its way for menus of kind `key`.
pub fn expect_prewarm(gn: u64, key: &str) {
    prewarm();
    with(|ui| ui.prewarm.push((gn, key.to_string())));
}

/// The window worker replies go to.
pub fn reply_window() -> Option<HWND> {
    prewarm();
    with(|ui| ui.pool[0].hwnd)
}

fn drain_late() {
    let late: Vec<worker::Reply> = LATE.with(|l| l.borrow_mut().drain(..).collect());
    for r in late {
        with(|ui| ui.on_reply(r));
    }
}

/// Opens a menu above a screen position. Returns the chosen command id.
pub fn track_commands(owner: HWND, at: POINT, items: &[CommandItem], centered: bool) -> Option<u32> {
    if items.is_empty() {
        return None;
    }
    let mut submenus = Vec::new();
    let root = model::command_rows(items, &mut submenus);
    let src = Src::Commands(Rc::new(Commands { root, submenus, centered }));
    let req = Request {
        src, owner, at, t0: Instant::now(), key: String::new(), items: false, more: "", fill: None,
    };
    match run(req) {
        Some(Pick::Command(id, _)) => Some(id),
        _ => None,
    }
}

/// Shows the Windows 11 menu and runs its modal loop; returns what was picked. The first frame
/// shows the previous menu of the same kind (or the static part of another); the handlers' rows
/// replace it when they arrive.
pub fn run(req: Request) -> Option<Pick> {
    prewarm();
    if with(|ui| ui.levels.is_empty()) != Some(true) {
        return None;
    }
    drain_late();
    let Request { src, key, items, more, owner, at, t0, mut fill } = req;
    // Worker selections are passed as IDs, so only the previous family's static rows apply.
    let lookup = if matches!(src, Src::Worker(_)) { "items" } else { &key };
    let cached = with(|ui| ui.recent.get(lookup)).flatten();
    // Nothing to show at once for a local menu: fill it first.
    let mut filled = None;
    if cached.is_none() {
        if let Some(f) = fill.take() {
            filled = Some(f());
        }
    }
    let started = with(|ui| {
        // SAFETY: DPI of the owner's monitor.
        ui.scale = unsafe { GetDpiForWindow(owner) } as f32 / 96.0;
        if ui.scale <= 0.0 {
            ui.scale = 1.0;
        }
        ui.src = Some(src);
        ui.key = key.clone();
        ui.items = items;
        ui.more = more;
        ui.at = at;
        ui.t0 = t0;
        ui.pending = None;
        ui.sub_wait = None;
        ui.pick = None;
        ui.done = false;
        ui.pressed = false;
        let (top, list, what) = match (&ui.src, &filled, cached) {
            (Some(Src::Commands(commands)), _, _) => {
                ui.ready = true;
                (Vec::new(), commands.root.clone(), "application commands")
            }
            (_, Some(entries), _) => {
                let (t, l) = model::arrange(entries, items, more);
                ui.recent.store(&key, &t, &l);
                ui.ready = true;
                (t, l, "filled")
            }
            (_, None, Some((t, l, same))) => {
                ui.ready = false;
                (t, l, if same { "previous menu of this kind" } else { "static part of another kind" })
            }
            (_, None, None) => {
                ui.ready = false;
                (Vec::new(), model::static_part(&model::arrange(&[], items, more).1), "nothing cached")
            }
        };
        let n = list.len();
        let r = ui.open(list, top, RECT { left: at.x, top: at.y, right: at.x, bottom: at.y }, false);
        if let Err(e) = &r {
            log::line(&format!("open: {e}"));
        }
        let _ = ui.pool[0].surface.slide_in(-ui.scale * 12.0, 0.2, 0.083);
        log::line(&format!("first frame presented {:.1} ms after the right-click ({n} rows, {what}, {key})", t0.elapsed().as_secs_f64() * 1000.0));
        r.is_ok()
    });
    if started != Some(true) {
        return None;
    }
    if let Some(f) = fill.take() {
        let entries = f();
        with(|ui| ui.merge(&entries));
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
        drain_late();
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
        if !matches!(ui.src, Some(Src::Worker(_))) {
            ui.src = None;
        }
        ui.pick.take()
    })
    .flatten()
}
