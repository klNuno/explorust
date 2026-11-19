//! tray-test.exe: a guest test client for the taskbar's tray and AppBar hosts. Logs to
//! `tray-test.log` beside the exe.
//!
//! `tray-test icon <seconds> <version 0|3|4>`: adds a notification icon, logs every callback
//! message, shows a menu on right click (closed after 3 s), adds the icon again on TaskbarCreated.
//! `tray-test appbar <seconds>`: docks a 60 px bar on the top edge, logs QUERYPOS, SETPOS,
//! GETTASKBARPOS, GETSTATE, the work area and ABN_ notifications, then removes it.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(not(target_os = "windows"))]
fn main() {}

#[cfg(target_os = "windows")]
fn main() {
    t::main();
}

#[cfg(target_os = "windows")]
mod t {
    use std::cell::Cell;
    use std::io::Write;
    use std::time::Instant;
    use windows::Win32::Foundation::*;
    use windows::Win32::Graphics::Gdi::{CreateSolidBrush, HBRUSH};
    use windows::Win32::Security::{GetTokenInformation, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenIntegrityLevel, GetSidSubAuthority, GetSidSubAuthorityCount};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::Win32::UI::Shell::*;
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows_core::{PCWSTR, w};

    const CB: u32 = WM_APP + 5;
    const APPBAR_CB: u32 = WM_APP + 6;
    const TIMER_END: usize = 1;
    const TIMER_MENU: usize = 2;

    thread_local! {
        static MODE_ICON: Cell<bool> = const { Cell::new(true) };
        static VERSION: Cell<u32> = const { Cell::new(4) };
        static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
        static T0: Cell<Option<Instant>> = const { Cell::new(None) };
    }

