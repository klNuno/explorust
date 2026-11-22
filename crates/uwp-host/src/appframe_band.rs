//! explorust: ApplicationFrame.dll's frame window when twinui creates it in this process.
//!
//! Win+V and Win+. go through twinui's TouchKeyboardExperienceManager, which asks
//! ApplicationFrame.dll (loaded here) for TextInputHost's frame. `CApplicationFrame::
//! RuntimeClassInitialize` creates `ApplicationFrameWindow` with `CreateWindowInBandEx(..., band 1,
//! type flags 3)` and returns E_ACCESSDENIED (appframe.cpp 1379) when the window is null. Under
//! explorust that call is denied. This guard redirects ApplicationFrame.dll's import: the call goes
//! through unchanged, and only when user32 answers null with ERROR_ACCESS_DENIED it is made again
//! with type flags 0. The retry can leave an empty frame onscreen, so keep it cloaked until
//! its CoreWindow attaches.

use std::{
    ffi::c_void,
    path::PathBuf,
    sync::{Mutex, atomic::{AtomicUsize, Ordering}},
    time::{Duration, Instant},
};

use libloading::Library;
use windows::{
    Win32::{
        Foundation::{HANDLE, HWND, LPARAM, RECT},
        Graphics::Dwm::{DWMWA_CLOAK, DwmSetWindowAttribute},
        UI::WindowsAndMessaging::{
            EnumChildWindows, GA_ROOT, GWL_STYLE, GetAncestor, GetClassNameW, GetClientRect,
            GetPropW, GetWindowLongW, IsWindowVisible, RemovePropW, SetPropW, WS_CHILD,
            WS_EX_NOREDIRECTIONBITMAP, WS_VISIBLE,
        },
    },
    core::{BOOL, w},
};

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
static UNATTACHED: Mutex<Vec<PendingFrame>> = Mutex::new(Vec::new());
const CLOAK_PROPERTY: windows::core::PCWSTR = w!("explorust.unattached-appframe");
const ATTACHMENT_GRACE: Duration = Duration::from_secs(30);

struct PendingFrame {
    hwnd: usize,
    since: Instant,
    reported: bool,
}

fn class_is(hwnd: HWND, class: &str) -> bool {
    let mut name = [0u16; 64];
    // SAFETY: the output buffer is writable for the call.
    let length = unsafe { GetClassNameW(hwnd, &mut name) }.max(0) as usize;
    name[..length].iter().copied().eq(class.encode_utf16())
}

fn core_child(frame: HWND, require_layout: bool) -> bool {
    struct Match {
        require_layout: bool,
        found: bool,
    }

    unsafe extern "system" fn visit(window: HWND, result: LPARAM) -> BOOL {
        // SAFETY: EnumChildWindows invokes this callback synchronously.
        let result = unsafe { &mut *(result.0 as *mut Match) };
        if !class_is(window, "Windows.UI.Core.CoreWindow") {
            return BOOL(1);
        }
        let mut rect = RECT::default();
        // SAFETY: the descendant is ready when it has a layout and wants to show. Check its own
        // style because the frame can still be hidden while waiting for that attachment.
        if !result.require_layout
            || unsafe { GetWindowLongW(window, GWL_STYLE) } as u32 & WS_VISIBLE.0 != 0
                && unsafe { GetClientRect(window, &mut rect) }.is_ok()
                && rect.right > rect.left && rect.bottom > rect.top
        {
            result.found = true;
            return BOOL(0);
        }
        BOOL(1)
    }

    let mut result = Match { require_layout, found: false };
    // SAFETY: the output lives until the synchronous enumeration returns.
    let _ = unsafe { EnumChildWindows(Some(frame), Some(visit), LPARAM(&mut result as *mut Match as isize)) };
    result.found
}

fn empty_retry_frame(hwnd: HWND, ex_style: u32, style: u32, parent: *mut c_void, band: u32, flags: u32) -> bool {
    band == 1 && flags == 3 && parent.is_null() && style & WS_CHILD.0 == 0
        && ex_style & WS_EX_NOREDIRECTIONBITMAP.0 != 0
        // SAFETY: queried immediately after the creation call returned this HWND.
        && unsafe { GetAncestor(hwnd, GA_ROOT) } == hwnd
        && class_is(hwnd, "ApplicationFrameWindow")
        && !core_child(hwnd, false)
}

fn cloak(hwnd: HWND, value: bool) -> windows::core::Result<()> {
    let enabled = BOOL::from(value);
    // SAFETY: the attribute uses a Win32 BOOL and the caller supplies a window handle.
    unsafe { DwmSetWindowAttribute(hwnd, DWMWA_CLOAK, (&enabled as *const BOOL).cast(), size_of::<BOOL>() as u32) }
}

