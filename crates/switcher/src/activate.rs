//! Bringing a window to the foreground from the shell. The hook that saw Alt+Tab runs in the shell,
//! and an empty mouse input from this process makes it the last one to send input, which lifts
//! the foreground lock (what PowerToys Run does); attaching to the foreground thread's input and
//! SwitchToThisWindow are the fallbacks. Each step is logged with the one that worked.

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND};
use windows::Win32::Security::{GetTokenInformation, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenIntegrityLevel};
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentProcess, GetCurrentThreadId, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT, INPUT_MOUSE, SendInput, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, BringWindowToTop, GA_ROOT, GetAncestor, GetForegroundWindow, GetWindowThreadProcessId,
    IsIconic, IsWindow, SW_RESTORE, SetForegroundWindow, ShowWindow, SwitchToThisWindow,
};

/// Asks the keys host (explorust-keys.exe, with UI access) to activate `h`: explorust at medium
/// integrity cannot move the foreground off an elevated window to a UWP frame. Activates here when
/// no keys host answers (a dev build with no hook). The caller checks the result.
pub fn request(h: HWND) -> &'static str {
    if bus::post(bus::Command::Activate, h.0 as isize) {
        return "explorust-keys";
    }
    activate(h)
}

/// The mandatory integrity level of a process token (its label's last sub-authority).
fn integrity(process: HANDLE) -> Option<u32> {
    // SAFETY: queries a token into a local buffer and closes it; the label's SID points into it.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(process, TOKEN_QUERY, &mut token).ok()?;
        let mut buf = [0u64; 16];
        let mut len = 0u32;
        let ok = GetTokenInformation(token, TokenIntegrityLevel, Some(buf.as_mut_ptr().cast()), 128, &mut len).is_ok();
        let _ = CloseHandle(token);
        if !ok {
            return None;
        }
        let label = &*(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL);
        let sid = label.Label.Sid;
        let n = *windows::Win32::Security::GetSidSubAuthorityCount(sid);
        Some(*windows::Win32::Security::GetSidSubAuthority(sid, n.saturating_sub(1) as u32))
    }
}

/// False when `fg` belongs to a process above this one's integrity level (or one we cannot
/// read): Windows then hides the key state from GetAsyncKeyState here.
pub fn key_state_readable(fg: HWND) -> bool {
    let mut pid = 0u32;
    // SAFETY: plain queries; the process handle is closed.
    unsafe {
        GetWindowThreadProcessId(fg, Some(&mut pid));
        if pid == 0 || pid == std::process::id() {
            return true;
        }
        let Ok(p) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
        let theirs = integrity(p);
        let _ = CloseHandle(p);
        match (theirs, integrity(GetCurrentProcess())) {
            (Some(t), Some(m)) => t <= m,
            _ => false,
        }
    }
}

pub fn in_front(h: HWND) -> bool {
    // SAFETY: plain queries.
    unsafe {
        let fg = GetForegroundWindow();
        fg == h || GetAncestor(fg, GA_ROOT) == h
    }
}

/// Restores `h` when minimized and makes it the foreground window. Returns the way that worked,
/// or `failed`.
pub fn activate(h: HWND) -> &'static str {
    // SAFETY: window manager calls on a handle; a stale one only fails them.
    unsafe {
        if !IsWindow(Some(h)).as_bool() {
            return "gone";
        }
        let input = INPUT { r#type: INPUT_MOUSE, ..Default::default() };
        SendInput(&[input], size_of::<INPUT>() as i32);
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        // Lets the target hand the foreground on (a UWP frame to its CoreWindow, an app to a dialog).
        let _ = AllowSetForegroundWindow(pid);
        if IsIconic(h).as_bool() {
            // SW_RESTORE activates too; it is the window's own restore with its animation.
            let _ = ShowWindow(h, SW_RESTORE);
        }
        if SetForegroundWindow(h).as_bool() && in_front(h) {
            return "SetForegroundWindow";
        }
        let fg = GetForegroundWindow();
        let ft = GetWindowThreadProcessId(fg, None);
        let me = GetCurrentThreadId();
        if ft != 0 && ft != me && AttachThreadInput(me, ft, true).as_bool() {
            let _ = BringWindowToTop(h);
            let _ = SetForegroundWindow(h);
            let _ = SetFocus(Some(h));
            let _ = AttachThreadInput(me, ft, false);
            if in_front(h) {
                return "AttachThreadInput";
            }
        }
        // Finishes asynchronously: the caller checks the result.
        SwitchToThisWindow(h, true);
        if in_front(h) { "SwitchToThisWindow" } else { "SwitchToThisWindow (pending)" }
    }
}
