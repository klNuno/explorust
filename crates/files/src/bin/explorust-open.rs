//! explorust-open.exe [path]: hands a folder to the resident File Explorer of explorust
//! (`explorust.files`) through `WM_COPYDATA` and exits 0. When no explorust answers, it starts
//! nothing and exits 1. No path opens the current directory.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
fn main() {
    std::process::exit(win::send());
}

#[cfg(target_os = "windows")]
mod win {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::System::DataExchange::COPYDATASTRUCT;
    use windows::Win32::UI::WindowsAndMessaging::{SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_COPYDATA};

    /// A path made absolute against the current directory; shell names (`shell:`, `::{`,
    /// `search-ms:`) pass as they are.
    fn resolve(arg: &str) -> String {
        let p = std::path::Path::new(arg);
        if arg.contains(':') && !p.is_absolute() && arg.len() > 2 || p.is_absolute() || arg.starts_with("::") {
            return arg.to_string();
        }
        std::env::current_dir().map(|d| d.join(p).display().to_string()).unwrap_or_else(|_| arg.to_string())
    }

    pub fn send() -> i32 {
        let arg = std::env::args().nth(1).unwrap_or_else(|| ".".into());
        let path = resolve(arg.trim_matches('"'));
        let Some(target) = bus::target(bus::Owner::Files) else { return 1 };
        let payload = files::handover::encode(&path);
        let cds = COPYDATASTRUCT {
            dwData: files::handover::MAGIC,
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
}

#[cfg(not(target_os = "windows"))]
fn main() {}
