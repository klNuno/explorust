//! One tab: Windows' `ExplorerBrowser` hosted in the frame with its frames on (the items view and
//! the navigation pane, an `INameSpaceTreeControl`, both Windows' own), the events it reports,
//! and the history of the places it showed.

use crate::history::{History, Move};
use crate::win::{frame, log};
use crate::win::shell::{self, Pidl};
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;
use windows::Win32::Foundation::{E_ABORT, E_INVALIDARG, E_NOINTERFACE, HWND, LPARAM, LRESULT, POINT, RECT, S_FALSE, WPARAM};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IServiceProvider, IServiceProvider_Impl};
use windows::Win32::System::Ole::IOleWindow;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::{GA_ROOT, GetAncestor, GetWindowRect, HWND_TOP, IsChild, PostMessageW, SW_HIDE, SW_SHOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos, ShowWindow, WM_NCDESTROY, WM_WINDOWPOSCHANGED};
use windows_core::{GUID, IUnknownImpl, Interface, Ref, Result, implement};

pub const MSG_NAVIGATION: u32 = 0x8000 + 0x120;
pub const MSG_VIEW: u32 = 0x8000 + 0x121;
pub const MSG_SELECTION: u32 = 0x8000 + 0x128;
pub const MSG_FOLDER_LAYOUT: u32 = 0x8000 + 0x12C;

pub enum Navigation {
    Complete(Pidl),
    Failed(Option<Pidl>),
}

#[derive(Default)]
struct ResultsNavigation {
    target: RefCell<Option<Pidl>>,
    filling: Cell<bool>,
}

#[implement(IExplorerBrowserEvents, Agile = false)]
struct Events {
    hwnd: HWND,
    id: usize,
    results: Option<Rc<ResultsNavigation>>,
    showing_results: Rc<Cell<bool>>,
    started: Cell<Option<Instant>>,
}

impl IExplorerBrowserEvents_Impl for Events_Impl {
    fn OnNavigationPending(&self, pidl: *const ITEMIDLIST) -> Result<()> {
        let redirect = self.results.as_ref().map_or_else(|| self.showing_results.get(), |results| {
            !(results.filling.get() || results.target.borrow().as_ref().is_some_and(|target| {
                !pidl.is_null() && unsafe { ILIsEqual(pidl, target.as_ptr()).as_bool() }
            }))
        });
        if redirect {
            if let Some(target) = Pidl::copy(pidl) {
                // Let the frame clear the search before navigating the owning folder browser.
                log::line(&format!("browser {}: routing navigation out of search", self.id));
                frame::browse_tab_later(self.hwnd, self.id, target);
            }
            return Err(E_ABORT.into());
        }
        self.started.set(Some(Instant::now()));
        Ok(())
    }

    fn OnViewCreated(&self, _view: Ref<IShellView>) -> Result<()> {
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), MSG_VIEW, WPARAM(self.id), LPARAM(0));
        }
        Ok(())
    }

    fn OnNavigationComplete(&self, pidl: *const ITEMIDLIST) -> Result<()> {
        if let Some(started) = self.started.take() {
            let view = if self.results.is_some() { "search" } else { "folder" };
            log::line(&format!(
                "Files {:#x} browser {}: {view} navigation completed in {:.1} ms",
                self.hwnd.0 as usize, self.id, started.elapsed().as_secs_f64() * 1000.0
            ));
        }
        if self.results.is_none() && let Some(pidl) = Pidl::copy(pidl) {
            frame::browser_navigated(self.hwnd, self.id, Navigation::Complete(pidl));
        }
        Ok(())
    }

    fn OnNavigationFailed(&self, pidl: *const ITEMIDLIST) -> Result<()> {
        self.started.set(None);
        if self.results.is_none() {
            frame::browser_navigated(self.hwnd, self.id, Navigation::Failed(Pidl::copy(pidl)));
        }
        Ok(())
    }
}

/// The browser's site: it asks for `IExplorerPaneVisibility` to know which panes to show.
#[implement(IServiceProvider, IExplorerPaneVisibility, ICommDlgBrowser, Agile = false)]
struct Site {
    hwnd: HWND,
    id: usize,
    searching: bool,
}

