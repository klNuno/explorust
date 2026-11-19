//! switcher-keys.exe: a guest test tool that holds keys across steps, which Alt+Tab needs (keys.exe
//! presses and releases a chord at once). Steps: `+alt` key down, `-alt` key up, `tab` a tap,
//! `wait:<ms>`, `move:<x>,<y>` the cursor, `click:<x>,<y>` a left click there, `min:<title>` minimizes
//! the window with that exact title (`_` for a space), `fg` the foreground.
//! Keys: alt shift tab esc. Each step and the foreground window's title go to `switcher-keys.log`
//! beside the exe.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
fn main() {
    use std::io::Write;
    let mut out = Vec::new();
    for arg in std::env::args().skip(1) {
        let r = win::step(&arg);
        out.push(format!("{arg}: {r}; foreground {}", win::foreground()));
    }
    let path = std::env::current_exe().unwrap_or_default().with_file_name("switcher-keys.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{}", out.join("\r\n"));
    }
}

#[cfg(not(target_os = "windows"))]
fn main() {}

#[cfg(target_os = "windows")]
mod win {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, MOUSEEVENTF_LEFTDOWN,
        MOUSEEVENTF_LEFTUP, MOUSEINPUT, SendInput, VIRTUAL_KEY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetForegroundWindow, GetWindowTextW, SW_MINIMIZE, SetCursorPos, ShowWindow};
    use windows_core::HSTRING;

    fn vk(name: &str) -> Option<u16> {
        Some(match name {
            "alt" => 0x12,
            "shift" => 0x10,
            "tab" => 0x09,
            "esc" => 0x1B,
            _ => return None,
        })
    }

    fn key(v: u16, up: bool) -> INPUT {
        let flags = if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) };
        INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(v), dwFlags: flags, ..Default::default() } } }
    }

    fn send(seq: &[INPUT]) -> String {
        // SAFETY: a slice of initialized INPUT structs and their size.
        let n = unsafe { SendInput(seq, size_of::<INPUT>() as i32) };
        format!("sent {n} of {}", seq.len())
    }

    fn point(s: &str) -> Option<(i32, i32)> {
        let (x, y) = s.split_once(',')?;
        Some((x.parse().ok()?, y.parse().ok()?))
    }

    pub fn step(arg: &str) -> String {
        if let Some(ms) = arg.strip_prefix("wait:") {
            std::thread::sleep(std::time::Duration::from_millis(ms.parse().unwrap_or(100)));
            return "ok".into();
        }
        if let Some((x, y)) = arg.strip_prefix("move:").and_then(point) {
            // SAFETY: moves the cursor.
            let _ = unsafe { SetCursorPos(x, y) };
            std::thread::sleep(std::time::Duration::from_millis(50));
            return "moved".into();
        }
        if let Some((x, y)) = arg.strip_prefix("click:").and_then(point) {
            // SAFETY: moves the cursor.
            let _ = unsafe { SetCursorPos(x, y) };
            std::thread::sleep(std::time::Duration::from_millis(50));
            let m = |f| INPUT { r#type: INPUT_MOUSE, Anonymous: INPUT_0 { mi: MOUSEINPUT { dwFlags: f, ..Default::default() } } };
            return send(&[m(MOUSEEVENTF_LEFTDOWN), m(MOUSEEVENTF_LEFTUP)]);
        }
        if let Some(t) = arg.strip_prefix("min:") {
            let title = HSTRING::from(t.replace('_', " "));
            // SAFETY: a window lookup by title, then the minimize command on it.
            return match unsafe { FindWindowW(None, &title) } {
                Ok(h) => format!("minimized {:?}: {}", h.0, unsafe { ShowWindow(h, SW_MINIMIZE) }.as_bool()),
                Err(e) => format!("no window {t}: {e}"),
            };
        }
        if arg == "fg" {
            return "ok".into();
        }
        let r = if let Some(k) = arg.strip_prefix('+').and_then(vk) {
            send(&[key(k, false)])
        } else if let Some(k) = arg.strip_prefix('-').and_then(vk) {
            send(&[key(k, true)])
        } else if let Some(k) = vk(arg) {
            send(&[key(k, false), key(k, true)])
        } else {
            return format!("unknown step {arg}");
        };
        std::thread::sleep(std::time::Duration::from_millis(30));
        r
    }

    pub fn foreground() -> String {
        let mut buf = [0u16; 256];
        // SAFETY: plain queries into a local buffer.
        let n = unsafe { GetWindowTextW(GetForegroundWindow(), &mut buf) };
        format!("{:?}", String::from_utf16_lossy(&buf[..n.max(0) as usize]))
    }
}
