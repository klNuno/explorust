//! notify-dev.exe: runs explorust's toasts beside the running shell. `--post center|clipboard|emoji
//! [arg]` sends that bus command to a running notify instead (test sessions where the shell's
//! hotkey hook does not see the keys).

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--post") {
        let cmd = match args.get(i + 1).map(String::as_str) {
            Some("clipboard") => bus::Command::ClipboardHistory,
            Some("emoji") => bus::Command::EmojiPanel,
            Some("quick") => bus::Command::QuickSettings,
            _ => bus::Command::NotificationCenter,
        };
        let arg = args.get(i + 2).and_then(|a| a.parse().ok()).unwrap_or(0);
        std::process::exit(if bus::post(cmd, arg) { 0 } else { 1 });
    }
    use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
    // SAFETY: process-wide setting made once, before any window exists.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    notify::run();
}

#[cfg(not(target_os = "windows"))]
fn main() {}