impl IServiceProvider_Impl for Site_Impl {
    fn QueryService(&self, service: *const GUID, riid: *const GUID, out: *mut *mut c_void) -> Result<()> {
        if out.is_null() || service.is_null() || riid.is_null() {
            return Err(E_INVALIDARG.into());
        }
        unsafe {
            *out = std::ptr::null_mut();
            if *service == IExplorerPaneVisibility::IID || *service == SID_SCommDlgBrowser || *service == ICommDlgBrowser::IID {
                return self.QueryInterface(riid, out).ok();
            }
        }
        Err(E_NOINTERFACE.into())
    }
}

impl ICommDlgBrowser_Impl for Site_Impl {
    fn OnDefaultCommand(&self, view: Ref<IShellView>) -> Result<()> {
        let target = view.as_ref()
            .and_then(shell::single_selection)
            .and_then(|item| shell::folder_target(&item));
        if let Some(target) = target {
            frame::browse_tab_later(self.hwnd, self.id, target);
            return Ok(());
        }
        // Let the shell's item view open files with their associated applications.
        Err(S_FALSE.into())
    }

    fn OnStateChange(&self, _: Ref<IShellView>, change: u32) -> Result<()> {
        if change == CDBOSC_SELCHANGE {
            unsafe {
                let _ = PostMessageW(Some(self.hwnd), MSG_SELECTION, WPARAM(self.id), LPARAM(0));
            }
        }
        Ok(())
    }

    fn IncludeObject(&self, _: Ref<IShellView>, _: *const ITEMIDLIST) -> Result<()> {
        Ok(())
    }
}

impl IExplorerPaneVisibility_Impl for Site_Impl {
    fn GetPaneState(&self, ep: *const GUID) -> Result<u32> {
        let ep = unsafe { *ep };
        let on = (EPS_DEFAULT_ON.0 | EPS_FORCE.0) as u32;
        let off = (EPS_DEFAULT_OFF.0 | EPS_FORCE.0) as u32;
        Ok(if ep == EP_NavPane && !self.searching {
            on
        } else {
            // Commands bar, ribbon, preview pane, the Windows 7 query pane: ours or none.
            off
        })
    }
}

pub struct Browser {
    pub eb: IExplorerBrowser,
    pub id: usize,
    results: Option<Rc<ResultsNavigation>>,
    showing_results: Rc<Cell<bool>>,
    cookie: u32,
}

impl Browser {
    fn new(hwnd: HWND, rc: RECT, searching: bool) -> Result<Self> {
        static NEXT_BROWSER: AtomicUsize = AtomicUsize::new(1);
        let id = NEXT_BROWSER.fetch_add(1, Ordering::Relaxed);
        let results = searching.then(|| Rc::new(ResultsNavigation::default()));
        let showing_results = Rc::new(Cell::new(false));
        unsafe {
            let eb: IExplorerBrowser = CoCreateInstance(&ExplorerBrowser, None, CLSCTX_INPROC_SERVER)?;
            let site: IServiceProvider = Site { hwnd, id, searching }.into();
            eb.SetOptions(EBO_SHOWFRAMES | EBO_NOBORDER | EBO_NOTRAVELLOG)?;
            let fs = FOLDERSETTINGS { ViewMode: FVM_DETAILS.0 as u32, fFlags: FWF_NOWEBVIEW.0 as u32 };
            let events: IExplorerBrowserEvents = Events {
                hwnd, id, results: results.clone(), showing_results: showing_results.clone(), started: Cell::new(None),
            }.into();
            let mut browser = Self { eb, id, results, showing_results, cookie: 0 };
            IUnknown_SetSite(&browser.eb, &site)?;
            browser.eb.Initialize(hwnd, &rc, Some(&fs))?;
            browser.cookie = browser.eb.Advise(&events)?;
            Ok(browser)
        }
    }

    pub fn browse_results(&self, pidl: Pidl) -> Result<()> {
        if let Some(nav) = &self.results {
            nav.target.replace(Some(pidl.clone()));
        }
        unsafe { self.eb.BrowseToIDList(pidl.as_ptr(), SBSP_SAMEBROWSER | SBSP_ACTIVATE_NOFOCUS) }
    }

    pub fn create_index_results(&self) -> Result<()> {
        let nav = self.results.as_ref().expect("results browser");
        nav.filling.set(true);
        let result = unsafe { self.eb.FillFromObject(None::<&windows_core::IUnknown>, EBF_NODROPTARGET) };
        nav.filling.set(false);
        result
    }

