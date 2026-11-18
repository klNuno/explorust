//! Named events: the ready signal the guard waits for and the quit request of `--quit`.

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{CreateEventW, EVENT_MODIFY_STATE, OpenEventW, SetEvent, WaitForSingleObject};
use windows::core::HSTRING;

pub const QUIT_EVENT: &str = r"Local\explorust-quit";

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

    pub fn wait(&self, ms: u32) -> bool {
        // SAFETY: the handle is valid while self lives.
        unsafe { WaitForSingleObject(self.0, ms) == WAIT_OBJECT_0 }
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: closes the handle this struct owns.
        let _ = unsafe { CloseHandle(self.0) };
    }
}
