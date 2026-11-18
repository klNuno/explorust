//! The minimal IShellBrowser the desktop's shell view needs: Progman is its window, it has no
//! menus, toolbars or status bar, and a folder opened from the desktop opens in a new window
//! through Windows' own verb (as explorer's desktop opens a new browser).

use crate::log;
use std::cell::RefCell;
use std::ffi::c_void;
use windows::Win32::Foundation::{E_FAIL, E_INVALIDARG, E_NOINTERFACE, E_NOTIMPL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{IServiceProvider, IServiceProvider_Impl, IStream};
use windows::Win32::System::Ole::{IOleWindow_Impl, OLEMENUGROUPWIDTHS};
use windows::Win32::UI::Controls::TBBUTTON;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    IShellBrowser, IShellBrowser_Impl, IShellView, SEE_MASK_FLAG_NO_UI, SEE_MASK_INVOKEIDLIST, SHELLEXECUTEINFOW,
    SID_STopLevelBrowser, ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::{HMENU, MSG, SW_SHOWNORMAL};
use windows_core::{BOOL, Error, GUID, IUnknownImpl, Interface, PCWSTR, Ref, Result, implement};

/// SID_SShellBrowser is IID_IShellBrowser.
const SID_SSHELLBROWSER: GUID = IShellBrowser::IID;
/// SID_SShellDesktop (CLSID_ShellDesktop): the view asks it to know it is the real desktop, which
/// hides the icons the user turned off and restores the saved icon layout.
const SID_SSHELLDESKTOP: GUID = GUID::from_u128(0x00021400_0000_0000_c000_000000000046);

#[implement(IShellBrowser, IServiceProvider)]
pub struct Browser {
    hwnd: HWND,
    pub view: RefCell<Option<IShellView>>,
    asked: RefCell<Vec<GUID>>,
}

impl Browser {
    pub fn new(hwnd: HWND) -> Browser {
        Browser { hwnd, view: RefCell::new(None), asked: RefCell::new(Vec::new()) }
    }
}

impl IOleWindow_Impl for Browser_Impl {
    fn GetWindow(&self) -> Result<HWND> {
        Ok(self.hwnd)
    }

    fn ContextSensitiveHelp(&self, _enter: BOOL) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
}

impl IShellBrowser_Impl for Browser_Impl {
    fn InsertMenusSB(&self, _menu: HMENU, _widths: *mut OLEMENUGROUPWIDTHS) -> Result<()> {
        Ok(())
    }

    fn SetMenuSB(&self, _menu: HMENU, _res: isize, _active: HWND) -> Result<()> {
        Ok(())
    }

    fn RemoveMenusSB(&self, _menu: HMENU) -> Result<()> {
        Ok(())
    }

    fn SetStatusTextSB(&self, _text: &PCWSTR) -> Result<()> {
        Ok(())
    }

    fn EnableModelessSB(&self, _enable: BOOL) -> Result<()> {
        Ok(())
    }

    fn TranslateAcceleratorSB(&self, _msg: *const MSG, _id: u16) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn BrowseObject(&self, pidl: *const ITEMIDLIST, flags: u32) -> Result<()> {
        log::line(&format!("BrowseObject flags {flags:#x}"));
        if pidl.is_null() {
            return Err(E_INVALIDARG.into());
        }
        // The desktop's own pidl is empty, so a relative pidl is absolute here.
        let mut sei = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_INVOKEIDLIST | SEE_MASK_FLAG_NO_UI,
            hwnd: self.hwnd,
            lpIDList: pidl as *mut c_void,
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        // SAFETY: `sei` is filled for an ID-list invocation; the pidl lives for the call.
        let r = unsafe { ShellExecuteExW(&mut sei) };
        log::line(&format!("BrowseObject open: {r:?}"));
        r
    }

    fn GetViewStateStream(&self, mode: u32) -> Result<IStream> {
        log::line(&format!("GetViewStateStream mode {mode:#x}: none"));
        Err(E_NOTIMPL.into())
    }

    fn GetControlWindow(&self, _id: u32) -> Result<HWND> {
        Err(E_NOTIMPL.into())
    }

    fn SendControlMsg(&self, _id: u32, _msg: u32, _w: WPARAM, _l: LPARAM, ret: *mut LRESULT) -> Result<()> {
        if !ret.is_null() {
            // SAFETY: the caller's optional result slot.
            unsafe { *ret = LRESULT(0) };
        }
        Ok(())
    }

    fn QueryActiveShellView(&self) -> Result<IShellView> {
        self.view.borrow().clone().ok_or_else(|| Error::from(E_FAIL))
    }

    fn OnViewWindowActive(&self, _view: Ref<IShellView>) -> Result<()> {
        Ok(())
    }

    fn SetToolbarItems(&self, _buttons: *const TBBUTTON, _n: u32, _flags: u32) -> Result<()> {
        Ok(())
    }
}

impl IServiceProvider_Impl for Browser_Impl {
    fn QueryService(&self, service: *const GUID, riid: *const GUID, out: *mut *mut c_void) -> Result<()> {
        if out.is_null() || service.is_null() || riid.is_null() {
            return Err(E_INVALIDARG.into());
        }
        // SAFETY: COM passes valid GUID pointers and an output slot.
        unsafe { *out = std::ptr::null_mut() };
        let s = unsafe { *service };
        if s == SID_SSHELLBROWSER || s == SID_STopLevelBrowser || s == SID_SSHELLDESKTOP {
            // SAFETY: QueryInterface on this object with the caller's slot.
            return unsafe { self.QueryInterface(riid, out) }.ok();
        }
        let mut asked = self.asked.borrow_mut();
        if !asked.contains(&s) {
            asked.push(s);
            log::line(&format!("QueryService {s:?} riid {:?}: none", unsafe { *riid }));
        }
        Err(E_NOINTERFACE.into())
    }
}
