//! files-dev.exe: runs explorust's File Explorer beside the running shell, which hands it Win+E
//! (`FilesOpen`) through the bus; `explorust-open.exe <path>` opens a folder in it.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
fn main() {
    use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
    // SAFETY: process-wide settings made once, before any window exists.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    if std::env::args().any(|a| a == "--post") {
        // Test aid: send FilesOpen to the running part, as the shell's Win+E hook does, and exit.
        std::process::exit(if bus::post(bus::Command::FilesOpen, 0) { 0 } else { 1 });
    }
    if std::env::args().any(|a| a == "--quit") {
        // Test aid: close the running part's windows (they leave the shell's window list) and end it.
        std::process::exit(if bus::post_to(bus::Owner::Files, bus::Command::Shutdown, 0) { 0 } else { 1 });
    }
    files::dark_mode_for_process();
    if std::env::args().any(|a| a == "--open") {
        // Test aid: open a window once the part listens, as Win+E would.
        std::thread::spawn(|| {
            for _ in 0..100 {
                std::thread::sleep(std::time::Duration::from_millis(100));
                if bus::post(bus::Command::FilesOpen, 0) {
                    return;
                }
            }
        });
    }
    files::run();
}

#[cfg(not(target_os = "windows"))]
fn main() {}
