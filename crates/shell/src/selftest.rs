//! The self-test run after a Windows update: open Calculator through the system activation
//! manager and wait until its frame is on screen (visible and not cloaked), then close it. It
//! proves the UWP host works on this build before the guard trusts explorust with the session.

use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, TRUE};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::System::Com::{CLSCTX_LOCAL_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
use windows::Win32::UI::Shell::{AO_NONE, ApplicationActivationManager, IApplicationActivationManager};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, GetClassNameW, GetWindowThreadProcessId, IsWindowVisible,
};
use windows::core::{BOOL, HSTRING};

const CALCULATOR: &str = "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App";
const FRAME_TIMEOUT: Duration = Duration::from_secs(45);

/// Milliseconds from activation to the frame on screen.
pub fn calculator() -> Result<u128, String> {
    std::thread::spawn(calculator_on_mta).join().map_err(|_| "self-test thread panicked".to_string())?
}

fn calculator_on_mta() -> Result<u128, String> {
    // SAFETY: this thread is ours; COM stays initialized until it ends.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok().map_err(|e| format!("COM: {e}"))?;
    // SAFETY: standard creation of the activation manager.
    let manager: IApplicationActivationManager =
        unsafe { CoCreateInstance(&ApplicationActivationManager, None, CLSCTX_LOCAL_SERVER) }
            .map_err(|e| format!("activation manager: {e}"))?;
    let start = Instant::now();
    // SAFETY: the AUMID and empty arguments are valid strings for the call.
    let pid = unsafe { manager.ActivateApplication(&HSTRING::from(CALCULATOR), &HSTRING::new(), AO_NONE) }
        .map_err(|e| format!("activate {CALCULATOR}: {e}"))?;
    let shown = loop {
        if frame_shown(pid) {
            break Ok(start.elapsed().as_millis());
        }
        if start.elapsed() > FRAME_TIMEOUT {
            break Err(format!("Calculator pid {pid} has no frame on screen after {} s", FRAME_TIMEOUT.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    close(pid);
    shown
}

fn class_is(hwnd: HWND, want: &str) -> bool {
    let mut buf = [0u16; 64];
    // SAFETY: the buffer outlives the call.
    let n = unsafe { GetClassNameW(hwnd, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n]) == want
}

fn cloaked(hwnd: HWND) -> bool {
    let mut v = 0u32;
    // SAFETY: DWMWA_CLOAKED writes one DWORD.
    let r = unsafe { DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, (&mut v as *mut u32).cast(), 4) };
    r.is_err() || v != 0
}

struct Search {
    pid: u32,
    found: bool,
}

unsafe extern "system" fn child(hwnd: HWND, lp: LPARAM) -> BOOL {
    // SAFETY: lp is the &mut Search passed by frame_shown for the duration of the enumeration.
    let s = unsafe { &mut *(lp.0 as *mut Search) };
    let mut owner = 0u32;
    // SAFETY: plain query.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut owner)) };
    if owner == s.pid && class_is(hwnd, "Windows.UI.Core.CoreWindow") {
        s.found = true;
        return false.into();
    }
    TRUE
}

unsafe extern "system" fn top(hwnd: HWND, lp: LPARAM) -> BOOL {
    // SAFETY: plain queries on a window handed out by EnumWindows.
    if class_is(hwnd, "ApplicationFrameWindow") && unsafe { IsWindowVisible(hwnd) }.as_bool() && !cloaked(hwnd) {
        // SAFETY: the same Search pointer, handed down to the child enumeration.
        let _ = unsafe { EnumChildWindows(Some(hwnd), Some(child), lp) };
        // SAFETY: as in `child`.
        if unsafe { &*(lp.0 as *const Search) }.found {
            return false.into();
        }
    }
    TRUE
}

/// True when a visible, uncloaked ApplicationFrameWindow hosts a CoreWindow of `pid`.
fn frame_shown(pid: u32) -> bool {
    let mut s = Search { pid, found: false };
    // SAFETY: `s` outlives the enumeration; the callbacks only read and write through it.
    let _ = unsafe { EnumWindows(Some(top), LPARAM(&mut s as *mut Search as isize)) };
    s.found
}

fn close(pid: u32) {
    // SAFETY: opens, terminates and closes a process handle of the Calculator we started.
    unsafe {
        if let Ok(h) = OpenProcess(PROCESS_TERMINATE, false, pid) {
            let _ = TerminateProcess(h, 0);
            let _ = CloseHandle(h);
        }
    }
}