fn cloak_until_attached(hwnd: HWND) {
    // SAFETY: this is the frame we just created. The property prevents an HWND recycled
    // between ticks from being mistaken for one we cloaked.
    if unsafe { SetPropW(hwnd, CLOAK_PROPERTY, Some(HANDLE(1usize as *mut c_void))) }.is_err() {
        return;
    }
    if let Err(error) = cloak(hwnd, true) {
        let _ = unsafe { RemovePropW(hwnd, CLOAK_PROPERTY) };
        hlog!("phase=appframe-window hwnd={:?} cloak-error={error}", hwnd.0);
        return;
    }
    UNATTACHED.lock().unwrap_or_else(|e| e.into_inner()).push(PendingFrame {
        hwnd: hwnd.0 as usize,
        since: Instant::now(),
        reported: false,
    });
    hlog!("phase=appframe-window hwnd={:?} result=cloaked-until-attached", hwnd.0);
}

fn release_cloaks(only_ready: bool) {
    let mut windows = UNATTACHED.lock().unwrap_or_else(|e| e.into_inner());
    windows.retain_mut(|pending| {
        let hwnd = HWND(pending.hwnd as *mut c_void);
        // SAFETY: the property is on retry frames only and disappears when they are destroyed.
        if unsafe { GetPropW(hwnd, CLOAK_PROPERTY) }.is_invalid() {
            return false;
        }
        if only_ready && !core_child(hwnd, true) {
            if pending.since.elapsed() < ATTACHMENT_GRACE {
                return true;
            }
            if !pending.reported {
                pending.reported = true;
                hlog!("phase=appframe-window hwnd={:?} result=attachment-timeout", hwnd.0);
            }
            // Keep a visible empty frame suppressed. The same HWND can be used again later.
            // A hidden window can safely lose our cloak and the host can show it normally.
            if unsafe { IsWindowVisible(hwnd) }.as_bool() {
                return true;
            }
        }
        if cloak(hwnd, false).is_err() {
            return true;
        }
        let _ = unsafe { RemovePropW(hwnd, CLOAK_PROPERTY) };
        hlog!("phase=appframe-window hwnd={:?} result=cloak-released", hwnd.0);
        false
    });
}

pub(crate) fn show_attached_frames() {
    release_cloaks(true);
}

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
    if !retry.is_null() && empty_retry_frame(HWND(retry), ex_style, style, parent, band, type_flags) {
        cloak_until_attached(HWND(retry));
    }
    // SAFETY: restore the result from the call that produced our return value.
    unsafe { SetLastError(retry_error) };
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
        release_cloaks(false);
    }
}

pub(crate) unsafe fn write_slot(slot: *mut usize, value: usize) -> Result<(), String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::{LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, WINDOW_EX_STYLE,
        WNDCLASSW, WS_POPUP,
    };
    use windows::core::PCWSTR;

    unsafe extern "system" fn fixture_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
    }

    #[test]
    fn a_retried_frame_accepts_nested_core_layout_on_reuse() {
        let frame_class = w!("ApplicationFrameWindow");
        let core_class = w!("Windows.UI.Core.CoreWindow");
        let helper_class = w!("explorust.test.appframe-layout");
        let module = unsafe { GetModuleHandleW(None).unwrap().into() };
        let make = |class: PCWSTR, style, parent| unsafe {
            CreateWindowExW(WINDOW_EX_STYLE(0), class, None, style, 0, 0, 80, 80, parent, None, Some(module), None).unwrap()
        };
        for name in [frame_class, core_class, helper_class] {
            let wc = WNDCLASSW { lpfnWndProc: Some(fixture_proc), hInstance: module, lpszClassName: name, ..Default::default() };
            assert_ne!(unsafe { RegisterClassW(&wc) }, 0);
        }
        let frame = make(frame_class, WS_POPUP, None);
        let retry_target = |hwnd, style, parent| empty_retry_frame(hwnd, WS_EX_NOREDIRECTIONBITMAP.0, style, parent, 1, 3);
        let helper = make(helper_class, WS_CHILD | WS_VISIBLE, Some(frame));
        assert!(!retry_target(helper, WS_CHILD.0, frame.0));
        assert!(retry_target(frame, WS_POPUP.0, std::ptr::null_mut()));
        assert!(!core_child(frame, true));
        for _ in 0..2 {
            let core = make(core_class, WS_CHILD | WS_VISIBLE, Some(helper));
            assert!(core_child(frame, false), "they must enumerate the nested CoreWindow");
            let mut rect = RECT::default();
            unsafe { GetClientRect(core, &mut rect).unwrap() };
            let style = unsafe { GetWindowLongW(core, GWL_STYLE) } as u32;
            assert!(core_child(frame, true), "nested client area {rect:?}, style {style:#x}");
            assert!(!retry_target(frame, WS_POPUP.0, std::ptr::null_mut()));
            unsafe { DestroyWindow(core).unwrap() };
            assert!(!core_child(frame, true));
        }
        unsafe { DestroyWindow(frame).unwrap() };
    }
}
