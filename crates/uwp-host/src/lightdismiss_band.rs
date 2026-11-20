//! explorust: the light dismiss overlay of Windows.UI.Immersive.dll (loaded here).
//!
//! twinui registers the notification center with `CLightDismissClientStore::Register` each time
//! it uncloaks. Register creates an overlay window (`CImmersiveLightDismissProvider::
//! CreateOverlayWindow`, `CreateWindowInBand` in a shell band) to catch clicks outside the flyout.
//! Under explorust user32 refuses that band with ERROR_ACCESS_DENIED, Register fails with
//! E_ACCESSDENIED and dismisses every client it holds, so the center closes about 5 ms after it
//! shows (notify-13). This guard redirects the DLL's import: the call goes through unchanged, and
//! only when user32 answers null with ERROR_ACCESS_DENIED it is made again in the desktop band,
//! each step logged.

use std::{
    ffi::c_void,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

use libloading::Library;

const MODULE: &str = "Windows.UI.Immersive.dll";
const IAT: &str = "__imp_CreateWindowInBand";
const DESKTOP_BAND: u32 = 1;
const ERROR_ACCESS_DENIED: u32 = 5;

type CreateWindowInBand = unsafe extern "system" fn(
    u32,
    *const u16,
    *const u16,
    u32,
    i32,
    i32,
    i32,
    i32,
    *mut c_void,
    *mut c_void,
    *mut c_void,
    *mut c_void,
    u32,
) -> *mut c_void;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLastError() -> u32;
    fn SetLastError(code: u32);
}

static ORIGINAL: AtomicUsize = AtomicUsize::new(0);

unsafe extern "system" fn create_window_in_band(
    ex_style: u32,
    class: *const u16,
    name: *const u16,
    style: u32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    parent: *mut c_void,
    menu: *mut c_void,
    instance: *mut c_void,
    parameter: *mut c_void,
    band: u32,
) -> *mut c_void {
    // SAFETY: install() stores the user32 export before it replaces the slot.
    let original: CreateWindowInBand = unsafe { std::mem::transmute(ORIGINAL.load(Ordering::Acquire)) };
    // SAFETY: same signature as user32!CreateWindowInBand, arguments passed through.
    let call = |band: u32| unsafe {
        original(ex_style, class, name, style, x, y, width, height, parent, menu, instance, parameter, band)
    };
    let hwnd = call(band);
    if !hwnd.is_null() {
        return hwnd;
    }
    // SAFETY: reads the error the failed call just set.
    let error = unsafe { GetLastError() };
    if error != ERROR_ACCESS_DENIED || band == DESKTOP_BAND {
        hlog!("phase=lightdismiss-window band={band} result=null last_error={error}");
        // SAFETY: hands the caller the error of its own call.
        unsafe { SetLastError(error) };
        return hwnd;
    }
    let retry = call(DESKTOP_BAND);
    if retry.is_null() {
        // SAFETY: reads the error of the retry.
        let retry_error = unsafe { GetLastError() };
        hlog!("phase=lightdismiss-window band={band} denied, retry band={DESKTOP_BAND} result=null last_error={retry_error}");
    } else {
        hlog!("phase=lightdismiss-window band={band} ex_style={ex_style:#x} denied, retry band={DESKTOP_BAND} hwnd={retry:?}");
    }
    retry
}

pub struct LightDismissBandRedirect {
    _module: Library,
    _user32: Library,
    slot: *mut usize,
    original_slot_value: usize,
}

impl LightDismissBandRedirect {
    pub fn install() -> Result<Self, String> {
        let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let system32 = PathBuf::from(system_root).join("System32");
        // SAFETY: absolute paths of installed Windows DLLs, kept loaded by the guard.
        let module = unsafe { Library::new(system32.join(MODULE)) }.map_err(|error| format!("load-{MODULE}: {error}"))?;
        let user32 =
            unsafe { Library::new(system32.join("user32.dll")) }.map_err(|error| format!("load-user32: {error}"))?;
        // SAFETY: user32 stays loaded in this guard.
        let original = unsafe {
            *user32
                .get::<CreateWindowInBand>(b"CreateWindowInBand\0")
                .map_err(|error| format!("resolve-CreateWindowInBand: {error}"))? as usize
        };
        let slot = crate::symbols::address(MODULE, IAT)? as *mut usize;
        // SAFETY: the slot is in the loaded module's import table.
        let current = unsafe { slot.read_volatile() };
        if current != original {
            return Err(format!("IAT target differs from user32 export: current={current:#x} export={original:#x}"));
        }
        ORIGINAL.store(original, Ordering::Release);
        // SAFETY: validated slot of a module this guard keeps loaded.
        unsafe { crate::appframe_band::write_slot(slot, create_window_in_band as *const () as usize) }?;
        Ok(Self { _module: module, _user32: user32, slot, original_slot_value: current })
    }
}

impl Drop for LightDismissBandRedirect {
    fn drop(&mut self) {
        // SAFETY: the guard still owns both DLLs and the validated slot.
        if let Err(error) = unsafe { crate::appframe_band::write_slot(self.slot, self.original_slot_value) } {
            hlog!("phase=restore-lightdismiss-import error={error}");
        }
    }
}
