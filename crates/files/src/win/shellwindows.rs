//! Our windows in the shell's window list (`CLSID_ShellWindows`), where apps look for open folder
//! windows: `SHOpenFolderAndSelectItems` (an app's "show in folder"), `Shell.Application.Windows()`
//! scripts, installers that refresh or close Explorer windows. Each window registers as
//! `SWC_EXPLORER` with an object that answers what callers ask of it: the folder it shows
//! (`OnNavigate` with its ID list, `LocationURL` and `LocationName` through `IDispatch`) and its
//! browser (`SID_STopLevelBrowser` gives an `IShellBrowser` whose active view selects items).

use crate::win::shell::Pidl;
use crate::win::{frame, log};
use std::cell::RefCell;
use std::ffi::c_void;
use std::time::Instant;
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Ole::{IOleWindow_Impl, OLEMENUGROUPWIDTHS};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::System::Variant::{InitVariantFromBuffer, VARIANT};
use windows::Win32::UI::Controls::TBBUTTON;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::{HMENU, MSG};
use windows_core::{BSTR, GUID, IUnknownImpl, Interface, PCWSTR, Ref, Result, implement};

/// IWebBrowser2 dispids callers read from a shell window.
const DISPID_LOCATIONNAME: i32 = 210;
const DISPID_LOCATIONURL: i32 = 211;
const DISPID_HWND: i32 = -515;

thread_local! {
    static LIST: RefCell<Option<IShellWindows>> = const { RefCell::new(None) };
    static ASKED: RefCell<Vec<(GUID, GUID)>> = const { RefCell::new(Vec::new()) };
}

/// The window list of the session (Explorer's, or shell32's local server without it).
fn list() -> Option<IShellWindows> {
    LIST.with(|l| {
        let mut l = l.borrow_mut();
        if l.is_none() {
            match unsafe { CoCreateInstance::<_, IShellWindows>(&ShellWindows, None, CLSCTX_LOCAL_SERVER) } {
                Ok(sw) => *l = Some(sw),
                Err(e) => log::line(&format!("ShellWindows unavailable: {e}")),
            }
        }
        l.clone()
    })
}

fn location(p: &Pidl) -> Option<VARIANT> {
    unsafe { InitVariantFromBuffer(p.0.as_ptr() as *const c_void, p.0.len() as u32).ok() }
}

/// Registers `hwnd` showing `place`; returns its cookie.
pub fn register(hwnd: HWND, place: &Pidl) -> Option<i32> {
    let t = Instant::now();
    let sw = list()?;
    let loc = location(place)?;
    let root = VARIANT::default();
    unsafe {
        let pending = sw.RegisterPending(GetCurrentThreadId() as i32, &loc, &root, SWC_EXPLORER).ok();
        let disp: IDispatch = Browser { hwnd }.into();
        let cookie = match sw.Register(&disp, hwnd.0 as isize as i32, SWC_EXPLORER) {
            Ok(c) => c,
            Err(e) => {
                log::line(&format!("ShellWindows Register failed: {e}"));
                if let Some(p) = pending {
                    let _ = sw.Revoke(p);
                }
                return None;
            }
        };
        if let Some(p) = pending.filter(|p| *p != cookie) {
            let _ = sw.Revoke(p);
        }
        let _ = sw.OnNavigate(cookie, &loc);
        let _ = sw.OnActivated(cookie, VARIANT_TRUE);
        log::line(&format!(
            "ShellWindows: window {:#x} registered, cookie {cookie} (pending {pending:?}), {:.1} ms",
            hwnd.0 as usize,
            t.elapsed().as_secs_f64() * 1000.0
        ));
        Some(cookie)
    }
}

/// The window of `cookie` now shows `place`.
pub fn navigate(cookie: i32, place: &Pidl) {
    let (Some(sw), Some(loc)) = (list(), location(place)) else { return };
    if let Err(e) = unsafe { sw.OnNavigate(cookie, &loc) } {
        log::line(&format!("ShellWindows OnNavigate failed: {e}"));
    }
}

pub fn activated(cookie: i32, on: bool) {
    if let Some(sw) = list() {
        let _ = unsafe { sw.OnActivated(cookie, if on { VARIANT_TRUE } else { VARIANT_FALSE }) };
    }
}

pub fn revoke(cookie: i32) {
    if let Some(sw) = list() {
        let _ = unsafe { sw.Revoke(cookie) };
        log::line(&format!("ShellWindows: cookie {cookie} revoked"));
    }
}

/// Logs a service a caller asked for, once.
fn note(what: &str, service: GUID, riid: GUID) {
    let new = ASKED.with(|a| {
        let mut a = a.borrow_mut();
        let fresh = !a.contains(&(service, riid));
        if fresh {
            a.push((service, riid));
        }
        fresh
    });
    if new {
        log::line(&format!("ShellWindows caller asks {what} {service:?} for {riid:?}"));
    }
}

/// What a registered window gives callers. Not agile: COM brings calls to the UI thread, where
/// the frames live.
#[implement(IDispatch, IServiceProvider, IShellBrowser, Agile = false)]
struct Browser {
    hwnd: HWND,
}

