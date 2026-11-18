//! Which windows get a task button, and the app identity they group on: the AUMID of the
//! window's property store, then the one of its process (for a UWP frame, the process of its
//! CoreWindow), then the exe path.

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
pub fn task_of(hwnd: HWND) -> Option<Task> {
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
    }
    if items::is_cloaked(hwnd) {
        return None;
    }
    let class = class_name(hwnd);
    if matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd") {
        return None;
    }
    let pid = pid_of(hwnd);
    if pid == std::process::id() {
        return None;
    }
    let exe = items::process_path(pid).unwrap_or_default();
    let exe_l = exe.to_lowercase();
    if exe_l.ends_with("\\explorust.exe") || exe_l.ends_with("\\explorust-guard.exe") {
        return None;
    }
    let key = if class == "ApplicationFrameWindow" {
        // A frame shows its app's CoreWindow; with none it is an empty or suspended frame.
        // SAFETY: a child lookup by class.
        let core = unsafe { FindWindowExW(Some(hwnd), None, w!("Windows.UI.Core.CoreWindow"), None) }.ok();
        let core_aumid = core.filter(|c| !c.is_invalid()).and_then(|c| items::process_app_id(pid_of(c)));
        items::window_app_id(hwnd).or(core_aumid)?
    } else {
        items::window_app_id(hwnd).or_else(|| items::process_app_id(pid)).unwrap_or_else(|| exe.clone())
    };
    if key.is_empty() {
        return None;
    }
    Some(Task { key: key.to_lowercase(), id: key, exe })
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
