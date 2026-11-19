//! The taskbar on its own, beside a running explorust: per-monitor DPI v2, then `taskbar::run()`.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

fn main() {
    #[cfg(target_os = "windows")]
    // SAFETY: sets this process's DPI awareness before any window exists.
    unsafe {
        use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    // Shut down, restart, sleep and sign out stay dry unless asked: a test never powers g1 off.
    taskbar::set_dry_power(!std::env::args().any(|a| a == "--live-power"));
    taskbar::run();
}
