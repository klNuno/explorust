//! Where the keyboard hook (`crates/keys`) runs. Installed in Program Files, explorust starts
//! `explorust-keys.exe` beside it through ShellExecuteEx (the AppInfo service gives that signed
//! exe UI access, so keys reach it over elevated windows), restarts it when it dies and stops it on
//! quit and reload; explorust itself keeps a plain token, and so does every app it starts. In a dev
//! build outside Program Files, or when the exe cannot be kept running, the same hook runs in this
//! process.

use crate::log;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::Threading::{
    CreateEventW, GetExitCodeProcess, GetProcessId, INFINITE, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
};
use windows::Win32::UI::Shell::{SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::core::{HSTRING, PCWSTR};

const EXE: &str = "explorust-keys.exe";
/// This many deaths within `DEATH_WINDOW` is a problem (`problem`): explorer takes the session.
const MAX_DEATHS: usize = 3;
const DEATH_WINDOW: Duration = Duration::from_secs(60);

/// The quit event and the running exe, as raw handle values (handles are not Send).
static STATE: Mutex<(isize, isize)> = Mutex::new((0, 0));

/// Starts the hook: in `explorust-keys.exe` when explorust runs installed, else in-process.
pub fn start() {
    match installed_exe() {
        Some(exe) => {
            let spawned = std::thread::Builder::new().name("keys-host".into()).spawn(move || supervise(&exe));
            if let Err(e) = spawned {
                log::line(&format!("keys host thread: {e}"));
                in_process();
            }
        }
        None => in_process(),
    }
}

/// Asks `explorust-keys.exe` to exit and waits a moment for it (quit and reload).
pub fn stop() {
    let (quit, child) = *STATE.lock().unwrap_or_else(|e| e.into_inner());
    if quit == 0 {
        return;
    }
    // SAFETY: both handles stay open for the life of the process (never closed once stored).
    unsafe {
        let _ = SetEvent(HANDLE(quit as *mut _));
        if child != 0 && WaitForSingleObject(HANDLE(child as *mut _), 1000) == WAIT_OBJECT_0 {
            log::line("explorust-keys stopped");
        }
    }
}

fn in_process() {
    match keys::spawn(log::line) {
        Ok(()) => log::line("hotkeys armed in-process"),
        Err(e) => log::line(&format!("hotkeys: {e}")),
    }
}

/// `explorust-keys.exe` beside this exe when it runs from Program Files.
fn installed_exe() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let exe = dir.join(EXE);
    let under = |var: &str| {
        std::env::var_os(var).is_some_and(|base| {
            let base = base.to_string_lossy().to_lowercase();
            let dir = dir.to_string_lossy().to_lowercase();
            dir.starts_with(base.trim_end_matches('\\'))
        })
    };
    (exe.exists() && (under("ProgramW6432") || under("ProgramFiles"))).then_some(exe)
}

fn supervise(exe: &Path) {
    // SAFETY: COM for ShellExecuteEx on this thread.
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let name = HSTRING::from(format!(r"Local\explorust-keys-quit-{}", std::process::id()));
    // SAFETY: a manual-reset event this process owns for its whole life.
    let quit = match unsafe { CreateEventW(None, true, false, &name) } {
        Ok(h) => h,
        Err(e) => {
            log::line(&format!("keys quit event: {e}"));
            return in_process();
        }
    };
    STATE.lock().unwrap_or_else(|e| e.into_inner()).0 = quit.0 as isize;
    let mut deaths: Vec<Instant> = Vec::new();
    loop {
        let child = match launch(exe) {
            Ok(h) => h,
            Err(e) => {
                log::line(&format!("{e}: hotkeys in-process instead"));
                return in_process();
            }
        };
        STATE.lock().unwrap_or_else(|e| e.into_inner()).1 = child.0 as isize;
        // SAFETY: both handles are valid; the wait only reads them.
        let r = unsafe { WaitForMultipleObjects(&[child, quit], false, INFINITE) };
        if r != WAIT_OBJECT_0 {
            return;
        }
        let mut code = 0u32;
        // SAFETY: a process handle owned here; it is replaced, then closed.
        unsafe {
            let _ = GetExitCodeProcess(child, &mut code);
            STATE.lock().unwrap_or_else(|e| e.into_inner()).1 = 0;
            let _ = CloseHandle(child);
        }
        let now = Instant::now();
        deaths.retain(|t| now.duration_since(*t) < DEATH_WINDOW);
        deaths.push(now);
        if deaths.len() >= MAX_DEATHS {
            // Installed, the in-process hook would lose keys over elevated windows: a problem, so
            // the guard hands the session to explorer.
            log::line(&format!("explorust-keys ended {MAX_DEATHS} times in a minute (last code {code:#x})"));
            crate::problem::report(format!("the keyboard helper explorust-keys.exe stopped {MAX_DEATHS} times in a minute"));
            return;
        }
        log::line(&format!("explorust-keys ended (code {code:#x}): starting it again"));
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// ShellExecuteEx, not CreateProcess: a uiAccess exe needs the AppInfo service to start it.
fn launch(exe: &Path) -> Result<HANDLE, String> {
    let file = HSTRING::from(exe.as_os_str());
    let params = HSTRING::from(format!("--shell {}", std::process::id()));
    let cwd = exe.parent().map(|p| HSTRING::from(p.as_os_str()));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: windows::core::w!("open"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        lpDirectory: cwd.as_ref().map(|c| PCWSTR(c.as_ptr())).unwrap_or(PCWSTR::null()),
        nShow: 0,
        ..Default::default()
    };
    // SAFETY: every string outlives the call; hProcess is ours to close.
    unsafe { ShellExecuteExW(&mut info) }.map_err(|e| format!("ShellExecuteEx {}: {e}", exe.display()))?;
    if info.hProcess.is_invalid() {
        return Err(format!("ShellExecuteEx {}: no process handle", exe.display()));
    }
    // SAFETY: a valid process handle.
    let pid = unsafe { GetProcessId(info.hProcess) };
    log::line(&format!("explorust-keys started, pid {pid}"));
    Ok(info.hProcess)
}