impl IDispatch_Impl for Browser_Impl {
    fn GetTypeInfoCount(&self) -> Result<u32> {
        Ok(0)
    }

    fn GetTypeInfo(&self, _: u32, _: u32) -> Result<ITypeInfo> {
        Err(E_NOTIMPL.into())
    }

    fn GetIDsOfNames(&self, _: *const GUID, names: *const PCWSTR, count: u32, _: u32, ids: *mut i32) -> Result<()> {
        if names.is_null() || ids.is_null() || count == 0 {
            return Err(E_INVALIDARG.into());
        }
        let name = unsafe { (*names).to_string().unwrap_or_default() };
        let id = match name.to_ascii_lowercase().as_str() {
            "locationurl" => DISPID_LOCATIONURL,
            "locationname" => DISPID_LOCATIONNAME,
            "hwnd" => DISPID_HWND,
            _ => {
                log::line(&format!("ShellWindows caller asks for the name {name:?}"));
                unsafe { *ids = -1 };
                return Err(DISP_E_UNKNOWNNAME.into());
            }
        };
        unsafe { *ids = id };
        Ok(())
    }

    fn Invoke(
        &self,
        id: i32,
        _: *const GUID,
        _: u32,
        _: DISPATCH_FLAGS,
        _: *const DISPPARAMS,
        result: *mut VARIANT,
        _: *mut EXCEPINFO,
        _: *mut u32,
    ) -> Result<()> {
        let v: VARIANT = match id {
            DISPID_LOCATIONURL | DISPID_LOCATIONNAME => {
                let Some((path, title)) = frame::location(self.hwnd) else { return Err(E_FAIL.into()) };
                let s = if id == DISPID_LOCATIONURL {
                    path.map(|p| crate::uri::file_url(&p)).unwrap_or_default()
                } else {
                    title
                };
                BSTR::from(s).into()
            }
            DISPID_HWND => VARIANT::from(self.hwnd.0 as isize as i32),
            _ => {
                log::line(&format!("ShellWindows caller invokes dispid {id}"));
                return Err(DISP_E_MEMBERNOTFOUND.into());
            }
        };
        if !result.is_null() {
            unsafe { *result = v };
        }
        Ok(())
    }
}

impl IServiceProvider_Impl for Browser_Impl {
    fn QueryService(&self, service: *const GUID, riid: *const GUID, out: *mut *mut c_void) -> Result<()> {
        if out.is_null() || service.is_null() || riid.is_null() {
            return Err(E_INVALIDARG.into());
        }
        unsafe {
            *out = std::ptr::null_mut();
            let (s, r) = (*service, *riid);
            note("service", s, r);
            if s == SID_STopLevelBrowser || s == IShellBrowser::IID {
                return self.QueryInterface(riid, out).ok();
            }
            // Anything else is the active tab's browser's to answer.
            if let Some(sp) = frame::browser_services(self.hwnd) {
                return (Interface::vtable(&sp).QueryService)(Interface::as_raw(&sp), service, riid, out).ok();
            }
        }
        Err(E_NOINTERFACE.into())
    }
}

impl IOleWindow_Impl for Browser_Impl {
    fn GetWindow(&self) -> Result<HWND> {
        Ok(self.hwnd)
    }

    fn ContextSensitiveHelp(&self, _: windows_core::BOOL) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
}

impl IShellBrowser_Impl for Browser_Impl {
    fn InsertMenusSB(&self, _: HMENU, _: *mut OLEMENUGROUPWIDTHS) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn SetMenuSB(&self, _: HMENU, _: isize, _: HWND) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn RemoveMenusSB(&self, _: HMENU) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn SetStatusTextSB(&self, _: &PCWSTR) -> Result<()> {
        Ok(())
    }
    fn EnableModelessSB(&self, _: windows_core::BOOL) -> Result<()> {
        Ok(())
    }
    fn TranslateAcceleratorSB(&self, _: *const MSG, _: u16) -> Result<()> {
        Err(S_FALSE.into())
    }

    fn BrowseObject(&self, pidl: *const ITEMIDLIST, flags: u32) -> Result<()> {
        log::line(&format!("ShellWindows caller browses (flags {flags:#x})"));
        let p = Pidl::copy(pidl).ok_or(windows_core::Error::from(E_INVALIDARG))?;
        frame::browse_later(self.hwnd, p);
        Ok(())
    }

    fn GetViewStateStream(&self, _: u32) -> Result<IStream> {
        Err(E_NOTIMPL.into())
    }
    fn GetControlWindow(&self, _: u32) -> Result<HWND> {
        Err(E_NOTIMPL.into())
    }
    fn SendControlMsg(&self, _: u32, _: u32, _: WPARAM, _: LPARAM, _: *mut LRESULT) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn QueryActiveShellView(&self) -> Result<IShellView> {
        note("the active view", GUID::zeroed(), IShellView::IID);
        frame::active_view(self.hwnd).ok_or_else(|| E_FAIL.into())
    }

    fn OnViewWindowActive(&self, _: Ref<IShellView>) -> Result<()> {
        Ok(())
    }
    fn SetToolbarItems(&self, _: *const TBBUTTON, _: u32, _: u32) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
}
