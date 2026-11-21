//! The named pipe: one instance per client connection, each served on a thread; the ACL lets
//! SYSTEM and administrators in fully and the logged-on user read and write (not create an
//! instance), and remote clients are refused.

use crate::log;
use fsindex::client::ACCESS;
use fsindex::proto::{self, Reply};
use std::fs::File;
use std::os::windows::io::FromRawHandle;
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows::Win32::Security::{GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT};
use windows::Win32::System::RemoteDesktop::{WTSGetActiveConsoleSessionId, WTSQueryUserToken};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_core::{HSTRING, PWSTR};

pub type Handler = Arc<dyn Fn(&str, u16) -> Reply + Send + Sync>;

fn token_sid(token: HANDLE) -> Option<String> {
    // SAFETY: two-call token query into a local buffer; the string is freed with LocalFree.
    unsafe {
        let mut need = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut need);
        let mut buf = vec![0u8; need as usize];
        GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), need, &mut need).ok()?;
        let tu = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut s = PWSTR::null();
        ConvertSidToStringSidW(tu.User.Sid, &mut s).ok()?;
        let r = s.to_string().ok();
        let _ = LocalFree(Some(HLOCAL(s.0.cast())));
        r
    }
}

/// The SID of the user at the console, else this process's user unless that is LocalSystem (the
/// service before anyone logged on: None, so interactive users get in until the logon refresh).
fn user_sid() -> Option<String> {
    // SAFETY: token queries; the tokens are closed.
    unsafe {
        let mut t = HANDLE::default();
        if WTSQueryUserToken(WTSGetActiveConsoleSessionId(), &mut t).is_ok() {
            let s = token_sid(t);
            let _ = CloseHandle(t);
            if s.is_some() {
                return s;
            }
        }
        let mut t = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut t).ok()?;
        let s = token_sid(t);
        let _ = CloseHandle(t);
        s.filter(|s| s != "S-1-5-18")
    }
}

/// The SDDL of the instance waiting for a client.
static PENDING: Mutex<String> = Mutex::new(String::new());

/// Called when a user logs on (and on any session change): when the waiting instance's ACL names
/// someone else, the service connects to it once itself, so the next instance is made with the
/// new user's SID.
pub fn refresh() {
    let now = sddl();
    let old = PENDING.lock().map(|p| p.clone()).unwrap_or_default();
    if old.is_empty() || old == now {
        return;
    }
    let r = std::fs::OpenOptions::new().read(true).write(true).open(fsindex::PIPE);
    log::line(&format!("pipe: session change, ACL {old} -> {now}, waiting instance recycled: {}", if r.is_ok() { "yes".to_string() } else { format!("{:?}", r.err()) }));
}

fn sddl() -> String {
    // Before anyone logs on: interactive users.
    let who = user_sid().unwrap_or_else(|| "IU".into());
    format!("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x{ACCESS:x};;;{who})")
}

fn create(first: bool) -> Option<HANDLE> {
    let text = sddl();
    let mut sd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: the descriptor is freed after the pipe instance holds its own copy.
    unsafe {
        if let Err(e) = ConvertStringSecurityDescriptorToSecurityDescriptorW(&HSTRING::from(text.as_str()), SDDL_REVISION_1, &mut sd, None) {
            log::line(&format!("pipe: SDDL {text}: {e}"));
            return None;
        }
        let sa = SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd.0, bInheritHandle: false.into() };
        let mode = PIPE_ACCESS_DUPLEX | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { Default::default() };
        let h = CreateNamedPipeW(
            &HSTRING::from(fsindex::PIPE),
            mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            64 << 10,
            4 << 10,
            0,
            Some(&sa),
        );
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        if h.is_invalid() {
            log::line(&format!("pipe: create failed: {}", windows_core::Error::from_thread()));
            return None;
        }
        if first {
            log::line(&format!("pipe: {} listening, {text}", fsindex::PIPE));
        }
        if let Ok(mut p) = PENDING.lock() {
            *p = text;
        }
        Some(h)
    }
}

/// Accepts clients forever.
pub fn serve(handler: Handler) {
    let mut first = true;
    loop {
        let Some(h) = create(first) else {
            std::thread::sleep(std::time::Duration::from_secs(2));
            continue;
        };
        first = false;
        // SAFETY: waits for a client on the instance just created.
        let r = unsafe { ConnectNamedPipe(h, None) };
        if let Err(e) = r {
            if e.code() != ERROR_PIPE_CONNECTED.to_hresult() {
                // SAFETY: closes the unused instance.
                let _ = unsafe { CloseHandle(h) };
                continue;
            }
        }
        let hd = handler.clone();
        let raw = h.0 as isize;
        let _ = std::thread::Builder::new().name("index-client".into()).spawn(move || {
            // SAFETY: the File takes the instance handle and closes it when the client leaves.
            let mut f = unsafe { File::from_raw_handle(raw as _) };
            while let Ok((q, max, version)) = proto::read_request(&mut f) {
                if proto::write_reply(&mut f, &hd(&q, max), version).is_err() {
                    break;
                }
            }
        });
    }
}
