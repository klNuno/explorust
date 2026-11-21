//! COM entry points for the Settings diagnostics TAP. The runtime supplies the diagnostics
//! site through IObjectWithSite; a process-wide worker handles changes in call order.

use super::session;
use super::state::SiteChange;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use windows::Win32::Foundation::{CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_FAIL, S_FALSE};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::Win32::System::Ole::{IObjectWithSite, IObjectWithSite_Impl};
use windows_core::{BOOL, GUID, HRESULT, IUnknown, Interface, Ref, implement};

#[unsafe(no_mangle)]
/// # Safety
/// COM's contract: valid GUID pointers and an out pointer.
pub unsafe extern "system" fn DllGetClassObject(clsid: *const GUID, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    std::panic::catch_unwind(|| {
        // SAFETY: the caller's pointers (COM's contract).
        unsafe {
            if out.is_null() || clsid.is_null() || *clsid != super::CLSID_TAP {
                return CLASS_E_CLASSNOTAVAILABLE;
            }
            let f: IClassFactory = Factory.into();
            f.query(iid, out)
        }
    })
    .unwrap_or(E_FAIL)
}

/// The TAP stays loaded for the host's life: its watch keeps running.
#[unsafe(no_mangle)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    S_FALSE
}

#[implement(IClassFactory)]
struct Factory;

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(&self, outer: Ref<IUnknown>, iid: *const GUID, out: *mut *mut c_void) -> windows_core::Result<()> {
        if !outer.is_null() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let id = TAPS.fetch_add(1, Ordering::Relaxed) + 1;
        let tap: IObjectWithSite = Tap { site: Mutex::new(None), id }.into();
        // SAFETY: COM's out pointer contract.
        unsafe { tap.query(iid, out).ok() }
    }

    fn LockServer(&self, _lock: BOOL) -> windows_core::Result<()> {
        Ok(())
    }
}

#[implement(IObjectWithSite)]
struct Tap {
    site: Mutex<Option<IUnknown>>,
    /// This TAP's number: only its own watch is stopped when the runtime lets go of it.
    id: u64,
}

static TAPS: AtomicU64 = AtomicU64::new(0);

impl IObjectWithSite_Impl for Tap_Impl {
    fn SetSite(&self, site: Ref<IUnknown>) -> windows_core::Result<()> {
        let site = site.cloned();
        let mut current = self.site.lock().unwrap_or_else(|e| e.into_inner());
        session::enqueue(match site.as_ref() {
            Some(s) => SiteChange::Attach(self.id, session::Site(s.clone())),
            None => SiteChange::Detach(self.id),
        })?;
        let previous = std::mem::replace(&mut *current, site);
        drop(current);
        drop(previous);
        Ok(())
    }

    fn GetSite(&self, iid: *const GUID, out: *mut *mut c_void) -> windows_core::Result<()> {
        match self.site.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            // SAFETY: COM's out pointer contract.
            Some(s) => unsafe { s.query(iid, out).ok() },
            None => Err(E_FAIL.into()),
        }
    }
}
