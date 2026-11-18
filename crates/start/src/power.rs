//! Sleep, shut down, restart, sign out and lock through Windows' own calls. The dev exe runs
//! them dry (logs the call it would make), so a test session never powers the guest off.

use crate::log;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use windows::Win32::Foundation::{CloseHandle, HANDLE, LUID};
use windows::Win32::Security::{
    AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED, SE_SHUTDOWN_NAME,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::System::Power::SetSuspendState;
use windows::Win32::System::Shutdown::{
    EWX_LOGOFF, EWX_POWEROFF, EWX_REBOOT, EWX_SHUTDOWN, EXIT_WINDOWS_FLAGS, ExitWindowsEx, LockWorkStation,
    SHTDN_REASON_FLAG_PLANNED, SHTDN_REASON_MAJOR_OTHER,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

static DRY: AtomicBool = AtomicBool::new(false);

/// Dry run: log the call instead of making it.
pub fn set_dry_run(dry: bool) {
    DRY.store(dry, Relaxed);
}

#[derive(Clone, Copy, Debug)]
pub enum Power {
    Sleep,
    Shutdown,
    Restart,
    SignOut,
    Lock,
}

impl Power {
    fn call(self) -> &'static str {
        match self {
            Power::Sleep => "SetSuspendState(FALSE, FALSE, FALSE)",
            Power::Shutdown => "ExitWindowsEx(EWX_SHUTDOWN | EWX_POWEROFF, SHTDN_REASON_MAJOR_OTHER | SHTDN_REASON_FLAG_PLANNED) with SeShutdownPrivilege",
            Power::Restart => "ExitWindowsEx(EWX_REBOOT, SHTDN_REASON_MAJOR_OTHER | SHTDN_REASON_FLAG_PLANNED) with SeShutdownPrivilege",
            Power::SignOut => "ExitWindowsEx(EWX_LOGOFF, 0)",
            Power::Lock => "LockWorkStation()",
        }
    }
}

pub fn run(p: Power) {
    if DRY.load(Relaxed) {
        log::line(&format!("power {p:?}: dry run, would call {}", p.call()));
        return;
    }
    log::line(&format!("power {p:?}: {}", p.call()));
    let reason = SHTDN_REASON_MAJOR_OTHER | SHTDN_REASON_FLAG_PLANNED;
    // SAFETY: documented power and session calls.
    let r = unsafe {
        match p {
            Power::Sleep => SetSuspendState(false, false, false).then_some(()).ok_or_else(windows_core::Error::from_thread),
            Power::Shutdown => exit(EWX_SHUTDOWN | EWX_POWEROFF, reason),
            Power::Restart => exit(EWX_REBOOT, reason),
            Power::SignOut => ExitWindowsEx(EWX_LOGOFF, Default::default()),
            Power::Lock => LockWorkStation(),
        }
    };
    if let Err(e) = r {
        log::line(&format!("power {p:?}: failed: {e}"));
    }
}

unsafe fn exit(flags: EXIT_WINDOWS_FLAGS, reason: windows::Win32::System::Shutdown::SHUTDOWN_REASON) -> windows_core::Result<()> {
    // SAFETY: enables SeShutdownPrivilege on our own token, then asks Windows to shut down.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token)?;
        let mut luid = LUID::default();
        LookupPrivilegeValueW(None, SE_SHUTDOWN_NAME, &mut luid)?;
        let tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES { Luid: luid, Attributes: SE_PRIVILEGE_ENABLED }],
        };
        let adjusted = AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None);
        let _ = CloseHandle(token);
        adjusted?;
        ExitWindowsEx(flags, reason)
    }
}
