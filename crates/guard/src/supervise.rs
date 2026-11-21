//! The logon loop: start explorust, wait for its ready signal, watch it, and at the first problem
//! start explorer.exe for the rest of the session and tell the user once (`notice`). Also falls
//! back on the hotkey, on Shift held at logon or when explorust.exe is missing, without a word.

use crate::notice::{self, Notice};
use crate::policy::{AtLogon, MAX_STREAK, PROBE, Watchdog, at_logon, ready_timeout};
use crate::proc::{Child, Event, wait_any};
use crate::taskbar::{self, Start};
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
        // A logon counts by its outcome: only one that ended without a problem clears the streak.
        if !reg::last_logon_had_problem() && reg::problem_streak() != 0 {
            log::line("the last logon ended without a problem: problem streak reset");
            reg::set_problem_streak(0);
        }
        reg::set_logon_had_problem(false);
        let streak = reg::problem_streak();
        if at_logon(streak) == AtLogon::TurnOff {
            return explorer_forever(Some(turn_off(&format!("explorust ran into a problem at {streak} sign-ins in a row"))));
        }
        if !exe.exists() {
            return explorer_forever(Some(turn_off(&format!("{} is missing", exe.display()))));
        }
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
                reg::set_logon_had_problem(true);
                log::line(&format!("problem: {why}; plain explorer for this session (problem logons in a row: {streak})"));
                if streak >= MAX_STREAK {
                    return explorer_forever(Some(turn_off(&format!("{why} (a problem at {streak} sign-ins in a row)"))));
                }
                return explorer_forever(Some(Notice::Problem(why)));
            }
        }
    }
}

/// Deletes the Shell value so the next logon is plain Windows. The notice says what happened: off
/// for good, or, when the value could not be deleted, a problem whose streak stays so the next
/// logon tries again.
fn turn_off(why: &str) -> Notice {
    match reg::delete_shell() {
        Ok(_) => {
            reg::set_problem_streak(0);
            reg::set_logon_had_problem(false);
            log::line(&format!("explorust turned off: {why}; the Shell value is deleted"));
            Notice::TurnedOff(why.to_string())
        }
        Err(e) => {
            log::line(&format!("explorust should turn off ({why}) but the Shell value stays: {e}"));
            Notice::Problem(format!("{why}; turning explorust off failed: {e}"))
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
fn watch(child: &Child, fallback: &Event) -> End {
    let t0 = Instant::now();
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
        if taskbar_off {
            continue;
        }
        let at = t0.elapsed();
        if let Some(why) = dog.probe(taskbar::probe(child.pid), at) {
            // A hung process may also ignore its quit event: kill waits up to 10 s, then the
            // fallback goes on anyway.
            child.kill();
            return End::Problem(why);
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

/// Keeps a plain explorer.exe as the shell for the rest of the session: started (or an explorer
/// whose taskbar already answers, adopted), confirmed by its answering taskbar, restarted when it
/// ends, hangs before its taskbar comes or fails to start, at most 5 times in 5 minutes. `notice`
/// shows once that taskbar answers; when it never does, the user is told how to start it by hand.
fn explorer_forever(notice: Option<Notice>) -> u32 {
    folders::handler(false);
    let mut notice = notice;
    let path = explorer_path();
    let mut starts: Vec<Instant> = Vec::new();
    let mut told = false;
    loop {
        let pid = match taskbar::explorer() {
            Some(pid) => {
                log::line(&format!("explorer pid {pid} already owns the taskbar"));
                pid
            }
            None => {
                let now = Instant::now();
                starts.retain(|t| now.duration_since(*t) < Duration::from_secs(300));
                if starts.len() >= 5 {
                    log::line("explorer did not come up 5 times in 5 minutes: telling the user, next try in 1 minute");
                    if !told {
                        told = true;
                        notice::show_later(Notice::NoShell);
                    }
                    starts.clear();
                    std::thread::sleep(Duration::from_secs(60));
                    continue;
                }
                starts.push(now);
                let Some(child) = spawn_explorer(&path) else { continue };
                let t0 = Instant::now();
                match taskbar::wait_explorer(&child, Duration::from_secs(60)) {
                    Start::Up(pid) => {
                        log::line(&format!("explorer taskbar answers (pid {pid}, started pid {}) after {} ms", child.pid, t0.elapsed().as_millis()));
                        pid
                    }
                    Start::Exited(code) => {
                        log::line(&format!("explorer pid {} ended before its taskbar came, code {code:#x}", child.pid));
                        std::thread::sleep(Duration::from_secs(2));
                        continue;
                    }
                    Start::Timeout => {
                        log::line(&format!("explorer pid {} has no taskbar after 60 s: restarting it", child.pid));
                        child.kill();
                        continue;
                    }
                }
            }
        };
        if let Some(n) = notice.take() {
            notice::show_later(n);
        }
        match Child::open(pid) {
            Ok(shell) => {
                wait_any(&[shell.handle], None);
                log::line(&format!("explorer pid {pid} ended, code {:#x}", shell.exit_code()));
            }
            Err(e) => log::line(&format!("explorer pid {pid}: {e}")),
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// Starts explorer.exe, three tries 2 s apart.
fn spawn_explorer(path: &Path) -> Option<Child> {
    for i in 1..=3 {
        match Child::spawn(path, "") {
            Ok(c) => {
                log::line(&format!("started explorer pid {}", c.pid));
                return Some(c);
            }
            Err(e) => log::line(&format!("explorer start {i}/3: {e}")),
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    None
}
