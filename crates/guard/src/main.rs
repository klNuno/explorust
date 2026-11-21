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
//! - `--setup`: the installer's run (`setup`): question, `--install`, Apps entry, sign-out offer.
//! - `--uninstall [--yes|--quiet]`: the user's way back (`uninstall`), asks Windows for rights.

#![windows_subsystem = "windows"]

mod apps;
mod folders;
mod hotkey;
mod install;
mod log;
mod notice;
mod package;
mod policy;
mod proc;
mod reg;
mod setup;
mod supervise;
mod ui;
mod uninstall;

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
        "--install" => {
            let code = install();
            if code == 0 && install::elevated() {
                say(&format!("install: {}", apps::register(&install::dir().join("explorust-guard.exe"))));
            }
            code
        }
        "--restore" => {
            let code = restore();
            if install::elevated() {
                say(&format!("restore: {}", apps::unregister()));
            }
            reg::set_problem_streak(0);
            code
        }
        "--setup" => setup::run(),
        "--uninstall" => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            uninstall::run(rest.iter().any(|a| a == "--yes"), rest.iter().any(|a| a == "--quiet"))
        }
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
                "Shell={:?} build={} last_good={:?} installed={} certificate_trusted={} apps_entry={} problem_streak={}",
                reg::shell(),
                reg::windows_build(),
                reg::last_good_build(),
                install::dir().join("explorust.exe").exists(),
                install::trusted(),
                reg::has_uninstall_entry(),
                reg::problem_streak()
            ));
            0
        }
        other => {
            say(&format!("unknown argument {other}; use --setup, --install, --uninstall, --restore, --fallback or --status"));
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

pub(crate) fn install() -> u32 {
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
                say(&format!("install: {}", install::index_service(true)));
                guard
            }
            Err(e) => {
                say(&format!("install failed: {e}"));
                return 1;
            }
        }
    } else {
        say("install: not elevated, so no Program Files copy: a uiAccess build will not start from here");
        me.clone()
    };
    // The toast listener's identity; explorust runs without it (it polls), so a failure only warns.
    let from = me.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let dir = guard.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    match package::add(&from, &dir) {
        Ok(p) => say(&format!("install: notification helper package {p}, external location {}", dir.display())),
        Err(e) => say(&format!("install: notification helper package not installed ({e}); toasts are polled")),
    }
    folders::viewer(&dir, true);
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

pub(crate) fn restore() -> u32 {
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
    folders::viewer(&install::dir(), false);
    let _ = proc::Event::set_existing(supervise::FALLBACK_EVENT);
    match package::remove() {
        Ok(r) => say(&format!("restore: notification helper package {r}")),
        Err(e) => say(&format!("restore: notification helper package: {e}")),
    }
    if !install::elevated() {
        if install::dir().exists() || install::trusted() {
            say("restore: run it elevated to remove the Program Files copy and the certificate");
        }
        return code;
    }
    // The running guard kills explorust on the fallback event; give it a moment to let go.
    std::thread::sleep(std::time::Duration::from_secs(2));
    say(&format!("restore: {}", install::index_service(false)));
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
