//! explorust: ApplicationFrame.dll's frame window when twinui creates it in this process.
//!
//! Win+V and Win+. go through twinui's TouchKeyboardExperienceManager, which asks
//! ApplicationFrame.dll (loaded here) for TextInputHost's frame. `CApplicationFrame::
//! RuntimeClassInitialize` creates `ApplicationFrameWindow` with `CreateWindowInBandEx(..., band 1,
//! type flags 3)` and returns E_ACCESSDENIED (appframe.cpp 1379) when the window is null. Under
//! explorust that call is denied. This guard redirects ApplicationFrame.dll's import: the call goes
//! through unchanged, and only when user32 answers null with ERROR_ACCESS_DENIED it is made again
//! with type flags 0, each step logged.

use std::{
    ffi::c_void,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

use libloading::Library;

const MODULE: &str = "ApplicationFrame.dll";
const IAT: &str = "__imp_CreateWindowInBandEx";
const DELAY_THUNK: &str = "__imp_load_CreateWindowInBandEx";
const PAGE_READWRITE: u32 = 4;

type CreateWindowInBandEx = unsafe extern "system" fn(
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
    u32,
) -> *mut c_void;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn VirtualProtect(address: *mut c_void, size: usize, protection: u32, old: *mut u32) -> i32;
    fn GetLastError() -> u32;
    fn SetLastError(code: u32);
}

static ORIGINAL: AtomicUsize = AtomicUsize::new(0);

unsafe extern "system" fn create_window_in_band_ex(
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
    type_flags: u32,
) -> *mut c_void {
    // SAFETY: install() stores the user32 export before it replaces the slot.
    let original: CreateWindowInBandEx = unsafe { std::mem::transmute(ORIGINAL.load(Ordering::Acquire)) };
    let call = |flags: u32| unsafe {
        original(
            ex_style, class, name, style, x, y, width, height, parent, menu, instance, parameter, band, flags,
        )
    };
    let hwnd = call(type_flags);
    if !hwnd.is_null() {
        hlog!("phase=appframe-window band={band} flags={type_flags} hwnd={hwnd:?}");
        return hwnd;
    }
    let error = unsafe { GetLastError() };
    hlog!("phase=appframe-window band={band} flags={type_flags} result=null last_error={error}");
    if error != 5 || type_flags == 0 {
        unsafe { SetLastError(error) };
        return hwnd;
    }
    let retry = call(0);
    let retry_error = unsafe { GetLastError() };
    hlog!("phase=appframe-window band={band} flags=0 retry hwnd={retry:?} last_error={retry_error}");
    retry
}

pub struct AppFrameBandRedirect {
    _module: Library,
    _user32: Library,
    slot: *mut usize,
    original_slot_value: usize,
}

impl AppFrameBandRedirect {
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
                .get::<CreateWindowInBandEx>(b"CreateWindowInBandEx\0")
                .map_err(|error| format!("resolve-CreateWindowInBandEx: {error}"))? as usize
        };
        let slot = crate::symbols::address(MODULE, IAT)? as *mut usize;
        // SAFETY: the slot is in the loaded module's import table.
        let current = unsafe { slot.read_volatile() };
        let thunk = crate::symbols::address(MODULE, DELAY_THUNK)?;
        if current != original && current != thunk {
            return Err(format!(
                "IAT target differs from user32 export and delay thunk: current={current:#x} export={original:#x} thunk={thunk:#x}"
            ));
        }
        ORIGINAL.store(original, Ordering::Release);
        // SAFETY: validated slot of a module this guard keeps loaded.
        unsafe { write_slot(slot, create_window_in_band_ex as *const () as usize) }?;
        Ok(Self { _module: module, _user32: user32, slot, original_slot_value: current })
    }
}

impl Drop for AppFrameBandRedirect {
    fn drop(&mut self) {
        // SAFETY: the guard still owns both DLLs and the validated slot.
        if let Err(error) = unsafe { write_slot(self.slot, self.original_slot_value) } {
            hlog!("phase=restore-appframe-import error={error}");
        }
    }
}

unsafe fn write_slot(slot: *mut usize, value: usize) -> Result<(), String> {
    let mut old = 0_u32;
    // SAFETY: the caller passes a validated IAT slot of this process.
    if unsafe { VirtualProtect(slot.cast(), size_of::<usize>(), PAGE_READWRITE, &mut old) } == 0 {
        return Err(format!("VirtualProtect-write failed: {}", std::io::Error::last_os_error()));
    }
    // SAFETY: writable for this store.
    unsafe { slot.write_volatile(value) };
    let mut ignored = 0_u32;
    // SAFETY: restores the page's protection.
    if unsafe { VirtualProtect(slot.cast(), size_of::<usize>(), old, &mut ignored) } == 0 {
        return Err(format!("VirtualProtect-restore failed: {}", std::io::Error::last_os_error()));
    }
    Ok(())
}
