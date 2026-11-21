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

/// Notification Center initializes the immersive shell's readiness signal, so start it only once
/// the host publishes its services. The other parts can create their UI immediately.
pub fn spawn_parts(host_ready: bool) {
    let off = parts_off();
    for (owner, run) in PARTS {
        if (owner == bus::Owner::Notify) != host_ready {
            continue;
        }
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
    if host_ready {
        return;
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
pub fn enabled(immersive: bool) -> Vec<(usize, bus::Owner)> {
    let off = parts_off();
    PARTS.iter().enumerate()
        .filter(|(_, (owner, _))| (*owner == bus::Owner::Notify) == immersive && !off.iter().any(|name| name == owner.name()))
        .map(|(i, (owner, _))| (i, *owner)).collect()
}

fn own_window(owner: bus::Owner) -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::UI::WindowsAndMessaging::HWND_MESSAGE;
    let class = windows::core::HSTRING::from(bus::class_of(owner));
    let pid = std::process::id();
    find_window_for_pid(Some(HWND_MESSAGE), &class, pid).or_else(|| find_window_for_pid(None, &class, pid))
}

fn find_window_for_pid(
    parent: Option<windows::Win32::Foundation::HWND>,
    class: &windows::core::HSTRING,
    expected_pid: u32,
) -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, GetWindowThreadProcessId};
    let mut after = None;
    // SAFETY: lookups on live HWNDs; if a window disappears the probe can retry on the next tick.
    unsafe {
        while let Ok(hwnd) = FindWindowExW(parent, after, class, None) {
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == expected_pid {
                return Some(hwnd);
            }
            after = Some(hwnd);
        }
    }
    None
}

pub fn ready(owner: bus::Owner) -> bool {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        IsWindowVisible, SMTO_ABORTIFHUNG, SMTO_BLOCK, SendMessageTimeoutW, WM_NULL,
    };
    let visible_class = match owner {
        bus::Owner::Taskbar => Some("Shell_TrayWnd"),
        bus::Owner::Desktop => Some("Progman"),
        _ => None,
    };
    let window = match visible_class {
        Some(class) => find_window_for_pid(None, &windows::core::HSTRING::from(class), std::process::id())
            .filter(|&window| unsafe { IsWindowVisible(window) }.as_bool()),
        None => own_window(owner),
    };
    // SAFETY: a bounded send of a message every window accepts.
    window.is_some_and(|window| unsafe {
        SendMessageTimeoutW(window, WM_NULL, WPARAM(0), LPARAM(0), SMTO_ABORTIFHUNG | SMTO_BLOCK, 50, None)
    }.0 != 0)
}

#[cfg(test)]
mod tests {
    use super::find_window_for_pid;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, FindWindowExW, GetWindowThreadProcessId,
        HWND_MESSAGE, RegisterClassW, WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSW,
    };
    use windows::core::{HSTRING, PCWSTR};

    unsafe extern "system" fn fixture_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
    }

    #[test]
    fn finds_the_supervised_pid_after_a_same_class_window() {
        const CLASS_ENV: &str = "EXPLORUST_TEST_COMMAND_WINDOW_CLASS";
        let inherited = std::env::var(CLASS_ENV).ok();
        let class = HSTRING::from(inherited.clone().unwrap_or_else(|| format!("explorust.test.command.{}", std::process::id())));
        let window = unsafe {
            let module = GetModuleHandleW(None).unwrap().into();
            let wc = WNDCLASSW { lpfnWndProc: Some(fixture_proc), hInstance: module, lpszClassName: PCWSTR(class.as_ptr()), ..Default::default() };
            assert_ne!(RegisterClassW(&wc), 0);
            CreateWindowExW(WINDOW_EX_STYLE(0), &class, None, WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(module), None).unwrap()
        };
        if inherited.is_some() {
            std::thread::sleep(Duration::from_secs(10));
            return;
        }
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "components::tests::finds_the_supervised_pid_after_a_same_class_window"])
            .env(CLASS_ENV, class.to_string_lossy())
            .stdout(Stdio::null()).spawn().unwrap();
        let mut checked = None;
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(5) {
            let first = unsafe { FindWindowExW(Some(HWND_MESSAGE), None, &class, None) }.unwrap();
            if let Ok(second) = unsafe { FindWindowExW(Some(HWND_MESSAGE), Some(first), &class, None) } {
                let mut pid = 0;
                unsafe { GetWindowThreadProcessId(second, Some(&mut pid)); }
                checked = Some((pid, second, find_window_for_pid(Some(HWND_MESSAGE), &class, pid)));
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let child_pid = child.id();
        let _ = child.kill();
        let _ = child.wait();
        unsafe { DestroyWindow(window).unwrap() };
        let (pid, second, found) = checked.expect("both fixture processes must create a window");
        assert!(pid == child_pid || pid == std::process::id());
        assert_eq!(found, Some(second));
    }
}

/// Every `problem::PROBE`, each running part's command window must answer within 2 s; one silent
/// for `problem::HUNG_FOR` is a problem, and so is a part whose window is gone that long. Parts off
/// in parts-off.txt are skipped.
pub fn spawn_watchdog(immersive: bool) {
    let _ = std::thread::Builder::new().name("watchdog".into()).spawn(move || {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{SMTO_ABORTIFHUNG, SMTO_BLOCK, SendMessageTimeoutW, WM_NULL};
        let parts = enabled(immersive);
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
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
    STOPPING.store(true, Ordering::SeqCst);
    explorust_inject::shutdown();
    let mut asked = 0;
    for (owner, _) in PARTS {
        if let Some(window) = own_window(owner)
            && unsafe {
                PostMessageW(Some(window), bus::MSG_COMMAND, WPARAM(bus::Command::Shutdown as usize), LPARAM(0))
            }.is_ok()
        {
            asked += 1;
        }
    }
    if asked > 0 {
        std::thread::sleep(Duration::from_millis(500));
    }
}
