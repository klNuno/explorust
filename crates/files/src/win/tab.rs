//! One tab: Windows' `ExplorerBrowser` hosted in the frame with its frames on (the items view and
//! the navigation pane, an `INameSpaceTreeControl`, both Windows' own), the events it reports,
//! and the history of the places it showed.

use crate::history::{History, Move};
use crate::win::{frame, log};
use crate::win::shell::{self, Pidl};
use std::cell::Cell;
use std::ffi::c_void;
use std::rc::Rc;
use std::time::Instant;
use windows::Win32::Foundation::{E_INVALIDARG, E_NOINTERFACE, HWND, LPARAM, RECT, S_FALSE, WPARAM};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IServiceProvider, IServiceProvider_Impl};
use windows::Win32::System::Ole::IOleWindow;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, SW_HIDE, SW_SHOW, ShowWindow};
use windows_core::{GUID, IUnknownImpl, Interface, Ref, Result, implement};

/// Posted to the frame: a navigation completed (wParam tab id, lParam an `ILClone`d ID list the
/// frame frees), the view was created (lParam 0), or a navigation failed (the target ID list).
pub const MSG_NAVIGATED: u32 = 0x8000 + 0x120;
pub const MSG_VIEW: u32 = 0x8000 + 0x121;
pub const MSG_FAILED: u32 = 0x8000 + 0x122;
pub const MSG_SELECTION: u32 = 0x8000 + 0x128;
pub const MSG_SEARCH_VIEW: u32 = 0x8000 + 0x12A;

#[implement(IExplorerBrowserEvents, Agile = false)]
struct Events {
    hwnd: HWND,
    id: usize,
    searching: bool,
    started: Cell<Option<Instant>>,
}

impl IExplorerBrowserEvents_Impl for Events_Impl {
    fn OnNavigationPending(&self, _pidl: *const ITEMIDLIST) -> Result<()> {
        self.started.set(Some(Instant::now()));
        Ok(())
    }

    fn OnViewCreated(&self, _view: Ref<IShellView>) -> Result<()> {
        unsafe {
            let msg = if self.searching { MSG_SEARCH_VIEW } else { MSG_VIEW };
            let _ = PostMessageW(Some(self.hwnd), msg, WPARAM(self.id), LPARAM(0));
        }
        Ok(())
    }

    fn OnNavigationComplete(&self, pidl: *const ITEMIDLIST) -> Result<()> {
        if let Some(started) = self.started.take() {
            let view = if self.searching { "search" } else { "folder" };
            log::line(&format!(
                "Files {:#x} tab {}: {view} navigation completed in {:.1} ms",
                self.hwnd.0 as usize, self.id, started.elapsed().as_secs_f64() * 1000.0
            ));
        }
        if self.searching {
            return Ok(());
        }
        unsafe {
            let copy = ILClone(pidl);
            if PostMessageW(Some(self.hwnd), MSG_NAVIGATED, WPARAM(self.id), LPARAM(copy as isize)).is_err() {
                ILFree(Some(copy));
            }
        }
        Ok(())
    }

    fn OnNavigationFailed(&self, pidl: *const ITEMIDLIST) -> Result<()> {
        self.started.set(None);
        unsafe {
            let copy = if pidl.is_null() { std::ptr::null_mut() } else { ILClone(pidl) };
            if PostMessageW(Some(self.hwnd), MSG_FAILED, WPARAM(self.id), LPARAM(copy as isize)).is_err() {
                ILFree(Some(copy));
            }
        }
        Ok(())
    }
}

/// The browser's site: it asks for `IExplorerPaneVisibility` to know which panes to show.
#[implement(IServiceProvider, IExplorerPaneVisibility, ICommDlgBrowser, Agile = false)]
struct Site {
    hwnd: HWND,
    id: usize,
    details: Rc<Cell<bool>>,
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
        Ok(if ep == EP_NavPane {
            on
        } else if ep == EP_DetailsPane {
            if self.details.get() { on } else { off }
        } else {
            // Commands bar, ribbon, preview pane, the Windows 7 query pane: ours or none.
            off
        })
    }
}

pub struct Browser {
    pub eb: IExplorerBrowser,
    cookie: u32,
}

impl Browser {
    fn new(hwnd: HWND, id: usize, rc: RECT, details: Rc<Cell<bool>>, searching: bool) -> Result<Self> {
        unsafe {
            let eb: IExplorerBrowser = CoCreateInstance(&ExplorerBrowser, None, CLSCTX_INPROC_SERVER)?;
            let site: IServiceProvider = Site { hwnd, id, details }.into();
            eb.SetOptions(EBO_SHOWFRAMES | EBO_NOBORDER | EBO_NOTRAVELLOG)?;
            let fs = FOLDERSETTINGS { ViewMode: FVM_DETAILS.0 as u32, fFlags: FWF_NOWEBVIEW.0 as u32 };
            let events: IExplorerBrowserEvents = Events { hwnd, id, searching, started: Cell::new(None) }.into();
            let mut browser = Self { eb, cookie: 0 };
            IUnknown_SetSite(&browser.eb, &site)?;
            browser.eb.Initialize(hwnd, &rc, Some(&fs))?;
            browser.cookie = browser.eb.Advise(&events)?;
            Ok(browser)
        }
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

pub struct Tab {
    pub id: usize,
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
    rect: Cell<RECT>,
}

impl Tab {
    pub fn new(hwnd: HWND, id: usize, rc: RECT, details: Rc<Cell<bool>>) -> Result<Self> {
        Ok(Self {
            id,
            host: Browser::new(hwnd, id, rc, details, false)?,
            results: None,
            history: History::new(),
            title: String::new(),
            crumbs: Vec::new(),
            item: None,
            icon: None,
            path: None,
            query: String::new(),
            generation: 0,
            rect: Cell::new(rc),
        })
    }

    pub fn browser(&self) -> &Browser {
        self.results.as_ref().map(|(_, view)| view).unwrap_or(&self.host)
    }

    pub fn source(&self, searching: bool) -> Option<&Browser> {
        if searching {
            self.results.as_ref().map(|(_, browser)| browser)
        } else {
            Some(&self.host)
        }
    }

    pub fn search_browser(&mut self, hwnd: HWND, rc: RECT, details: Rc<Cell<bool>>, index: bool) -> Result<&Browser> {
        if self.results.as_ref().is_none_or(|(mode, _)| *mode != index) {
            let browser = Browser::new(hwnd, self.id, rc, details, true)?;
            self.results = Some((index, browser));
        }
        Ok(&self.results.as_ref().unwrap().1)
    }

    pub fn show_results(&self, active: bool) {
        if let Some((_, browser)) = &self.results {
            self.host.show(false);
            browser.show(active);
        }
    }

    pub fn clear_search(&mut self, active: bool) {
        self.query.clear();
        self.generation = self.generation.wrapping_add(1);
        self.dismiss_results(active);
    }

    pub fn dismiss_results(&mut self, active: bool) {
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
        if let Some((_, browser)) = &self.results {
            browser.set_rect(rc);
        }
    }

    pub fn show(&self, on: bool) {
        self.browser().show(on);
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
