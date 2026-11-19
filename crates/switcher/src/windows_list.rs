//! The Alt+Tab windows: taskbar-eligible top-level windows in Z-order with the foreground one
//! first, their titles, and the app identity their icon comes from (the AUMID of the window or its
//! process, for a UWP frame the process of its CoreWindow, else the exe path).

use shelldata::items;
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowExW, GA_ROOT, GW_OWNER, GWL_EXSTYLE, GetAncestor, GetClassNameW, GetForegroundWindow,
    GetWindow, GetWindowLongW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible, WS_EX_APPWINDOW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};
use windows_core::{BOOL, w};

#[derive(Clone, Debug)]
pub struct Entry {
    pub hwnd: HWND,
    pub title: String,
    /// Lowercase icon key: the AUMID, else the exe path.
    pub key: String,
    pub aumid: Option<String>,
    pub exe: String,
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    // SAFETY: fills a local buffer.
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

pub fn title(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: fills a local buffer.
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

pub fn pid_of(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    // SAFETY: writes the process id into a local.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

/// The entry of a window that belongs in Alt+Tab, None otherwise.
pub fn entry(hwnd: HWND, own: HWND) -> Option<Entry> {
    if hwnd == own {
        return None;
    }
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
    let title = title(hwnd);
    let pid = pid_of(hwnd);
    let exe = items::process_path(pid).unwrap_or_default();
    let aumid = if class == "ApplicationFrameWindow" {
        // A frame shows its app's CoreWindow; with none it is an empty or suspended frame.
        // SAFETY: a child lookup by class.
        let core = unsafe { FindWindowExW(Some(hwnd), None, w!("Windows.UI.Core.CoreWindow"), None) }.ok();
        let core_aumid = core.filter(|c| !c.is_invalid()).and_then(|c| items::process_app_id(pid_of(c)));
        Some(items::window_app_id(hwnd).or(core_aumid)?)
    } else {
        items::window_app_id(hwnd).or_else(|| items::process_app_id(pid))
    };
    let key = aumid.clone().unwrap_or_else(|| exe.clone()).to_lowercase();
    Some(Entry { hwnd, title, key, aumid, exe })
}

/// Every Alt+Tab window, the foreground one first, then the others in Z-order.
pub fn snapshot(own: HWND) -> Vec<Entry> {
    unsafe extern "system" fn cb(hwnd: HWND, lp: LPARAM) -> BOOL {
        // SAFETY: `lp` is the Vec passed below, alive for the enumeration.
        unsafe { (*(lp.0 as *mut Vec<HWND>)).push(hwnd) };
        true.into()
    }
    let mut all: Vec<HWND> = Vec::new();
    // SAFETY: enumerates into a local Vec.
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut all as *mut _ as isize));
    }
    let mut out: Vec<Entry> = all.into_iter().filter_map(|h| entry(h, own)).collect();
    // SAFETY: a plain query.
    let fg = unsafe { GetForegroundWindow() };
    let fg_root = unsafe { GetAncestor(fg, GA_ROOT) };
    if let Some(i) = out.iter().position(|e| e.hwnd == fg || e.hwnd == fg_root) {
        let e = out.remove(i);
        out.insert(0, e);
    }
    out
}

/// The app icon of an entry at `px` pixels, from the shell's image factory. COM must be initialised.
pub fn icon(aumid: Option<&str>, exe: &str, px: i32) -> Option<shelldata::Image> {
    aumid
        .and_then(items::apps_folder_item)
        .and_then(|item| shelldata::icons::icon_for_item(&item, px))
        .or_else(|| (!exe.is_empty()).then(|| shelldata::icons::icon_for_path(exe, px)).flatten())
}
