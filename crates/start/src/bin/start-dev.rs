//! start-dev.exe: runs explorust's Start beside the running shell, which hands it the Win key through
//! the bus. Power actions are logged, not run, unless `--live-power` is given.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
fn main() {
    use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
    // SAFETY: process-wide setting made once, before any window exists.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    start::set_dry_power(!std::env::args().any(|a| a == "--live-power"));
    start::run();
}

#[cfg(not(target_os = "windows"))]
fn main() {}
