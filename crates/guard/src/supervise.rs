//! The logon loop: start explorust, wait for its ready signal, restart it once, and fall back to
//! explorer.exe on the second failure, on the hotkey, on Shift held at logon or when it is missing.

use crate::policy::{Next, Policy, ready_timeout};
use crate::proc::{Child, Event, wait_any};
use crate::{folders, hotkey, log, reg};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const FALLBACK_EVENT: &str = r"Local\explorust-guard-fallback";

enum End {
    Failed(String),
    Fallback(String),
    /// explorust exited with code 5 (`explorust --reload`).
    Reload,
}

/// The exit code of `explorust --reload`.
const RELOAD: u32 = 5;

/// `adopt`: the pid of a running explorust whose guard died; it is watched instead of starting one.
pub fn run(adopt: Option<u32>) -> u32 {
    let fallback = match Event::named(FALLBACK_EVENT) {
        Ok(e) => e,
        Err(e) => {
            log::line(&format!("guard: {e}; starting explorer"));
            return explorer_forever();
        }
    };
    hotkey::spawn(fallback.0);
    let exe = shell_exe();
    let build = reg::windows_build();
    let mut policy = Policy::new(Duration::from_secs(300));
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
            return explorer_forever();
        }
    }
    if !exe.exists() {
        log::line("explorust.exe missing: plain explorer");
        return explorer_forever();
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
                return explorer_forever();
            }
            End::Failed(why) => match policy.failed(Instant::now()) {
                Next::Restart => log::line(&format!("explorust failed ({why}): restarting once")),
                Next::Fallback => {
                    log::line(&format!("explorust failed again ({why}): plain explorer"));
                    return explorer_forever();
                }
            },
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
        Err(e) => return End::Failed(e),
    };
    log::line(&format!("started explorust pid {} ({args})", child.pid));
    let timeout = ready_timeout(self_test);
    match wait_any(&[child.handle, ready.0, fallback.0], Some(timeout.as_millis() as u32)) {
        Some(0) => return End::Failed(format!("exited before ready, code {}", child.exit_code())),
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
            return End::Failed(format!("not ready after {} s", timeout.as_secs()));
        }
    }
    watch(&child, fallback)
}

/// Waits for a ready explorust to end or for the escape hatch.
fn watch(child: &Child, fallback: &Event) -> End {
    match wait_any(&[child.handle, fallback.0], None) {
        Some(0) => match child.exit_code() {
            0 => End::Fallback("explorust exited cleanly".into()),
            RELOAD => End::Reload,
            code => End::Failed(format!("crashed after ready, code {code:#x}")),
        },
        _ => {
            child.kill();
            End::Fallback("hotkey or --fallback".into())
        }
    }
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
/// ends (at most 5 times in 5 minutes).
fn explorer_forever() -> u32 {
    folders::handler(false);
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
        wait_any(&[child.handle], None);
        log::line(&format!("explorer pid {} ended, code {:#x}", child.pid, child.exit_code()));
        std::thread::sleep(Duration::from_secs(2));
    }
}
