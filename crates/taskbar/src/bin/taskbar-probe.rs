//! taskbar-probe.exe: a guest test tool for the taskbar, windowless like keys.exe. Each argument
//! is a step; results go to `taskbar-probe.log` beside the exe.
//!
//! `click:x,y` `mclick:x,y` `sclick:x,y` (Shift+click) `move:x,y` `wait:<s>` `fg` (foreground
//! title) `state:<title part>` (rect, minimized) `max:<title part>` `restore:<title part>`
//! `close:<title part>` `workarea` `shutdown` (bus Shutdown to the taskbar) `run:<file or uri>`
//! `win:<key>` (Win+key, a digit or letter) `shot:<name>` (the screen to `<name>.bmp` beside the exe)
//! `rclick:x,y` `esc` `trays` (every `Shell_TrayWnd` with its process id) `flash:<title part>`
//! (FlashWindowEx with FLASHW_TRAY | FLASHW_TIMERNOFG) `winx` `trayfocus` `taskfocus` (bus commands)
//! `key:left|right|enter|apps|esc` `progress:<title part>,<percent>` `pstate:<title part>,<TBPFLAG>`
//! `overlay:<title part>` (a stock warning badge) `overlayoff:<title part>` (ITaskbarList3 calls made
//! here on that window) `res:<w>x<h>` (display resolution; `res:0` puts the registry mode back).

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
fn main() {
    use std::io::Write;
    let path = std::env::current_exe().unwrap_or_default().with_file_name("taskbar-probe.log");
    let log = |line: String| {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(f, "{line}");
        }
    };
    log(format!("--- {:?}", std::env::args().skip(1).collect::<Vec<_>>()));
    for arg in std::env::args().skip(1) {
        let (cmd, val) = arg.split_once(':').unwrap_or((arg.as_str(), ""));
        let line = probe::step(cmd, val);
        log(format!("{arg}: {line}"));
        std::thread::sleep(std::time::Duration::from_millis(120));
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
            "esc" => {
                // SAFETY: input injection.
                unsafe {
                    key(VK_ESCAPE, false);
                    key(VK_ESCAPE, true);
                }
                "sent".into()
            }
            "trays" => {
                let mut out = Vec::new();
                let mut after: Option<HWND> = None;
                // SAFETY: window lookups by class name.
                unsafe {
                    while let Ok(h) = FindWindowExW(None, after, windows_core::w!("Shell_TrayWnd"), None) {
                        if h.is_invalid() {
                            break;
                        }
                        let mut pid = 0;
                        GetWindowThreadProcessId(h, Some(&mut pid));
                        out.push(format!("{:#x} pid {pid}", h.0 as usize));
                        after = Some(h);
                    }
                }
                format!("{} [{}]", out.len(), out.join(", "))
            }
            "click" | "mclick" | "sclick" | "move" | "rclick" => {
                let Some((x, y)) = xy() else { return "bad x,y".into() };
                // SAFETY: input injection and cursor moves.
                unsafe {
                    let _ = SetCursorPos(x, y);
                    std::thread::sleep(std::time::Duration::from_millis(60));
                    let (down, up) = match cmd {
                        "mclick" => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
                        "rclick" => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
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
            "flash" => {
                let Some(h) = find(val) else { return "not found".into() };
                let fi = FLASHWINFO { cbSize: size_of::<FLASHWINFO>() as u32, hwnd: h, dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG, uCount: 0, dwTimeout: 0 };
                // SAFETY: flashes a found window's taskbar button until it comes to the foreground.
                let was = unsafe { FlashWindowEx(&fi) }.as_bool();
                format!("FlashWindowEx {} ({:#x}): was active {was}", title(h), h.0 as usize)
            }
            "progress" | "pstate" | "overlay" | "overlayoff" => {
                let (part, n) = val.split_once(',').unwrap_or((val, "0"));
                let Some(h) = find(part) else { return "not found".into() };
                let n: u64 = n.trim().parse().unwrap_or(0);
                taskbar_list(cmd, h, n)
            }
            "res" => {
                use windows::Win32::Graphics::Gdi::{CDS_TYPE, ChangeDisplaySettingsW, DEVMODEW, DM_PELSHEIGHT, DM_PELSWIDTH};
                let r = if val == "0" {
                    // SAFETY: restores the registry display mode.
                    unsafe { ChangeDisplaySettingsW(None, CDS_TYPE(0)) }
                } else {
                    let Some((w, hh)) = val.split_once('x') else { return "bad WxH".into() };
                    let mut dm = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
                    dm.dmPelsWidth = w.parse().unwrap_or(1280);
                    dm.dmPelsHeight = hh.parse().unwrap_or(800);
                    dm.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT;
                    // SAFETY: a dynamic mode change (not saved to the registry).
                    unsafe { ChangeDisplaySettingsW(Some(&dm), CDS_TYPE(0)) }
                };
                format!("ChangeDisplaySettings: {}", r.0)
            }
            "winx" => format!("posted={}", bus::post(bus::Command::WinX, 0)),
            "trayfocus" => format!("posted={}", bus::post(bus::Command::TrayFocus, 0)),
            "taskfocus" => format!("posted={}", bus::post(bus::Command::TaskbarFocus, 0)),
            "key" => {
                let vk = match val { "left" => VK_LEFT, "right" => VK_RIGHT, "enter" => VK_RETURN, "apps" => VK_APPS, _ => VK_ESCAPE };
                // SAFETY: input injection.
                unsafe {
                    key(vk, false);
                    key(vk, true);
                }
                "sent".into()
            }
            "workarea" => {
                let mut r = RECT::default();
                // SAFETY: reads a RECT.
                let _ = unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut r as *mut _ as _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)) };
                format!("{},{} {}x{}", r.left, r.top, r.right - r.left, r.bottom - r.top)
            }
            "run" => {
                let f: Vec<u16> = val.encode_utf16().chain(Some(0)).collect();
                // SAFETY: opens a file or URI through the shell.
                let r = unsafe { windows::Win32::UI::Shell::ShellExecuteW(None, windows_core::w!("open"), windows_core::PCWSTR(f.as_ptr()), None, None, SW_SHOWNORMAL) };
                format!("{}", r.0 as isize)
            }
            "win" => {
                let Some(c) = val.chars().next() else { return "no key".into() };
                // SAFETY: input injection.
                unsafe {
                    key(VK_LWIN, false);
                    key(VIRTUAL_KEY(c.to_ascii_uppercase() as u16), false);
                    key(VIRTUAL_KEY(c.to_ascii_uppercase() as u16), true);
                    key(VK_LWIN, true);
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
                format!("sent, foreground {}", title(unsafe { GetForegroundWindow() }))
            }
            "shot" => shot(val),
            "shutdown" => format!("posted={}", bus::post_to(bus::Owner::Taskbar, bus::Command::Shutdown, 0)),
            _ => "unknown step".into(),
        }
    }

    /// One ITaskbarList3 call on `h`, made the way any app makes it (CLSID_TaskbarList in this process).
    fn taskbar_list(cmd: &str, h: HWND, n: u64) -> String {
        use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx};
        use windows::Win32::UI::Shell::{ITaskbarList3, TBPFLAG, TaskbarList};
        // SAFETY: COM on this thread, then calls on the taskbar list object.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let t: ITaskbarList3 = match CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER) {
                Ok(t) => t,
                Err(e) => return format!("CoCreateInstance: {e}"),
            };
            let init = t.HrInit();
            let r = match cmd {
                "progress" => t.SetProgressValue(h, n, 100),
                "pstate" => t.SetProgressState(h, TBPFLAG(n as i32)),
                "overlay" => {
                    let icon = LoadIconW(None, IDI_WARNING).unwrap_or_default();
                    t.SetOverlayIcon(h, icon, windows_core::w!("Warning"))
                }
                _ => t.SetOverlayIcon(h, HICON::default(), windows_core::PCWSTR::null()),
            };
            format!("{} ({:#x}): HrInit {:?}, call {:?}", title(h), h.0 as usize, init, r)
        }
    }

    /// Saves the screen as a 24-bit BMP beside the exe.
    fn shot(name: &str) -> String {
        use windows::Win32::Graphics::Gdi::*;
        // SAFETY: GDI capture of the screen into a DIB we own and free.
        unsafe {
            let (w, h) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
            let screen = GetDC(None);
            let mem = CreateCompatibleDC(Some(screen));
            let bmp = CreateCompatibleBitmap(screen, w, h);
            let old = SelectObject(mem, bmp.into());
            let _ = BitBlt(mem, 0, 0, w, h, Some(screen), 0, 0, SRCCOPY | CAPTUREBLT);
            SelectObject(mem, old);
            let stride = ((w * 3 + 3) & !3) as usize;
            let mut px = vec![0u8; stride * h as usize];
            let mut info = BITMAPINFO::default();
            info.bmiHeader = BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: h, biPlanes: 1, biBitCount: 24, biCompression: BI_RGB.0, ..Default::default() };
            GetDIBits(mem, bmp, 0, h as u32, Some(px.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS);
            let _ = DeleteObject(bmp.into());
            let _ = DeleteDC(mem);
            ReleaseDC(None, screen);
            let mut f = Vec::with_capacity(54 + px.len());
            f.extend_from_slice(b"BM");
            f.extend_from_slice(&((54 + px.len()) as u32).to_le_bytes());
            f.extend_from_slice(&0u32.to_le_bytes());
            f.extend_from_slice(&54u32.to_le_bytes());
            let ih = std::slice::from_raw_parts(&info.bmiHeader as *const _ as *const u8, 40);
            f.extend_from_slice(ih);
            f.extend_from_slice(&px);
            let path = std::env::current_exe().unwrap_or_default().with_file_name(format!("{name}.bmp"));
            match std::fs::write(&path, f) {
                Ok(()) => format!("saved {}", path.display()),
                Err(e) => format!("{e}"),
            }
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
