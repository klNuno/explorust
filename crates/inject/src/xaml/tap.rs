//! The TAP object Settings loads when explorust calls `InitializeXamlDiagnosticsEx` on it: the
//! XAML runtime asks this DLL's class factory for `CLSID_TAP`, then hands the object its
//! diagnostics site (`IObjectWithSite::SetSite`). From the site: the rules (the initialization
//! data) and the visual tree service, advised from a thread of our own (advising on the thread
//! that called `SetSite` blocks the runtime).

use super::abi::*;
use super::report;
use super::rules::Rules;
use super::watch::{self, Watch};
use std::ffi::c_void;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_FAIL, S_FALSE};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, IClassFactory, IClassFactory_Impl};
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
        let tap: IObjectWithSite = Tap { site: Mutex::new(None) }.into();
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
}

impl IObjectWithSite_Impl for Tap_Impl {
    fn SetSite(&self, site: Ref<IUnknown>) -> windows_core::Result<()> {
        let site = site.cloned();
        *self.site.lock().unwrap_or_else(|e| e.into_inner()) = site.clone();
        if let Some(s) = site
            && let Err(e) = start(&s)
        {
            report::line(format!("tap: not started: {e}"));
        }
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

/// Moves a COM pointer to the advise thread; the diagnostics service is free-threaded.
struct Send<T>(T);
// SAFETY: see above; XAML's own tools advise from a second thread the same way.
unsafe impl<T> std::marker::Send for Send<T> {}

fn start(site: &IUnknown) -> windows_core::Result<()> {
    let t0 = Instant::now();
    let diag: IXamlDiagnostics = site.cast()?;
    let svc: IVisualTreeService = site.cast()?;
    let mut data: *mut u16 = std::ptr::null_mut();
    // SAFETY: an out BSTR we free.
    let text = unsafe {
        let hr = diag.GetInitializationData(&mut data);
        let t = if hr.is_ok() { bstr(data) } else { String::new() };
        SysFreeString(data);
        t
    };
    let rules = Rules::parse(&text);
    report::line(format!(
        "tap: site set in pid {}, {} rules, dump {}",
        std::process::id(),
        rules.len(),
        rules.dump
    ));
    let (w, st) = Watch::new(svc.clone(), diag, rules, t0);
    let cb: IVisualTreeServiceCallback2 = w.into();
    let moved = Send((svc, cb));
    std::thread::Builder::new()
        .name("explorust-xaml".into())
        .spawn(move || {
            let m = moved;
            let (svc, cb) = &m.0;
            // SAFETY: COM on this thread; the callback lives as long as the service holds it.
            let hr = unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                svc.AdviseVisualTreeChange(cb.as_raw())
            };
            report::line(format!("tap: advise {:#010x} in {} ms", hr.0, t0.elapsed().as_millis()));
            // The first burst reports the existing tree; send what the dump holds after it.
            for wait in [2, 6, 20] {
                std::thread::sleep(Duration::from_secs(wait));
                watch::flush(&st, t0);
            }
        })
        .map(|_| ())
        .map_err(|_| E_FAIL.into())
}
