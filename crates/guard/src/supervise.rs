//! The logon loop: start explorust, wait for its ready signal, restart it once, and fall back to
//! explorer.exe on the second failure, on the hotkey, on Shift held at logon or when it is missing.

use crate::policy::{Next, Policy, ready_timeout};
use crate::proc::{Child, Event, wait_any};
use crate::{hotkey, log, reg};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const FALLBACK_EVENT: &str = r"Local\explorust-guard-fallback";

enum End {
    Failed(String),
    Fallback(String),
}

pub fn run() -> u32 {
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
    log::line(&format!("guard start: windows {build}, shell {}", exe.display()));
    if hotkey::shift_held() {
        log::line("Shift held at logon: plain explorer");
        return explorer_forever();
    }
    if !exe.exists() {
        log::line("explorust.exe missing: plain explorer");
        return explorer_forever();
    }
    let mut policy = Policy::new(Duration::from_secs(300));
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let self_test = reg::last_good_build().as_deref() != Some(build.as_str());
        match run_once(&exe, attempt, self_test, &build, &fallback) {
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
    let mut args = format!("--ready-event {name}");
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
    match wait_any(&[child.handle, fallback.0], None) {
        Some(0) => match child.exit_code() {
            0 => End::Fallback("explorust exited cleanly".into()),
            code => End::Failed(format!("crashed after ready, code {code:#x}")),
        },
        _ => {
            child.kill();
            End::Fallback("hotkey or --fallback".into())
        }
    }
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
