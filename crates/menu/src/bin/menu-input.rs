//! menu-input.exe: a guest test tool for the context menu. Mouse and keys from one process with no
//! window, so no console comes in front between the right-click and the keys (the menu closes when
//! it loses the foreground). Each argument: `rclick|click|move:<x>,<y>` (screen pixels),
//! `key:<chord>` (`+`-joined: ctrl alt shift esc enter space tab home end up down left right apps
//! delete f1..f12 a..z 0..9 or vk<hex>), `type:<text>`, `down:<key>`, `up:<key>`, `wait:<s>`.
//! Results go to `menu-input.log` beside the exe.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(not(target_os = "windows"))]
fn main() {}

#[cfg(target_os = "windows")]
fn main() {
    use std::io::Write;
    use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
    // SAFETY: process-wide DPI mode of this test tool before reading coordinates.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let mut out = vec![format!("consoles minimized: {}", win::minimize_consoles())];
    for arg in std::env::args().skip(1) {
        let (op, rest) = arg.split_once(':').unwrap_or((arg.as_str(), ""));
        let r = match op {
            "wait" => {
                std::thread::sleep(std::time::Duration::from_secs_f32(rest.parse().unwrap_or(1.0)));
                Ok(())
            }
            "rclick" | "click" | "move" => win::mouse(op, rest),
            "key" => win::chord(rest),
            "down" | "up" => win::key(rest, op == "up"),
            "type" => win::text(rest),
            _ => Err(format!("unknown step {arg}")),
        };
        out.push(format!("{arg}: {} (foreground {})", r.err().unwrap_or_else(|| "ok".into()), win::foreground()));
        std::thread::sleep(std::time::Duration::from_millis(120));
    }
    let path = std::env::current_exe().unwrap_or_default().with_file_name("menu-input.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{}", out.join("\r\n"));
    }
}

#[cfg(target_os = "windows")]
mod win {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetForegroundWindow, GetSystemMetrics, IsIconic, IsWindowVisible, SM_CXSCREEN, SM_CYSCREEN,
        SW_MINIMIZE, ShowWindow,
    };
    use windows_core::BOOL;

    fn vk(name: &str) -> Option<u16> {
        Some(match name {
            "ctrl" => VK_CONTROL.0,
            "alt" => VK_MENU.0,
            "shift" => VK_SHIFT.0,
            "esc" => VK_ESCAPE.0,
            "enter" => VK_RETURN.0,
            "space" => VK_SPACE.0,
            "tab" => VK_TAB.0,
            "home" => VK_HOME.0,
            "end" => VK_END.0,
            "up" => VK_UP.0,
            "down" => VK_DOWN.0,
            "left" => VK_LEFT.0,
            "right" => VK_RIGHT.0,
            "apps" => VK_APPS.0,
            "delete" => VK_DELETE.0,
            n if n.len() > 1 && n.starts_with('f') && n[1..].parse::<u16>().is_ok() => VK_F1.0 + n[1..].parse::<u16>().ok()? - 1,
            n if n.starts_with("vk") => u16::from_str_radix(&n[2..], 16).ok()?,
            n if n.len() == 1 => n.chars().next()?.to_ascii_uppercase() as u16,
            _ => return None,
        })
    }

    fn kin(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
        }
    }

    fn send(v: &[INPUT]) -> Result<(), String> {
        // SAFETY: an array of initialized inputs.
        let n = unsafe { SendInput(v, size_of::<INPUT>() as i32) };
        if n as usize == v.len() { Ok(()) } else { Err(format!("SendInput sent {n} of {}", v.len())) }
    }

    fn ext(vk: u16) -> KEYBD_EVENT_FLAGS {
        let e = [VK_HOME.0, VK_END.0, VK_UP.0, VK_DOWN.0, VK_LEFT.0, VK_RIGHT.0, VK_DELETE.0, VK_APPS.0];
        if e.contains(&vk) { KEYEVENTF_EXTENDEDKEY } else { KEYBD_EVENT_FLAGS(0) }
    }

    pub fn key(name: &str, up: bool) -> Result<(), String> {
        let k = vk(name).ok_or(format!("unknown key {name}"))?;
        let f = ext(k) | if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) };
        send(&[kin(k, 0, f)])
    }

    pub fn chord(s: &str) -> Result<(), String> {
        let keys: Vec<u16> = s.split('+').map(|n| vk(n).ok_or(format!("unknown key {n}"))).collect::<Result<_, _>>()?;
        let mut v: Vec<INPUT> = keys.iter().map(|&k| kin(k, 0, ext(k))).collect();
        v.extend(keys.iter().rev().map(|&k| kin(k, 0, ext(k) | KEYEVENTF_KEYUP)));
        send(&v)
    }

    pub fn text(s: &str) -> Result<(), String> {
        let mut v = Vec::new();
        for u in s.encode_utf16() {
            v.push(kin(0, u, KEYEVENTF_UNICODE));
            v.push(kin(0, u, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
        }
        send(&v)
    }

    pub fn mouse(op: &str, at: &str) -> Result<(), String> {
        let (x, y) = at.split_once(',').ok_or("want x,y")?;
        let (x, y): (i32, i32) = (x.trim().parse().map_err(|_| "bad x")?, y.trim().parse().map_err(|_| "bad y")?);
        // SAFETY: plain metrics.
        let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
        let (ax, ay) = ((x * 65535) / (w - 1).max(1), (y * 65535) / (h - 1).max(1));
        let m = |flags: MOUSE_EVENT_FLAGS| INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 { mi: MOUSEINPUT { dx: ax, dy: ay, mouseData: 0, dwFlags: flags | MOUSEEVENTF_ABSOLUTE, time: 0, dwExtraInfo: 0 } },
        };
        let mut v = vec![m(MOUSEEVENTF_MOVE)];
        match op {
            "rclick" => v.extend([m(MOUSEEVENTF_RIGHTDOWN), m(MOUSEEVENTF_RIGHTUP)]),
            "click" => v.extend([m(MOUSEEVENTF_LEFTDOWN), m(MOUSEEVENTF_LEFTUP)]),
            _ => {}
        }
        send(&v)
    }

    unsafe extern "system" fn console(h: HWND, lp: LPARAM) -> BOOL {
        let mut b = [0u16; 64];
        // SAFETY: class name into a local buffer; the count lives in the caller's frame.
        unsafe {
            let n = GetClassNameW(h, &mut b) as usize;
            if String::from_utf16_lossy(&b[..n]) == "ConsoleWindowClass" && IsWindowVisible(h).as_bool() && !IsIconic(h).as_bool() {
                let _ = ShowWindow(h, SW_MINIMIZE);
                *(lp.0 as *mut u32) += 1;
            }
        }
        true.into()
    }

    /// The elevated consoles of `os win run --desktop` stay in front: minimize them all.
    pub fn minimize_consoles() -> u32 {
        let mut n = 0u32;
        // SAFETY: enumerates top-level windows with a counter that outlives the call.
        unsafe {
            let _ = EnumWindows(Some(console), LPARAM(&mut n as *mut u32 as isize));
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        n
    }

    pub fn foreground() -> String {
        let mut b = [0u16; 64];
        // SAFETY: class name of the foreground window into a local buffer.
        let n = unsafe { GetClassNameW(GetForegroundWindow(), &mut b) } as usize;
        String::from_utf16_lossy(&b[..n])
    }
}
