//! switcher-dev.exe: runs explorust's Alt+Tab beside the running shell, whose hotkey hook hands it
//! Alt+Tab, Alt release and Esc through the bus.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
fn main() {
    use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
    // SAFETY: process-wide setting made once, before any window exists.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    switcher::run();
}

#[cfg(not(target_os = "windows"))]
fn main() {}