    pub fn shell_view(&self) -> Option<IShellView> {
        unsafe { self.eb.GetCurrentView().ok() }
    }

    pub fn folder_view(&self) -> Option<IFolderView2> {
        unsafe { self.eb.GetCurrentView().ok() }
    }

    /// The browser's own window (the view and the panes live under it).
    pub fn window(&self) -> Option<HWND> {
        let w: IOleWindow = self.eb.cast().ok()?;
        unsafe { w.GetWindow().ok() }
    }

    pub fn set_rect(&self, rc: RECT) {
        let _ = unsafe { self.eb.SetRect(None, rc) };
    }

    pub fn show(&self, on: bool) {
        if let Some(h) = self.window() {
            unsafe {
                let _ = ShowWindow(h, if on { SW_SHOW } else { SW_HIDE });
            }
        }
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        unsafe {
            if self.cookie != 0 {
                let _ = self.eb.Unadvise(self.cookie);
            }
            let _ = IUnknown_SetSite(&self.eb, None::<&windows_core::IUnknown>);
            let _ = self.eb.Destroy();
        }
    }
}

/// The native navigation splitter resizes the folder view without resizing the Files frame.
pub fn watch_folder_layout(view: &IShellView, id: usize) {
    if let Ok(hwnd) = unsafe { view.GetWindow() } {
        let _ = unsafe { SetWindowSubclass(hwnd, Some(folder_layout), id, 0) };
    }
}

unsafe extern "system" fn folder_layout(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM, id: usize, _: usize) -> LRESULT {
    unsafe {
        if msg == WM_WINDOWPOSCHANGED {
            let root = GetAncestor(hwnd, GA_ROOT);
            let _ = PostMessageW(Some(root), MSG_FOLDER_LAYOUT, WPARAM(id), LPARAM(0));
        } else if msg == WM_NCDESTROY {
            let _ = RemoveWindowSubclass(hwnd, Some(folder_layout), id);
        }
        DefSubclassProc(hwnd, msg, wp, lp)
    }
}

pub struct Tab {
    pub id: usize,
    hwnd: HWND,
    host: Browser,
    results: Option<(bool, Browser)>,
    pub history: History<Pidl>,
    pub title: String,
    pub crumbs: Vec<(String, Pidl)>,
    pub item: Option<IShellItem>,
    pub icon: Option<(u32, u32, Vec<u8>)>,
    pub path: Option<String>,
    pub query: String,
    pub generation: usize,
    pub address_generation: usize,
    rect: Cell<RECT>,
}

impl Tab {
    pub fn new(hwnd: HWND, id: usize, rc: RECT) -> Result<Self> {
        Ok(Self {
            id,
            hwnd,
            host: Browser::new(hwnd, rc, false)?,
            results: None,
            history: History::new(),
            title: String::new(),
            crumbs: Vec::new(),
            item: None,
            icon: None,
            path: None,
            query: String::new(),
            generation: 0,
            address_generation: 0,
            rect: Cell::new(rc),
        })
    }

    pub fn browser(&self) -> &Browser {
        self.results.as_ref().map(|(_, view)| view).unwrap_or(&self.host)
    }

    /// Only the visible items view handles shell-view accelerators. The native navigation tree
    /// receives its own messages, including when the results browser covers the folder's items.
    pub fn focused_items(&self, focused: HWND) -> Option<IShellView> {
        let view = self.shell_view()?;
        let hwnd = unsafe { view.GetWindow().ok()? };
        (!hwnd.is_invalid() && (hwnd == focused || unsafe { IsChild(hwnd, focused).as_bool() })).then_some(view)
    }

    pub fn invalidate_address(&mut self) -> usize {
        self.address_generation = self.address_generation.wrapping_add(1);
        self.address_generation
    }

    pub fn source(&self, id: usize) -> Option<&Browser> {
        if self.host.id == id {
            Some(&self.host)
        } else {
            self.results.as_ref().map(|(_, browser)| browser).filter(|browser| browser.id == id)
        }
    }

    pub fn folder_browser(&self, id: usize) -> bool {
        self.host.id == id
    }

    pub fn index_results_id(&self) -> Option<usize> {
        self.results.as_ref().filter(|(indexed, _)| *indexed).map(|(_, view)| view.id)
    }

