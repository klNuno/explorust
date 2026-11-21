//! A File Explorer window: our chrome on a composition swap chain over Mica at the top, Windows'
//! `ExplorerBrowser` (one per tab) below it. The frame is custom (the tab strip is the caption)
//! but keeps Windows' resize borders, shadow, rounded corners and animations.

use crate::edit::LineEdit;
use crate::history::Move;
use crate::layout::{self as L, Cmd, Hit};
use crate::text::Strings;
use crate::win::chrome::{self, TabView};
use crate::win::gfx::{Canvas, Shared};
use crate::win::shell::{self, Pidl};
use crate::win::tab::{self, Tab};
use crate::win::{log, menus, search, shellwindows};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::{BLACK_BRUSH, BeginPaint, EndPaint, FillRect, GetStockObject, HBRUSH, HDC, ScreenToClient};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
use windows::Win32::UI::Controls::{MARGINS, WM_MOUSELEAVE};
use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::System::Com::IServiceProvider;
use windows::Win32::UI::Shell::{
    FVM_DETAILS, FVM_ICON, IShellView, SIGDN_DESKTOPABSOLUTEEDITING, SVGIO_ALLVIEW, SVGIO_SELECTION,
    SVSI_DESELECTOTHERS, SVSI_ENSUREVISIBLE, SVSI_FOCUSED, SVSI_SELECT, SVUIA_ACTIVATE_FOCUS,
};
use windows_core::Interface;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{PCWSTR, w};

const CLASS: PCWSTR = w!("explorust.FilesWindow");
pub const MSG_RELAYOUT: u32 = 0x8000 + 0x123;
/// A browser or a caller of the window list queued a destination.
const MSG_BROWSE: u32 = 0x8000 + 0x124;
/// A middle click on the items view selected what it hit: open it in a tab.
const MSG_MIDDLE: u32 = 0x8000 + 0x125;
const TIMER_SEARCH: usize = 2;
const SEARCH_DELAY_MS: u32 = 125;

pub struct Frame {
    pub hwnd: HWND,
    canvas: Canvas,
    status: Canvas,
    status_drawn: Option<(i32, i32, bool, Hit, Hit)>,
    tabs: Vec<Tab>,
    active: usize,
    next_id: usize,
    hover: Hit,
    pressed: Hit,
    spans: Vec<(f32, f32)>,
    address: Option<LineEdit>,
    search: LineEdit,
    search_focus: bool,
    search_timer: bool,
    search_worker: Option<search::Worker>,
    details: Rc<Cell<bool>>,
    s: &'static Strings,
    icons: HashMap<usize, ID2D1Bitmap1>,
    window_active: bool,
    tracking: bool,
    view_hidden: bool,
    /// Client size and DPI of the last layout.
    laid_out: (i32, i32, u32),
    /// When the window was asked for, until its view is ready (logged then).
    pub opened: Option<Instant>,
    /// Cookie of the window in the shell's window list.
    sw_cookie: Option<i32>,
    /// An item (absolute ID list) to select once its folder shows.
    select: Option<Pidl>,
    /// A tab dragged off the strip, torn into a new window on release.
    tear: Option<usize>,
}

thread_local! {
    static FRAMES: RefCell<Vec<(HWND, Rc<RefCell<Frame>>)>> = const { RefCell::new(Vec::new()) };
    static SHARED: RefCell<Option<(Rc<Shared>, &'static Strings)>> = const { RefCell::new(None) };
    static NEW_WINDOW: Cell<Option<HWND>> = const { Cell::new(None) };
    static BROWSES: RefCell<Vec<(HWND, usize, Pidl)>> = const { RefCell::new(Vec::new()) };
}

/// Registers the window class and keeps the shared drawing devices; `cmd` receives the
/// "new window" requests (Ctrl+N).
pub fn init(sh: Rc<Shared>, s: &'static Strings, cmd: HWND) {
    SHARED.with(|x| *x.borrow_mut() = Some((sh, s)));
    NEW_WINDOW.with(|c| c.set(Some(cmd)));
    unsafe {
        let inst = GetModuleHandleW(None).unwrap_or_default();
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: inst.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            ..Default::default()
        };
        RegisterClassExW(&class);
    }
}

fn find(hwnd: HWND) -> Option<Rc<RefCell<Frame>>> {
    FRAMES.with(|f| f.borrow().iter().find(|(h, _)| *h == hwnd).map(|(_, r)| r.clone()))
}

/// Runs `f` on the frame of `hwnd` unless it is already in use further up the stack.
fn with<R>(hwnd: HWND, f: impl FnOnce(&mut Frame) -> R) -> Option<R> {
    let rc = find(hwnd)?;
    let mut fr = rc.try_borrow_mut().ok()?;
    Some(f(&mut fr))
}

pub fn count() -> usize {
    FRAMES.with(|f| f.borrow().len())
}

fn scale(hwnd: HWND) -> f32 {
    unsafe { GetDpiForWindow(hwnd).max(96) as f32 / 96.0 }
}

fn client(hwnd: HWND) -> RECT {
    let mut r = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut r);
    }
    r
}

