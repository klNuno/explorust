//! The TAP waits on the loader's process handle for exit. A new loader wakes existing TAPs
//! after publishing its message window, so Settings can reconnect without a new XAML site.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows::Win32::Foundation::{HANDLE, WAIT_TIMEOUT};
use windows::Win32::System::Threading::{
    CreateEventW, EVENT_MODIFY_STATE, OpenEventW, OpenProcess, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW, SetEvent,
    WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, GetWindowThreadProcessId, HWND_MESSAGE};
use windows_core::{HSTRING, PCWSTR, PWSTR};

fn event_name(pid: u32) -> HSTRING {
    HSTRING::from(format!(r"Local\explorust-settings-tap-{pid}"))
}

pub fn handle(owned: &OwnedHandle) -> HANDLE {
    HANDLE(owned.as_raw_handle())
}

pub fn tap_wake() -> windows_core::Result<OwnedHandle> {
    // SAFETY: the auto-reset event's successful handle is owned by this TAP session.
    unsafe {
        let event = CreateEventW(None, false, false, &event_name(std::process::id()))?;
        Ok(OwnedHandle::from_raw_handle(event.0))
    }
}

pub fn notify_tap(pid: u32) -> bool {
    // SAFETY: opens an existing event, signals it and closes our handle.
    unsafe {
        let Ok(event) = OpenEventW(EVENT_MODIFY_STATE, false, &event_name(pid)) else {
            return false;
        };
        let event = OwnedHandle::from_raw_handle(event.0);
        SetEvent(handle(&event)).is_ok()
    }
}

pub struct Loader(OwnedHandle);

impl Loader {
    pub fn connect(image: &str) -> Option<Self> {
        // SAFETY: the message-only window is in this session. Pin its owner with a limited
        // process handle and recheck the PID after opening it.
        unsafe {
            let class = HSTRING::from(crate::WINDOW_CLASS);
            let window = FindWindowExW(Some(HWND_MESSAGE), None, &class, PCWSTR::null()).ok()?;
            let mut pid = 0;
            if GetWindowThreadProcessId(window, Some(&mut pid)) == 0 {
                return None;
            }
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, false, pid).ok()?;
            let owner = Self(OwnedHandle::from_raw_handle(process.0));
            let mut name = vec![0u16; 32_768];
            let mut len = name.len() as u32;
            QueryFullProcessImageNameW(owner.handle(), PROCESS_NAME_WIN32, PWSTR(name.as_mut_ptr()), &mut len).ok()?;
            if !String::from_utf16_lossy(&name[..len as usize]).eq_ignore_ascii_case(image) {
                return None;
            }
            let mut current = 0;
            GetWindowThreadProcessId(window, Some(&mut current));
            (current == pid && owner.alive()).then_some(owner)
        }
    }

    pub fn handle(&self) -> HANDLE {
        handle(&self.0)
    }

    pub fn alive(&self) -> bool {
        // SAFETY: this process handle stays open while self is borrowed.
        (unsafe { WaitForSingleObject(self.handle(), 0) }) == WAIT_TIMEOUT
    }
}
