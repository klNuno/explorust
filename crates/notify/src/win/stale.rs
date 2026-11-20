//! A ShellExperienceHost started for a previous explorust instance (before a reload or a crash
//! restart) stays up, and the new instance's twinui cannot drive it: Win+N and Win+Alt+D send the
//! toggle but the center never shows (g1, 2026-11-20). Ending it at start lets twinui start a
//! fresh one on the first use, which shows (`captures/n12/n12-b1-winn.png`).

use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    TerminateProcess,
};

fn created(process: HANDLE) -> Option<u64> {
    let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: a valid process handle with query rights and four local outputs.
    unsafe { GetProcessTimes(process, &mut c, &mut e, &mut k, &mut u) }.ok()?;
    Some((u64::from(c.dwHighDateTime) << 32) | u64::from(c.dwLowDateTime))
}

/// Ends the ShellExperienceHost processes of this session that started before this process.
pub fn end_stale_host() -> String {
    // SAFETY: the pseudo handle of this process.
    let Some(ours) = created(unsafe { GetCurrentProcess() }) else { return "own start time unknown".into() };
    let mut session = 0;
    // SAFETY: plain query of this process's session.
    let _ = unsafe { ProcessIdToSessionId(std::process::id(), &mut session) };
    // SAFETY: a process snapshot walked and closed here.
    let Ok(snap) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else { return "no process snapshot".into() };
    let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
    let mut ended = Vec::new();
    let mut ok = unsafe { Process32FirstW(snap, &mut entry) }.is_ok();
    while ok {
        let len = entry.szExeFile.iter().position(|c| *c == 0).unwrap_or(entry.szExeFile.len());
        let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
        let pid = entry.th32ProcessID;
        let mut their_session = u32::MAX;
        // SAFETY: plain query by process id.
        let _ = unsafe { ProcessIdToSessionId(pid, &mut their_session) };
        if name.eq_ignore_ascii_case("ShellExperienceHost.exe") && their_session == session {
            // SAFETY: a handle opened, used and closed here.
            if let Ok(h) = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE, false, pid) } {
                if created(h).is_some_and(|t| t < ours) {
                    let r = unsafe { TerminateProcess(h, 1) };
                    ended.push(format!("{pid} ({})", if r.is_ok() { "ended" } else { "not ended" }));
                }
                let _ = unsafe { CloseHandle(h) };
            }
        }
        ok = unsafe { Process32NextW(snap, &mut entry) }.is_ok();
    }
    let _ = unsafe { CloseHandle(snap) };
    if ended.is_empty() { "no stale ShellExperienceHost".into() } else { format!("stale ShellExperienceHost {}", ended.join(", ")) }
}
