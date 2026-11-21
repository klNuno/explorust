//! The shell's parts, each on a thread of its own: taskbar, desktop, Start, switcher, notification
//! center and File Explorer. A panic in one is logged and that part starts again once; a second
//! panic within 5 minutes, or a part window that stops answering (`watchdog`), is a problem: the
//! shell exits and the guard hands the session to explorer.exe (`problem`). The injector only
//! styles other apps, so its crashes stay a log line (at most 3 restarts in 5 minutes).

use crate::{log, problem};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Set by `shutdown_all`: a part that returns after it was asked to close is not a problem.
static STOPPING: AtomicBool = AtomicBool::new(false);

const PARTS: [(bus::Owner, fn()); 6] = [
    (bus::Owner::Taskbar, taskbar::run),
    (bus::Owner::Desktop, desktop::run),
    (bus::Owner::Start, start::run),
    (bus::Owner::Switcher, switcher::run),
    (bus::Owner::Notify, notify::run),
    (bus::Owner::Files, files::run),
];

/// Crashes within `CRASH_WINDOW` that stop a part: 2 for the parts (a problem), 3 for the injector.
const MAX_CRASHES: usize = 2;
const INJECT_MAX_CRASHES: usize = 3;
const CRASH_WINDOW: Duration = Duration::from_secs(300);

/// Logs every panic with its place, whatever thread it happens on.
pub fn log_panics() {
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current().name().unwrap_or("?").to_string();
        log::line(&format!("panic in {thread}: {info}"));
    }));
}

/// Parts left out of this run: one name per line in `%LOCALAPPDATA%\explorust\parts-off.txt`, so
/// a part's dev exe can stand in for it in a test, or a broken part can be set aside.
fn parts_off() -> Vec<String> {
    let base = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    std::fs::read_to_string(base.join("explorust").join("parts-off.txt"))
        .map(|s| s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default()
}

pub fn spawn_all() {
    let off = parts_off();
    for (owner, run) in PARTS {
        let name = owner.name();
        if off.iter().any(|o| o == name) {
            log::line(&format!("{name}: off (parts-off.txt)"));
            continue;
        }
        let spawned = std::thread::Builder::new().name(name.into()).spawn(move || supervise(name, run, true));
        if let Err(e) = spawned {
            problem::report(format!("the {name} part did not start: {e}"));
        }
    }
    // The injector has no bus owner: it answers no hotkey, only window shows of other apps.
    if off.iter().any(|o| o == "inject") {
        log::line("inject: off (parts-off.txt)");
        // A Settings process styled by an earlier run still holds the TAP: it undoes that.
        let _ = std::thread::Builder::new().name("inject-undo".into()).spawn(explorust_inject::undo_settings);
    } else if let Err(e) = std::thread::Builder::new().name("inject".into()).spawn(|| supervise("inject", explorust_inject::run, false)) {
        log::line(&format!("inject: thread not started: {e}"));
    }
}

/// `critical`: a part whose repeated crash is a problem for the session.
fn supervise(name: &str, run: fn(), critical: bool) {
    let mut crashes: Vec<Instant> = Vec::new();
    let max = if critical { MAX_CRASHES } else { INJECT_MAX_CRASHES };
    loop {
        match std::panic::catch_unwind(run) {
            Ok(()) => {
                if critical && !STOPPING.load(Ordering::SeqCst) {
                    problem::report(format!("the {name} part stopped on its own"));
                }
                return;
            }
            Err(_) => {
                let now = Instant::now();
                crashes.retain(|t| now.duration_since(*t) < CRASH_WINDOW);
                crashes.push(now);
                if crashes.len() >= max {
                    log::line(&format!("{name}: {max} crashes in 5 minutes, left stopped"));
                    if critical {
                        problem::report(format!("the {name} part crashed {max} times in 5 minutes"));
                    }
                    return;
                }
                log::line(&format!("{name}: crashed, starting it again"));
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

/// The running parts (not off in parts-off.txt) and whether one of their command windows exists in
/// this process: a stand-in dev exe of the same part in the session is not ours to probe.
fn enabled() -> Vec<(usize, bus::Owner)> {
    let off = parts_off();
    PARTS.iter().enumerate().filter(|(_, (o, _))| !off.iter().any(|n| n == o.name())).map(|(i, (o, _))| (i, *o)).collect()
}

fn own_window(owner: bus::Owner) -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    let h = bus::target(owner)?;
    let mut pid = 0u32;
    // SAFETY: a lookup on a window that may be gone (pid stays 0).
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    (pid == std::process::id()).then_some(h)
}

/// Waits until every running part has its command window, at most `limit`. A part that did not
/// come up is a problem before the shell says it is ready, so the guard never trusts a half shell.
pub fn wait_ready(limit: Duration) -> Result<(), String> {
    let t0 = Instant::now();
    let parts = enabled();
    loop {
        let missing: Vec<&str> = parts.iter().filter(|(_, o)| own_window(*o).is_none()).map(|(_, o)| o.name()).collect();
        if missing.is_empty() {
            log::line(&format!("parts ready in {} ms", t0.elapsed().as_millis()));
            return Ok(());
        }
        if t0.elapsed() >= limit {
            return Err(format!("the {} part did not start within {} s", missing.join(", "), limit.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Every `problem::PROBE`, each running part's command window must answer within 2 s; one silent
/// for `problem::HUNG_FOR` is a problem, and so is a part whose window is gone that long. Parts off
/// in parts-off.txt are skipped.
pub fn spawn_watchdog() {
    let _ = std::thread::Builder::new().name("watchdog".into()).spawn(|| {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{SMTO_ABORTIFHUNG, SMTO_BLOCK, SendMessageTimeoutW, WM_NULL};
        let parts = enabled();
        let mut hung = problem::Hung::new(PARTS.len());
        let t0 = Instant::now();
        loop {
            std::thread::sleep(problem::PROBE);
            if STOPPING.load(Ordering::SeqCst) {
                return;
            }
            for (i, owner) in &parts {
                let answered = own_window(*owner).is_some_and(|h| {
                    let mut out = 0usize;
                    // SAFETY: a bounded send to a window that may be gone (then it fails).
                    unsafe { SendMessageTimeoutW(h, WM_NULL, WPARAM(0), LPARAM(0), SMTO_ABORTIFHUNG | SMTO_BLOCK, 2000, Some(&mut out)) }.0 != 0
                });
                if hung.probe(*i, answered, t0.elapsed()) {
                    problem::report(format!("the {} part has not answered for {} s", owner.name(), problem::HUNG_FOR.as_secs()));
                    return;
                }
            }
        }
    });
}

/// Asks every part to close its windows, and gives them a moment to do it.
pub fn shutdown_all() {
    STOPPING.store(true, Ordering::SeqCst);
    explorust_inject::shutdown();
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
