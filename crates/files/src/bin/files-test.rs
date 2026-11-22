//! files-test.exe: a guest test tool for the Files part, run from a process with no window.
//! Steps: `click|dclick|mclick|move:<x>,<y>` (screen pixels), `drag:<x1>,<y1>,<x2>,<y2>` (left
//! button), `wait:<s>`, `select:<path>` (`SHOpenFolderAndSelectItems`, an app's "show in folder"),
//! `exec:<path>` (`ShellExecuteEx` open, as the Run box does), `windows` (what the shell's window
//! list reports for each window: its URL and its active view's folder), `frames` (visibility
//! and DWM cloak state of Files windows), `activate:<x>,<y>` (click a verified Files window),
//! `maximize` or `place:<x>,<y>,<width>,<height>` (the foreground Files window), `focus` (native
//! focus and ancestor classes). Results go to `files-test.log` beside the exe.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
fn main() {
    use std::io::Write;
    use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
    // SAFETY: process-wide settings of this tool, before any other call.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
    }
    let mut out = Vec::new();
    for arg in std::env::args().skip(1) {
        let r = win::step(&arg);
        out.push(format!("{arg}: {r}"));
    }
    let path = std::env::current_exe().unwrap_or_default().with_file_name("files-test.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{}", out.join("\r\n"));
    }
}

#[cfg(target_os = "windows")]
mod win {
    use std::time::Duration;
    use windows::Win32::Foundation::{HWND, POINT};
    use windows::Win32::System::Com::*;
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    use windows::Win32::UI::Shell::*;
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows_core::{BSTR, Interface, PCWSTR};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn nums(s: &str) -> Vec<i32> {
        s.split(',').filter_map(|v| v.trim().parse().ok()).collect()
    }

    fn mouse(flags: MOUSE_EVENT_FLAGS) {
        let i = INPUT { r#type: INPUT_MOUSE, Anonymous: INPUT_0 { mi: MOUSEINPUT { dwFlags: flags, ..Default::default() } } };
        unsafe { SendInput(&[i], size_of::<INPUT>() as i32) };
        std::thread::sleep(Duration::from_millis(40));
    }

    fn goto(x: i32, y: i32) {
        unsafe {
            let _ = SetCursorPos(x, y);
        }
        std::thread::sleep(Duration::from_millis(60));
    }

    fn under(x: i32, y: i32) -> String {
        unsafe {
            let h = WindowFromPoint(POINT { x, y });
            let mut buf = [0u16; 64];
            let n = GetClassNameW(h, &mut buf) as usize;
            format!("under {}", String::from_utf16_lossy(&buf[..n]))
        }
    }

    fn is_files(hwnd: HWND) -> bool {
        let mut class = [0u16; 64];
        let len = unsafe { GetClassNameW(hwnd, &mut class) } as usize;
        String::from_utf16_lossy(&class[..len]) == "explorust.FilesWindow"
    }

    fn activate(x: i32, y: i32) -> String {
        goto(x, y);
        let hwnd = unsafe { GetAncestor(WindowFromPoint(POINT { x, y }), GA_ROOT) };
        if !is_files(hwnd) {
            return "activation point is covered by another window".into();
        }
        mouse(MOUSEEVENTF_LEFTDOWN);
        mouse(MOUSEEVENTF_LEFTUP);
        if unsafe { GetForegroundWindow() } == hwnd {
            "ok".into()
        } else {
            "Files did not get the foreground".into()
        }
    }

    pub fn step(arg: &str) -> String {
        let (verb, rest) = arg.split_once(':').unwrap_or((arg, ""));
        let n = nums(rest);
        match verb {
            "wait" => {
                std::thread::sleep(Duration::from_secs_f32(rest.parse().unwrap_or(1.0)));
                "ok".into()
            }
            "move" | "click" | "dclick" | "mclick" if n.len() == 2 => {
                if verb != "move" && !is_files(unsafe { GetForegroundWindow() }) {
                    return "Files does not have the foreground".into();
                }
                goto(n[0], n[1]);
                let (down, up) = if verb == "mclick" {
                    (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP)
                } else {
                    (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP)
                };
                let times = match verb {
                    "move" => 0,
                    "dclick" => 2,
                    _ => 1,
                };
                for _ in 0..times {
                    mouse(down);
                    mouse(up);
                }
                under(n[0], n[1])
            }
            "drag" if n.len() == 4 => {
                goto(n[0], n[1]);
                mouse(MOUSEEVENTF_LEFTDOWN);
                for s in 1..=10 {
                    goto(n[0] + (n[2] - n[0]) * s / 10, n[1] + (n[3] - n[1]) * s / 10);
                }
                mouse(MOUSEEVENTF_LEFTUP);
                under(n[2], n[3])
            }
            "select" => select(rest),
            "exec" => exec(rest),
            "windows" => windows(),
            "frames" => frames(),
            "activate" if n.len() == 2 => activate(n[0], n[1]),
            "maximize" => maximize(),
            "place" if n.len() == 4 => place(&n),
            "focus" => focus(),
            _ => "unknown step".into(),
        }
    }

    fn maximize() -> String {
        unsafe {
            let hwnd = GetForegroundWindow();
            if !is_files(hwnd) {
                return "Files does not have the foreground".into();
            }
            let _ = ShowWindow(hwnd, SW_MAXIMIZE);
            "ok".into()
        }
    }

    fn place(n: &[i32]) -> String {
        unsafe {
            let hwnd = GetForegroundWindow();
            if !is_files(hwnd) {
                return "Files does not have the foreground".into();
            }
            let _ = ShowWindowAsync(hwnd, SW_RESTORE);
            format!("{:?}", SetWindowPos(hwnd, None, n[0], n[1], n[2], n[3], SWP_NOACTIVATE | SWP_NOZORDER | SWP_ASYNCWINDOWPOS))
        }
    }

    fn focus() -> String {
        unsafe {
            let root = GetForegroundWindow();
            let thread = GetWindowThreadProcessId(root, None);
            let mut info = GUITHREADINFO { cbSize: size_of::<GUITHREADINFO>() as u32, ..Default::default() };
            if let Err(error) = GetGUIThreadInfo(thread, &mut info) {
                return format!("{error}");
            }
            let mut hwnd = info.hwndFocus;
            let mut ancestors = Vec::new();
            while !hwnd.is_invalid() {
                let mut class = [0u16; 64];
                let len = GetClassNameW(hwnd, &mut class) as usize;
                ancestors.push(String::from_utf16_lossy(&class[..len]));
                match GetParent(hwnd) {
                    Ok(parent) => hwnd = parent,
                    Err(_) => break,
                }
            }
            ancestors.join(" > ")
        }
    }

    fn frames() -> String {
        use windows::Win32::Foundation::LPARAM;
        use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
        use windows_core::BOOL;
        unsafe extern "system" fn one(hwnd: HWND, data: LPARAM) -> BOOL {
            unsafe {
                if !is_files(hwnd) {
                    return true.into();
                }
                let mut cloak = 0u32;
                let result = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, (&mut cloak as *mut u32).cast(), 4);
                let mut rect = Default::default();
                let _ = GetWindowRect(hwnd, &mut rect);
                let output = &mut *(data.0 as *mut Vec<String>);
                output.push(format!("{:#x} visible={} cloak={cloak} {result:?} {rect:?}", hwnd.0 as usize, IsWindowVisible(hwnd).as_bool()));
            }
            true.into()
        }

        let mut output = Vec::<String>::new();
        unsafe { let _ = EnumWindows(Some(one), LPARAM((&mut output as *mut Vec<String>) as isize)); }
        output.join("; ")
    }

