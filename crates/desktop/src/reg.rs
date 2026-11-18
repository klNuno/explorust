//! Reads of the current user's settings that Windows keeps in the registry.

use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW};
use windows_core::HSTRING;

pub fn string(key: &str, value: &str) -> Option<String> {
    let mut buf = [0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    // SAFETY: the buffer and its byte length are valid for the call.
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            &HSTRING::from(value),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
    };
    if r.is_err() {
        return None;
    }
    let n = (len as usize / 2).min(buf.len());
    let s = String::from_utf16_lossy(&buf[..n]);
    Some(s.trim_end_matches('\0').to_string())
}

pub fn dword(key: &str, value: &str) -> Option<u32> {
    let mut v = 0u32;
    let mut len = 4u32;
    // SAFETY: a DWORD output and its size.
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            &HSTRING::from(value),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut v as *mut u32).cast()),
            Some(&mut len),
        )
    };
    r.is_ok().then_some(v)
}

pub fn binary(key: &str, value: &str) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; 4096];
    let mut len = buf.len() as u32;
    // SAFETY: the buffer and its byte length are valid for the call.
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            &HSTRING::from(value),
            RRF_RT_REG_BINARY,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
    };
    r.is_ok().then(|| {
        buf.truncate(len as usize);
        buf
    })
}
