//! The loader's side, in explorust: asks Settings' XAML runtime to load the TAP. The call
//! (`InitializeXamlDiagnosticsEx`, exported by Windows.UI.Xaml.dll) waits for the target to answer,
//! so it runs on a thread of its own; each Settings process gets it once.

use crate::log;
use std::path::{Path, PathBuf};
use std::time::Instant;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::core::{GUID, HRESULT, HSTRING, PCWSTR, s, w};

type InitializeXamlDiagnosticsEx =
    unsafe extern "system" fn(PCWSTR, u32, PCWSTR, PCWSTR, GUID, PCWSTR) -> HRESULT;

/// HRESULT_FROM_WIN32(ERROR_NOT_FOUND): that connection name is taken, try the next.
const NOT_FOUND: HRESULT = HRESULT(0x8007_0490_u32 as i32);

fn base() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("explorust")
}

/// `parts-off.txt` says `settingsxaml`.
pub fn switched_off() -> bool {
    std::fs::read_to_string(base().join("parts-off.txt")).map(|s| super::switched_off(&s)).unwrap_or(false)
}

/// The rules sent to the TAP: `settings-style.txt`, plus the tree dump when a file
/// `settings-xaml-dump` sits beside it. None when neither exists: the TAP is then not loaded and
/// Settings keeps Windows' own look (its Mica), the default.
pub fn rules_text() -> Option<String> {
    let dir = base();
    let style = std::fs::read_to_string(dir.join("settings-style.txt")).ok();
    let dump = dir.join("settings-xaml-dump").exists();
    if style.is_none() && !dump {
        return None;
    }
    let mut text = style.unwrap_or_default();
    if dump {
        text.push_str("\n!dump\n");
    }
    Some(text)
}

/// Loads the TAP into the Settings process `pid`, from `dll` (explorust_inject.dll beside
/// explorust), with these rules.
pub fn attach(pid: u32, dll: &Path, rules: String) {
    let dll = dll.to_path_buf();
    let _ = std::thread::Builder::new().name("inject-xaml".into()).spawn(move || {
        let t0 = Instant::now();
        // SAFETY: a system DLL; the export has the documented signature (xamlom.h).
        let f = unsafe {
            let Ok(m) = LoadLibraryW(w!("Windows.UI.Xaml.dll")) else {
                log::line("settings xaml: Windows.UI.Xaml.dll not loaded");
                return;
            };
            match GetProcAddress(m, s!("InitializeXamlDiagnosticsEx")) {
                Some(p) => std::mem::transmute::<unsafe extern "system" fn() -> isize, InitializeXamlDiagnosticsEx>(p),
                None => {
                    log::line("settings xaml: no InitializeXamlDiagnosticsEx");
                    return;
                }
            }
        };
        let path = HSTRING::from(dll.as_os_str());
        let rules = HSTRING::from(rules);
        let mut hr = NOT_FOUND;
        let mut n = 0;
        for i in 1..=10 {
            n = i;
            let conn = HSTRING::from(format!("VisualDiagConnection{i}"));
            // SAFETY: live wide strings for the call's duration.
            hr = unsafe { f(PCWSTR(conn.as_ptr()), pid, w!(""), PCWSTR(path.as_ptr()), super::CLSID_TAP, PCWSTR(rules.as_ptr())) };
            if hr != NOT_FOUND {
                break;
            }
        }
        log::line(&format!(
            "settings xaml: InitializeXamlDiagnosticsEx(pid {pid}, connection {n}) {:#010x} in {} ms",
            hr.0,
            t0.elapsed().as_millis()
        ));
    });
}
