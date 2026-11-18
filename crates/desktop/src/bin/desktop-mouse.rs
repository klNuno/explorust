//! desktop-mouse.exe: a guest test tool for the desktop. Clicks through SendInput from a process
//! with no window. `desktop-mouse rclick:600,400 wait:1 dclick:40,40`: each argument is
//! `move|click|rclick|dclick:<x>,<y>` in screen pixels, or `wait:<s>`. The window under each point
//! and the foreground go to `desktop-mouse.log` beside the exe.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
fn main() {
    use std::io::Write;
    use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
    // SAFETY: process-wide DPI mode before any call that reads coordinates.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let mut out = Vec::new();
    for arg in std::env::args().skip(1) {
        let line = match arg.split_once(':') {
            Some(("wait", s)) => {
                std::thread::sleep(std::time::Duration::from_secs_f32(s.parse().unwrap_or(1.0)));
                format!("{arg}: ok")
            }
            Some((what, at)) => match at.split_once(',').and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?))) {
                Some((x, y)) => win::act(what, x, y).map_or_else(|e| format!("{arg}: {e}"), |s| format!("{arg}: {s}")),
                None => format!("{arg}: bad point"),
            },
            None => format!("{arg}: unknown"),
        };
        out.push(line);
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
    let path = std::env::current_exe().unwrap_or_default().with_file_name("desktop-mouse.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{}", out.join("\r\n"));
    }
}

#[cfg(not(target_os = "windows"))]
fn main() {}

#[cfg(target_os = "windows")]
mod win {
    use windows::Win32::Foundation::{HWND, POINT};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_MOUSE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
        MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT, SendInput,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetForegroundWindow, GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
        SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, WindowFromPoint,
    };

    fn input(x: i32, y: i32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
        // SAFETY: plain metric queries.
        let (vx, vy, vw, vh) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2),
                GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2),
            )
        };
        let dx = ((x - vx) as i64 * 65535 / (vw - 1) as i64) as i32;
        let dy = ((y - vy) as i64 * 65535 / (vh - 1) as i64) as i32;
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    dwFlags: flags | MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                    ..Default::default()
                },
            },
        }
    }

    fn class(h: HWND) -> String {
        let mut buf = [0u16; 128];
        // SAFETY: the buffer outlives the call.
        let n = unsafe { GetClassNameW(h, &mut buf) } as usize;
        String::from_utf16_lossy(&buf[..n])
    }

    pub fn act(what: &str, x: i32, y: i32) -> Result<String, String> {
        let mv = MOUSE_EVENT_FLAGS(0);
        let seq: Vec<INPUT> = match what {
            "move" => vec![input(x, y, mv)],
            "click" => vec![input(x, y, mv), input(x, y, MOUSEEVENTF_LEFTDOWN), input(x, y, MOUSEEVENTF_LEFTUP)],
            "rclick" => vec![input(x, y, mv), input(x, y, MOUSEEVENTF_RIGHTDOWN), input(x, y, MOUSEEVENTF_RIGHTUP)],
            "dclick" => vec![
                input(x, y, mv),
                input(x, y, MOUSEEVENTF_LEFTDOWN),
                input(x, y, MOUSEEVENTF_LEFTUP),
                input(x, y, MOUSEEVENTF_LEFTDOWN),
                input(x, y, MOUSEEVENTF_LEFTUP),
            ],
            _ => return Err(format!("unknown action {what}")),
        };
        // SAFETY: a slice of initialized INPUT structs and their size.
        let n = unsafe { SendInput(&seq, size_of::<INPUT>() as i32) };
        if n as usize != seq.len() {
            return Err(format!("SendInput sent {n} of {}", seq.len()));
        }
        // SAFETY: plain window queries.
        let (under, fg) = unsafe { (WindowFromPoint(POINT { x, y }), GetForegroundWindow()) };
        Ok(format!("under {:?} {}, foreground {:?} {}", under.0, class(under), fg.0, class(fg)))
    }
}