    pub fn log(s: &str) {
        let path = std::env::current_exe().unwrap_or_default().with_file_name("tray-test.log");
        let t = T0.with(|t| t.get()).map(|t| t.elapsed().as_millis()).unwrap_or(0);
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "[{} +{t} ms] {s}", std::process::id());
        }
    }

    fn integrity() -> u32 {
        // SAFETY: reads this process's token label into a local buffer.
        unsafe {
            let mut tok = HANDLE::default();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok).is_err() {
                return 0;
            }
            let mut buf = [0u8; 128];
            let mut len = 0;
            let ok = GetTokenInformation(tok, TokenIntegrityLevel, Some(buf.as_mut_ptr().cast()), buf.len() as u32, &mut len).is_ok();
            let _ = CloseHandle(tok);
            if !ok {
                return 0;
            }
            let label = &*(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL);
            let n = *GetSidSubAuthorityCount(label.Label.Sid);
            *GetSidSubAuthority(label.Label.Sid, n as u32 - 1)
        }
    }

    fn nid(hwnd: HWND) -> NOTIFYICONDATAW {
        let mut n = NOTIFYICONDATAW { cbSize: size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: 7, ..Default::default() };
        n.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        n.uCallbackMessage = CB;
        // SAFETY: a system icon handle, shared and never freed.
        n.hIcon = unsafe { LoadIconW(None, IDI_INFORMATION) }.unwrap_or_default();
        let tip: Vec<u16> = "explorust tray test".encode_utf16().collect();
        n.szTip[..tip.len()].copy_from_slice(&tip);
        n
    }

    fn add_icon(hwnd: HWND) {
        let mut n = nid(hwnd);
        // SAFETY: Shell_NotifyIcon with a filled struct.
        let added = unsafe { Shell_NotifyIconW(NIM_ADD, &n) }.as_bool();
        n.Anonymous.uVersion = VERSION.with(|v| v.get());
        let set = unsafe { Shell_NotifyIconW(NIM_SETVERSION, &n) }.as_bool();
        let tray = unsafe { FindWindowW(w!("Shell_TrayWnd"), None) }.unwrap_or_default();
        log(&format!("icon ADD {added}, SETVERSION {} {set}, Shell_TrayWnd {:#x}", unsafe { n.Anonymous.uVersion }, tray.0 as usize));
    }

    fn event_name(m: u32) -> String {
        match m {
            WM_MOUSEMOVE => "WM_MOUSEMOVE".into(),
            WM_LBUTTONDOWN => "WM_LBUTTONDOWN".into(),
            WM_LBUTTONUP => "WM_LBUTTONUP".into(),
            WM_LBUTTONDBLCLK => "WM_LBUTTONDBLCLK".into(),
            WM_RBUTTONDOWN => "WM_RBUTTONDOWN".into(),
            WM_RBUTTONUP => "WM_RBUTTONUP".into(),
            WM_CONTEXTMENU => "WM_CONTEXTMENU".into(),
            0x400 => "NIN_SELECT".into(),
            0x401 => "NIN_KEYSELECT".into(),
            0x406 => "NIN_POPUPOPEN".into(),
            0x407 => "NIN_POPUPCLOSE".into(),
            m => format!("{m:#x}"),
        }
    }

    fn show_menu(hwnd: HWND, x: i32, y: i32) {
        // SAFETY: a popup menu owned by this thread, destroyed after use.
        unsafe {
            let fg = SetForegroundWindow(hwnd).as_bool();
            let m = CreatePopupMenu().unwrap();
            let _ = AppendMenuW(m, MF_STRING, 101, w!("Tray test item"));
            let _ = AppendMenuW(m, MF_STRING, 102, w!("Quit"));
            SetTimer(Some(hwnd), TIMER_MENU, 3000, None);
            log(&format!("menu at {x},{y}, foreground {fg}"));
            let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON, x, y, None, hwnd, None);
            log(&format!("menu closed, command {}", cmd.0));
            let _ = DestroyMenu(m);
        }
    }

    unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        // SAFETY (whole body): plain calls on this thread's windows.
        unsafe {
            if msg == TASKBAR_CREATED.with(|t| t.get()) && msg != 0 {
                log("TaskbarCreated received");
                add_icon(hwnd);
                return LRESULT(0);
            }
            match msg {
                CB => {
                    let v = VERSION.with(|v| v.get());
                    let (ev, uid, x, y) = if v >= 4 {
                        ((lp.0 & 0xffff) as u32, (lp.0 >> 16 & 0xffff) as u32, (wp.0 & 0xffff) as i16 as i32, (wp.0 >> 16 & 0xffff) as i16 as i32)
                    } else {
                        let mut p = POINT::default();
                        let _ = GetCursorPos(&mut p);
                        (lp.0 as u32, wp.0 as u32, p.x, p.y)
                    };
                    if ev != WM_MOUSEMOVE {
                        log(&format!("callback {} uid {uid} at {x},{y}", event_name(ev)));
                    }
                    let menu = if v >= 3 { ev == WM_CONTEXTMENU } else { ev == WM_RBUTTONUP };
                    if menu {
                        show_menu(hwnd, x, y);
                    }
                    LRESULT(0)
                }
                APPBAR_CB => {
                    log(&format!("ABN {} lParam {}", wp.0, lp.0));
                    LRESULT(0)
                }
                WM_TIMER if wp.0 == TIMER_MENU => {
                    let _ = KillTimer(Some(hwnd), TIMER_MENU);
                    let _ = EndMenu();
                    LRESULT(0)
                }
                WM_TIMER if wp.0 == TIMER_END => {
                    if MODE_ICON.with(|m| m.get()) {
                        let n = nid(hwnd);
                        let ok = Shell_NotifyIconW(NIM_DELETE, &n).as_bool();
                        log(&format!("icon DELETE {ok}"));
                    } else {
                        appbar_remove(hwnd);
                    }
                    PostQuitMessage(0);
                    LRESULT(0)
                }
                _ => DefWindowProcW(hwnd, msg, wp, lp),
            }
        }
    }

    fn abd(hwnd: HWND) -> APPBARDATA {
        APPBARDATA { cbSize: size_of::<APPBARDATA>() as u32, hWnd: hwnd, uCallbackMessage: APPBAR_CB, uEdge: ABE_TOP, ..Default::default() }
    }

    fn work_area() -> String {
        let mut r = RECT::default();
        // SAFETY: reads a RECT.
        let _ = unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut r as *mut _ as _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)) };
        format!("{},{} {}x{}", r.left, r.top, r.right - r.left, r.bottom - r.top)
    }

    fn rc(r: RECT) -> String {
        format!("{},{},{},{}", r.left, r.top, r.right, r.bottom)
    }

    fn appbar_dock(hwnd: HWND) {
        // SAFETY: SHAppBarMessage calls with a filled APPBARDATA, then a move of our window.
        unsafe {
            let mut d = abd(hwnd);
            let new = SHAppBarMessage(ABM_NEW, &mut d);
            log(&format!("ABM_NEW {new}, work area before {}", work_area()));
            let w = GetSystemMetrics(SM_CXSCREEN);
            d.rc = RECT { left: 0, top: 0, right: w, bottom: 60 };
            let q = SHAppBarMessage(ABM_QUERYPOS, &mut d);
            log(&format!("ABM_QUERYPOS {q}: {}", rc(d.rc)));
            d.rc.bottom = d.rc.top + 60;
            let s = SHAppBarMessage(ABM_SETPOS, &mut d);
            log(&format!("ABM_SETPOS {s}: {}", rc(d.rc)));
            let _ = MoveWindow(hwnd, d.rc.left, d.rc.top, d.rc.right - d.rc.left, d.rc.bottom - d.rc.top, true);
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            let mut t = abd(hwnd);
            let tp = SHAppBarMessage(ABM_GETTASKBARPOS, &mut t);
            log(&format!("ABM_GETTASKBARPOS {tp}: {} edge {}", rc(t.rc), t.uEdge));
            let mut g = abd(hwnd);
            log(&format!("ABM_GETSTATE {}", SHAppBarMessage(ABM_GETSTATE, &mut g)));
            let mut a = abd(hwnd);
            log(&format!("ABM_ACTIVATE {}", SHAppBarMessage(ABM_ACTIVATE, &mut a)));
            let mut h = abd(hwnd);
            h.uEdge = ABE_TOP;
            log(&format!("ABM_GETAUTOHIDEBAR {}", SHAppBarMessage(ABM_GETAUTOHIDEBAR, &mut h)));
            log(&format!("work area docked {}", work_area()));
        }
    }

    fn appbar_remove(hwnd: HWND) {
        let mut d = abd(hwnd);
        // SAFETY: unregisters our bar.
        let r = unsafe { SHAppBarMessage(ABM_REMOVE, &mut d) };
        std::thread::sleep(std::time::Duration::from_millis(300));
        log(&format!("ABM_REMOVE {r}, work area after {}", work_area()));
    }

    pub fn main() {
        T0.with(|t| t.set(Some(Instant::now())));
        let args: Vec<String> = std::env::args().skip(1).collect();
        let mode = args.first().map(String::as_str).unwrap_or("icon");
        let secs: u32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(30);
        let version: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(4);
        MODE_ICON.with(|m| m.set(mode == "icon"));
        VERSION.with(|v| v.set(version));
        log(&format!("--- {mode} {secs} s, version {version}, integrity {:#x}", integrity()));
        // SAFETY: window creation and a message loop on this thread.
        unsafe {
            let inst = GetModuleHandleW(None).unwrap();
            let brush: HBRUSH = CreateSolidBrush(COLORREF(0x00_80_40_00));
            let wc = WNDCLASSW { lpfnWndProc: Some(proc_), hInstance: inst.into(), lpszClassName: w!("explorust.traytest"), hbrBackground: brush, ..Default::default() };
            RegisterClassW(&wc);
            let (ex, style) = if mode == "icon" { (WS_EX_TOOLWINDOW, WS_POPUP) } else { (WS_EX_TOOLWINDOW | WS_EX_TOPMOST, WS_POPUP) };
            let hwnd = CreateWindowExW(ex, w!("explorust.traytest"), w!("tray test"), style, 0, 0, 10, 10, None, None, Some(inst.into()), None).unwrap();
            let tc = RegisterWindowMessageW(w!("TaskbarCreated"));
            TASKBAR_CREATED.with(|t| t.set(tc));
            let _ = ChangeWindowMessageFilterEx(hwnd, tc, MSGFLT_ALLOW, None);
            if mode == "icon" {
                add_icon(hwnd);
            } else {
                appbar_dock(hwnd);
            }
            SetTimer(Some(hwnd), TIMER_END, secs * 1000, None);
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let _ = DestroyWindow(hwnd);
            log("exit");
            let _ = PCWSTR::null();
        }
    }
}
