//! desktop-mouse.exe: a guest test tool for the desktop. Clicks through SendInput from a process
//! with no window. `desktop-mouse rclick:600,400 wait:1 dclick:40,40`: each argument is
//! `move|click|rclick|dclick:<x>,<y>` in screen pixels, `wait:<s>`, or `workarea:<bottom>` (sets
//! the primary monitor's work area bottom edge, not saved, as a taskbar would; logs the old one),
//! `res:<w>x<h>` or `res:reset` (display mode, not saved), `wall:<path>` (SPI_SETDESKWALLPAPER,
//! saved and broadcast, the old path kept in `desktop-mouse.wall`; `wall:restore` puts it back),
//! `idw:<path>`, `idwcolor:<RRGGBB>` and `idw:advance` (IDesktopWallpaper, the API of Settings >
//! Personalization). The window under each point
//! and the foreground go to `desktop-mouse.log` beside the exe.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
fn main() {
    use std::io::Write;
    use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
    // SAFETY: process-wide DPI mode before any call that reads coordinates.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let path = std::env::current_exe().unwrap_or_default().with_file_name("desktop-mouse.log");
    for arg in std::env::args().skip(1) {
        let line = match arg.split_once(':') {
            Some(("wait", s)) => {
                std::thread::sleep(std::time::Duration::from_secs_f32(s.parse().unwrap_or(1.0)));
                format!("{arg}: ok")
            }
            Some(("res", m)) => format!("{arg}: {}", win::resolution(m)),
            Some(("wall", p)) => format!("{arg}: {}", win::wallpaper(p)),
            Some(("idw", p)) => format!("{arg}: {}", win::idw(p, None)),
            Some(("idwcolor", c)) => format!("{arg}: {}", win::idw("", u32::from_str_radix(c, 16).ok())),
            Some(("workarea", b)) => match b.parse() {
                Ok(b) => format!("{arg}: {}", win::work_area(b)),
                Err(_) => format!("{arg}: bad bottom"),
            },
            Some((what, at)) => match at.split_once(',').and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?))) {
                Some((x, y)) => win::act(what, x, y).map_or_else(|e| format!("{arg}: {e}"), |s| format!("{arg}: {s}")),
                None => format!("{arg}: bad point"),
            },
            None => format!("{arg}: unknown"),
        };
        // One line per step as it ends, so a session can follow the steps from outside.
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = write!(f, "{line}\r\n");
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
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

    pub fn resolution(m: &str) -> String {
        use windows::Win32::Graphics::Gdi::{
            CDS_TYPE, ChangeDisplaySettingsExW, DEVMODEW, DM_PELSHEIGHT, DM_PELSWIDTH, ENUM_CURRENT_SETTINGS,
            EnumDisplaySettingsW,
        };
        // SAFETY: DEVMODEW with its size set, in and out of the display calls.
        unsafe {
            let mut dm = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
            let _ = EnumDisplaySettingsW(None, ENUM_CURRENT_SETTINGS, &mut dm);
            let was = format!("was {}x{}", dm.dmPelsWidth, dm.dmPelsHeight);
            let r = if m == "reset" {
                ChangeDisplaySettingsExW(None, None, None, CDS_TYPE(0), None)
            } else {
                let Some((w, h)) = m.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))) else {
                    return "bad mode".into();
                };
                dm.dmPelsWidth = w;
                dm.dmPelsHeight = h;
                dm.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT;
                ChangeDisplaySettingsExW(None, Some(&dm), None, CDS_TYPE(0), None)
            };
            format!("{was}, result {}", r.0)
        }
    }

    fn get_wallpaper() -> String {
        use windows::Win32::UI::WindowsAndMessaging::{
            SPI_GETDESKWALLPAPER, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
        };
        let mut buf = [0u16; 520];
        // SAFETY: the buffer and its length in characters.
        let _ = unsafe {
            SystemParametersInfoW(SPI_GETDESKWALLPAPER, buf.len() as u32, Some(buf.as_mut_ptr().cast()), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0))
        };
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..n])
    }

    pub fn wallpaper(p: &str) -> String {
        use windows::Win32::UI::WindowsAndMessaging::{
            SPI_SETDESKWALLPAPER, SPIF_SENDCHANGE, SPIF_UPDATEINIFILE, SystemParametersInfoW,
        };
        let keep = std::env::current_exe().unwrap_or_default().with_file_name("desktop-mouse.wall");
        let was = get_wallpaper();
        let target = if p == "restore" {
            match std::fs::read_to_string(&keep) {
                Ok(s) => s,
                Err(e) => return format!("nothing to restore: {e}"),
            }
        } else {
            if !keep.exists() {
                let _ = std::fs::write(&keep, &was);
            }
            p.to_string()
        };
        let mut w: Vec<u16> = target.encode_utf16().chain([0]).collect();
        let t = std::time::Instant::now();
        // SAFETY: a NUL-terminated path that outlives the call.
        let r = unsafe {
            SystemParametersInfoW(SPI_SETDESKWALLPAPER, 0, Some(w.as_mut_ptr().cast()), SPIF_UPDATEINIFILE | SPIF_SENDCHANGE)
        };
        if p == "restore" && r.is_ok() {
            let _ = std::fs::remove_file(&keep);
        }
        format!("was {was:?}, set {target:?}: {r:?} in {} ms", t.elapsed().as_millis())
    }

    pub fn idw(p: &str, color: Option<u32>) -> String {
        use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx};
        use windows::Win32::UI::Shell::{DesktopWallpaper, IDesktopWallpaper};
        use windows::Win32::Foundation::COLORREF;
        use windows_core::HSTRING;
        // SAFETY: COM on this tool's main thread; the object is released before returning.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let dw: IDesktopWallpaper = match CoCreateInstance(&DesktopWallpaper, None, CLSCTX_ALL) {
                Ok(d) => d,
                Err(e) => return format!("CoCreateInstance: {e}"),
            };
            let r = if let Some(c) = color {
                dw.SetBackgroundColor(COLORREF(((c & 0xFF) << 16) | (c & 0xFF00) | (c >> 16)))
            } else if p == "advance" {
                dw.AdvanceSlideshow(None, windows::Win32::UI::Shell::DSD_FORWARD)
            } else {
                dw.SetWallpaper(None, &HSTRING::from(p))
            };
            format!("{r:?}, now {:?}", get_wallpaper())
        }
    }

    /// Sets the primary work area's bottom edge (SPIF_SENDCHANGE, not written to the profile).
    pub fn work_area(bottom: i32) -> String {
        use windows::Win32::Foundation::RECT;
        use windows::Win32::UI::WindowsAndMessaging::{
            SPI_GETWORKAREA, SPI_SETWORKAREA, SPIF_SENDCHANGE, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
        };
        let mut rc = RECT::default();
        // SAFETY: RECT in and out of SystemParametersInfo.
        unsafe {
            let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some((&mut rc as *mut RECT).cast()), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
            let was = format!("was {},{},{},{}", rc.left, rc.top, rc.right, rc.bottom);
            rc.bottom = bottom;
            let r = SystemParametersInfoW(SPI_SETWORKAREA, 0, Some((&mut rc as *mut RECT).cast()), SPIF_SENDCHANGE);
            format!("{was}, set bottom {bottom}: {r:?}")
        }
    }

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
