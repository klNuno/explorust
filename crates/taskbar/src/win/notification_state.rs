//! The `Shell_TrayWnd` query sent by shell32's `SHQueryUserNotificationState`.

use windows::Win32::Foundation::{WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::System::Threading::{
    OpenEventW, OpenMutexW, ReleaseMutex, SYNCHRONIZATION_SYNCHRONIZE, WaitForSingleObject,
};
use windows::Win32::UI::Shell::{
    QUERY_USER_NOTIFICATION_STATE, QUNS_ACCEPTS_NOTIFICATIONS, QUNS_BUSY, QUNS_NOT_PRESENT,
    QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    SPI_GETSCREENSAVERRUNNING, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
    WTS_CONSOLE_CONNECT, WTS_CONSOLE_DISCONNECT, WTS_REMOTE_CONNECT, WTS_REMOTE_DISCONNECT,
    WTS_SESSION_LOCK, WTS_SESSION_LOGOFF, WTS_SESSION_LOGON, WTS_SESSION_UNLOCK,
};
use windows_core::{BOOL, Owned, w};

pub const QUERY: u32 = 0x04ef;

#[derive(Default)]
pub struct Session {
    locked: bool,
    disconnected: bool,
}

impl Session {
    pub fn changed(&mut self, event: u32) {
        match event {
            WTS_SESSION_LOCK => self.locked = true,
            WTS_SESSION_UNLOCK => self.locked = false,
            WTS_CONSOLE_DISCONNECT | WTS_REMOTE_DISCONNECT | WTS_SESSION_LOGOFF => {
                self.disconnected = true
            }
            WTS_CONSOLE_CONNECT | WTS_REMOTE_CONNECT | WTS_SESSION_LOGON => {
                self.disconnected = false
            }
            _ => {}
        }
    }

    pub fn inactive(&self) -> bool {
        self.locked || self.disconnected
    }
}

pub fn query(
    session_inactive: bool,
    fullscreen_on_taskbar_monitor: impl FnOnce() -> bool,
) -> QUERY_USER_NOTIFICATION_STATE {
    if session_inactive || screen_saver_running() {
        QUNS_NOT_PRESENT
    } else if presenting() {
        QUNS_PRESENTATION_MODE
    } else if directdraw_exclusive() {
        QUNS_RUNNING_D3D_FULL_SCREEN
    } else if fullscreen_on_taskbar_monitor() {
        QUNS_BUSY
    } else {
        QUNS_ACCEPTS_NOTIFICATIONS
    }
}

fn screen_saver_running() -> bool {
    let mut running = BOOL(0);
    // SAFETY: this SPI action writes one BOOL into the supplied output buffer.
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETSCREENSAVERRUNNING,
            0,
            Some((&mut running as *mut BOOL).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    running.as_bool()
}

fn presenting() -> bool {
    // SAFETY: Presentation Settings keeps this event open while presenting. Close our copy on drop.
    unsafe {
        OpenEventW(
            SYNCHRONIZATION_SYNCHRONIZE,
            false,
            w!("{A1965210-3A9D-4bca-822B-433645B3F5A2}"),
        )
        .map(|event| Owned::new(event))
        .is_ok()
    }
}

fn directdraw_exclusive() -> bool {
    let access = SYNCHRONIZATION_SYNCHRONIZE;
    // SAFETY: these are session-local mutexes used by DirectDraw. Acquire the check mutex before
    // testing ownership of the exclusive mutex. Release every acquisition before closing handles.
    unsafe {
        let Ok(gate) = OpenMutexW(access, false, w!(r"Local\__DDrawCheckExclMode__")) else {
            return false;
        };
        let gate = Owned::new(gate);
        if !matches!(
            WaitForSingleObject(*gate, 30),
            WAIT_OBJECT_0 | WAIT_ABANDONED
        ) {
            return false;
        }
        let mut exclusive = false;
        if let Ok(mode) = OpenMutexW(access, false, w!(r"Local\__DDrawExclMode__")) {
            let mode = Owned::new(mode);
            match WaitForSingleObject(*mode, 0) {
                WAIT_TIMEOUT => exclusive = true,
                WAIT_OBJECT_0 | WAIT_ABANDONED => {
                    let _ = ReleaseMutex(*mode);
                }
                _ => {}
            }
        }
        let _ = ReleaseMutex(*gate);
        exclusive
    }
}
