//! Explicit session switch for the setup and Start menu shortcut.

#![windows_subsystem = "windows"]

mod switch;

fn main() {
    let code = match std::env::args().nth(1).as_deref() {
        None | Some("--switch") => switch::run(),
        Some("--switch-worker") => switch::run_worker(),
        Some("--watch-switch") => switch::watcher::run(std::env::args().skip(2)),
        Some("--close-folder") => switch::folder_close::run(std::env::args().skip(2)),
        _ => 2,
    };
    std::process::exit(code as i32);
}
