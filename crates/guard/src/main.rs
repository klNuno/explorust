//! explorust-guard: the per-user Winlogon shell. It starts explorust and keeps a way back to plain
//! Windows at every step. No UI dependencies, so it keeps working when explorust cannot.
//!
//! - no argument: run as the shell (see `supervise`).
//! - `--install`: set the per-user Winlogon `Shell` value to this exe; takes effect at next logon.
//!   Elevated, it first copies the shell to `%ProgramFiles%\explorust` and trusts the signing
//!   certificate (the uiAccess build, see `install`), and the Shell value points to that copy.
//! - `--restore`: delete that value (next logon is plain Windows) and start explorer if no shell runs.
//!   Elevated, it also removes the Program Files copy and the certificate.
//! - `--fallback`: ask a running guard to kill explorust and start explorer (same as Ctrl+Alt+Shift+F12).
//! - `--status`: print the Shell value, the Windows build and the last good build.
//! - `--adopt <pid>`: started by explorust when its guard died; supervises that explorust.

#![windows_subsystem = "windows"]

mod folders;
mod hotkey;
mod install;
mod log;
mod policy;
mod proc;
mod reg;
mod supervise;

use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows::core::HSTRING;

fn say(msg: &str) {
    log::line(msg);
    println!("{msg}");
}

fn main() {
    // SAFETY: lets a console caller see the output; harmless without one.
    let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    let arg = std::env::args().nth(1).unwrap_or_default();
    let code = match arg.as_str() {
        "" => run_shell(None),
        "--adopt" => match std::env::args().nth(2).and_then(|p| p.parse().ok()) {
            Some(pid) => run_shell(Some(pid)),
            None => {
                say("--adopt needs the explorust pid");
                2
            }
        },
        "--install" => install(),
        "--restore" => restore(),
        "--fallback" => match proc::Event::set_existing(supervise::FALLBACK_EVENT) {
            Ok(()) => {
                say("fallback requested");
                0
            }
            Err(e) => {
                say(&format!("no running guard: {e}"));
                1
            }
        },
        "--status" => {
            say(&format!(
                "Shell={:?} build={} last_good={:?} installed={} certificate_trusted={}",
                reg::shell(),
                reg::windows_build(),
                reg::last_good_build(),
                install::dir().join("explorust.exe").exists(),
                install::trusted()
            ));
            0
        }
        other => {
            say(&format!("unknown argument {other}; use --install, --restore, --fallback or --status"));
            2
        }
    };
    std::process::exit(code as i32);
}

fn run_shell(adopt: Option<u32>) -> u32 {
    // SAFETY: one guard per session; the mutex lives until the process ends.
    let _mutex = unsafe { CreateMutexW(None, false, &HSTRING::from(r"Local\explorust-guard")) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        log::line("another guard runs in this session: exiting");
        return 0;
    }
    supervise::run(adopt)
}

fn install() -> u32 {
    let me = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            say(&format!("install: {e}"));
            return 1;
        }
    };
    let guard = if install::elevated() {
        let from = me.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let copied = install::copy_files(&from).and_then(|g| install::trust(true).map(|t| (g, t)));
        match copied {
            Ok((guard, trust)) => {
                say(&format!("install: shell copied to {}, certificate trusted ({trust})", install::dir().display()));
                guard
            }
            Err(e) => {
                say(&format!("install failed: {e}"));
                return 1;
            }
        }
    } else {
        say("install: not elevated, so no Program Files copy: a uiAccess build will not start from here");
        me
    };
    match reg::set_shell(&format!("\"{}\"", guard.display())) {
        Ok(()) => {
            say(&format!("installed: next logon starts {}", guard.display()));
            0
        }
        Err(e) => {
            say(&format!("install failed: {e}"));
            1
        }
    }
}

fn restore() -> u32 {
    let code = match reg::delete_shell() {
        Ok(true) => {
            say("restored: the per-user Shell value is gone, next logon is plain Windows");
            0
        }
        Ok(false) => {
            say("nothing to restore: no per-user Shell value");
            0
        }
        Err(e) => {
            say(&format!("restore failed: {e}"));
            1
        }
    };
    folders::handler(false);
    let _ = proc::Event::set_existing(supervise::FALLBACK_EVENT);
    if !install::elevated() {
        if install::dir().exists() || install::trusted() {
            say("restore: run it elevated to remove the Program Files copy and the certificate");
        }
        return code;
    }
    // The running guard kills explorust on the fallback event; give it a moment to let go.
    std::thread::sleep(std::time::Duration::from_secs(2));
    say(&format!("restore: {}", install::remove_files()));
    match install::trust(false) {
        Ok(t) => say(&format!("restore: certificate {t}")),
        Err(e) => {
            say(&format!("restore: certificate: {e}"));
            return 1;
        }
    }
    code
}