/// Creates a hidden window on `place`, its view loading; `show` puts it on screen.
pub fn create(place: &Pidl, x: i32, y: i32) -> windows_core::Result<HWND> {
    let (sh, s) = SHARED.with(|x| x.borrow().clone()).ok_or_else(windows_core::Error::empty)?;
    unsafe {
        let inst = GetModuleHandleW(None)?;
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            w!("File Explorer"),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            x,
            y,
            1100,
            680,
            None,
            None,
            Some(inst.into()),
            None,
        )?;
        // The prepared window never reaches composition, including during shell view creation.
        cloak(hwnd, true);
        let k = scale(hwnd);
        let on: i32 = 1;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &on as *const _ as _, 4);
        let kind = DWMSBT_MAINWINDOW;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &kind as *const _ as _, 4);
        shell::allow_dark(hwnd);
        let rc = client(hwnd);
        let chrome_height = (L::CHROME_H * k).ceil() as u32;
        let status_height = (L::STATUS_H * k).ceil() as u32;
        let canvas = Canvas::new(sh, hwnd, rc.right as u32, chrome_height, 96.0 * k);
        let layers = canvas.and_then(|canvas| {
            let status = canvas.add_layer(rc.right as u32, status_height, 96.0 * k)?;
            status.set_y(rc.bottom - status_height as i32)?;
            Ok((canvas, status))
        });
        let (canvas, status) = match layers {
            Ok(layers) => layers,
            Err(error) => {
                let _ = DestroyWindow(hwnd);
                return Err(error);
            }
        };
        let frame = Frame {
            hwnd,
            canvas,
            status,
            status_drawn: None,
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            hover: Hit::None,
            pressed: Hit::None,
            spans: Vec::new(),
            address: None,
            search: LineEdit::default(),
            search_focus: false,
            search_timer: false,
            search_worker: None,
            details: Rc::new(Cell::new(false)),
            s,
            icons: HashMap::new(),
            window_active: false,
            tracking: false,
            view_hidden: false,
            laid_out: (0, 0, 0),
            opened: None,
            sw_cookie: None,
            select: None,
            tear: None,
        };
        let rc = Rc::new(RefCell::new(frame));
        FRAMES.with(|f| f.borrow_mut().push((hwnd, rc.clone())));
        // The frame becomes the whole window except Windows' resize borders.
        let prepared = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE)
            .and_then(|()| {
                let mut fr = rc.borrow_mut();
                fr.extend_frame();
                fr.add_tab(place)
            });
        if let Err(error) = prepared {
            let _ = DestroyWindow(hwnd);
            return Err(error);
        }
        Ok(hwnd)
    }
}

/// Shows a window made by `create`, draws its first frame and returns when it is presented.
/// Shows the window without activating it, presents the chrome, then activates it (the view's
/// focus work runs after the first frame). Returns where the time went: show, draw and Present,
/// activation (ms).
pub fn show(hwnd: HWND) -> (f64, f64, f64) {
    let t = Instant::now();
    let ms = |t: Instant| t.elapsed().as_secs_f64() * 1000.0;
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNA);
    }
    let shown = ms(t);
    with(hwnd, |f| {
        f.window_active = true;
        f.draw();
    });
    let drawn = ms(t);
    unsafe {
        cloak(hwnd, false);
        let _ = windows::Win32::System::DataExchange::AddClipboardFormatListener(hwnd);
        let _ = SetForegroundWindow(hwnd);
    }
    (shown, drawn - shown, ms(t) - drawn)
}

fn cloak(hwnd: HWND, hide: bool) {
    let value = i32::from(hide);
    if let Err(error) = unsafe { DwmSetWindowAttribute(hwnd, DWMWA_CLOAK, (&value as *const i32).cast(), 4) } {
        log::line(&format!("Files window {:#x}, cloak {hide}: {error}", hwnd.0 as usize));
    }
}

/// Hides the active view until its next navigation completes (a ready window sent elsewhere
/// shows no stale folder).
pub fn hide_view(hwnd: HWND) {
    with(hwnd, |f| {
        if let Some(t) = f.tab() {
            t.show(false);
        }
        f.view_hidden = true;
    });
}

/// Navigates the active tab of `hwnd` to `place`.
pub fn browse(hwnd: HWND, place: &Pidl) {
    with(hwnd, |f| {
        f.clear_search();
        if let Some(t) = f.tabs.get(f.active) {
            let _ = t.browse(place);
        }
    });
}

/// Puts `hwnd` in the shell's window list, showing `place`.
pub fn announce(hwnd: HWND, place: &Pidl) {
    // Outside the frame's borrow: the cross-process call can let callers in.
    let cookie = shellwindows::register(hwnd, place);
    with(hwnd, |f| f.sw_cookie = cookie);
}

/// Selects `item` (absolute) once the active tab shows its folder.
pub fn select_after(hwnd: HWND, item: Pidl) {
    with(hwnd, |f| f.select = Some(item));
}

/// The file system path (if any) and the display name of the folder `hwnd` shows.
pub fn location(hwnd: HWND) -> Option<(Option<String>, String)> {
    with(hwnd, |f| f.tab().map(|t| (t.path.clone(), t.title.clone()))).flatten()
}

/// The active tab's view.
pub fn active_view(hwnd: HWND) -> Option<IShellView> {
    with(hwnd, |f| f.tab().and_then(|t| t.shell_view())).flatten()
}

/// The active tab's browser as a service provider.
pub fn browser_services(hwnd: HWND) -> Option<IServiceProvider> {
    with(hwnd, |f| f.tab().and_then(|t| t.browser().eb.cast().ok())).flatten()
}

/// Navigates the active tab to `place` from the message loop (a COM caller is on the stack).
pub fn browse_later(hwnd: HWND, place: Pidl) {
    browse_tab_later(hwnd, 0, place);
}

pub fn browse_tab_later(hwnd: HWND, id: usize, place: Pidl) {
    unsafe {
        if PostMessageW(Some(hwnd), MSG_BROWSE, WPARAM(0), LPARAM(0)).is_ok() {
            BROWSES.with(|pending| pending.borrow_mut().push((hwnd, id, place)));
        }
    }
}

/// A middle click on a folder in an items view opens it in a new tab, as Explorer does. The view
/// has no hit test, so the click is replayed as a left click (which selects what it hits) and the
/// frame reads the selection after it.
pub fn middle_click(msg: &MSG) -> bool {
    if msg.message != WM_MBUTTONUP && msg.message != WM_MBUTTONDOWN {
        return false;
    }
    unsafe {
        if class_of(msg.hwnd) != "DirectUIHWND" {
            return false;
        }
        let parent = GetParent(msg.hwnd).unwrap_or_default();
        if class_of(parent) != "SHELLDLL_DefView" {
            return false;
        }
        let root = GetAncestor(msg.hwnd, GA_ROOT);
        if find(root).is_none() {
            return false;
        }
        if msg.message == WM_MBUTTONUP {
            let _ = PostMessageW(Some(msg.hwnd), WM_LBUTTONDOWN, WPARAM(1), msg.lParam);
            let _ = PostMessageW(Some(msg.hwnd), WM_LBUTTONUP, WPARAM(0), msg.lParam);
            let _ = PostMessageW(Some(root), MSG_MIDDLE, WPARAM(0), LPARAM(0));
        }
    }
    true
}

pub fn set_opened(hwnd: HWND, t: Instant) {
    with(hwnd, |f| {
        // A prepared tab already sent NavigationComplete. Charging its next user navigation
        // to this open includes the time spent between opening the window and selecting a folder.
        f.opened = (!f.view_ready()).then_some(t);
    });
}

