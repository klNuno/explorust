//! The shell activates the notification listener through its AUMID. This process subscribes to
//! Windows notification changes, wakes the shell's toast UI, and exits when the shell ends.
//! Usage: explorust-notify.exe --parent <pid> --hwnd <command window>; `--toast <text>` (dev)
//! sends a toast as the package.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--toast") {
        std::process::exit(notify::helper_toast(args.get(i + 1).map(String::as_str).unwrap_or("toast")));
    }
    let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).and_then(|v| v.parse::<isize>().ok());
    let (Some(parent), Some(hwnd)) = (arg("--parent"), arg("--hwnd")) else { return };
    std::process::exit(notify::helper(parent as u32, hwnd));
}

#[cfg(not(target_os = "windows"))]
fn main() {}
