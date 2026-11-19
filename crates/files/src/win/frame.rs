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
use crate::win::{log, menus};
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
use windows::Win32::UI::Shell::{SIGDN_DESKTOPABSOLUTEEDITING, SVUIA_ACTIVATE_FOCUS};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{PCWSTR, w};

const CLASS: PCWSTR = w!("explorust.FilesWindow");
pub const MSG_RELAYOUT: u32 = 0x8000 + 0x123;

pub struct Frame {
    pub hwnd: HWND,
    canvas: Canvas,
    tabs: Vec<Tab>,
    active: usize,
    next_id: usize,
    hover: Hit,
    pressed: Hit,
    spans: Vec<(f32, f32)>,
    address: Option<LineEdit>,
    search: LineEdit,
    search_focus: bool,
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
}

thread_local! {
    static FRAMES: RefCell<Vec<(HWND, Rc<RefCell<Frame>>)>> = const { RefCell::new(Vec::new()) };
    static SHARED: RefCell<Option<(Rc<Shared>, &'static Strings)>> = const { RefCell::new(None) };
    static NEW_WINDOW: Cell<Option<HWND>> = const { Cell::new(None) };
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
        let k = scale(hwnd);
        let on: i32 = 1;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &on as *const _ as _, 4);
        let kind = DWMSBT_MAINWINDOW;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &kind as *const _ as _, 4);
        shell::allow_dark(hwnd);
        let rc = client(hwnd);
        let canvas = Canvas::new(sh, hwnd, rc.right as u32, (L::CHROME_H * k).ceil() as u32, 96.0 * k)?;
        let frame = Frame {
            hwnd,
            canvas,
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            hover: Hit::None,
            pressed: Hit::None,
            spans: Vec::new(),
            address: None,
            search: LineEdit::default(),
            search_focus: false,
            details: Rc::new(Cell::new(false)),
            s,
            icons: HashMap::new(),
            window_active: false,
            tracking: false,
            view_hidden: false,
            laid_out: (0, 0, 0),
            opened: None,
        };
        let rc = Rc::new(RefCell::new(frame));
        FRAMES.with(|f| f.borrow_mut().push((hwnd, rc.clone())));
        // The frame becomes the whole window except Windows' resize borders.
        SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE)?;
        let mut fr = rc.borrow_mut();
        fr.extend_frame();
        fr.add_tab(place)?;
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
        let _ = SetForegroundWindow(hwnd);
    }
    (shown, drawn - shown, ms(t) - drawn)
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
        if let Some(t) = f.tabs.get(f.active) {
            let _ = t.browse(place);
        }
    });
}