pub fn ready(hwnd: HWND) -> bool {
    with(hwnd, |f| f.view_ready()).unwrap_or(false)
}

/// Closes every window (shutdown).
pub fn close_all() {
    let all: Vec<HWND> = FRAMES.with(|f| f.borrow().iter().map(|(h, _)| *h).collect());
    for h in all {
        unsafe {
            let _ = SendMessageW(h, WM_CLOSE, None, None);
        }
    }
}

/// Keys of the message loop: Explorer's shortcuts, then the view's own accelerators (F2,
/// Delete, Ctrl+C...). True when the message was used.
pub fn accelerator(msg: &MSG) -> bool {
    if msg.message != WM_KEYDOWN && msg.message != WM_SYSKEYDOWN {
        return false;
    }
    let root = unsafe { GetAncestor(msg.hwnd, GA_ROOT) };
    if root == msg.hwnd {
        // Our own window has the focus: the address or search box edits.
        return false;
    }
    with(root, |f| f.accelerator(msg)).unwrap_or(false)
}

fn key_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetKeyState(vk.0 as i32) < 0 }
}

fn class_of(h: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(h, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n])
}

impl Frame {
    fn scale(&self) -> f32 {
        scale(self.hwnd)
    }

    fn chrome_px(&self) -> i32 {
        (L::CHROME_H * self.scale()).ceil() as i32
    }

    fn status_px(&self) -> i32 {
        (L::STATUS_H * self.scale()).ceil() as i32
    }

    fn extend_frame(&self) {
        let m = MARGINS {
            cxLeftWidth: 0, cxRightWidth: 0, cyTopHeight: self.chrome_px(), cyBottomHeight: self.status_px()
        };
        unsafe {
            let _ = DwmExtendFrameIntoClientArea(self.hwnd, &m);
        }
    }

    fn view_rect(&self) -> RECT {
        let r = client(self.hwnd);
        RECT { left: 0, top: self.chrome_px(), right: r.right, bottom: (r.bottom - self.status_px()).max(self.chrome_px()) }
    }

    fn tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    fn view_ready(&self) -> bool {
        !self.view_hidden && self.tab().is_some_and(|tab| tab.history.current().is_some())
    }

    fn add_tab(&mut self, place: &Pidl) -> windows_core::Result<()> {
        let id = self.next_id;
        self.next_id += 1;
        let t = Tab::new(self.hwnd, id, self.view_rect(), self.details.clone())?;
        t.browse(place)?;
        if let Some(old) = self.tab() {
            old.show(false);
        }
        self.tabs.push(t);
        self.active = self.tabs.len() - 1;
        Ok(())
    }

    /// A tab after the active one that loads behind it (middle click).
    fn add_background_tab(&mut self, place: &Pidl) -> windows_core::Result<()> {
        let id = self.next_id;
        self.next_id += 1;
        let t = Tab::new(self.hwnd, id, self.view_rect(), self.details.clone())?;
        t.show(false);
        t.browse(place)?;
        self.tabs.insert(self.active + 1, t);
        Ok(())
    }

    /// Middle click replayed: the folder the click selected opens in a background tab.
    fn middle_open(&mut self) {
        let Some(item) = self.tab().and_then(|t| t.shell_view()).as_ref().and_then(shell::single_selection) else {
            return;
        };
        let Some(p) = shell::folder_target(&item) else { return };
        match self.add_background_tab(&p) {
            Ok(()) => log::line(&format!("middle click: {:?} opened in tab {}", shell::name(&item, windows::Win32::UI::Shell::SIGDN_NORMALDISPLAY), self.active + 2)),
            Err(e) => log::line(&format!("middle click tab failed: {e}")),
        }
        self.draw();
    }

    /// The window list follows the active tab.
    fn tell_list(&self) {
        if let (Some(c), Some(p)) = (self.sw_cookie, self.tab().and_then(|t| t.history.current())) {
            shellwindows::navigate(c, p);
        }
    }

    fn switch(&mut self, i: usize) {
        if i >= self.tabs.len() || i == self.active {
            return;
        }
        if self.search_timer {
            self.submit_search();
        }
        if let Some(t) = self.tab() {
            t.show(false);
        }
        self.active = i;
        if let Some(t) = self.tab() {
            t.set_rect(self.view_rect());
            t.show(true);
        }
        self.address = None;
        self.sync_edit();
        self.title();
        self.tell_list();
    }

