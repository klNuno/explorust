//! Keep install independent of the network while bounding the background downloader's lifetime.

use crate::log;
use std::os::windows::io::AsRawHandle;
use std::process::Command;
use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::WaitForSingleObject;

const LIMIT_MS: u32 = 120_000;

pub fn run() -> i32 {
    let mut child = match std::env::current_exe()
        .and_then(|exe| Command::new(exe).arg("--download-symbols").spawn())
    {
        Ok(child) => child,
        Err(e) => {
            log::line(&format!("prefetch: could not start downloader: {e}"));
            return 1;
        }
    };
    // SAFETY: the handle belongs to the child for the duration of the wait.
    let wait = |child: &std::process::Child, limit| unsafe { WaitForSingleObject(HANDLE(child.as_raw_handle()), limit) } == WAIT_OBJECT_0;
    if !wait(&child, LIMIT_MS) {
        let _ = child.kill();
        log::line("prefetch: download time limit reached");
        // TerminateProcess is asynchronous. Give the worker time to close its files.
        if !wait(&child, 5_000) {
            return 1;
        }
    }
    match child.try_wait() {
        Ok(Some(status)) => {
            if !status.success() {
                uwp_host::clean_prefetch_downloads(child.id());
            }
            log::line(&format!("prefetch: downloader {status}"));
            i32::from(!status.success())
        }
        _ => 1,
    }
}
