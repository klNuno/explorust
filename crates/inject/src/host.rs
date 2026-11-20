//! Which processes the loader may put the DLL into: same user, x64, not elevated, not protected,
//! integrity at most explorust's own. A process whose token cannot be read is skipped.

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_ELEVATION, TOKEN_MANDATORY_LABEL,
    TOKEN_QUERY, TokenElevation, TokenIntegrityLevel,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, IsWow64Process, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::core::{BOOL, PWSTR};

pub struct Host {
    pub exe: String,
    /// Why the DLL must not enter it, if so.
    pub refused: Option<&'static str>,
}

/// Elevation and integrity RID of a process handle's token.
fn token_of(process: HANDLE) -> Option<(bool, u32)> {
    // SAFETY: a query-only token handle, closed before returning; the buffers outlive the calls.
    unsafe {
        let mut tok = HANDLE::default();
        OpenProcessToken(process, TOKEN_QUERY, &mut tok).ok()?;
        let mut el = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let e = GetTokenInformation(tok, TokenElevation, Some(&mut el as *mut _ as _), 4, &mut len);
        let mut buf = [0u64; 16];
        let i = GetTokenInformation(tok, TokenIntegrityLevel, Some(buf.as_mut_ptr() as _), 128, &mut len);
        let _ = CloseHandle(tok);
        e.ok()?;
        i.ok()?;
        let label = &*(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL);
        let sid = label.Label.Sid;
        let n = *GetSidSubAuthorityCount(sid);
        if n == 0 {
            return None;
        }
        Some((el.TokenIsElevated != 0, *GetSidSubAuthority(sid, n as u32 - 1)))
    }
}

pub fn host(pid: u32) -> Host {
    // SAFETY: a limited query handle, closed before returning.
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return Host { exe: String::new(), refused: Some("process not readable") };
        };
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let exe = if QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok() {
            String::from_utf16_lossy(&buf[..len as usize])
        } else {
            String::new()
        };
        let mut wow = BOOL(0);
        let _ = IsWow64Process(h, &mut wow);
        let theirs = token_of(h);
        let _ = CloseHandle(h);
        let ours = token_of(GetCurrentProcess());
        let refused = crate::target::refusal(wow.as_bool(), theirs, ours.map(|t| t.1));
        Host { exe, refused }
    }
}
