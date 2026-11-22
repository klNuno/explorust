//! Which windows get a task button, and the app identity they group on: the AUMID of the
//! window's property store, then the explicit or packaged AUMID of its process, then the exe path.

use super::app_resolver::Resolver;
use shelldata::items;
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowExW, GA_ROOT, GW_OWNER, GWL_EXSTYLE, GetAncestor, GetClassNameW, GetWindow, GetWindowLongW,
    GetWindowThreadProcessId, IsWindowVisible, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};
use windows_core::{BOOL, w};

/// A window that gets a button: its group key (lowercase) and its exe path.
pub struct Task {
    pub key: String,
    /// The app id as Windows gave it (case kept, for launching through AppsFolder).
    pub id: String,
    pub exe: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub handle: isize,
    pid: u32,
    app_frame: bool,
    core_pid: Option<u32>,
}

pub fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    // SAFETY: fills a local buffer.
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn pid_of(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    // SAFETY: writes the process id into a local.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

/// The windows that belong on a taskbar: visible, top-level, not cloaked, not a tool window,
/// unowned or WS_EX_APPWINDOW, plus UWP frames once they hold their app. None otherwise.
pub fn candidate(hwnd: HWND) -> Option<Window> {
    // SAFETY: style and state reads on a window handle (a stale one only fails the reads).
    unsafe {
        if hwnd.is_invalid() || !IsWindowVisible(hwnd).as_bool() || GetAncestor(hwnd, GA_ROOT) != hwnd {
            return None;
        }
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        let app = ex & WS_EX_APPWINDOW.0 != 0;
        let owned = GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid());
        if !app && (ex & WS_EX_TOOLWINDOW.0 != 0 || ex & WS_EX_NOACTIVATE.0 != 0 || owned) {
            return None;
        }
        // ITaskbarList::DeleteTab marks the window with this property.
        if !windows::Win32::UI::WindowsAndMessaging::GetPropW(hwnd, windows_core::w!("ITaskList_Deleted")).is_invalid() {
            return None;
        }
    }
    if items::is_cloaked(hwnd) {
        return None;
    }
    let class = class_name(hwnd);
    // A top-level CoreWindow is a shell experience (the notification center, Start, search, the
    // touch keyboard): visible, unowned and not a tool window, yet explorer gives it no button; an
    // app's CoreWindow sits inside its ApplicationFrameWindow. (The notification center got a
    // button with each open, g1, 2026-11-20.)
    if matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "Windows.UI.Core.CoreWindow") {
        return None;
    }
    let pid = pid_of(hwnd);
    if pid == 0 || pid == std::process::id() {
        return None;
    }
    let app_frame = class == "ApplicationFrameWindow";
    let core_pid = if app_frame {
        // A frame shows its app's CoreWindow; with none it is an empty or suspended frame.
        // SAFETY: a child lookup by class.
        unsafe { FindWindowExW(Some(hwnd), None, w!("Windows.UI.Core.CoreWindow"), None) }
            .ok().map(pid_of).filter(|&pid| pid != 0)
    } else {
        None
    };
    Some(Window { handle: hwnd.0 as isize, pid, app_frame, core_pid })
}

impl Window {
    pub fn resolve(self, resolver: Option<&Resolver>) -> Option<Task> {
        let hwnd = HWND(self.handle as _);
        let exe = items::process_path(self.pid).unwrap_or_default();
        let exe_l = exe.to_lowercase();
        if exe_l.ends_with("\\explorust.exe") || exe_l.ends_with("\\explorust-guard.exe") {
            return None;
        }
        let app_pid = if self.app_frame { self.core_pid } else { Some(self.pid) };
        let id = items::window_app_id(hwnd).or_else(|| {
            let pid = app_pid?;
            resolver.and_then(|r| r.explicit_id(pid, (!self.app_frame).then_some(hwnd)))
                .or_else(|| items::process_app_id(pid))
        });
        // An empty UWP frame should never get a button for ApplicationFrameHost.exe.
        let id = id.or_else(|| (!self.app_frame && !exe.is_empty()).then(|| exe.clone()))?;
        Some(Task { key: id.to_lowercase(), id, exe })
    }
}

/// Every top-level window, in z-order.
pub fn top_level() -> Vec<HWND> {
    unsafe extern "system" fn cb(hwnd: HWND, lp: LPARAM) -> BOOL {
        // SAFETY: `lp` is the Vec passed below, alive for the enumeration.
        unsafe { (*(lp.0 as *mut Vec<HWND>)).push(hwnd) };
        true.into()
    }
    let mut out: Vec<HWND> = Vec::new();
    // SAFETY: enumerates into a local Vec.
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut out as *mut _ as isize));
    }
    out
}
