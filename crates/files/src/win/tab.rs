//! One tab: Windows' `ExplorerBrowser` hosted in the frame with its frames on (the items view and
//! the navigation pane, an `INameSpaceTreeControl`, both Windows' own), the events it reports,
//! and the history of the places it showed.

use crate::history::{History, Move};
use crate::win::shell::{self, Pidl};
use std::cell::Cell;
use std::ffi::c_void;
use std::rc::Rc;
use windows::Win32::Foundation::{E_INVALIDARG, E_NOINTERFACE, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IServiceProvider, IServiceProvider_Impl};
use windows::Win32::System::Ole::IOleWindow;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, SW_HIDE, SW_SHOW, ShowWindow};
use windows_core::{GUID, IUnknownImpl, Interface, Ref, Result, implement};

/// Posted to the frame: a navigation completed (wParam tab id, lParam an `ILClone`d ID list the
/// frame frees), the view was created (lParam 0), or a navigation failed (lParam -1).
pub const MSG_NAVIGATED: u32 = 0x8000 + 0x120;
pub const MSG_VIEW: u32 = 0x8000 + 0x121;
pub const MSG_FAILED: u32 = 0x8000 + 0x122;

#[implement(IExplorerBrowserEvents)]
struct Events {
    hwnd: HWND,
    id: usize,
}

impl IExplorerBrowserEvents_Impl for Events_Impl {
    fn OnNavigationPending(&self, _pidl: *const ITEMIDLIST) -> Result<()> {
        Ok(())
    }

    fn OnViewCreated(&self, _view: Ref<IShellView>) -> Result<()> {
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), MSG_VIEW, WPARAM(self.id), LPARAM(0));
        }
        Ok(())
    }

    fn OnNavigationComplete(&self, pidl: *const ITEMIDLIST) -> Result<()> {
        unsafe {
            let copy = ILClone(pidl);
            if PostMessageW(Some(self.hwnd), MSG_NAVIGATED, WPARAM(self.id), LPARAM(copy as isize)).is_err() {
                ILFree(Some(copy));
            }
        }
        Ok(())
    }

    fn OnNavigationFailed(&self, _pidl: *const ITEMIDLIST) -> Result<()> {
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), MSG_FAILED, WPARAM(self.id), LPARAM(0));
        }
        Ok(())
    }
}

/// The browser's site: it asks for `IExplorerPaneVisibility` to know which panes to show.
#[implement(IServiceProvider, IExplorerPaneVisibility)]
struct Site {
    details: Rc<Cell<bool>>,
}

impl IServiceProvider_Impl for Site_Impl {
    fn QueryService(&self, service: *const GUID, riid: *const GUID, out: *mut *mut c_void) -> Result<()> {
        if out.is_null() || service.is_null() || riid.is_null() {
            return Err(E_INVALIDARG.into());
        }
        unsafe {
            *out = std::ptr::null_mut();
            if *service == IExplorerPaneVisibility::IID {
                return self.QueryInterface(riid, out).ok();
            }
        }
        Err(E_NOINTERFACE.into())
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

pub struct Tab {
    pub id: usize,
    pub eb: IExplorerBrowser,
    cookie: u32,
    pub history: History<Pidl>,
    pub title: String,
    pub crumbs: Vec<(String, Pidl)>,
    pub item: Option<IShellItem>,
    pub icon: Option<(u32, u32, Vec<u8>)>,
    pub view_window: Option<HWND>,
}

impl Tab {
    pub fn new(hwnd: HWND, id: usize, rc: RECT, details: Rc<Cell<bool>>) -> Result<Tab> {
        unsafe {
            let eb: IExplorerBrowser = CoCreateInstance(&ExplorerBrowser, None, CLSCTX_INPROC_SERVER)?;
            let site: IServiceProvider = Site { details }.into();
            IUnknown_SetSite(&eb, &site)?;
            eb.SetOptions(EBO_SHOWFRAMES | EBO_NOBORDER)?;
            let fs = FOLDERSETTINGS { ViewMode: FVM_DETAILS.0 as u32, fFlags: FWF_NOWEBVIEW.0 as u32 };
            eb.Initialize(hwnd, &rc, Some(&fs))?;
            let events: IExplorerBrowserEvents = Events { hwnd, id }.into();
            let cookie = eb.Advise(&events)?;
            Ok(Tab {
                id,
                eb,
                cookie,
                history: History::new(),
                title: String::new(),
                crumbs: Vec::new(),
                item: None,
                icon: None,
                view_window: None,
            })
        }
    }

    pub fn browse(&self, p: &Pidl) -> Result<()> {
        unsafe { self.eb.BrowseToIDList(p.as_ptr(), SBSP_ABSOLUTE) }
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

    /// Records a completed navigation: history, title, breadcrumb and icon.
    pub fn navigated(&mut self, p: Pidl) {
        let item = p.item().ok();
        self.history.navigated(p);
        if let Some(item) = item {
            self.title = shell::name(&item, SIGDN_NORMALDISPLAY);
            self.crumbs = shell::crumbs(&item);
            self.icon = shell::icon(&item, 16);
            self.item = Some(item);
        }
    }

    pub fn destroy(&self) {
        unsafe {
            let _ = self.eb.Unadvise(self.cookie);
            let _ = IUnknown_SetSite(&self.eb, None::<&windows_core::IUnknown>);
            let _ = self.eb.Destroy();
        }
    }
}
