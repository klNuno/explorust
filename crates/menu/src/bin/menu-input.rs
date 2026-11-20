//! menu-input.exe: a guest test tool for the context menu. Mouse and keys from one process with no
//! window, so no console comes in front between the right-click and the keys (the menu closes when
//! it loses the foreground). Each argument: `rclick|click|move:<x>,<y>` (screen pixels),
//! `key:<chord>` (`+`-joined: ctrl alt shift esc enter space tab home end up down left right apps
//! delete f1..f12 a..z 0..9 or vk<hex>), `type:<text>`, `down:<key>`, `up:<key>`, `wait:<s>`,
//! `shot:<file.bmp>` (the screen through GDI), `run:<exe>|<arg>|...` (started, not waited for),
//! `ensure:<window class>:<chord>` (the chord pressed up to three times until the foreground
//! window has that class), `close:<title part>` (WM_CLOSE to the visible top-level windows whose
//! title contains it), `note:<text>`. `@<file>` reads one step per line from a file (`#` lines
//! skipped), so one `os win run --desktop` call (about a minute each in g1) runs a whole session.
//! Results go to `menu-input.log` beside the exe, one timestamped line per step, `done` last.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(not(target_os = "windows"))]
fn main() {}

#[cfg(target_os = "windows")]
fn main() {
    use std::io::Write;
    use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
    // SAFETY: process-wide DPI mode of this test tool before reading coordinates.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let path = std::env::current_exe().unwrap_or_default().with_file_name("menu-input.log");
    let t0 = std::time::Instant::now();
    let log = |line: String| {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(f, "{:8.3} {line}\r", t0.elapsed().as_secs_f64());
        }
    };
    log(format!("consoles minimized: {}", win::minimize_consoles()));
    let mut steps = Vec::new();
    for arg in std::env::args().skip(1) {
        match arg.strip_prefix('@') {
            Some(file) => match std::fs::read_to_string(file) {
                Ok(text) => steps.extend(text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(String::from)),
                Err(e) => log(format!("{arg}: {e}")),
            },
            None => steps.push(arg),
        }
    }
    for arg in steps {
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
            "shot" => win::shot(rest),
            "note" => Ok(()),
            "close" => win::close(rest),
            "run" => {
                let mut parts = rest.split('|');
                let exe = parts.next().unwrap_or_default();
                std::process::Command::new(exe).args(parts).spawn().map(|_| ()).map_err(|e| format!("{exe}: {e}"))
            }
            "ensure" => match rest.split_once(':') {
                Some((class, chord)) => {
                    let mut r = Ok(());
                    for _ in 0..3 {
                        if win::foreground() == class {
                            break;
                        }
                        r = win::chord(chord);
                        std::thread::sleep(std::time::Duration::from_millis(1500));
                    }
                    if r.is_ok() && win::foreground() != class { Err(format!("foreground is not {class}")) } else { r }
                }
                None => Err("want class:chord".into()),
            },
            _ => Err(format!("unknown step {arg}")),
        };
        log(format!("{arg}: {} (foreground {})", r.err().unwrap_or_else(|| "ok".into()), win::foreground()));
        std::thread::sleep(std::time::Duration::from_millis(120));
    }
    log("done".into());
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

    /// The screen as DWM composes it, saved as a 32-bit bottom-up BMP.
    pub fn shot(file: &str) -> Result<(), String> {
        use windows::Win32::Graphics::Gdi::*;
        // SAFETY: GDI objects made, used and released here; the buffer outlives GetDIBits.
        unsafe {
            let (w, h) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
            let screen = GetDC(None);
            let mem = CreateCompatibleDC(Some(screen));
            let bmp = CreateCompatibleBitmap(screen, w, h);
            let old = SelectObject(mem, bmp.into());
            let ok = BitBlt(mem, 0, 0, w, h, Some(screen), 0, 0, SRCCOPY | CAPTUREBLT);
            SelectObject(mem, old);
            let mut info = BITMAPINFO::default();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            };
            let mut px = vec![0u8; (w * h * 4) as usize];
            let lines = GetDIBits(mem, bmp, 0, h as u32, Some(px.as_mut_ptr() as _), &mut info, DIB_RGB_COLORS);
            let _ = DeleteObject(bmp.into());
            let _ = DeleteDC(mem);
            ReleaseDC(None, screen);
            ok.map_err(|e| format!("BitBlt: {e}"))?;
            if lines != h {
                return Err(format!("GetDIBits gave {lines} of {h} lines"));
            }
            let mut out = Vec::with_capacity(54 + px.len());
            out.extend_from_slice(b"BM");
            out.extend_from_slice(&((54 + px.len()) as u32).to_le_bytes());
            out.extend_from_slice(&[0u8; 4]);
            out.extend_from_slice(&54u32.to_le_bytes());
            out.extend_from_slice(&40u32.to_le_bytes());
            out.extend_from_slice(&w.to_le_bytes());
            out.extend_from_slice(&h.to_le_bytes());
            out.extend_from_slice(&1u16.to_le_bytes());
            out.extend_from_slice(&32u16.to_le_bytes());
            out.extend_from_slice(&[0u8; 24]);
            out.extend_from_slice(&px);
            std::fs::write(file, out).map_err(|e| format!("{file}: {e}"))
        }
    }

    unsafe extern "system" fn closer(h: HWND, lp: LPARAM) -> BOOL {
        // SAFETY: the caller's (part, count) pair outlives EnumWindows.
        unsafe {
            let (part, n) = &mut *(lp.0 as *mut (&str, u32));
            let mut b = [0u16; 256];
            let len = windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(h, &mut b) as usize;
            if IsWindowVisible(h).as_bool() && String::from_utf16_lossy(&b[..len]).contains(*part) {
                let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(Some(h), windows::Win32::UI::WindowsAndMessaging::WM_CLOSE, Default::default(), LPARAM(0));
                *n += 1;
            }
        }
        true.into()
    }

    /// Closes the visible top-level windows whose title contains `part`.
    pub fn close(part: &str) -> Result<(), String> {
        let mut st = (part, 0u32);
        // SAFETY: enumerates top-level windows with state that outlives the call.
        unsafe {
            let _ = EnumWindows(Some(closer), LPARAM(&mut st as *mut _ as isize));
        }
        if st.1 > 0 { Ok(()) } else { Err(format!("no window titled *{part}*")) }
    }

    pub fn foreground() -> String {
        let mut b = [0u16; 64];
        // SAFETY: class name of the foreground window into a local buffer.
        let n = unsafe { GetClassNameW(GetForegroundWindow(), &mut b) } as usize;
        String::from_utf16_lossy(&b[..n])
    }
}