pub fn set_opened(hwnd: HWND, t: Instant) {
    with(hwnd, |f| f.opened = Some(t));
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

    fn extend_frame(&self) {
        let m = MARGINS { cxLeftWidth: 0, cxRightWidth: 0, cyTopHeight: self.chrome_px(), cyBottomHeight: 0 };
        unsafe {
            let _ = DwmExtendFrameIntoClientArea(self.hwnd, &m);
        }
    }

    fn view_rect(&self) -> RECT {
        let r = client(self.hwnd);
        RECT { left: 0, top: self.chrome_px(), right: r.right, bottom: r.bottom }
    }

    fn tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
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

    fn switch(&mut self, i: usize) {
        if i >= self.tabs.len() || i == self.active {
            return;
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
        self.search = LineEdit::default();
        self.title();
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
        t.destroy();
        if self.active >= self.tabs.len() || i < self.active {
            self.active = self.active.saturating_sub(1).min(self.tabs.len() - 1);
        }
        if let Some(t) = self.tab() {
            t.set_rect(self.view_rect());
            t.show(true);
        }
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
        let selection = t.and_then(|t| t.folder_view()).map(|v| shell::has_selection(&v)).unwrap_or(false);
        let view = chrome::View {
            w,
            tabs,
            active: self.active,
            can_back: t.map(|t| t.history.can_back()).unwrap_or(false),
            can_forward: t.map(|t| t.history.can_forward()).unwrap_or(false),
            can_up: t.map(|t| t.can_up()).unwrap_or(false),
            crumbs: t.map(|t| t.crumbs.iter().map(|c| c.0.as_str()).collect()).unwrap_or_default(),
            address: self.address.as_ref(),
            search: &self.search,
            search_focus: self.search_focus,
            hover: self.hover,
            pressed: self.pressed,
            selection,
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
        self.extend_frame();
        if let Some(t) = self.tab() {
            t.set_rect(self.view_rect());
        }
        self.draw();
    }

    fn hit(&self, x_px: i32, y_px: i32) -> Hit {
        let k = self.scale();
        let w = client(self.hwnd).right as f32 / k;
        L::hit(x_px as f32 / k, y_px as f32 / k, w, self.tabs.len(), &self.spans)
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
        if let Some(t) = self.tab().and_then(|t| t.view_window).or_else(|| self.tab().and_then(|t| t.window())) {
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
                if let Some(t) = self.tab() {
                    t.up()
                }
            }
            Hit::Refresh => {
                if let Some(t) = self.tab() {
                    t.refresh()
                }
            }
            Hit::Crumb(i) => {
                if let Some(t) = self.tab() {
                    if let Some((_, p)) = t.crumbs.get(i) {
                        let _ = t.browse(p);
                    }
                }
            }
            Hit::Address => self.edit_address(),
            Hit::Search => self.edit_search(),
            Hit::Tab(i) => self.switch(i),
            Hit::TabClose(i) => self.close_tab(i),
            Hit::NewTab => self.new_tab(),
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
        if let Some(t) = self.tabs.get_mut(self.active) {
            t.go(m);
        }
    }

    fn new_tab(&mut self) {
        if let Some(home) = shell::home() {
            if let Err(e) = self.add_tab(&home) {
                log::line(&format!("new tab failed: {e}"));
            }
        }
    }

    fn command(&mut self, c: Cmd, k: f32) {
        let Some(t) = self.tab() else { return };
        let fv = t.folder_view();
        let sv = t.shell_view();
        let below = |x: f32| {
            let mut p = POINT { x: (x * k) as i32, y: ((L::CMD_CY + 18.0) * k) as i32 };
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::ClientToScreen(self.hwnd, &mut p);
            }
            p
        };
        let r = match c {
            Cmd::Cut | Cmd::Copy | Cmd::Delete | Cmd::Share => {
                let verb = match c {
                    Cmd::Cut => "cut",
                    Cmd::Copy => "copy",
                    Cmd::Delete => "delete",
                    _ => "Windows.ModernShare",
                };
                match fv.as_ref().and_then(shell::selection_menu) {
                    Some(m) => shell::invoke(&m, self.hwnd, verb),
                    None => Ok(()),
                }
            }
            Cmd::Rename => fv.as_ref().map(|v| unsafe { v.DoRename() }).unwrap_or(Ok(())),
            Cmd::Paste => match sv.as_ref().and_then(shell::background_menu) {
                Some(m) => shell::invoke(&m, self.hwnd, "paste"),
                None => Ok(()),
            },
            Cmd::New => match sv.as_ref().and_then(shell::background_menu) {
                Some(m) => shell::invoke(&m, self.hwnd, "NewFolder"),
                None => Ok(()),
            },
            Cmd::Sort => {
                if let Some(v) = &fv {
                    menus::sort(self.hwnd, v, self.s, below(L::CMD_LABELLED[1].5));
                }
                Ok(())
            }
            Cmd::View => {
                if let Some(v) = &fv {
                    menus::view(self.hwnd, v, self.s, below(L::CMD_LABELLED[2].5));
                }
                Ok(())
            }
            Cmd::More => {
                if let (Some(v), Some(s)) = (&fv, &sv) {
                    menus::more(self.hwnd, v, s, self.s, below(L::CMD_MORE_CX - 20.0));
                }
                Ok(())
            }
            Cmd::Details => {
                self.details.set(!self.details.get());
                // The browser asks the pane states again on the next navigation.
                if let Some(p) = t.history.current().cloned() {
                    let _ = unsafe { t.eb.BrowseToIDList(p.as_ptr(), windows::Win32::UI::Shell::SBSP_SAMEBROWSER) };
                }
                Ok(())
            }
        };
        if let Err(e) = r {
            log::line(&format!("command {c:?} failed: {e}"));
        }
    }

    fn accelerator(&mut self, msg: &MSG) -> bool {
        let vk = VIRTUAL_KEY(msg.wParam.0 as u16);
        let ctrl = key_down(VK_CONTROL);
        let alt = key_down(VK_MENU);
        let in_edit = class_of(msg.hwnd) == "Edit";
        let ours = match vk {
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
                    self.search = LineEdit::default();
                }
                self.end_edit();
                self.focus_view();
                return true;
            }
            VK_RETURN => {
                let text = e.text();
                self.end_edit();
                if searching {
                    self.run_search(&text);
                } else {
                    self.go_to(&text);
                }
                self.focus_view();
                return true;
            }
            _ => return false,
        }
        self.draw();
        true
    }

    fn go_to(&self, text: &str) {
        let Some(t) = self.tab() else { return };
        match Pidl::parse(text.trim()) {
            Some(p) => {
                let _ = t.browse(&p);
            }
            None => {
                log::line(&format!("address: {text:?} not found"));
                unsafe {
                    let _ = MessageBeep(MB_ICONWARNING);
                }
            }
        }
    }

    fn run_search(&self, q: &str) {
        let Some(t) = self.tab() else { return };
        if q.trim().is_empty() {
            return;
        }
        let loc = t.item.as_ref().and_then(shell::fs_path);
        let uri = crate::uri::search(q, loc.as_deref(), &t.title);
        match Pidl::parse(&uri) {
            Some(p) => {
                let _ = t.browse(&p);
            }
            None => log::line(&format!("search: {uri} did not parse")),
        }
    }

    fn navigated(&mut self, id: usize, p: Pidl) {
        let active_id = self.tab().map(|t| t.id);
        let view_rect = self.view_rect();
        let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) else { return };
        t.navigated(p);
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
            self.search = LineEdit::default();
        }
        self.draw();
    }

    fn view_created(&mut self, id: usize) {
        let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) else { return };
        if let Some(root) = t.window() {
            shell::dark_children(root);
        }
        t.view_window = t.shell_view().and_then(|v| unsafe { windows::Win32::System::Ole::IOleWindow::GetWindow(&v).ok() });
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
                with(hwnd, |f| {
                    f.window_active = (wp.0 & 0xffff) != 0;
                    f.draw();
                });
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
            tab::MSG_VIEW => {
                with(hwnd, |f| f.view_created(wp.0));
                LRESULT(0)
            }
            tab::MSG_FAILED => {
                log::line("navigation failed");
                LRESULT(0)
            }
            WM_CLOSE => {
                with(hwnd, |f| {
                    for t in f.tabs.drain(..) {
                        t.destroy();
                    }
                });
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                FRAMES.with(|f| f.borrow_mut().retain(|(h, _)| *h != hwnd));
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
