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
        "" if option_env!("EXPLORUST_PACKAGE_BUILD").is_some() => bootstrap(),
        "" => run_shell(None, None, None),
        "--adopt" => adopt(),
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

/// Winlogon must be able to run this program even if the loader rejects the bound supervisor.
/// Hand over only once the supervisor has checked its identity and owns the session mutex.
fn bootstrap() -> u32 {
    let start = || -> Result<(), String> {
        let name = format!(r"Local\explorust-supervisor-{}", std::process::id());
        let ready = Event::named(&name)?;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?
            .with_file_name(install::SUPERVISOR_EXE);
        let child = proc::Child::spawn(&exe, &format!("--bootstrap {name}"))?;
        match proc::wait_any(&[ready.0, child.handle], Some(10_000)) {
            Some(0) if !child.ended() => Ok(()),
            _ => {
                if !child.ended() {
                    child.kill();
                }
                Err(format!("supervisor pid {} did not take over (code {:#x})", child.pid, child.exit_code()))
            }
        }
    };
    match start() {
        Ok(()) => 0,
        Err(error) => {
            log::line(&format!("guard: {error}; starting Explorer without package identity"));
            run_shell(None, None, Some(Logon::Explorer))
        }
    }
}

pub fn run_supervisor() {
    if let Err(error) = crate::msix_identity::require() {
        log::line(&format!("supervisor: {error}"));
        std::process::exit(1);
    }
    log::line(&format!("supervisor: package identity {}", crate::msix_identity::FAMILY));
    let mut args = std::env::args().skip(1);
    let code = match args.next().as_deref() {
        Some("--bootstrap") => match args.next().as_deref().and_then(|name| Event::open_for_signal(name).ok()) {
            Some(ready) => run_shell(None, None, Some(Logon::Takeover(ready))),
            None => 2,
        },
        Some("--adopt") => adopt(),
        Some("--live") => run_live(),
        _ => 2,
    };
    std::process::exit(code as i32);
}

fn adopt() -> u32 {
    match std::env::args().nth(2).and_then(|p| p.parse().ok()) {
        Some(pid) => run_shell(Some((pid, std::env::args().nth(3))), None, None),
        None => {
            say("--adopt needs the explorust pid");
            2
        }
    }
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
    let guard = install::dir().join(install::GUARD_EXE);
    if Event::is_signalled(CANCEL_FOR_RESTORE)
        || !reg::shell().is_some_and(|shell| shell.eq_ignore_ascii_case(&format!("\"{}\"", guard.display())))
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
    run_shell(None, Some((stamp, rollback)), None)
}

enum Logon {
    Takeover(Event),
    Explorer,
}

fn run_shell(adopt: Option<(u32, Option<String>)>, handoff: Option<(u64, Event)>, logon: Option<Logon>) -> u32 {
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
    match logon {
        Some(Logon::Takeover(event)) => {
            if let Err(error) = event.signal() {
                log::line(&format!("guard: cannot acknowledge the Winlogon handoff: {error}"));
                return 1;
            }
        }
        Some(Logon::Explorer) => return supervise::explorer_forever(None),
        None => {}
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
