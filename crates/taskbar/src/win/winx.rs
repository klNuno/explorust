//! The Win+X menu: shows `crate::winx`'s items above Start and runs the chosen one. Opens and
//! launches run on a short-lived STA thread; power actions are dry in the dev exe.

use super::menu::{self, Node};
use super::log;
use crate::winx::{self as X, Act, Entry};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LUID};
use windows::Win32::Security::{
    AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED, SE_SHUTDOWN_NAME,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Power::SetSuspendState;
use windows::Win32::System::Shutdown::{
    EWX_LOGOFF, EWX_POWEROFF, EWX_REBOOT, EWX_SHUTDOWN, EXIT_WINDOWS_FLAGS, ExitWindowsEx, SHTDN_REASON_FLAG_PLANNED,
    SHTDN_REASON_MAJOR_OTHER, SHUTDOWN_REASON,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::Shell::{SEE_MASK_FLAG_NO_UI, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::{SW_SHOWNORMAL, TPM_BOTTOMALIGN, TPM_LEFTALIGN};
use windows_core::{HSTRING, PCSTR, PCWSTR, w};

static DRY_POWER: AtomicBool = AtomicBool::new(false);

/// Power items only log the call they would make (the dev exe, so a test never powers g1 off).
pub fn set_dry_power(dry: bool) {
    DRY_POWER.store(dry, Relaxed);
}

/// Shows the menu with its bottom-left corner at screen (x, y); the bus command to post when the
/// item belongs to another part.
pub fn show(owner: HWND, x: i32, y: i32) -> Option<bus::Command> {
    let fr = menu::french();
    let nodes: Vec<Node> = X::entries()
        .iter()
        .map(|e| match e {
            Entry::Item(i) => Node::Item(i.id, i.label(fr).to_string()),
            Entry::Separator => Node::Separator,
            Entry::Power { en, fr: f } => Node::Sub(
                (if fr { *f } else { *en }).to_string(),
                X::POWER.iter().map(|i| Node::Item(i.id, i.label(fr).to_string())).collect(),
            ),
        })
        .collect();
    let id = menu::track(owner, x, y, &nodes, TPM_LEFTALIGN | TPM_BOTTOMALIGN);
    let item = X::by_id(id)?;
    log::line(&format!("win+x picked {id} {:?}", item.act));
    match item.act {
        Act::Search => Some(bus::Command::Search),
        Act::Files => Some(bus::Command::FilesOpen),
        Act::Desktop => Some(bus::Command::ShowDesktop),
        act => {
            let _ = std::thread::Builder::new().name("taskbar-winx".into()).spawn(move || run(act));
            None
        }
    }
}

fn run(act: Act) {
    // SAFETY: COM for this short-lived thread, needed by ShellExecute and the Run dialog.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let ok = match act {
        Act::Open(target) => open(target, None, None),
        Act::Terminal { admin } => {
            let verb = admin.then_some("runas");
            open(r"shell:AppsFolder\Microsoft.WindowsTerminal_8wekyb3d8bbwe!App", verb, None)
                || open("powershell.exe", verb, None)
        }
        Act::Run => run_dialog(),
        Act::SignOut | Act::Sleep | Act::ShutDown | Act::Restart => power(act),
        Act::Search | Act::Files | Act::Desktop => false,
    };
    log::line(&format!("win+x {act:?}: {}", if ok { "ok" } else { "failed" }));
    // SAFETY: matches the CoInitializeEx above.
    unsafe { CoUninitialize() };
}

fn open(file: &str, verb: Option<&str>, args: Option<&str>) -> bool {
    let f = HSTRING::from(file);
    let v = verb.map(HSTRING::from);
    let a = args.map(HSTRING::from);
    let mut sei = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_FLAG_NO_UI,
        lpVerb: v.as_ref().map_or(w!("open"), |v| PCWSTR(v.as_ptr())),
        lpFile: PCWSTR(f.as_ptr()),
        lpParameters: a.as_ref().map_or(PCWSTR::null(), |a| PCWSTR(a.as_ptr())),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: the strings outlive the call.
    unsafe { ShellExecuteExW(&mut sei) }.is_ok()
}

type RunFileDlg = unsafe extern "system" fn(isize, isize, isize, isize, isize, u32) -> i32;

/// shell32's RunFileDlg (ordinal 61), the Run dialog of Win+R.
fn run_dialog() -> bool {
    // SAFETY: shell32 exports RunFileDlg as ordinal 61; modal on this thread.
    unsafe {
        let Ok(shell32) = LoadLibraryW(w!("shell32.dll")) else { return false };
        let Some(f) = GetProcAddress(shell32, PCSTR(61 as *const u8)) else { return false };
        let f: RunFileDlg = std::mem::transmute(f);
        f(0, 0, 0, 0, 0, 0);
    }
    true
}

fn power(act: Act) -> bool {
    let call = match act {
        Act::Sleep => "SetSuspendState(FALSE, FALSE, FALSE)",
        Act::ShutDown => "ExitWindowsEx(EWX_SHUTDOWN | EWX_POWEROFF) with SeShutdownPrivilege",
        Act::Restart => "ExitWindowsEx(EWX_REBOOT) with SeShutdownPrivilege",
        _ => "ExitWindowsEx(EWX_LOGOFF)",
    };
    if DRY_POWER.load(Relaxed) {
        log::line(&format!("power {act:?}: dry run, would call {call}"));
        return true;
    }
    let reason = SHTDN_REASON_MAJOR_OTHER | SHTDN_REASON_FLAG_PLANNED;
    // SAFETY: documented power and session calls.
    let r = unsafe {
        match act {
            Act::Sleep => SetSuspendState(false, false, false).then_some(()).ok_or_else(windows_core::Error::from_thread),
            Act::ShutDown => exit(EWX_SHUTDOWN | EWX_POWEROFF, reason),
            Act::Restart => exit(EWX_REBOOT, reason),
            _ => ExitWindowsEx(EWX_LOGOFF, Default::default()),
        }
    };
    r.is_ok()
}

unsafe fn exit(flags: EXIT_WINDOWS_FLAGS, reason: SHUTDOWN_REASON) -> windows_core::Result<()> {
    // SAFETY: enables SeShutdownPrivilege on our own token, then asks Windows to shut down.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token)?;
        let mut luid = LUID::default();
        LookupPrivilegeValueW(None, SE_SHUTDOWN_NAME, &mut luid)?;
        let tp = TOKEN_PRIVILEGES { PrivilegeCount: 1, Privileges: [LUID_AND_ATTRIBUTES { Luid: luid, Attributes: SE_PRIVILEGE_ENABLED }] };
        let adjusted = AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None);
        let _ = CloseHandle(token);
        adjusted?;
        ExitWindowsEx(flags, reason)
    }
}
