//! Named events: the ready signal the guard waits for and the quit request of `--quit`.

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{CreateEventW, EVENT_MODIFY_STATE, OpenEventW, ResetEvent, SetEvent, WaitForSingleObject};
use windows::Win32::System::Threading::SYNCHRONIZATION_SYNCHRONIZE;
use windows::core::HSTRING;

pub const QUIT_EVENT: &str = r"Local\explorust-quit";
pub use crate::launch_timing::RELOAD_EVENT;

/// Sets an event another process created (the guard's ready event, or a running shell's quit event).
pub fn set_existing(name: &str) -> Result<(), String> {
    // SAFETY: opens, sets and closes a handle owned here.
    unsafe {
        let h = OpenEventW(EVENT_MODIFY_STATE, false, &HSTRING::from(name)).map_err(|e| format!("{name}: {e}"))?;
        let r = SetEvent(h).map_err(|e| format!("{name}: {e}"));
        let _ = CloseHandle(h);
        r
    }
}

pub struct Owned(HANDLE);

impl Owned {
    /// A manual-reset event this process owns.
    pub fn create(name: &str) -> Result<Owned, String> {
        // SAFETY: plain event creation.
        unsafe { CreateEventW(None, true, false, &HSTRING::from(name)) }
            .map(Owned)
            .map_err(|e| format!("CreateEvent {name}: {e}"))
    }

    /// Opens a named event offered by the supervising guard without creating it.
    pub fn open_for_signal(name: &str) -> Option<Self> {
        // SAFETY: the returned handle is closed by the owner.
        unsafe { OpenEventW(EVENT_MODIFY_STATE, false, &HSTRING::from(name)) }.ok().map(Self)
    }

    pub fn wait(&self, ms: u32) -> bool {
        // SAFETY: the handle is valid while self lives.
        unsafe { WaitForSingleObject(self.0, ms) == WAIT_OBJECT_0 }
    }

    pub fn is_signalled(name: &str) -> bool {
        // SAFETY: the returned handle, when present, is closed by the owner.
        unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, false, &HSTRING::from(name)) }
            .is_ok_and(|handle| Self(handle).wait(0))
    }

    pub fn signal(&self) -> Result<(), String> {
        // SAFETY: the handle is valid while self lives.
        unsafe { SetEvent(self.0) }.map_err(|e| e.to_string())
    }

    pub fn reset(&self) -> Result<(), String> {
        // SAFETY: the handle is valid while self lives.
        unsafe { ResetEvent(self.0) }.map_err(|e| e.to_string())
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: closes the handle this struct owns.
        let _ = unsafe { CloseHandle(self.0) };
    }
}
