#![windows_subsystem = "windows"]
//! `explorust-taskman.exe`: one window; a second start brings the first to the front.

#[cfg(target_os = "windows")]
fn main() {
    use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    std::process::exit(taskman::run());
}

#[cfg(not(target_os = "windows"))]
fn main() {}
