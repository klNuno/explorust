#![windows_subsystem = "windows"]
//! `explorust-viewer.exe <file>`; `--register` and `--unregister` add or remove the handler
//! (HKCU only), `--defaults` opens Settings > Default apps on it.

#[cfg(target_os = "windows")]
fn main() {
    use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(viewer::run(&args));
}

#[cfg(not(target_os = "windows"))]
fn main() {}
