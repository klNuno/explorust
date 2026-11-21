//! The logon loop: start explorust, wait for its ready signal, watch it, and at the first problem
//! start explorer.exe for the rest of the session and tell the user once (`notice`). Also falls
//! back on the hotkey, on Shift held at logon or when explorust.exe is missing, without a word.

use crate::notice::{self, Notice};
use crate::policy::{AtLogon, HEALTHY_AFTER, PROBE, Probe, Watchdog, at_logon, ready_timeout};
use crate::proc::{Child, Event, wait_any};
use crate::{folders, hotkey, log, reg};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const FALLBACK_EVENT: &str = r"Local\explorust-guard-fallback";

enum End {
    /// Something went wrong: explorer takes over and the user is told.
    Problem(String),
    /// Asked for (hotkey, `--fallback`, `--restore`, a clean exit): explorer, no message.
    Fallback(String),
    /// explorust exited with code 5 (`explorust --reload`).
    Reload,
}

/// The exit code of `explorust --reload`.
const RELOAD: u32 = 5;
/// explorust's exit code when it found a problem itself (a part that crashed twice or hangs, the
/// keyboard helper dying): its reason is in `problem.txt`.
const PROBLEM: u32 = 6;

/// `adopt`: the pid of a running explorust whose guard died; it is watched instead of starting one.
pub fn run(adopt: Option<u32>) -> u32 {
    let fallback = match Event::named(FALLBACK_EVENT) {
        Ok(e) => e,
        Err(e) => {
            log::line(&format!("guard: {e}; starting explorer"));
            return explorer_forever(None);
        }
    };
    hotkey::spawn(fallback.0);
    let exe = shell_exe();
    let build = reg::windows_build();
    let mut attempt = 0u32;
    let mut adopted = None;
    if let Some(pid) = adopt {
        log::line(&format!("guard adopts explorust pid {pid}: its guard ended"));
        match Child::open(pid) {
            Ok(child) => adopted = Some(child),
            Err(e) => log::line(&format!("adopt: {e}; starting a new explorust")),
        }
    } else {
        log::line(&format!("guard start: windows {build}, shell {}", exe.display()));
        if hotkey::shift_held() {
            log::line("Shift held at logon: plain explorer");
            return explorer_forever(None);
        }
        let streak = reg::problem_streak();
        if at_logon(streak) == AtLogon::TurnOff {
            let gone = reg::delete_shell();
            log::line(&format!("{streak} logons in a row ended in a problem: explorust turned off (Shell value deleted: {gone:?})"));
            reg::set_problem_streak(0);
            return explorer_forever(Some(Notice::TurnedOff));
        }
    }
    if !exe.exists() {
        log::line("explorust.exe missing: plain explorer");
        return explorer_forever(None);
    }
    loop {
        let end = match adopted.take() {
            Some(child) => watch(&child, &fallback),
            None => {
                attempt += 1;
                let self_test = reg::last_good_build().as_deref() != Some(build.as_str());
                run_once(&exe, attempt, self_test, &build, &fallback)
            }
        };
        match end {
            End::Reload => swap_next(&exe),
            End::Fallback(why) => {
                log::line(&format!("fallback: {why}"));
                return explorer_forever(None);
            }
            End::Problem(why) => {
                let streak = reg::problem_streak() + 1;
                reg::set_problem_streak(streak);
                log::line(&format!("problem: {why}; plain explorer for this session (problem logons in a row: {streak})"));
                return explorer_forever(Some(Notice::Problem(why)));
            }
        }
    }
}

fn run_once(exe: &Path, attempt: u32, self_test: bool, build: &str, fallback: &Event) -> End {
    let name = format!(r"Local\explorust-ready-{}-{attempt}", std::process::id());
    let ready = match Event::named(&name) {
        Ok(e) => e,
        Err(e) => return End::Fallback(e),
    };
    let mut args = format!("--ready-event {name} --guard {}", std::process::id());
    if self_test {
        args.push_str(" --self-test");
    }
    let child = match Child::spawn(exe, &args) {
        Ok(c) => c,
        Err(e) => return End::Problem(format!("explorust did not start: {e}")),
    };
    let _ = std::fs::remove_file(problem_file());
    log::line(&format!("started explorust pid {} ({args})", child.pid));
    let timeout = ready_timeout(self_test);
    match wait_any(&[child.handle, ready.0, fallback.0], Some(timeout.as_millis() as u32)) {
        Some(0) => {
            let code = child.exit_code();
            return End::Problem(match problem_reason(code) {
                Some(r) => format!("explorust stopped while starting: {r}"),
                None => format!("explorust stopped while starting (code {code:#x})"),
            });
        }
        Some(1) => {
            log::line(&format!("explorust ready (self-test: {self_test})"));
            folders::handler(true);
            if self_test && let Err(e) = reg::set_last_good_build(build) {
                log::line(&format!("could not record the good build: {e}"));
            }
        }
        Some(_) => {
            child.kill();
            return End::Fallback("hotkey or --fallback before ready".into());
        }
        None => {
            child.kill();
            return End::Problem(format!("explorust was not ready after {} s", timeout.as_secs()));
        }
    }
    watch(&child, fallback)
}

