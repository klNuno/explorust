//! The unpackaged Winlogon entry remains outside the Task Manager group.

use crate::proc::{Child, Event, wait_any};
use crate::switch_events::{BOOTSTRAP_ACTIVE, switch_event};
use crate::{desktop_recovery, folders, install, log, processes, taskbar};
use std::path::Path;
use windows::Win32::System::Threading::INFINITE;
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SHUTTINGDOWN};

pub fn run(
    exe: &Path,
    args: &str,
    takeover: Option<&Event>,
    handoff: Option<u64>,
) -> Result<(), String> {
    let active = Event::named(BOOTSTRAP_ACTIVE)?;
    active.signal()?;
    let name = format!(r"Local\explorust-explorer-ready-{}", std::process::id());
    let explorer_ready = Event::named(&name)?;
    let child = Child::spawn(exe, &format!("--watchdog {name} {args}"))?;
    if let Some(ready) = takeover
        && wait_any(&[ready.0, child.handle], Some(10_000)) != Some(0)
    {
        if !child.ended() {
            child.kill();
        }
        return Err(format!(
            "supervisor pid {} did not take over (code {:#x})",
            child.pid,
            child.exit_code()
        ));
    }
    log::line(&format!(
        "bootstrap: waiting for supervisor pid {}",
        child.pid
    ));
    match wait_any(&[child.handle, explorer_ready.0], None) {
        Some(0) => {
            // SAFETY: this read-only metric indicates that the user session is ending.
            if unsafe { GetSystemMetrics(SM_SHUTTINGDOWN) } != 0 {
                return Ok(());
            }
            if let Some(stamp) = handoff {
                // The original switch watcher still owns recovery until it confirms the handoff.
                let _ = Event::set_existing(&switch_event(stamp, "rollback"));
            }
            log::line(&format!(
                "bootstrap: supervisor pid {} ended (code {:#x})",
                child.pid,
                child.exit_code()
            ));
            match recover(child.pid) {
                Ok(status) | Err(status) => log::line(&format!("bootstrap: {status}")),
            }
        }
        Some(1) => log::line("bootstrap: Explorer owns the desktop; exiting"),
        _ => return Err("could not wait for the supervisor".into()),
    }
    Ok(())
}

fn recover(supervisor: u32) -> Result<String, String> {
    let _lock = install::recovery_lock_for(INFINITE)?;
    let shell = install::dir().join("explorust.exe");
    let expected = shell.to_string_lossy();
    for candidate in processes::in_session("explorust.exe")?
        .into_iter()
        .filter(|p| p.parent == supervisor)
    {
        let Ok(child) = Child::open(candidate.pid) else {
            continue;
        };
        if !taskbar::process_image_path(child.handle)
            .is_some_and(|image| image.eq_ignore_ascii_case(&expected))
        {
            continue;
        }
        // End task may still be closing the other members. If the shell survives, stop its
        // verified process before Explorer can claim the desktop.
        if wait_any(&[child.handle], Some(1_000)).is_none() {
            child.kill();
        }
        if !child.ended() {
            return Err(format!("explorust pid {} did not stop", child.pid));
        }
    }
    folders::handler(false);
    desktop_recovery::restore_desktop(&shell, None).map(|desktop| desktop.description)
}