    fn close_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        if self.tabs.len() == 1 {
            unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            return;
        }
        let t = self.tabs.remove(i);
        self.icons.remove(&t.id);
        drop(t);
        if self.active >= self.tabs.len() || i < self.active {
            self.active = self.active.saturating_sub(1).min(self.tabs.len() - 1);
        }
        if let Some(t) = self.tab() {
            t.set_rect(self.view_rect());
            t.show(true);
        }
        self.sync_edit();
        self.title();
    }

    fn title(&self) {
        if let Some(t) = self.tab() {
            let w = shelldata::wide(&format!("{} - File Explorer", t.title));
            unsafe {
                let _ = SetWindowTextW(self.hwnd, PCWSTR(w.as_ptr()));
            }
        }
    }

    pub fn draw(&mut self) {
        let k = self.scale();
        let w = client(self.hwnd).right as f32 / k;
        let tabs: Vec<TabView> =
            self.tabs.iter().map(|t| TabView { title: &t.title, icon: self.icons.get(&t.id) }).collect();
        let t = self.tabs.get(self.active);
        let folder_view = t.and_then(|t| t.folder_view());
        let ready = folder_view.is_some();
        let mut selected = 0;
        let mut details_view = false;
        let items = folder_view.as_ref().map_or(-1, |view| unsafe {
            selected = view.ItemCount(SVGIO_SELECTION).unwrap_or(0);
            details_view = view.GetCurrentViewMode().is_ok_and(|m| m == FVM_DETAILS.0 as u32);
            view.ItemCount(SVGIO_ALLVIEW).unwrap_or(0)
        });
        let selection = selected > 0;
        let create = t.is_some_and(|t| t.path.is_some() && t.query.is_empty());
        let view = chrome::View {
            w,
            tabs,
            active: self.active,
            can_back: t.is_some_and(|t| t.history.can_back() || !t.query.is_empty()),
            can_forward: t.map(|t| t.history.can_forward()).unwrap_or(false),
            can_up: t.is_some_and(|t| t.can_up() || !t.query.is_empty()),
            crumbs: t.map(|t| t.crumbs.iter().map(|c| c.0.as_str()).collect()).unwrap_or_default(),
            address: self.address.as_ref(),
            search: &self.search,
            search_focus: self.search_focus,
            hover: self.hover,
            pressed: self.pressed,
            selection,
            paste: ready && create && menus::file_clipboard(),
            create,
            ready,
            details: self.details.get(),
            window_active: self.window_active,
            maximized: unsafe { IsZoomed(self.hwnd) }.as_bool(),
            s: self.s,
        };
        self.canvas.begin();
        let spans = chrome::draw(&self.canvas, &view);
        if let Err(e) = self.canvas.end() {
            log::line(&format!("draw failed: {e}"));
        }
        self.spans = spans;
        let status_hit = |hit| match hit {
            Hit::StatusDetails | Hit::StatusIcons => hit,
            _ => Hit::None,
        };
        let status = (items, selected, details_view, status_hit(self.hover), status_hit(self.pressed));
        if self.status_drawn != Some(status) {
            self.status.begin();
            chrome::status(&self.status, &view, items, selected, details_view);
            match self.status.end() {
                Ok(()) => self.status_drawn = Some(status),
                Err(error) => log::line(&format!("status draw failed: {error}")),
            }
        }
    }

    fn layout(&mut self) {
        let r = client(self.hwnd);
        let k = self.scale();
        let now = (r.right, r.bottom, unsafe { GetDpiForWindow(self.hwnd) });
        if now == self.laid_out {
            return;
        }
        self.laid_out = now;
        if let Err(e) = self.canvas.resize(r.right as u32, (L::CHROME_H * k).ceil() as u32, 96.0 * k) {
            log::line(&format!("resize failed: {e}"));
        }
        let status_height = self.status_px();
        if let Err(error) = self.status.resize(r.right as u32, status_height as u32, 96.0 * k)
            .and_then(|()| self.status.set_y(r.bottom - status_height))
        {
            log::line(&format!("status resize failed: {error}"));
        }
        self.status_drawn = None;
        self.extend_frame();
        if let Some(t) = self.tab() {
            t.set_rect(self.view_rect());
        }
        self.draw();
    }

    fn hit(&self, x_px: i32, y_px: i32) -> Hit {
        let k = self.scale();
        let rect = client(self.hwnd);
        let w = rect.right as f32 / k;
        if (rect.bottom - self.status_px()..rect.bottom).contains(&y_px) {
            return L::status_hit(x_px as f32 / k, w);
        }
        L::hit(x_px as f32 / k, y_px as f32 / k, w, self.tabs.len(), self.active, &self.spans)
    }

    fn set_hover(&mut self, h: Hit) {
        if self.hover != h {
            self.hover = h;
            self.draw();
        }
    }

    fn focus_view(&self) {
        if let Some(v) = self.tab().and_then(|t| t.shell_view()) {
            unsafe {
                let _ = v.UIActivate(SVUIA_ACTIVATE_FOCUS.0 as u32);
            }
        }
        let view_window = self.tab().and_then(|t| t.shell_view()).and_then(|view| unsafe { view.GetWindow().ok() });
        if let Some(t) = view_window.or_else(|| self.tab().and_then(|t| t.window())) {
            unsafe {
                if GetFocus() == self.hwnd {
                    let _ = SetFocus(Some(t));
                }
            }
        }
    }

    fn edit_address(&mut self) {
        let text = self
            .tab()
            .and_then(|t| t.item.as_ref())
            .map(|i| shell::fs_path(i).unwrap_or_else(|| shell::name(i, SIGDN_DESKTOPABSOLUTEEDITING)))
            .unwrap_or_default();
        let mut e = LineEdit::default();
        e.set(&text);
        self.address = Some(e);
        self.search_focus = false;
        unsafe {
            let _ = SetFocus(Some(self.hwnd));
        }
        self.draw();
    }

    fn edit_search(&mut self) {
        self.address = None;
        self.search_focus = true;
        self.search.select_all();
        unsafe {
            let _ = SetFocus(Some(self.hwnd));
        }
        self.draw();
    }

    fn end_edit(&mut self) {
        self.address = None;
        self.search_focus = false;
        self.draw();
    }

    fn act(&mut self, h: Hit) {
        let k = self.scale();
        match h {
            Hit::Back => self.go(Move::Back),
            Hit::Forward => self.go(Move::Forward),
            Hit::Up => {
                if !self.clear_search() {
                    if let Some(t) = self.tab() {
                        t.up()
                    }
                }
            }
            Hit::Refresh => {
                if let Some(t) = self.tab() {
                    t.refresh()
                }
            }
            Hit::Crumb(i) => {
                if let Some(p) = self.tab().and_then(|tab| tab.crumbs.get(i).map(|(_, p)| p.clone())) {
                    self.browse_active(&p);
                }
            }
            Hit::Address => self.edit_address(),
            Hit::Search => self.edit_search(),
            Hit::Tab(i) => self.switch(i),
            Hit::TabClose(i) => self.close_tab(i),
            Hit::NewTab => self.new_tab(),
            Hit::TabsBack => self.switch(L::tab_strip(client(self.hwnd).right as f32 / k, self.tabs.len(), self.active).visible.start.saturating_sub(1)),
            Hit::TabsForward => self.switch(L::tab_strip(client(self.hwnd).right as f32 / k, self.tabs.len(), self.active).visible.end),
            Hit::StatusDetails | Hit::StatusIcons => {
                if let Some(view) = self.tab().and_then(Tab::folder_view) {
                    let (mode, size) = if h == Hit::StatusDetails { (FVM_DETAILS, 16) } else { (FVM_ICON, 48) };
                    let result = unsafe { view.SetViewModeAndIconSize(mode, size) };
                    log::line(&format!("status view {mode:?}: {result:?}"));
                }
            }
            Hit::Cmd(c) => self.command(c, k),
            Hit::Min => unsafe {
                let _ = ShowWindow(self.hwnd, SW_MINIMIZE);
            },
            Hit::Max => unsafe {
                let zoomed = IsZoomed(self.hwnd).as_bool();
                let _ = ShowWindow(self.hwnd, if zoomed { SW_RESTORE } else { SW_MAXIMIZE });
            },
            Hit::Close => unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            },
            Hit::None | Hit::Caption => {}
        }
        self.draw();
    }

    fn go(&mut self, m: Move) {
        if m == Move::Back && self.clear_search() {
            return;
        }
        if let Some(t) = self.tabs.get_mut(self.active) {
            t.go(m);
        }
    }

    fn new_tab(&mut self) {
        if self.search_timer {
            self.submit_search();
        }
        if let Some(home) = shell::home() {
            if let Err(e) = self.add_tab(&home) {
                log::line(&format!("new tab failed: {e}"));
            }
            self.sync_edit();
        }
    }

    fn command(&mut self, c: Cmd, k: f32) {
        let Some(t) = self.tab() else { return };
        let fv = t.folder_view();
        let sv = t.shell_view();
        let w = client(self.hwnd).right as f32 / k;
        let below = |x: f32| {
            let mut p = POINT { x: (x * k) as i32, y: ((L::CMD_CY + 18.0) * k) as i32 };
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::ClientToScreen(self.hwnd, &mut p);
            }
            p
        };
        let r = match c {
            Cmd::Cut | Cmd::Copy | Cmd::Delete => {
                let verb = match c {
                    Cmd::Cut => "cut",
                    Cmd::Copy => "copy",
                    _ => "delete",
                };
                let Some(view) = &sv else { return };
                let Some(menu) = shell::selection_menu(view) else { return };
                shell::invoke(&menu, view, self.hwnd, verb)
            }
            Cmd::Rename => {
                let Some(v) = fv.as_ref().filter(|v| shell::has_selection(v)) else { return };
                unsafe { v.DoRename() }
            }
            Cmd::Paste => {
                if t.path.is_none() || !t.query.is_empty() || !menus::file_clipboard() {
                    return;
                }
                let Some(view) = &sv else { return };
                let Some(menu) = shell::background_menu(view) else { return };
                shell::invoke(&menu, view, self.hwnd, "paste")
            }
            Cmd::New => {
                let Some(folder) = t.item.as_ref().filter(|_| t.path.is_some() && t.query.is_empty()) else { return };
                let result = shell::new_folder(self.hwnd, folder, self.s.new_folder);
                if result.is_ok() {
                    t.refresh();
                }
                result
            }
            Cmd::Sort => {
                if let Some(v) = &fv {
                    let (cmd, _, x, _, _, l, r) = L::CMD_LABELLED[1];
                    menus::sort(self.hwnd, v, self.s, below(L::label_bounds(w, cmd, x, l, r).1))
                } else {
                    Ok(())
                }
            }
            Cmd::View => {
                if let Some(v) = &fv {
                    let (cmd, _, x, _, _, l, r) = L::CMD_LABELLED[2];
                    menus::view(self.hwnd, v, self.s, below(L::label_bounds(w, cmd, x, l, r).1))
                } else {
                    Ok(())
                }
            }
            Cmd::More => {
                if let (Some(v), Some(s)) = (&fv, &sv) {
                    menus::more(self.hwnd, v, s, self.s, below(L::command_more_x(w) - 17.0))
                } else {
                    Ok(())
                }
            }
            Cmd::Details => {
                self.details.set(!self.details.get());
                if !t.query.is_empty() {
                    let active = &mut self.tabs[self.active];
                    active.dismiss_results(true);
                    active.generation = active.generation.wrapping_add(1);
                    self.submit_search();
                    return;
                }
                let browser = &t.browser().eb;
                unsafe {
                    use windows::Win32::UI::Shell::{EBO_ALWAYSNAVIGATE, SBSP_SAMEBROWSER};
                    browser.GetOptions().and_then(|options| {
                        browser.SetOptions(options | EBO_ALWAYSNAVIGATE)?;
                        let result = t.history.current().map_or(Ok(()), |p| browser.BrowseToIDList(p.as_ptr(), SBSP_SAMEBROWSER));
                        let _ = browser.SetOptions(options);
                        result
                    })
                }
            }
        };
        log::line(&format!("command {c:?}: {r:?}"));
    }

    fn accelerator(&mut self, msg: &MSG) -> bool {
        let vk = VIRTUAL_KEY(msg.wParam.0 as u16);
        let ctrl = key_down(VK_CONTROL);
        let alt = key_down(VK_MENU);
        let in_edit = class_of(msg.hwnd) == "Edit";
        let ours = match vk {
            VK_ESCAPE if !in_edit && self.clear_search() => {
                self.draw();
                return true;
            }
            VK_LEFT if alt => Some(Hit::Back),
            VK_RIGHT if alt => Some(Hit::Forward),
            VK_UP if alt => Some(Hit::Up),
            VK_BACK if !in_edit && !ctrl && !alt => Some(Hit::Back),
            VK_F5 => Some(Hit::Refresh),
            VK_F4 if !alt => Some(Hit::Address),
            VK_L if ctrl => Some(Hit::Address),
            VK_D if alt => Some(Hit::Address),
            VK_E | VK_F if ctrl => Some(Hit::Search),
            VK_F3 => Some(Hit::Search),
            VK_T if ctrl => Some(Hit::NewTab),
            VK_W if ctrl => Some(Hit::TabClose(self.active)),
            _ => None,
        };
        if let Some(h) = ours {
            self.act(h);
            return true;
        }
        if vk == VK_TAB && ctrl {
            let n = self.tabs.len();
            let next = if key_down(VK_SHIFT) { (self.active + n - 1) % n } else { (self.active + 1) % n };
            self.switch(next);
            self.draw();
            return true;
        }
        if vk == VK_N && ctrl && !key_down(VK_SHIFT) {
            if let Some(cmd) = NEW_WINDOW.with(|c| c.get()) {
                unsafe {
                    let _ = PostMessageW(Some(cmd), bus::MSG_COMMAND, WPARAM(bus::Command::FilesOpen as usize), LPARAM(0));
                }
            }
            return true;
        }
        if in_edit {
            return false;
        }
        match self.tab().and_then(|t| t.shell_view()) {
            Some(v) => (unsafe { v.TranslateAccelerator(msg) }) == S_OK,
            None => false,
        }
    }

    /// Keys while the address or search box has the focus.
    fn edit_key(&mut self, vk: VIRTUAL_KEY) -> bool {
        let ctrl = key_down(VK_CONTROL);
        let searching = self.search_focus;
        let Some(e) = (if searching { Some(&mut self.search) } else { self.address.as_mut() }) else { return false };
        match vk {
            VK_LEFT => e.left(),
            VK_RIGHT => e.right(),
            VK_HOME => e.home(),
            VK_END => e.end(),
            VK_DELETE => e.delete(),
            VK_A if ctrl => e.select_all(),
            VK_V if ctrl => {
                if let Some(s) = menus::clipboard_text(self.hwnd) {
                    e.insert(&s);
                }
            }
            VK_ESCAPE => {
                if searching {
                    self.clear_search();
                }
                self.end_edit();
                self.focus_view();
                return true;
            }
            VK_RETURN => {
                let text = e.text();
                self.end_edit();
                if searching {
                    self.submit_search();
                } else {
                    self.go_to(&text);
                }
                self.focus_view();
                return true;
            }
            _ => return false,
        }
        if searching {
            self.search_changed();
        }
        self.draw();
        true
    }

    fn go_to(&mut self, text: &str) {
        let text = text.trim().trim_matches('"');
        match Pidl::parse_result(text) {
            Ok(p) => {
                if let Some(folder) = p.item().ok().as_ref().and_then(shell::folder_target) {
                    self.browse_active(&folder);
                } else if let Err(e) = shell::open_item(self.hwnd, &p) {
                    log::line(&format!("address: opening {text:?} failed: {e}"));
                }
            }
            Err(e) => {
                log::line(&format!("address: navigation failed for {text:?} (SHParseDisplayName HRESULT {:#010x}): {e}", e.code().0 as u32));
                unsafe {
                    let _ = MessageBeep(MB_ICONWARNING);
                }
            }
        }
    }

    fn browse_active(&mut self, folder: &Pidl) {
        self.browse_tab(0, folder);
    }

    fn browse_tab(&mut self, id: usize, folder: &Pidl) {
        let index = if id == 0 { Some(self.active) } else { self.tabs.iter().position(|t| t.id == id) };
        let Some(index) = index else { return };
        if index == self.active {
            self.clear_search();
        }
        if let Some(tab) = self.tabs.get_mut(index) {
            if index != self.active {
                tab.clear_search(false);
            }
            let _ = tab.browse(folder);
        }
    }

    fn sync_edit(&mut self) {
        let query = self.tab().map(|t| t.query.clone()).unwrap_or_default();
        self.search.set(&query);
        self.search.end();
        self.search_focus = false;
    }

    fn clear_search(&mut self) -> bool {
        if let Some(t) = self.tabs.get_mut(self.active).filter(|t| !t.query.is_empty()) {
            t.clear_search(true);
            self.search = LineEdit::default();
            self.search_timer = false;
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_SEARCH);
            }
            true
        } else {
            false
        }
    }

    fn search_changed(&mut self) {
        let query = self.search.text();
        if query.trim().is_empty() {
            self.clear_search();
            return;
        }
        let Some(t) = self.tabs.get_mut(self.active) else { return };
        if t.query == query {
            return;
        }
        t.query = query;
        t.generation = t.generation.wrapping_add(1);
        self.search_timer = unsafe { SetTimer(Some(self.hwnd), TIMER_SEARCH, SEARCH_DELAY_MS, None) } != 0;
        if !self.search_timer {
            self.submit_search();
        }
    }

    fn submit_search(&mut self) {
        self.search_timer = false;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_SEARCH);
        }
        let Some(t) = self.tab().filter(|t| !t.query.is_empty()) else { return };
        let Some(folder) = t.history.current().cloned() else { return };
        let request = search::Request {
            tab: t.id,
            generation: t.generation,
            folder,
            path: t.path.clone(),
            query: t.query.trim().to_owned(),
        };
        if self.search_worker.is_none() {
            match search::Worker::new(self.hwnd) {
                Ok(worker) => self.search_worker = Some(worker),
                Err(e) => log::line(&format!("search worker failed: {e}")),
            }
        }
        if let Some(worker) = &self.search_worker {
            worker.submit(request);
        }
    }

    fn search_results(&mut self) {
        let Some(worker) = &self.search_worker else { return };
        for response in worker.take() {
            self.apply_search(response);
        }
    }

    fn apply_search(&mut self, response: search::Response) {
        let rect = self.view_rect();
        let hwnd = self.hwnd;
        let active = self.tab().is_some_and(|t| t.id == response.tab);
        let Some(t) = self.tabs.iter_mut().find(|t| t.id == response.tab && t.generation == response.generation) else {
            return;
        };
        let result = response.results.and_then(|hits| {
            let indexed = matches!(hits, search::Results::Indexed(_));
            let browser = t.search_browser(hwnd, rect, self.details.clone(), indexed)?;
            search::present(browser, hits)
        });
        match result {
            Ok(()) => {
                t.show_results(active);
                log::line(&format!("tab {}: search {:?} presented", t.id, t.query));
            }
            Err(e) => {
                log::line(&format!("tab {}: search {:?} in {:?} failed: {e}", t.id, t.query, t.path));
                // Discard a view which was only partially initialized.
                t.dismiss_results(active);
            }
        }
        if active && self.search_focus {
            unsafe { let _ = SetFocus(Some(hwnd)); }
        }
        self.draw();
    }

    fn navigated(&mut self, id: usize, p: Pidl) {
        let active_id = self.tab().map(|t| t.id);
        let view_rect = self.view_rect();
        let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) else { return };
        t.navigated(p);
        log::line(&format!("tab {id}: displaying {:?}", t.path.as_deref().unwrap_or(&t.title)));
        if let Some((w, h, px)) = &t.icon {
            if let Ok(b) = self.canvas.bitmap(*w, *h, px) {
                self.icons.insert(id, b);
            }
        }
        let title = t.title.clone();
        if active_id == Some(id) && self.view_hidden {
            self.view_hidden = false;
            t.set_rect(view_rect);
            t.show(true);
        }
        if let Some(t0) = self.opened.take() {
            log::line(&format!(
                "view ready on {title:?}: {:.1} ms after the open",
                t0.elapsed().as_secs_f64() * 1000.0
            ));
        }
        if active_id == Some(id) {
            self.title();
            self.tell_list();
            self.select_pending();
        }
        self.draw();
    }

    /// Selects the item a caller asked for when the active tab shows its folder.
    fn select_pending(&mut self) {
        let Some(item) = self.select.take() else { return };
        let Some(t) = self.tab() else { return };
        let same = match (item.parent(), t.history.current()) {
            (Some(a), Some(b)) => unsafe { windows::Win32::UI::Shell::ILIsEqual(a.as_ptr(), b.as_ptr()) }.as_bool(),
            _ => false,
        };
        if !same {
            self.select = Some(item);
            return;
        }
        let Some(v) = t.shell_view() else { return };
        let flags = (SVSI_SELECT.0 | SVSI_DESELECTOTHERS.0 | SVSI_ENSUREVISIBLE.0 | SVSI_FOCUSED.0) as u32;
        let r = unsafe { v.SelectItem(item.last(), flags) };
        log::line(&format!("select {:?}: {r:?}", item.display()));
    }

    /// Ends a tab drag that left the strip: the tab closes here and its folder is returned for a
    /// window of its own.
    fn tear_off(&mut self) -> Option<Pidl> {
        let i = self.tear.take()?;
        self.pressed = Hit::None;
        let place = self.tabs.get(i).and_then(|t| t.history.current().cloned())?;
        self.close_tab(i);
        self.draw();
        log::line(&format!("tab {i} torn off into a new window"));
        Some(place)
    }

    fn view_created(&mut self, id: usize, search: bool) {
        let Some(browser) = self.tabs.iter().find(|t| t.id == id).and_then(|t| t.source(search)) else { return };
        if let Some(root) = browser.window() {
            shell::dark_children(root);
        }
        if let Some(view) = browser.shell_view() {
            menu::hook(&view);
        }
        self.draw();
    }
}

