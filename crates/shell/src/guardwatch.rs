//! The guard's keeper. If the guard that started explorust ends (killed from Task Manager, say),
//! nothing would bring explorer back on a crash and the escape hotkey would be gone, so explorust
//! starts a new supervisor that adopts it and watches that one.

use crate::{events, log, switch_events::BOOTSTRAP_ACTIVE};
use std::os::windows::process::CommandExt;
use std::sync::atomic::{AtomicU32, Ordering};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
static GUARD_PID: AtomicU32 = AtomicU32::new(0);

pub fn spawn(guard_pid: u32, staged_event: Option<String>) {
    GUARD_PID.store(guard_pid, Ordering::Relaxed);
    let _ = std::thread::Builder::new().name("guard-watch".into()).spawn(move || watch(guard_pid, staged_event));
}

pub fn pid() -> u32 {
    GUARD_PID.load(Ordering::Relaxed)
}

fn watch(mut pid: u32, staged_event: Option<String>) {
    let guard = std::env::current_exe().map(|p| p.with_file_name("explorust-guard.exe"));
    let mut try_packaged = option_env!("EXPLORUST_PACKAGE_BUILD").is_some();
    loop {
        // SAFETY: opens, waits on and closes a handle owned here.
        unsafe {
            match OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
                Ok(h) => {
                    WaitForSingleObject(h, INFINITE);
                    let _ = CloseHandle(h);
                }
                Err(e) => log::line(&format!("guard pid {pid}: {e}")),
            }
        }
        if events::Owned::is_signalled(BOOTSTRAP_ACTIVE) {
            // The unpackaged guard owns recovery when this supervisor dies.
            return;
        }
        let Ok(exe) = &guard else { return };
        let me = std::process::id().to_string();
        let start = |exe: &std::path::Path| {
            let mut command = std::process::Command::new(exe);
            command.args(["--adopt", &me]);
            if let Some(name) = &staged_event {
                command.arg(name);
            }
            command.creation_flags(CREATE_NO_WINDOW).spawn()
        };
        let replacement = if try_packaged {
            match start(&exe.with_file_name("explorust-supervisor.exe")) {
                Ok(mut child) => {
                    // A failed identity check exits before taking over. Use the unbound guard
                    // if Windows can no longer start the supervisor in this session.
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    if child.try_wait().is_ok_and(|status| status.is_none()) {
                        Ok(child)
                    } else {
                        try_packaged = false;
                        start(exe)
                    }
                }
                Err(error) => {
                    log::line(&format!("could not restart the packaged supervisor: {error}"));
                    try_packaged = false;
                    start(exe)
                }
            }
        } else {
            start(exe)
        };
        match replacement {
            Ok(child) => {
                log::line(&format!("guard pid {pid} ended: started guard pid {} to adopt this shell", child.id()));
                pid = child.id();
                GUARD_PID.store(pid, Ordering::Relaxed);
            }
            Err(e) => {
                log::line(&format!("guard pid {pid} ended and a new guard failed to start: {e}"));
                return;
            }
        }
    }
}
