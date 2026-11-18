//! desktop-dev.exe: the desktop part alone, beside the running shell (it gets Win+D through the
//! bus). Sets per-monitor DPI awareness v2 like the shell, then runs the part until Shutdown.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
fn main() {
    use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
    // SAFETY: process-wide DPI mode, set before any window exists.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    desktop::run();
}

#[cfg(not(target_os = "windows"))]
fn main() {}
