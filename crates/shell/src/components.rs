//! The shell's parts, each on a thread of its own: taskbar, desktop, Start, switcher, notification
//! center and File Explorer. A panic in one is logged and that part starts again (at most 3 times in
//! 5 minutes); the others and the UWP host keep running.

use crate::log;
use std::time::{Duration, Instant};

const PARTS: [(bus::Owner, fn()); 6] = [
    (bus::Owner::Taskbar, taskbar::run),
    (bus::Owner::Desktop, desktop::run),
    (bus::Owner::Start, start::run),
    (bus::Owner::Switcher, switcher::run),
    (bus::Owner::Notify, notify::run),
    (bus::Owner::Files, files::run),
];

const MAX_CRASHES: usize = 3;
const CRASH_WINDOW: Duration = Duration::from_secs(300);

/// Logs every panic with its place, whatever thread it happens on.
pub fn log_panics() {
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current().name().unwrap_or("?").to_string();
        log::line(&format!("panic in {thread}: {info}"));
    }));
}

pub fn spawn_all() {
    for (owner, run) in PARTS {
        let name = owner.name();
        let spawned = std::thread::Builder::new().name(name.into()).spawn(move || supervise(name, run));
        if let Err(e) = spawned {
            log::line(&format!("{name}: thread not started: {e}"));
        }
    }
}

fn supervise(name: &str, run: fn()) {
    let mut crashes: Vec<Instant> = Vec::new();
    loop {
        match std::panic::catch_unwind(run) {
            Ok(()) => return,
            Err(_) => {
                let now = Instant::now();
                crashes.retain(|t| now.duration_since(*t) < CRASH_WINDOW);
                crashes.push(now);
                if crashes.len() >= MAX_CRASHES {
                    log::line(&format!("{name}: {MAX_CRASHES} crashes in 5 minutes, left stopped"));
                    return;
                }
                log::line(&format!("{name}: crashed, starting it again"));
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

/// Asks every part to close its windows, and gives them a moment to do it.
pub fn shutdown_all() {
    let mut asked = 0;
    for (owner, _) in PARTS {
        if bus::post_to(owner, bus::Command::Shutdown, 0) {
            asked += 1;
        }
    }
    if asked > 0 {
        std::thread::sleep(Duration::from_millis(500));
    }
}
