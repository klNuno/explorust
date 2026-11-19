//! Startup apps: the Run keys of the user and the machine (and WOW6432Node) and the Startup
//! folders, with their state from StartupApproved, as Task Manager lists them.

use super::log;
use crate::parse::approved;
use windows::Win32::System::Registry::*;
use windows_core::{HSTRING, PWSTR};

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub command: String,
    pub source: &'static str,
    pub enabled: bool,
    root: HKEY,
    approved_key: &'static str,
}

const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved";

fn values(root: HKEY, path: &str) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(root, &HSTRING::from(path), None, KEY_READ, &mut key).is_err() {
            return out;
        }
        for i in 0.. {
            let mut name = [0u16; 512];
            let mut nlen = name.len() as u32;
            let mut data = vec![0u8; 4096];
            let mut dlen = data.len() as u32;
            let r = RegEnumValueW(key, i, Some(PWSTR(name.as_mut_ptr())), &mut nlen, None, None, Some(data.as_mut_ptr()), Some(&mut dlen));
            if r.is_err() {
                break;
            }
            data.truncate(dlen as usize);
            out.push((String::from_utf16_lossy(&name[..nlen as usize]), data));
        }
        let _ = RegCloseKey(key);
    }
    out
}

fn sz(data: &[u8]) -> String {
    let w: Vec<u16> = data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
    String::from_utf16_lossy(&w)
}

pub fn list() -> Vec<Entry> {
    let mut out = Vec::new();
    let runs: [(HKEY, &str, &'static str, &'static str); 3] = [
        (HKEY_CURRENT_USER, r"Software\Microsoft\Windows\CurrentVersion\Run", "Registry (user)", "Run"),
        (HKEY_LOCAL_MACHINE, r"Software\Microsoft\Windows\CurrentVersion\Run", "Registry (machine)", "Run"),
        (HKEY_LOCAL_MACHINE, r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run", "Registry (machine, 32-bit)", "Run32"),
    ];
    for (root, path, source, ak) in runs {
        let state = values(root, &format!(r"{APPROVED}\{ak}"));
        for (name, data) in values(root, path) {
            let st = state.iter().find(|(n, _)| n.eq_ignore_ascii_case(&name)).map(|(_, d)| d.as_slice());
            out.push(Entry { command: sz(&data), enabled: approved(st), name, source, root, approved_key: ak });
        }
    }
    let folders = [
        (std::env::var("APPDATA").unwrap_or_default() + r"\Microsoft\Windows\Start Menu\Programs\Startup", HKEY_CURRENT_USER, "Startup folder (user)"),
        (std::env::var("ProgramData").unwrap_or_default() + r"\Microsoft\Windows\Start Menu\Programs\Startup", HKEY_LOCAL_MACHINE, "Startup folder (all users)"),
    ];
    for (dir, root, source) in folders {
        let state = values(root, &format!(r"{APPROVED}\StartupFolder"));
        for f in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = f.file_name().to_string_lossy().to_string();
            if name.eq_ignore_ascii_case("desktop.ini") {
                continue;
            }
            let st = state.iter().find(|(n, _)| n.eq_ignore_ascii_case(&name)).map(|(_, d)| d.as_slice());
            out.push(Entry { command: f.path().display().to_string(), enabled: approved(st), name, source, root, approved_key: "StartupFolder" });
        }
    }
    out.sort_by_key(|e| e.name.to_lowercase());
    out
}

/// Turns an entry on or off the way Task Manager does: a 12-byte StartupApproved value, first
/// byte 2 (enabled) or 3 (disabled) followed by the time of the change.
pub fn set(e: &Entry, enabled: bool) -> bool {
    let mut v = [0u8; 12];
    v[0] = if enabled { 2 } else { 3 };
    if !enabled {
        let t = unsafe { windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime() };
        v[4..8].copy_from_slice(&t.dwLowDateTime.to_le_bytes());
        v[8..12].copy_from_slice(&t.dwHighDateTime.to_le_bytes());
    }
    let key = HSTRING::from(format!(r"{APPROVED}\{}", e.approved_key));
    let r = unsafe { RegSetKeyValueW(e.root, &key, &HSTRING::from(e.name.as_str()), REG_BINARY.0, Some(v.as_ptr().cast()), 12) };
    log::line(&format!("startup {} -> {enabled}: {r:?}", e.name));
    r.is_ok()
}