fn point(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xffff) as i16 as i32, ((lp.0 >> 16) & 0xffff) as i16 as i32)
}

fn track(hwnd: HWND, nonclient: bool) {
    let mut t = TRACKMOUSEEVENT {
        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: if nonclient { TME_LEAVE | TME_NONCLIENT } else { TME_LEAVE },
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    unsafe {
        let _ = TrackMouseEvent(&mut t);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_NCCALCSIZE if wp.0 != 0 => {
                // Keep Windows' left, right and bottom frame (the resize borders), drop its caption.
                let params = lp.0 as *mut NCCALCSIZE_PARAMS;
                let top = (*params).rgrc[0].top;
                let r = DefWindowProcW(hwnd, msg, wp, lp);
                (*params).rgrc[0].top = top;
                if IsZoomed(hwnd).as_bool() {
                    let dpi = GetDpiForWindow(hwnd);
                    (*params).rgrc[0].top +=
                        GetSystemMetricsForDpi(SM_CYFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
                }
                r
            }
            WM_NCHITTEST => {
                let r = DefWindowProcW(hwnd, msg, wp, lp);
                if r.0 != HTCLIENT as isize {
                    return r;
                }
                let (sx, sy) = point(lp);
                let mut p = POINT { x: sx, y: sy };
                let _ = ScreenToClient(hwnd, &mut p);
                let k = scale(hwnd);
                if p.y < (5.0 * k) as i32 && !IsZoomed(hwnd).as_bool() {
                    return LRESULT(HTTOP as isize);
                }
                let h = with(hwnd, |f| f.hit(p.x, p.y)).unwrap_or(Hit::None);
                LRESULT(match h {
                    Hit::Caption => HTCAPTION,
                    Hit::Min => HTMINBUTTON,
                    Hit::Max => HTMAXBUTTON,
                    Hit::Close => HTCLOSE,
                    _ => HTCLIENT,
                } as isize)
            }
            WM_NCMOUSEMOVE => {
                let h = match wp.0 as u32 {
                    HTMINBUTTON => Hit::Min,
                    HTMAXBUTTON => Hit::Max,
                    HTCLOSE => Hit::Close,
                    _ => Hit::None,
                };
                with(hwnd, |f| {
                    if !f.tracking {
                        track(hwnd, true);
                        f.tracking = true;
                    }
                    f.set_hover(h)
                });
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_NCLBUTTONDOWN if matches!(wp.0 as u32, HTMINBUTTON | HTMAXBUTTON | HTCLOSE) => {
                let h = match wp.0 as u32 {
                    HTMINBUTTON => Hit::Min,
                    HTMAXBUTTON => Hit::Max,
                    _ => Hit::Close,
                };
                with(hwnd, |f| {
                    f.pressed = h;
                    f.draw()
                });
                LRESULT(0)
            }
            WM_NCLBUTTONUP if matches!(wp.0 as u32, HTMINBUTTON | HTMAXBUTTON | HTCLOSE) => {
                let h = match wp.0 as u32 {
                    HTMINBUTTON => Hit::Min,
                    HTMAXBUTTON => Hit::Max,
                    _ => Hit::Close,
                };
                with(hwnd, |f| {
                    let was = std::mem::replace(&mut f.pressed, Hit::None);
                    if was == h {
                        f.act(h);
                    } else {
                        f.draw();
                    }
                });
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let (x, y) = point(lp);
                with(hwnd, |f| {
                    if !f.tracking {
                        track(hwnd, false);
                        f.tracking = true;
                    }
                    if let Hit::Tab(i) = f.pressed {
                        let k = f.scale();
                        let off = y < (-24.0 * k) as i32 || y > ((L::TABSTRIP_H + 24.0) * k) as i32;
                        if off && f.tabs.len() > 1 && f.tear.is_none() {
                            f.tear = Some(i);
                            let _ = SetCursor(LoadCursorW(None, IDC_SIZEALL).ok());
                        }
                    }
                    let h = f.hit(x, y);
                    f.set_hover(h)
                });
                LRESULT(0)
            }
            WM_MOUSELEAVE | WM_NCMOUSELEAVE => {
                with(hwnd, |f| {
                    f.tracking = false;
                    if f.pressed == Hit::None {
                        f.set_hover(Hit::None);
                    }
                });
                LRESULT(0)
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                let (x, y) = point(lp);
                with(hwnd, |f| {
                    let h = f.hit(x, y);
                    if matches!(h, Hit::Address | Hit::Search) {
                        f.act(h);
                        return;
                    }
                    if f.address.is_some() || f.search_focus {
                        f.end_edit();
                    }
                    f.pressed = h;
                    let _ = SetCapture(hwnd);
                    f.draw();
                });
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let (x, y) = point(lp);
                let _ = ReleaseCapture();
                if let Some(Some(place)) = with(hwnd, |f| f.tear_off()) {
                    let mut at = POINT::default();
                    let _ = GetCursorPos(&mut at);
                    let k = scale(hwnd);
                    crate::win::open_window(&place, Some((at.x - (120.0 * k) as i32, at.y - (20.0 * k) as i32)));
                    return LRESULT(0);
                }
                with(hwnd, |f| {
                    let was = std::mem::replace(&mut f.pressed, Hit::None);
                    let h = f.hit(x, y);
                    if was == h && h != Hit::None {
                        f.act(h);
                    } else {
                        f.draw();
                    }
                });
                LRESULT(0)
            }
            WM_MBUTTONUP => {
                let (x, y) = point(lp);
                with(hwnd, |f| {
                    if let Hit::Tab(i) = f.hit(x, y) {
                        f.close_tab(i);
                        f.draw();
                    }
                });
                LRESULT(0)
            }
            WM_KEYDOWN => {
                let used = with(hwnd, |f| f.edit_key(VIRTUAL_KEY(wp.0 as u16))).unwrap_or(false);
                if used { LRESULT(0) } else { DefWindowProcW(hwnd, msg, wp, lp) }
            }
            WM_CHAR => {
                let c = wp.0 as u32;
                with(hwnd, |f| {
                    let searching = f.search_focus;
                    let e = if searching { Some(&mut f.search) } else { f.address.as_mut() };
                    let Some(e) = e else { return };
                    if c == 8 {
                        e.backspace();
                    } else if let Some(ch) = char::from_u32(c).filter(|ch| !ch.is_control()) {
                        e.insert(&ch.to_string());
                    } else {
                        return;
                    }
                    if searching {
                        f.search_changed();
                    }
                    f.draw();
                });
                LRESULT(0)
            }
            WM_KILLFOCUS => {
                with(hwnd, |f| {
                    if f.address.is_some() || f.search_focus {
                        f.end_edit();
                    }
                });
                LRESULT(0)
            }
            WM_SETFOCUS => {
                with(hwnd, |f| {
                    if f.address.is_none() && !f.search_focus {
                        f.focus_view();
                    }
                });
                LRESULT(0)
            }
            WM_ACTIVATE => {
                let cookie = with(hwnd, |f| {
                    f.window_active = (wp.0 & 0xffff) != 0;
                    f.draw();
                    f.sw_cookie
                });
                if let Some(Some(c)) = cookie {
                    shellwindows::activated(c, (wp.0 & 0xffff) != 0);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_SIZE => {
                if with(hwnd, |f| f.layout()).is_none() {
                    let _ = PostMessageW(Some(hwnd), MSG_RELAYOUT, WPARAM(0), LPARAM(0));
                }
                LRESULT(0)
            }
            MSG_RELAYOUT => {
                with(hwnd, |f| f.layout());
                LRESULT(0)
            }
            WM_DPICHANGED => {
                let r = &*(lp.0 as *const RECT);
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
                LRESULT(0)
            }
            WM_GETMINMAXINFO => {
                let mm = &mut *(lp.0 as *mut MINMAXINFO);
                let k = scale(hwnd);
                mm.ptMinTrackSize = POINT { x: (520.0 * k) as i32, y: (320.0 * k) as i32 };
                LRESULT(0)
            }
            WM_ERASEBKGND => {
                // Black is transparent in the extended frame: Mica shows under the chrome.
                let r = client(hwnd);
                FillRect(HDC(wp.0 as _), &r, HBRUSH(GetStockObject(BLACK_BRUSH).0));
                LRESULT(1)
            }
            WM_PAINT => {
                let mut ps = Default::default();
                BeginPaint(hwnd, &mut ps);
                with(hwnd, |f| f.draw());
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            tab::MSG_NAVIGATED => {
                let p = Pidl::copy(lp.0 as *const _);
                windows::Win32::UI::Shell::ILFree(Some(lp.0 as *const _));
                if let Some(p) = p {
                    with(hwnd, |f| f.navigated(wp.0, p));
                }
                LRESULT(0)
            }
            tab::MSG_VIEW | tab::MSG_SEARCH_VIEW => {
                with(hwnd, |f| f.view_created(wp.0, msg == tab::MSG_SEARCH_VIEW));
                LRESULT(0)
            }
            tab::MSG_FAILED => {
                let target = Pidl::copy(lp.0 as *const _);
                windows::Win32::UI::Shell::ILFree(Some(lp.0 as *const _));
                // ExplorerBrowser's failure callback only supplies the destination PIDL.
                log::line(&format!("tab {}: navigation failed for {:?} (ExplorerBrowser event)", wp.0, target.map(|p| p.display())));
                with(hwnd, |f| {
                    if f.view_hidden {
                        f.view_hidden = false;
                        f.select = None;
                        f.opened = None;
                        if let Some(t) = f.tab() {
                            t.show(true);
                        }
                    }
                });
                LRESULT(0)
            }
            tab::MSG_SELECTION | WM_CLIPBOARDUPDATE => {
                with(hwnd, |f| f.draw());
                LRESULT(0)
            }
            WM_TIMER if wp.0 == TIMER_SEARCH => {
                with(hwnd, Frame::submit_search);
                LRESULT(0)
            }
            search::MSG_RESULTS => {
                with(hwnd, Frame::search_results);
                LRESULT(0)
            }
            WM_CLOSE => {
                with(hwnd, |f| f.tabs.clear());
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                let _ = windows::Win32::System::DataExchange::RemoveClipboardFormatListener(hwnd);
                BROWSES.with(|pending| pending.borrow_mut().retain(|(window, _, _)| *window != hwnd));
                if let Some(Some(c)) = with(hwnd, |f| f.sw_cookie.take()) {
                    shellwindows::revoke(c);
                }
                FRAMES.with(|f| f.borrow_mut().retain(|(h, _)| *h != hwnd));
                LRESULT(0)
            }
            MSG_BROWSE => {
                with(hwnd, |f| {
                    let next = BROWSES.with(|pending| {
                        let mut pending = pending.borrow_mut();
                        let index = pending.iter().position(|(window, _, _)| *window == hwnd)?;
                        Some(pending.remove(index))
                    });
                    if let Some((_, id, p)) = next {
                        log::line(&format!("tab {id}: in-place browse {:?}", p.display()));
                        f.browse_tab(id, &p);
                    }
                });
                LRESULT(0)
            }
            MSG_MIDDLE => {
                with(hwnd, |f| f.middle_open());
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
