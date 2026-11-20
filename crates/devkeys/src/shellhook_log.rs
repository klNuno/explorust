//! Dev probe: registers a hidden window for shell hook messages and logs every code with its
//! lparam window's class and process, so the codes a shell gets can be compared under explorer and
//! under explorust. Usage: `shellhook-log.exe <seconds> [log path]` (default
//! `C:\explorust\shellhook.log`), appends one line per message.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(not(target_os = "windows"))]
fn main() {}

#[cfg(target_os = "windows")]
fn main() {
    win::run();
}

#[cfg(target_os = "windows")]
mod win {
    use std::io::Write;
    use std::sync::OnceLock;
    use std::time::Instant;
    use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClassNameW, GetMessageW,
        GetWindowThreadProcessId, IsWindow, KillTimer, MSG, PostQuitMessage, RegisterClassW,
        RegisterShellHookWindow, RegisterWindowMessageW, SetTimer, WINDOW_EX_STYLE, WM_TIMER,
        WNDCLASSW, WS_OVERLAPPED,
    };
    use windows::core::{PWSTR, w};

    static LOG: OnceLock<std::path::PathBuf> = OnceLock::new();
    static T0: OnceLock<Instant> = OnceLock::new();
    static SHELLHOOK: OnceLock<u32> = OnceLock::new();

    fn line(text: &str) {
        let path = LOG.get().expect("log path");
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let t = T0.get().map_or(0.0, |t| t.elapsed().as_secs_f64());
            let _ = writeln!(f, "{t:9.3} {text}\r");
        }
    }

    fn describe(hwnd: HWND) -> String {
        // SAFETY: Windows validates the HWND; buffers are writable and sized.
        unsafe {
            if hwnd.is_invalid() || !IsWindow(Some(hwnd)).as_bool() {
                return "-".into();
            }
            let mut class = [0u16; 128];
            let n = GetClassNameW(hwnd, &mut class).max(0) as usize;
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            let mut exe = String::from("?");
            if let Ok(p) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                let mut buf = [0u16; 260];
                let mut len = buf.len() as u32;
                if QueryFullProcessImageNameW(p, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok() {
                    let full = String::from_utf16_lossy(&buf[..len as usize]);
                    exe = full.rsplit('\\').next().unwrap_or_default().to_string();
                }
                let _ = CloseHandle(p);
            }
            format!("{} {exe}", String::from_utf16_lossy(&class[..n]))
        }
    }

    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if Some(&msg) == SHELLHOOK.get() {
            line(&format!("code={:#x} lparam={:#x} {}", wparam.0, lparam.0, describe(HWND(lparam.0 as *mut _))));
            return LRESULT(0);
        }
        if msg == WM_TIMER {
            line("done");
            // SAFETY: our own timer and thread.
            unsafe {
                let _ = KillTimer(Some(hwnd), 1);
                PostQuitMessage(0);
            }
            return LRESULT(0);
        }
        // SAFETY: default handling for every other message.
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    pub fn run() {
        let mut args = std::env::args().skip(1);
        let seconds: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(30);
        let path = args.next().unwrap_or_else(|| r"C:\explorust\shellhook.log".into());
        let _ = LOG.set(path.into());
        let _ = T0.set(Instant::now());
        // SAFETY: a plain hidden top-level window on this thread, its own message loop.
        unsafe {
            let _ = SHELLHOOK.set(RegisterWindowMessageW(w!("SHELLHOOK")));
            let class = WNDCLASSW { lpfnWndProc: Some(proc), lpszClassName: w!("explorust.shellhook.log"), ..Default::default() };
            RegisterClassW(&class);
            let Ok(hwnd) = CreateWindowExW(WINDOW_EX_STYLE(0), w!("explorust.shellhook.log"), w!(""), WS_OVERLAPPED, 0, 0, 0, 0, None, None, None, None) else {
                line("CreateWindowExW failed");
                return;
            };
            let ok = RegisterShellHookWindow(hwnd).as_bool();
            line(&format!("start: window {:#x}, RegisterShellHookWindow={ok}, {seconds} s", hwnd.0 as usize));
            SetTimer(Some(hwnd), 1, seconds * 1000, None);
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                DispatchMessageW(&msg);
            }
        }
    }
}
