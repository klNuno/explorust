//! inject-dev.exe: the injector's loader on its own, for tests beside a shell that does not run
//! one (an older explorust). Needs `explorust_inject.dll` next to it; the kill switch applies.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
fn main() {
    use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
    // SAFETY: process-wide setting made before any window exists.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    explorust_inject::run();
}

#[cfg(not(target_os = "windows"))]
fn main() {}