/// Watches a ready explorust until it ends, its taskbar stops answering, or the escape hatch.
/// After `HEALTHY_AFTER` without a problem the streak of problem logons goes back to zero.
fn watch(child: &Child, fallback: &Event) -> End {
    let since = Instant::now();
    let mut healthy = false;
    let mut dog = Watchdog::default();
    let taskbar_off = parts_off().iter().any(|p| p == "taskbar");
    loop {
        match wait_any(&[child.handle, fallback.0], Some(PROBE.as_millis() as u32)) {
            Some(0) => {
                return match child.exit_code() {
                    0 => End::Fallback("explorust exited cleanly".into()),
                    RELOAD => End::Reload,
                    code => End::Problem(match problem_reason(code) {
                        Some(r) => r,
                        None => format!("explorust crashed (code {code:#x})"),
                    }),
                };
            }
            Some(_) => {
                child.kill();
                return End::Fallback("hotkey or --fallback".into());
            }
            None => {}
        }
        if !healthy && since.elapsed() >= HEALTHY_AFTER {
            healthy = true;
            if reg::problem_streak() != 0 {
                reg::set_problem_streak(0);
                log::line(&format!("explorust healthy for {} s: problem streak reset", HEALTHY_AFTER.as_secs()));
            }
        }
        if taskbar_off {
            continue;
        }
        if let Some(why) = dog.probe(taskbar_probe(child.pid)) {
            // A hung process may also ignore its quit event: kill waits up to 10 s, then the
            // fallback goes on anyway.
            child.kill();
            return End::Problem(why);
        }
    }
}

/// Asks explorust's taskbar window for an answer within 2 s.
fn taskbar_probe(pid: u32) -> Probe {
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowExW, GetWindowThreadProcessId, SMTO_ABORTIFHUNG, SMTO_BLOCK, SendMessageTimeoutW, WM_NULL,
    };
    use windows::core::w;
    // SAFETY: window lookups and a bounded send; a stale handle only fails them.
    unsafe {
        let mut after: Option<HWND> = None;
        loop {
            let Ok(h) = FindWindowExW(None, after, w!("Shell_TrayWnd"), None) else { return Probe::Missing };
            if h.is_invalid() {
                return Probe::Missing;
            }
            let mut owner = 0u32;
            GetWindowThreadProcessId(h, Some(&mut owner));
            if owner == pid {
                let mut out = 0usize;
                let r = SendMessageTimeoutW(h, WM_NULL, WPARAM(0), LPARAM(0), SMTO_ABORTIFHUNG | SMTO_BLOCK, 2000, Some(&mut out));
                return if r.0 != 0 { Probe::Answered } else { Probe::NoAnswer };
            }
            after = Some(h);
        }
    }
}

/// Where explorust writes why it stopped with `PROBLEM` (same folder as the logs).
fn problem_file() -> PathBuf {
    log::dir().join("problem.txt")
}

fn problem_reason(code: u32) -> Option<String> {
    if code != PROBLEM {
        return None;
    }
    let text = std::fs::read_to_string(problem_file()).ok()?;
    let _ = std::fs::remove_file(problem_file());
    Some(text.trim().to_string()).filter(|t| !t.is_empty())
}

/// Parts set aside in `parts-off.txt` (the shell reads the same file).
fn parts_off() -> Vec<String> {
    std::fs::read_to_string(log::dir().join("parts-off.txt"))
        .map(|s| s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default()
}

/// A reload: `explorust.next.exe` beside explorust.exe, if present, replaces it before the restart.
fn swap_next(exe: &Path) {
    let next = exe.with_file_name("explorust.next.exe");
    if !next.exists() {
        // Installed in Program Files this guard cannot write beside explorust.exe: the elevated
        // deploy (tools/g1-deploy.sh) has already swapped the new exe in.
        log::line(&format!("reload: restarting {}", exe.display()));
        return;
    }
    // The old process has exited, but its image can stay mapped for a moment.
    for _ in 0..20 {
        match std::fs::rename(&next, exe) {
            Ok(()) => {
                log::line("reload: explorust.next.exe swapped in");
                return;
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    log::line("reload: explorust.exe still locked after 2 s, restarting the old one");
}

fn shell_exe() -> PathBuf {
    let me = std::env::current_exe().unwrap_or_default();
    me.with_file_name("explorust.exe")
}

pub fn explorer_path() -> PathBuf {
    let windir = std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    windir.join("explorer.exe")
}

/// Keeps a plain explorer.exe running as the shell for the rest of the session, restarting it when it
/// ends (at most 5 times in 5 minutes). `notice`, once explorer runs, tells the user why.
fn explorer_forever(notice: Option<Notice>) -> u32 {
    folders::handler(false);
    let mut notice = notice;
    let path = explorer_path();
    let mut starts: Vec<Instant> = Vec::new();
    loop {
        let now = Instant::now();
        starts.retain(|t| now.duration_since(*t) < Duration::from_secs(300));
        if starts.len() >= 5 {
            log::line("explorer ended 5 times in 5 minutes: guard stops restarting it");
            return 1;
        }
        starts.push(now);
        let child = match Child::spawn(&path, "") {
            Ok(c) => c,
            Err(e) => {
                log::line(&format!("explorer: {e}"));
                return 1;
            }
        };
        log::line(&format!("started explorer pid {}", child.pid));
        if let Some(n) = notice.take() {
            notice::show_later(n);
        }
        wait_any(&[child.handle], None);
        log::line(&format!("explorer pid {} ended, code {:#x}", child.pid, child.exit_code()));
        std::thread::sleep(Duration::from_secs(2));
    }
}
