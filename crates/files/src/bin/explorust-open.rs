//! explorust-open.exe [path]: hands a folder to the resident File Explorer of explorust
//! (`explorust.files`) through `WM_COPYDATA` and exits 0. When no explorust answers, it starts
//! nothing and exits 1. No path opens the current directory. Explorer's `/select,<path>` opens the
//! item's folder with it selected.
//!
//! `--register-folder-handler` points the per-user open and explore verbs of folders, directories
//! and drives at this exe (the shell then starts it for Run, desktop double-clicks and apps that
//! open a folder); `--unregister-folder-handler` removes them, so Windows' own handler answers again.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("--register-folder-handler") => win::register(),
        Some("--unregister-folder-handler") => win::unregister(),
        _ => win::send(&args),
    };
    std::process::exit(code);
}

#[cfg(target_os = "windows")]
mod win {
    use files::handler;
    use windows::Win32::Foundation::{ERROR_SUCCESS, LPARAM, WPARAM};
    use windows::Win32::System::DataExchange::COPYDATASTRUCT;
    use windows::Win32::System::Registry::*;
    use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
    use windows::Win32::UI::WindowsAndMessaging::{SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_COPYDATA};
    use windows_core::PCWSTR;

    /// A path made absolute against the current directory; shell names (`shell:`, `::{`,
    /// `search-ms:`) pass as they are.
    fn resolve(arg: &str) -> String {
        let p = std::path::Path::new(arg);
        if arg.contains(':') && !p.is_absolute() && arg.len() > 2 || p.is_absolute() || arg.starts_with("::") {
            return arg.to_string();
        }
        std::env::current_dir().map(|d| d.join(p).display().to_string()).unwrap_or_else(|_| arg.to_string())
    }

    pub fn send(args: &[String]) -> i32 {
        let (path, select) = handler::parse_args(args);
        let path = resolve(&path.unwrap_or_else(|| ".".into()));
        let Some(target) = bus::target(bus::Owner::Files) else { return 1 };
        let payload = files::handover::encode(&path);
        let cds = COPYDATASTRUCT {
            dwData: if select { files::handover::MAGIC_SELECT } else { files::handover::MAGIC },
            cbData: (payload.len() * 2) as u32,
            lpData: payload.as_ptr() as *mut _,
        };
        let mut result = 0usize;
        // SAFETY: `cds` and `payload` outlive the synchronous send.
        let sent = unsafe {
            SendMessageTimeoutW(
                target,
                WM_COPYDATA,
                WPARAM(0),
                LPARAM(&cds as *const _ as isize),
                SMTO_ABORTIFHUNG,
                5000,
                Some(&mut result),
            )
        };
        if sent.0 != 0 && result == 1 { 0 } else { 1 }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn set(key: HKEY, name: &str, value: &str) -> bool {
        let data: Vec<u8> = wide(value).iter().flat_map(|u| u.to_le_bytes()).collect();
        let n = wide(name);
        let name = if name.is_empty() { PCWSTR::null() } else { PCWSTR(n.as_ptr()) };
        unsafe { RegSetValueExW(key, name, None, REG_SZ, Some(&data)) == ERROR_SUCCESS }
    }

    fn notify() {
        unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
    }

    /// Writes `HKCU\Software\Classes\<class>\shell\<verb>\command`: the default value runs this exe
    /// on `%1`, an empty `DelegateExecute` hides Windows' own delegate (Explorer's).
    pub fn register() -> i32 {
        let Ok(exe) = std::env::current_exe() else { return 2 };
        let command = handler::command(&exe.display().to_string());
        let mut ok = true;
        for verb in handler::verb_keys() {
            let path = wide(&format!("{verb}\\command"));
            let mut key = HKEY::default();
            let r = unsafe {
                RegCreateKeyW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr()), &mut key)
            };
            if r != ERROR_SUCCESS {
                ok = false;
                continue;
            }
            ok &= set(key, "", &command) && set(key, "DelegateExecute", "");
            unsafe {
                let _ = RegCloseKey(key);
            }
            let vp = wide(&verb);
            let r = unsafe {
                RegCreateKeyW(HKEY_CURRENT_USER, PCWSTR(vp.as_ptr()), &mut key)
            };
            if r == ERROR_SUCCESS {
                ok &= set(key, handler::MARKER, "1");
                unsafe {
                    let _ = RegCloseKey(key);
                }
            }
        }
        notify();
        if ok { 0 } else { 3 }
    }

    /// Deletes the verb keys `register` wrote (those carrying its marker), then the `shell` and
    /// class keys it leaves empty.
    pub fn unregister() -> i32 {
        for verb in handler::verb_keys() {
            let vp = wide(&verb);
            let mut key = HKEY::default();
            let opened = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(vp.as_ptr()), None, KEY_READ, &mut key) };
            if opened != ERROR_SUCCESS {
                continue;
            }
            let m = wide(handler::MARKER);
            let ours = unsafe { RegQueryValueExW(key, PCWSTR(m.as_ptr()), None, None, None, None) } == ERROR_SUCCESS;
            unsafe {
                let _ = RegCloseKey(key);
            }
            if ours {
                unsafe {
                    let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(vp.as_ptr()));
                }
            }
            // `shell`, then the class key, when nothing else lives there (RegDeleteKeyW refuses
            // a key with subkeys; values are checked first).
            let mut parent = verb.clone();
            for _ in 0..2 {
                let Some(cut) = parent.rfind('\\') else { break };
                parent.truncate(cut);
                if !empty(&parent) {
                    break;
                }
                let pp = wide(&parent);
                unsafe {
                    let _ = RegDeleteKeyW(HKEY_CURRENT_USER, PCWSTR(pp.as_ptr()));
                }
            }
        }
        notify();
        0
    }

    fn empty(path: &str) -> bool {
        let p = wide(path);
        let mut key = HKEY::default();
        if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(p.as_ptr()), None, KEY_READ, &mut key) } != ERROR_SUCCESS {
            return false;
        }
        let (mut subkeys, mut values) = (0u32, 0u32);
        let r = unsafe {
            RegQueryInfoKeyW(key, None, None, None, Some(&mut subkeys), None, None, Some(&mut values), None, None, None, None)
        };
        unsafe {
            let _ = RegCloseKey(key);
        }
        r == ERROR_SUCCESS && subkeys == 0 && values == 0
    }
}

#[cfg(not(target_os = "windows"))]
fn main() {}
