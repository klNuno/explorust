//! The registry values the guard owns: the per-user Winlogon `Shell` and its own state key.

use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, REG_SZ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegDeleteKeyValueW,
    RegGetValueW, RegSetKeyValueW,
};
use windows::core::HSTRING;

const WINLOGON: &str = r"Software\Microsoft\Windows NT\CurrentVersion\Winlogon";
const STATE: &str = r"Software\explorust";
const NT_VERSION: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";

fn get_sz(root: HKEY, key: &str, name: &str) -> Option<String> {
    let (key, name) = (HSTRING::from(key), HSTRING::from(name));
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: the buffer and its size in bytes match.
    let rc = unsafe {
        RegGetValueW(root, &key, &name, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut size))
    };
    if rc.is_err() {
        return None;
    }
    let len = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buf[..len]))
}

fn get_dword(root: HKEY, key: &str, name: &str) -> Option<u32> {
    let (key, name) = (HSTRING::from(key), HSTRING::from(name));
    let mut v = 0u32;
    let mut size = 4u32;
    // SAFETY: a DWORD destination of 4 bytes.
    let rc = unsafe {
        RegGetValueW(root, &key, &name, RRF_RT_REG_DWORD, None, Some((&mut v as *mut u32).cast()), Some(&mut size))
    };
    rc.is_ok().then_some(v)
}

fn set_sz(root: HKEY, key: &str, name: &str, value: &str) -> Result<(), String> {
    let (key, name) = (HSTRING::from(key), HSTRING::from(name));
    let data: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
    // SAFETY: data is a NUL-terminated UTF-16 string of the given byte length.
    unsafe {
        RegSetKeyValueW(root, &key, &name, REG_SZ.0, Some(data.as_ptr().cast()), (data.len() * 2) as u32)
    }
    .ok()
    .map_err(|e| format!("set {name}: {e}"))
}

fn delete(root: HKEY, key: &str, name: &str) -> Result<bool, String> {
    let (key, name) = (HSTRING::from(key), HSTRING::from(name));
    // SAFETY: plain value deletion.
    let rc = unsafe { RegDeleteKeyValueW(root, &key, &name) };
    if rc == ERROR_FILE_NOT_FOUND {
        return Ok(false);
    }
    rc.ok().map(|_| true).map_err(|e| format!("delete {name}: {e}"))
}

pub fn shell() -> Option<String> {
    get_sz(HKEY_CURRENT_USER, WINLOGON, "Shell")
}

pub fn set_shell(cmd: &str) -> Result<(), String> {
    set_sz(HKEY_CURRENT_USER, WINLOGON, "Shell", cmd)
}

pub fn delete_shell() -> Result<bool, String> {
    delete(HKEY_CURRENT_USER, WINLOGON, "Shell")
}

/// `26300.9457`: the build and its update revision, the key that changes with every Windows update.
pub fn windows_build() -> String {
    let build = get_sz(HKEY_LOCAL_MACHINE, NT_VERSION, "CurrentBuildNumber").unwrap_or_else(|| "?".into());
    let ubr = get_dword(HKEY_LOCAL_MACHINE, NT_VERSION, "UBR").unwrap_or(0);
    format!("{build}.{ubr}")
}

/// The last build on which explorust passed its self-test.
pub fn last_good_build() -> Option<String> {
    get_sz(HKEY_CURRENT_USER, STATE, "LastGoodBuild")
}

pub fn set_last_good_build(build: &str) -> Result<(), String> {
    set_sz(HKEY_CURRENT_USER, STATE, "LastGoodBuild", build)
}
