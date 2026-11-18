//! taskbar-probe.exe: a guest test tool for the taskbar, windowless like keys.exe. Each argument
//! is a step; results go to `taskbar-probe.log` beside the exe.
//!
//! `click:x,y` `mclick:x,y` `sclick:x,y` (Shift+click) `move:x,y` `wait:<s>` `fg` (foreground
//! title) `state:<title part>` (rect, minimized) `max:<title part>` `restore:<title part>`
//! `close:<title part>` `workarea` `shutdown` (bus Shutdown to the taskbar).

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
fn main() {
    use std::io::Write;
    let mut out = vec![format!("--- {:?}", std::env::args().skip(1).collect::<Vec<_>>())];
    for arg in std::env::args().skip(1) {
        let (cmd, val) = arg.split_once(':').unwrap_or((arg.as_str(), ""));
        let line = probe::step(cmd, val);
        out.push(format!("{arg}: {line}"));
        std::thread::sleep(std::time::Duration::from_millis(120));
    }
    let path = std::env::current_exe().unwrap_or_default().with_file_name("taskbar-probe.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{}", out.join("\r\n"));
    }
}

#[cfg(not(target_os = "windows"))]
fn main() {}

#[cfg(target_os = "windows")]
mod probe {
    use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows_core::BOOL;

    pub fn step(cmd: &str, val: &str) -> String {
        let xy = || {
            let (x, y) = val.split_once(',')?;
            Some((x.trim().parse::<i32>().ok()?, y.trim().parse::<i32>().ok()?))
        };
        match cmd {
            "wait" => {
                std::thread::sleep(std::time::Duration::from_secs_f32(val.parse().unwrap_or(1.0)));
                "ok".into()
            }
            "click" | "mclick" | "sclick" | "move" => {
                let Some((x, y)) = xy() else { return "bad x,y".into() };
                // SAFETY: input injection and cursor moves.
                unsafe {
                    let _ = SetCursorPos(x, y);
                    std::thread::sleep(std::time::Duration::from_millis(60));
                    let (down, up) = match cmd {
                        "mclick" => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
                        _ => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
                    };
                    if cmd != "move" {
                        if cmd == "sclick" {
                            key(VK_SHIFT, false);
                        }
                        mouse(down);
                        std::thread::sleep(std::time::Duration::from_millis(80));
                        mouse(up);
                        if cmd == "sclick" {
                            key(VK_SHIFT, true);
                        }
                    }
                }
                format!("ok, foreground {}", title(unsafe { GetForegroundWindow() }))
            }
            "fg" => title(unsafe { GetForegroundWindow() }),
            "state" | "max" | "restore" | "close" => {
                let Some(h) = find(val) else { return "not found".into() };
                // SAFETY: window calls on a found handle.
                unsafe {
                    match cmd {
                        "max" => {
                            let _ = ShowWindow(h, SW_MAXIMIZE);
                            std::thread::sleep(std::time::Duration::from_millis(600));
                        }
                        "restore" => {
                            let _ = ShowWindow(h, SW_RESTORE);
                            std::thread::sleep(std::time::Duration::from_millis(400));
                        }
                        "close" => {
                            let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
                            return "posted WM_CLOSE".into();
                        }
                        _ => {}
                    }
                    let mut r = RECT::default();
                    let _ = GetWindowRect(h, &mut r);
                    format!("{} rect {},{} {}x{} minimized={} maximized={}", title(h), r.left, r.top, r.right - r.left, r.bottom - r.top, IsIconic(h).as_bool(), IsZoomed(h).as_bool())
                }
            }
            "workarea" => {
                let mut r = RECT::default();
                // SAFETY: reads a RECT.
                let _ = unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut r as *mut _ as _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)) };
                format!("{},{} {}x{}", r.left, r.top, r.right - r.left, r.bottom - r.top)
            }
            "shutdown" => format!("posted={}", bus::post_to(bus::Owner::Taskbar, bus::Command::Shutdown, 0)),
            _ => "unknown step".into(),
        }
    }

    unsafe fn mouse(flags: MOUSE_EVENT_FLAGS) {
        let i = INPUT { r#type: INPUT_MOUSE, Anonymous: INPUT_0 { mi: MOUSEINPUT { dwFlags: flags, ..Default::default() } } };
        // SAFETY: one input event.
        unsafe { SendInput(&[i], size_of::<INPUT>() as i32) };
    }

    unsafe fn key(vk: VIRTUAL_KEY, up: bool) {
        let flags = if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) };
        let i = INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, dwFlags: flags, ..Default::default() } } };
        // SAFETY: one input event.
        unsafe { SendInput(&[i], size_of::<INPUT>() as i32) };
    }

    fn title(h: HWND) -> String {
        let mut buf = [0u16; 256];
        // SAFETY: fills a local buffer.
        let n = unsafe { GetWindowTextW(h, &mut buf) };
        format!("{:#x} [{}]", h.0 as usize, String::from_utf16_lossy(&buf[..n.max(0) as usize]))
    }

    /// The first visible top-level window whose title contains `part`.
    fn find(part: &str) -> Option<HWND> {
        struct S<'a>(&'a str, Option<HWND>);
        unsafe extern "system" fn cb(h: HWND, lp: LPARAM) -> BOOL {
            // SAFETY: `lp` is the S below, alive for the enumeration.
            let s = unsafe { &mut *(lp.0 as *mut S) };
            let mut buf = [0u16; 256];
            let n = unsafe { GetWindowTextW(h, &mut buf) };
            let t = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
            if unsafe { IsWindowVisible(h) }.as_bool() && t.contains(s.0) {
                s.1 = Some(h);
                return false.into();
            }
            true.into()
        }
        let mut s = S(part, None);
        // SAFETY: enumerates into a local.
        unsafe {
            let _ = EnumWindows(Some(cb), LPARAM(&mut s as *mut _ as isize));
        }
        s.1
    }
}
