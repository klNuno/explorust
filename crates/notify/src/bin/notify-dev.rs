//! notify-dev.exe: runs explorust's toasts beside the running shell. `--post center|clipboard|emoji
//! [arg]` sends that bus command to a running notify instead (test sessions where the shell's
//! hotkey hook does not see the keys). `--shell-ready` publishes the shell-ready signal twinui's
//! notification center waits for, when nobody did in this session.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--wnf-scan") {
        let out = args.get(i + 1).map(String::as_str).unwrap_or(r"C:\explorust\wnf-scan.txt");
        let n = |k: usize, d: u64| args.get(i + k).and_then(|a| a.parse().ok()).unwrap_or(d);
        notify::dev_wnf_scan(std::path::Path::new(out), n(2, 6), n(3, 10));
        return;
    }
    if args.iter().any(|a| a == "--shell-ready") {
        notify::dev_shell_ready();
        return;
    }
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
