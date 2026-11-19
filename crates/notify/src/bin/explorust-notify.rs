//! explorust-notify.exe: the toast listener's wake-up, the only explorust exe with package
//! identity (the sparse package of `crates/notify/package`, installed with C:\explorust as its
//! external location). `UserNotificationListener.NotificationChanged` refuses a process without
//! identity, so the notify part (inside explorust.exe, which keeps no identity: its children must
//! not inherit one) activates this helper through the package's AUMID and waits; the helper subscribes to the event and posts
//! `MSG_WAKE` to the part's window on every change. It exits with its parent.
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
