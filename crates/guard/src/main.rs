//! explorust-guard: the per-user Winlogon shell. It starts explorust and keeps a way back to plain
//! Windows at every step. No UI dependencies, so it keeps working when explorust cannot.
//!
//! - no argument: run as the shell (see `supervise`).
//! - `--install`: set the per-user Winlogon `Shell` value to this exe; takes effect at next logon.
//!   Elevated, it first copies the shell to `%ProgramFiles%\explorust`, trusts the signing
//!   certificate (the uiAccess build, see `install`) and writes the Apps entry, and the Shell value
//!   points to that copy (`manage`).
//! - `--restore`: delete that value (next logon is plain Windows) and bring explorer back now.
//!   Elevated, it also removes the Program Files copy, the certificate and the Apps entry, unless
//!   another account still uses them.
//! - `--fallback`: ask a running guard to kill explorust and start explorer (same as Ctrl+Alt+Shift+F12).
//! - `--status`: print the Shell value, the Windows build and the last good build.
//! - `--adopt <pid>`: started by explorust when its guard died; supervises that explorust.
//! - `--setup`: the installer's run (`setup`): question, `--install`, Apps entry, sign-out offer.
//! - `--uninstall [--yes|--quiet]`: the user's way back (`uninstall`), asks Windows for rights.
//!   `--machine [--for <sid>]` is its elevated half.

#![windows_subsystem = "windows"]

mod account;
mod apps;
mod folders;
mod hotkey;
mod install;
mod log;
mod manage;
mod notice;
mod package;
mod policy;
mod prefetch;
mod proc;
mod reg;
mod setup;
mod supervise;
mod taskbar;
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
        "--install" => manage::install(),
        "--restore" => manage::restore(),
        "--setup" => setup::run(),
        "--uninstall" => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            let has = |f: &str| rest.iter().any(|a| a == f);
            if has("--machine") {
                let sid = rest.iter().position(|a| a == "--for").and_then(|i| rest.get(i + 1)).cloned();
                uninstall::machine(sid.as_deref())
            } else {
                uninstall::run(has("--yes"), has("--quiet"))
            }
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