    fn select(path: &str) -> String {
        let w = wide(path);
        let mut p = std::ptr::null_mut();
        unsafe {
            if let Err(e) = SHParseDisplayName(PCWSTR(w.as_ptr()), None, &mut p, 0, None) {
                return format!("parse failed: {e}");
            }
            let t = std::time::Instant::now();
            let r = SHOpenFolderAndSelectItems(p, None, 0);
            CoTaskMemFree(Some(p as *const _));
            format!("{r:?} in {:.0} ms", t.elapsed().as_secs_f64() * 1000.0)
        }
    }

    fn exec(path: &str) -> String {
        let w = wide(path);
        let mut info = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            lpFile: PCWSTR(w.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        format!("{:?}", unsafe { ShellExecuteExW(&mut info) })
    }

    fn windows() -> String {
        unsafe {
            let sw: IShellWindows = match CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER) {
                Ok(s) => s,
                Err(e) => return format!("ShellWindows: {e}"),
            };
            let count = sw.Count().unwrap_or(-1);
            let mut out = vec![format!("{count} windows")];
            for i in 0..count {
                let Ok(d) = sw.Item(&VARIANT::from(i)) else { continue };
                let mut line = format!("[{i}]");
                line += &format!(" url {:?}", prop(&d, "LocationURL"));
                line += &format!(" hwnd {:?}", prop(&d, "HWND"));
                let sb: windows_core::Result<IShellBrowser> =
                    d.cast::<IServiceProvider>().and_then(|sp| sp.QueryService(&SID_STopLevelBrowser));
                match sb {
                    Ok(sb) => {
                        let folder = sb
                            .QueryActiveShellView()
                            .and_then(|v| v.cast::<IFolderView>())
                            .and_then(|fv| fv.GetFolder::<IPersistFolder2>())
                            .and_then(|pf| pf.GetCurFolder())
                            .and_then(|pidl| {
                                let item: IShellItem = SHCreateItemFromIDList(pidl)?;
                                CoTaskMemFree(Some(pidl as *const _));
                                let s = item.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING)?;
                                let r = s.to_string().unwrap_or_default();
                                CoTaskMemFree(Some(s.0 as *const _));
                                Ok(r)
                            });
                        line += &format!(" view folder {folder:?}");
                    }
                    Err(e) => line += &format!(" no browser: {e}"),
                }
                out.push(line);
            }
            out.join("; ")
        }
    }

    fn prop(d: &IDispatch, name: &str) -> String {
        unsafe {
            let w = wide(name);
            let names = [PCWSTR(w.as_ptr())];
            let mut id = 0i32;
            if let Err(e) = d.GetIDsOfNames(&windows_core::GUID::zeroed(), names.as_ptr(), 1, 0, &mut id) {
                return format!("({e})");
            }
            let params = DISPPARAMS::default();
            let mut v = VARIANT::default();
            match d.Invoke(id, &windows_core::GUID::zeroed(), 0, DISPATCH_PROPERTYGET, &params, Some(&mut v), None, None) {
                Ok(()) => BSTR::try_from(&v).map(|b| b.to_string()).or_else(|_| i32::try_from(&v).map(|n| format!("{n:#x}"))).unwrap_or_else(|_| "?".into()),
                Err(e) => format!("({e})"),
            }
        }
    }

}

#[cfg(not(target_os = "windows"))]
fn main() {}