    pub fn remove_index_results(&self, id: usize, items: &[Pidl]) {
        if self.index_results_id() != Some(id) {
            return;
        }
        if let Some(results) = self.folder_view().and_then(|view| unsafe { view.GetFolder::<IResultsFolder>().ok() }) {
            for item in items {
                let _ = unsafe { results.RemoveIDList(item.as_ptr()) };
            }
        }
    }

    pub fn search_browser(&mut self, index: bool) -> Result<&Browser> {
        if self.results.as_ref().is_none_or(|(mode, _)| *mode != index) {
            let browser = Browser::new(self.hwnd, self.results_rect(), true)?;
            self.results = Some((index, browser));
        }
        Ok(&self.results.as_ref().unwrap().1)
    }

    pub fn show_results(&self, active: bool) {
        self.host.showing_results.set(true);
        self.show(active);
    }

    pub fn clear_search(&mut self, active: bool) {
        self.query.clear();
        self.generation = self.generation.wrapping_add(1);
        self.dismiss_results(active);
    }

    pub fn dismiss_results(&mut self, active: bool) {
        self.host.showing_results.set(false);
        if self.results.take().is_some() {
            self.host.set_rect(self.rect.get());
            self.host.show(active);
        }
    }

    pub fn browse(&self, p: &Pidl) -> Result<()> {
        let result = unsafe { self.host.eb.BrowseToIDList(p.as_ptr(), SBSP_SAMEBROWSER) };
        if let Err(error) = &result {
            log::line(&format!(
                "tab {}: navigation failed for {:?} (BrowseToIDList HRESULT {:#010x}): {error}",
                self.id, p.display(), error.code().0 as u32
            ));
        }
        result
    }

    pub fn go(&mut self, m: Move) {
        if let Some(p) = self.history.begin(m) {
            let _ = self.browse(&p);
        }
    }

    pub fn up(&self) {
        let Some(item) = &self.item else { return };
        if let Some(parent) = unsafe { item.GetParent().ok() }.and_then(|p| Pidl::of(&p)) {
            let _ = self.browse(&parent);
        }
    }

    pub fn can_up(&self) -> bool {
        self.crumbs.len() > 1
    }

    pub fn refresh(&self) {
        if let Some(v) = self.shell_view() {
            let _ = unsafe { v.Refresh() };
        }
    }

    pub fn shell_view(&self) -> Option<IShellView> {
        self.browser().shell_view()
    }

    pub fn folder_view(&self) -> Option<IFolderView2> {
        self.browser().folder_view()
    }

    pub fn window(&self) -> Option<HWND> {
        self.browser().window()
    }

    pub fn set_rect(&self, rc: RECT) {
        self.rect.set(rc);
        self.host.set_rect(rc);
        self.layout_results();
    }

    pub fn layout_results(&self) {
        if let Some((_, browser)) = &self.results {
            browser.set_rect(self.results_rect());
        }
    }

    fn results_rect(&self) -> RECT {
        let mut rect = self.rect.get();
        // The owning folder browser keeps its native navigation pane during search. Cover only
        // its items area so that one pane owns all folder navigation, including indexed searches.
        let Some(view) = self.host.shell_view() else { return rect };
        unsafe {
            if let Ok(hwnd) = view.GetWindow() {
                let mut area = RECT::default();
                if GetWindowRect(hwnd, &mut area).is_ok() {
                    let mut left = POINT { x: area.left, y: area.top };
                    if ScreenToClient(self.hwnd, &mut left).as_bool() {
                        rect.left = left.x.max(rect.left).min(rect.right);
                    }
                }
            }
        }
        rect
    }

    pub fn show(&self, on: bool) {
        self.host.show(on);
        if let Some((_, browser)) = &self.results {
            if on {
                self.layout_results();
                if let Some(hwnd) = browser.window() {
                    let _ = unsafe { SetWindowPos(hwnd, Some(HWND_TOP), 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE) };
                }
            }
            browser.show(on);
        }
    }

    /// Records a completed navigation: history, title, breadcrumb and icon.
    pub fn navigated(&mut self, p: Pidl) {
        let item = p.item().ok();
        self.history.navigated(p);
        if let Some(item) = item {
            self.title = shell::name(&item, SIGDN_NORMALDISPLAY);
            self.crumbs = shell::crumbs(&item);
            self.icon = shell::icon(&item, 16);
            self.path = shell::fs_path(&item);
            self.item = Some(item);
        }
    }
}
