//! Who runs this process and who owns the session. A standard user who types an administrator's
//! password gets an elevated process of that other account, whose HKCU is not theirs: the
//! machine-wide steps may still run, the per-user ones (the Winlogon `Shell` value) must not.

use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, LookupAccountSidW, SID_NAME_USE, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows::Win32::System::RemoteDesktop::{
    WTS_CURRENT_SERVER_HANDLE, WTS_CURRENT_SESSION, WTS_INFO_CLASS, WTSDomainName, WTSFreeMemory, WTSQuerySessionInformationW,
    WTSUserName,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{PCWSTR, PWSTR};

/// Aligned storage for TOKEN_USER and the SID it points to.
fn token_user(token: HANDLE) -> Option<Vec<usize>> {
    // SAFETY: the buffer has at least the size the first call asks for.
    unsafe {
        let mut len = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        let mut buf = vec![0usize; (len as usize).div_ceil(size_of::<usize>())];
        GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), len, &mut len).ok()?;
        Some(buf)
    }
}

fn with_own_token<T>(read: impl FnOnce(HANDLE) -> Option<T>) -> Option<T> {
    let mut token = HANDLE::default();
    // SAFETY: the output is valid and the synchronous reader borrows the open handle.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.ok()?;
    let result = read(token);
    let _ = unsafe { CloseHandle(token) };
    result
}

/// `S-1-5-21-...` of the account this process runs as.
pub fn own_sid() -> Option<String> {
    with_own_token(token_sid)
}

pub fn token_sid(token: HANDLE) -> Option<String> {
    let buf = token_user(token)?;
    // SAFETY: the buffer holds a TOKEN_USER whose SID points inside it; the string is freed.
    unsafe {
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut s = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut s).ok()?;
        let out = s.to_string().ok();
        let _ = LocalFree(Some(HLOCAL(s.0.cast())));
        out
    }
}

/// `DOMAIN\name` of the account this process runs as.
fn own_account() -> Option<String> {
    let buf = with_own_token(token_user)?;
    // SAFETY: the SID points inside `buf`; both name buffers have the sizes given.
    unsafe {
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let (mut name, mut domain) = (vec![0u16; 256], vec![0u16; 256]);
        let (mut n, mut d) = (256u32, 256u32);
        let mut kind = SID_NAME_USE::default();
        LookupAccountSidW(
            PCWSTR::null(),
            user.User.Sid,
            Some(PWSTR(name.as_mut_ptr())),
            &mut n,
            Some(PWSTR(domain.as_mut_ptr())),
            &mut d,
            &mut kind,
        )
        .ok()?;
        Some(format!("{}\\{}", String::from_utf16_lossy(&domain[..d as usize]), String::from_utf16_lossy(&name[..n as usize])))
    }
}

fn session_info(class: WTS_INFO_CLASS) -> Option<String> {
    // SAFETY: WTS allocates the buffer; it is freed after the copy.
    unsafe {
        let mut p = PWSTR::null();
        let mut len = 0u32;
        WTSQuerySessionInformationW(Some(WTS_CURRENT_SERVER_HANDLE), WTS_CURRENT_SESSION, class, &mut p, &mut len).ok()?;
        let out = p.to_string().ok();
        WTSFreeMemory(p.0.cast());
        out
    }
}

/// `DOMAIN\name` of the user signed in to this session.
fn session_account() -> Option<String> {
    Some(format!("{}\\{}", session_info(WTSDomainName)?, session_info(WTSUserName)?))
}

/// False only when both are known and differ: an elevation with another account's password.
pub fn is_session_user() -> bool {
    match (own_account(), session_account()) {
        (Some(me), Some(session)) => {
            let same = me.eq_ignore_ascii_case(&session);
            if !same {
                crate::log::line(&format!("account: this process runs as {me}, the session belongs to {session}"));
            }
            same
        }
        _ => true,
    }
}
