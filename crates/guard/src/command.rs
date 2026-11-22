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
//! - `--setup`: interactive installation or an update of the running shell (`setup`).
//! - `--live`: supervise explorust after the separate switch command has freed the desktop.
//! - `--uninstall [--yes|--quiet]`: the user's way back (`uninstall`), asks Windows for rights.
//!   `--machine [--for <sid>]` is its elevated half.

use crate::{account, install, log, manage, proc, reg, reopen, setup, supervise, taskbar, uninstall};
use crate::proc::Event;
use crate::switch_events::{CANCEL_FOR_RESTORE, GUARD_MUTEX, SWITCH_EXE, switch_event};
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows::core::HSTRING;

fn say(msg: &str) {
    log::line(msg);
    println!("{msg}");
}

pub fn run() {
    let arg = std::env::args().nth(1).unwrap_or_default();
    if arg != "--live" {
        // SAFETY: lets a console caller see the output; harmless without one.
        let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    }
    let code = match arg.as_str() {
        "" => run_shell(None, None),
        "--adopt" => match std::env::args().nth(2).and_then(|p| p.parse().ok()) {
            Some(pid) => run_shell(Some((pid, std::env::args().nth(3))), None),
            None => {
                say("--adopt needs the explorust pid");
                2
            }
        },
        "--install" => manage::install(),
        "--restore" => manage::restore(),
        "--setup" => setup::run(),
        "--live" => run_live(),
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

fn run_live() -> u32 {
    let mut args = std::env::args().skip(2);
    let Some(worker) = args.next().and_then(|pid| pid.parse::<u32>().ok()) else {
        return 2;
    };
    let Some(stamp) = args.next().and_then(|stamp| stamp.parse::<u64>().ok()) else {
        return 2;
    };
    let rollback = match Event::open_for_wait(&switch_event(stamp, "rollback")) {
        Ok(event) => event,
        Err(error) => {
            log::line(&format!("guard: live handoff event unavailable: {error}"));
            return 1;
        }
    };
    let switch = install::dir().join(SWITCH_EXE);
    if Event::is_signalled(CANCEL_FOR_RESTORE)
        || install::elevated()
        || !account::is_session_user()
        || reopen::guard_stamp(worker) != Some(stamp)
        || !taskbar::image_path(worker).is_some_and(|path| {
            path.eq_ignore_ascii_case(&switch.to_string_lossy())
        })
        || taskbar::explorer_window().is_some()
    {
        log::line("guard: live handoff requires the installed switch worker and a free desktop");
        return 1;
    }
    run_shell(None, Some((stamp, rollback)))
}

fn run_shell(adopt: Option<(u32, Option<String>)>, handoff: Option<(u64, Event)>) -> u32 {
    // SAFETY: one guard per session; the last-error value is read immediately after creation.
    let handle = match unsafe { CreateMutexW(None, false, &HSTRING::from(GUARD_MUTEX)) } {
        Ok(handle) => handle,
        Err(e) => {
            log::line(&format!("guard mutex: {e}"));
            return 1;
        }
    };
    let already = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    // SAFETY: the supervisory loop owns and closes the handle, even if a dialog is still open.
    let _mutex = unsafe { OwnedHandle::from_raw_handle(handle.0) };
    if already {
        log::line("another guard runs in this session: exiting");
        return 0;
    }
    if let Some((stamp, _)) = &handoff {
        let Some(guard) = reopen::guard_stamp(std::process::id()) else {
            log::line("guard: could not identify this supervisor for the Files handoff");
            return 1;
        };
        let dir = log::dir();
        if let Err(error) = reopen::take(&dir, *stamp).and_then(|folders| reopen::save(&dir, guard, &folders)) {
            log::line(&format!("guard: could not transfer the Files handoff: {error}"));
            return 1;
        }
    }
    supervise::run(adopt, handoff.map(|(_, rollback)| rollback))
}
