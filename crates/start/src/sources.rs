//! The in-memory sources of search, loaded once off the UI thread: Settings pages resolved in the
//! UI language, the usage counts of apps; and the actions on a result (open, run as
//! administrator, open file location, copy).

use crate::settings::{self, RawSetting};
use crate::{log, search};
use std::path::PathBuf;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::UI::Shell::{SEE_MASK_FLAG_NO_UI, SHELLEXECUTEINFOW, SHLoadIndirectString, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows_core::{HSTRING, PCWSTR};

pub struct Setting {
    pub name: String,
    pub link: String,
    pub keywords: Vec<String>,
}

fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(a) = rest.find('%') {
        let Some(b) = rest[a + 1..].find('%') else { break };
        let var = &rest[a + 1..a + 1 + b];
        out.push_str(&rest[..a]);
        match std::env::var(var) {
            Ok(v) => out.push_str(&v),
            Err(_) => out.push_str(&rest[a..a + b + 2]),
        }
        rest = &rest[a + b + 2..];
    }
    out.push_str(rest);
    out
}

/// An indirect string (`@dll,-id`, `@{pri?ms-resource:...}`) in the UI language, or the text itself.
fn resolve(s: &str) -> Option<String> {
    if !s.starts_with('@') {
        return Some(s.to_string());
    }
    let src = HSTRING::from(expand_env(s));
    let mut buf = [0u16; 1024];
    // SAFETY: the buffer outlives the call.
    unsafe { SHLoadIndirectString(&src, &mut buf, None) }.ok()?;
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..n])).filter(|s| !s.is_empty())
}

fn read_text(p: &PathBuf) -> Option<String> {
    let b = std::fs::read(p).ok()?;
    if b.starts_with(&[0xFF, 0xFE]) {
        let w: Vec<u16> = b[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        return Some(String::from_utf16_lossy(&w));
    }
    Some(String::from_utf8_lossy(b.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&b)).into_owned())
}

/// Every Settings page of `%WINDIR%\ImmersiveControlPanel\Settings\AllSystemSettings_*.xml`.
pub fn load_settings() -> Vec<Setting> {
    let win = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    let dir = PathBuf::from(win).join("ImmersiveControlPanel").join("Settings");
    let mut raw: Vec<RawSetting> = Vec::new();
    let mut files = 0;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with("AllSystemSettings") && name.ends_with(".xml") {
                if let Some(t) = read_text(&e.path()) {
                    files += 1;
                    raw.extend(settings::parse(&t));
                }
            }
        }
    }
    let mut out: Vec<Setting> = Vec::new();
    let mut unresolved = 0;
    for r in raw {
        let Some(name) = resolve(&r.name) else {
            unresolved += 1;
            continue;
        };
        if out.iter().any(|s| s.name.eq_ignore_ascii_case(&name) && s.link == r.link) {
            continue;
        }
        let keywords = r.keywords.iter().filter_map(|k| resolve(k)).flat_map(|k| settings::split_keywords(&k)).collect();
        out.push(Setting { name, link: r.link, keywords });
    }
    let sample: Vec<String> = out.iter().filter(|s| s.name.to_lowercase().contains("display")).take(3).map(|s| format!("{} -> {}", s.name, s.link)).collect();
    log::line(&format!("settings: sample {}", sample.join(" | ")));
    log::line(&format!("settings: {} pages from {files} files in {}, {unresolved} names not resolved", out.len(), dir.display()));
    out
}

/// Words an app is also found by: its exe or package name (`cmd`, `msedge`, `WindowsCalculator`).
pub fn app_keywords(aumid: &str) -> Vec<String> {
    let mut out = Vec::new();
    let last = aumid.rsplit(['\\', '/']).next().unwrap_or(aumid);
    let stem = last.split('!').next().unwrap_or(last);
    let stem = stem.split('_').next().unwrap_or(stem);
    let stem = stem.strip_suffix(".exe").unwrap_or(stem);
    if let Some(word) = stem.rsplit('.').next().filter(|w| w.len() > 1) {
        out.push(word.to_string());
    }
    out
}

pub fn usage_path() -> PathBuf {
    log::dir().join("start-usage.tsv")
}

pub fn load_usage() -> Vec<(String, u32)> {
    std::fs::read_to_string(usage_path()).map(|t| search::usage_from_tsv(&t)).unwrap_or_default()
}

pub fn save_usage(u: &[(String, u32)]) {
    let _ = std::fs::write(usage_path(), search::usage_to_tsv(u));
}

/// ShellExecuteEx with a verb (`runas` for run as administrator).
pub fn shell_execute(file: &str, verb: Option<&str>, params: Option<&str>) -> Result<(), String> {
    let f = HSTRING::from(file);
    let v = verb.map(HSTRING::from);
    let p = params.map(HSTRING::from);
    let mut sei = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_FLAG_NO_UI,
        hwnd: HWND::default(),
        lpVerb: v.as_ref().map_or(PCWSTR::null(), |v| PCWSTR(v.as_ptr())),
        lpFile: PCWSTR(f.as_ptr()),
        lpParameters: p.as_ref().map_or(PCWSTR::null(), |p| PCWSTR(p.as_ptr())),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: the struct and its strings live through the call.
    unsafe { ShellExecuteExW(&mut sei) }.map_err(|e| format!("ShellExecuteEx: {e}"))
}

/// Puts text on the clipboard (the calculator's result).
pub fn copy_text(owner: HWND, text: &str) -> bool {
    let w: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    // SAFETY: the clipboard owns the global memory once SetClipboardData succeeds.
    unsafe {
        if OpenClipboard(Some(owner)).is_err() {
            return false;
        }
        let _ = EmptyClipboard();
        let mut ok = false;
        if let Ok(g) = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2) {
            let p = GlobalLock(g) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
                let _ = GlobalUnlock(g);
                ok = SetClipboardData(13, Some(HANDLE(g.0))).is_ok(); // CF_UNICODETEXT
            }
            if !ok {
                let _ = GlobalFree(Some(HGLOBAL(g.0)));
            }
        }
        let _ = CloseClipboard();
        ok
    }
}
