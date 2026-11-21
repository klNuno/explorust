//! The registry values the guard owns: the per-user Winlogon `Shell` and its own state key.

use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, HKEY_USERS, KEY_READ, REG_DWORD, REG_SZ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
    RegCloseKey, RegDeleteKeyValueW, RegDeleteTreeW, RegEnumValueW, RegGetValueW, RegOpenKeyExW, RegSetKeyValueW,
};
use windows::core::{HSTRING, PWSTR};

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

fn set_dword(root: HKEY, key: &str, name: &str, value: u32) -> Result<(), String> {
    let (key, name) = (HSTRING::from(key), HSTRING::from(name));
    // SAFETY: a 4-byte DWORD.
    unsafe { RegSetKeyValueW(root, &key, &name, REG_DWORD.0, Some((&value as *const u32).cast()), 4) }
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

/// True when the previous logon of this user ended in a problem (`supervise` keeps it per logon).
pub fn last_logon_had_problem() -> bool {
    get_dword(HKEY_CURRENT_USER, STATE, "LastLogonProblem").unwrap_or(0) != 0
}

pub fn set_logon_had_problem(yes: bool) {
    if let Err(e) = set_dword(HKEY_CURRENT_USER, STATE, "LastLogonProblem", yes as u32) {
        crate::log::line(&format!("logon problem flag: {e}"));
    }
}

/// Logons in a row that ended in a problem (`policy::MAX_STREAK` turns explorust off).
pub fn problem_streak() -> u32 {
    get_dword(HKEY_CURRENT_USER, STATE, "ProblemStreak").unwrap_or(0)
}

pub fn set_problem_streak(n: u32) {
    if let Err(e) = set_dword(HKEY_CURRENT_USER, STATE, "ProblemStreak", n) {
        crate::log::line(&format!("problem streak: {e}"));
    }
}

/// The Apps & features entry, under HKLM (elevated only).
const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\explorust";

pub fn set_uninstall_entry(values: &[(&str, &str)], dwords: &[(&str, u32)]) -> Result<(), String> {
    for (n, v) in values {
        set_sz(HKEY_LOCAL_MACHINE, UNINSTALL, n, v)?;
    }
    for (n, v) in dwords {
        set_dword(HKEY_LOCAL_MACHINE, UNINSTALL, n, *v)?;
    }
    Ok(())
}

pub fn delete_uninstall_entry() -> Result<bool, String> {
    let key = HSTRING::from(UNINSTALL);
    // SAFETY: deletes a key this program created.
    let rc = unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, &key) };
    if rc == ERROR_FILE_NOT_FOUND {
        return Ok(false);
    }
    rc.ok().map(|_| true).map_err(|e| format!("delete the uninstall entry: {e}"))
}

pub fn has_uninstall_entry() -> bool {
    get_sz(HKEY_LOCAL_MACHINE, UNINSTALL, "UninstallString").is_some()
}

/// The accounts that turned explorust on, one value per SID under HKLM (elevated only): an
/// uninstall by one of them keeps the shared files while another still has its `Shell` value.
const MACHINE: &str = r"SOFTWARE\explorust";
const MACHINE_USERS: &str = r"SOFTWARE\explorust\Users";

pub fn add_machine_user(sid: &str) -> Result<(), String> {
    set_dword(HKEY_LOCAL_MACHINE, MACHINE_USERS, sid, 1)
}

pub fn remove_machine_user(sid: &str) -> Result<bool, String> {
    delete(HKEY_LOCAL_MACHINE, MACHINE_USERS, sid)
}

pub fn machine_users() -> Vec<String> {
    let mut out = Vec::new();
    let mut key = HKEY::default();
    // SAFETY: the key is opened for reading and closed; each name buffer has the length given.
    unsafe {
        if RegOpenKeyExW(HKEY_LOCAL_MACHINE, &HSTRING::from(MACHINE_USERS), None, KEY_READ, &mut key).is_err() {
            return out;
        }
        for i in 0.. {
            let mut name = vec![0u16; 256];
            let mut len = name.len() as u32;
            if RegEnumValueW(key, i, Some(PWSTR(name.as_mut_ptr())), &mut len, None, None, None, None).is_err() {
                break;
            }
            out.push(String::from_utf16_lossy(&name[..len as usize]));
        }
        let _ = RegCloseKey(key);
    }
    out
}

/// The `Shell` value of another account: None when its registry hive is not loaded (signed out),
/// Some(None) when loaded without the value.
pub fn user_shell(sid: &str) -> Option<Option<String>> {
    let mut key = HKEY::default();
    // SAFETY: opened for reading and closed.
    unsafe {
        if RegOpenKeyExW(HKEY_USERS, &HSTRING::from(sid), None, KEY_READ, &mut key).is_err() {
            return None;
        }
        let _ = RegCloseKey(key);
    }
    Some(get_sz(HKEY_USERS, &format!(r"{sid}\{WINLOGON}"), "Shell"))
}

/// Removes the machine-wide state key (the users list) once nothing is installed.
pub fn delete_machine_state() {
    let key = HSTRING::from(MACHINE);
    // SAFETY: deletes a key this program created.
    let _ = unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, &key) };
}

/// True when the account still has a profile on this computer (a deleted account's SID stays in
/// the users list otherwise and would keep the shared files forever).
pub fn profile_exists(sid: &str) -> bool {
    let path = HSTRING::from(format!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList\{sid}"));
    let mut key = HKEY::default();
    // SAFETY: opened for reading and closed.
    unsafe {
        if RegOpenKeyExW(HKEY_LOCAL_MACHINE, &path, None, KEY_READ, &mut key).is_err() {
            return false;
        }
        let _ = RegCloseKey(key);
    }
    true
}
