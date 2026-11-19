//! keys.exe: a test tool for the guest. Presses key chords through SendInput from a process with no
//! window, so the foreground stays where it was and shell hotkeys (Win+Tab, Win+N...) see the keys.
//!
//! `keys lwin+tab wait:2 esc`: each argument is a chord of `+`-joined key names, or `wait:<s>`.
//! Names: ctrl alt shift lwin tab esc enter space left right up down f1..f24 a..z 0..9 period,
//! or `vk<hex>`. `down:<name>` and `up:<name>` press or release one key alone (hold Alt across a
//! few Tabs), `type:<text>` types text as Unicode characters. The result goes to `keys.log` beside
//! the exe.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
fn main() {
    use std::io::Write;
    let mut out = Vec::new();
    for arg in std::env::args().skip(1) {
        let line = match arg.strip_prefix("wait:") {
            Some(s) => {
                std::thread::sleep(std::time::Duration::from_secs_f32(s.parse().unwrap_or(1.0)));
                format!("{arg}: ok")
            }
            None if arg.starts_with("down:") || arg.starts_with("up:") => {
                let (up, name) = arg.split_once(':').map(|(a, b)| (a == "up", b)).unwrap_or((false, ""));
                match win::one(name, up) {
                    Ok(()) => format!("{arg}: ok"),
                    Err(e) => format!("{arg}: {e}"),
                }
            }
            None if arg.starts_with("type:") => match win::text(&arg[5..]) {
                Ok(n) => format!("type: sent {n} events, foreground {}", win::foreground()),
                Err(e) => format!("{arg}: {e}"),
            },
            None => match win::chord(&arg) {
                Ok(n) => format!("{arg}: sent {n} events, foreground {}", win::foreground()),
                Err(e) => format!("{arg}: {e}"),
            },
        };
        out.push(line);
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
    let path = std::env::current_exe().unwrap_or_default().with_file_name("keys.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{}", out.join("\r\n"));
    }
}

#[cfg(not(target_os = "windows"))]
fn main() {}

#[cfg(target_os = "windows")]
mod win {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
        KEYEVENTF_UNICODE,
        SendInput, VIRTUAL_KEY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetForegroundWindow};

    fn vk(name: &str) -> Result<u16, String> {
        let n = name.to_ascii_lowercase();
        let named = match n.as_str() {
            "ctrl" => 0x11,
            "alt" => 0x12,
            "shift" => 0x10,
            "lwin" | "win" => 0x5B,
            "tab" => 0x09,
            "esc" => 0x1B,
            "enter" => 0x0D,
            "space" => 0x20,
            "left" => 0x25,
            "up" => 0x26,
            "right" => 0x27,
            "down" => 0x28,
            "period" => 0xBE,
            "semicolon" => 0xBA,
            _ => 0,
        };
        if named != 0 {
            return Ok(named);
        }
        if let Some(h) = n.strip_prefix("vk") {
            return u16::from_str_radix(h, 16).map_err(|_| format!("bad {name}"));
        }
        if let Some(f) = n.strip_prefix('f').and_then(|d| d.parse::<u16>().ok()) {
            if (1..=24).contains(&f) {
                return Ok(0x6F + f);
            }
        }
        match n.as_bytes() {
            [c @ b'a'..=b'z'] => Ok((c - b'a' + b'A') as u16),
            [c @ b'0'..=b'9'] => Ok(*c as u16),
            _ => Err(format!("unknown key {name}")),
        }
    }

    fn extended(v: u16) -> bool {
        matches!(v, 0x5B | 0x25..=0x28)
    }

    fn input(v: u16, up: bool) -> INPUT {
        let mut flags = KEYBD_EVENT_FLAGS(0);
        if up {
            flags |= KEYEVENTF_KEYUP;
        }
        if extended(v) {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(v), dwFlags: flags, ..Default::default() } },
        }
    }

    /// Presses the keys of `chord` in order, then releases them in reverse.
    pub fn chord(chord: &str) -> Result<u32, String> {
        let keys = chord.split('+').map(vk).collect::<Result<Vec<_>, _>>()?;
        let mut seq: Vec<INPUT> = keys.iter().map(|&k| input(k, false)).collect();
        seq.extend(keys.iter().rev().map(|&k| input(k, true)));
        // SAFETY: a slice of initialized INPUT structs and their size.
        let n = unsafe { SendInput(&seq, size_of::<INPUT>() as i32) };
        if n as usize == seq.len() { Ok(n) } else { Err(format!("SendInput sent {n} of {}", seq.len())) }
    }

    /// Presses or releases one key.
    pub fn one(name: &str, up: bool) -> Result<(), String> {
        let k = vk(name)?;
        // SAFETY: one initialized INPUT and its size.
        let n = unsafe { SendInput(&[input(k, up)], size_of::<INPUT>() as i32) };
        if n == 1 { Ok(()) } else { Err("SendInput refused".into()) }
    }

    /// Types `text` as Unicode characters, one down and up per UTF-16 unit, 40 ms apart.
    pub fn text(text: &str) -> Result<u32, String> {
        let mut sent = 0;
        for u in text.encode_utf16() {
            let ev = |up: bool| INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wScan: u,
                        dwFlags: if up { KEYEVENTF_UNICODE | KEYEVENTF_KEYUP } else { KEYEVENTF_UNICODE },
                        ..Default::default()
                    },
                },
            };
            // SAFETY: two initialized INPUT structs and their size.
            sent += unsafe { SendInput(&[ev(false), ev(true)], size_of::<INPUT>() as i32) };
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        Ok(sent)
    }

    pub fn foreground() -> String {
        // SAFETY: plain queries; the buffer outlives the call.
        unsafe {
            let h = GetForegroundWindow();
            let mut buf = [0u16; 128];
            let n = GetClassNameW(h, &mut buf) as usize;
            format!("{:?} {}", h.0, String::from_utf16_lossy(&buf[..n]))
        }
    }
}
