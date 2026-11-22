//! The guard's keeper. If the guard that started explorust ends (killed from Task Manager, say),
//! nothing would bring explorer back on a crash and the escape hotkey would be gone, so explorust
//! starts a new guard that adopts it (`explorust-guard --adopt <pid>`), and watches that one.

use crate::log;
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
        let Ok(exe) = &guard else { return };
        let me = std::process::id().to_string();
        let mut command = std::process::Command::new(exe);
        command.args(["--adopt", &me]);
        if let Some(name) = &staged_event {
            command.arg(name);
        }
        match command.creation_flags(CREATE_NO_WINDOW).spawn() {
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
