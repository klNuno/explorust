//! start-dev.exe: runs explorust's Start beside the running shell, which hands it the Win key through
//! the bus. Power actions are logged, not run, unless `--live-power` is given. `--post search` or
//! `--post toggle` sends that bus command to a running Start instead.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
fn main() {
    // `start-dev --post search|toggle`: posts the bus command to the running Start and exits, for
    // test sessions where the shell's hotkey hook does not see injected keys.
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--post") {
        let cmd = match args.get(i + 1).map(String::as_str) {
            Some("toggle") => bus::Command::StartToggle,
            _ => bus::Command::Search,
        };
        std::process::exit(if bus::post(cmd, 0) { 0 } else { 1 });
    }
    use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
    // SAFETY: process-wide setting made once, before any window exists.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    start::set_dry_power(!std::env::args().any(|a| a == "--live-power"));
    start::run();
}

#[cfg(not(target_os = "windows"))]
fn main() {}
