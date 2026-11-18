//! What a button does to windows and apps: activate, minimize, launch through the shell.

use super::log;
use shelldata::wide;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Shell::{
    SEE_MASK_FLAG_NO_UI, SEE_MASK_INVOKEIDLIST, SHELLEXECUTEINFOW, SHGetIDListFromObject, ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, IsIconic, PostMessageW, SC_MINIMIZE, SC_RESTORE,
    SW_SHOWNORMAL, SetForegroundWindow, SwitchToThisWindow, WM_SYSCOMMAND,
};
use windows_core::{PCWSTR, w};

/// How to start an app.
#[derive(Clone, Debug)]
pub enum Launch {
    /// A pin's absolute PIDL (AppsFolder item or shortcut).
    Pidl(Vec<u8>),
    /// An AUMID, started through its `shell:AppsFolder` item.
    Aumid(String),
    /// An exe path.
    Path(String),
}

/// Starts the app on a short-lived STA thread, so a slow shell verb never blocks the bar.
pub fn launch(what: Launch) {
    let _ = std::thread::Builder::new().name("taskbar-launch".into()).spawn(move || {
        // SAFETY: COM for this thread only, released at its end.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let ok = run_launch(&what);
            log::line(&format!("launch {what:?}: {}", if ok { "ok" } else { "failed" }));
            CoUninitialize();
        }
    });
}

unsafe fn run_launch(what: &Launch) -> bool {
    // SAFETY: the shell calls get pointers that live for the call.
    unsafe {
        match what {
            Launch::Pidl(p) => exec_idlist(p.as_ptr() as _),
            Launch::Aumid(id) => {
                let Some(item) = shelldata::items::apps_folder_item(id) else { return false };
                let Ok(pidl) = SHGetIDListFromObject(&item) else { return false };
                let ok = exec_idlist(pidl as _);
                CoTaskMemFree(Some(pidl as _));
                ok
            }
            Launch::Path(path) => {
                let f = wide(path);
                let mut sei = SHELLEXECUTEINFOW {
                    cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
                    fMask: SEE_MASK_FLAG_NO_UI,
                    lpVerb: w!("open"),
                    lpFile: PCWSTR(f.as_ptr()),
                    nShow: SW_SHOWNORMAL.0,
                    ..Default::default()
                };
                ShellExecuteExW(&mut sei).is_ok()
            }
        }
    }
}

unsafe fn exec_idlist(pidl: *mut core::ffi::c_void) -> bool {
    let mut sei = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_INVOKEIDLIST | SEE_MASK_FLAG_NO_UI,
        lpIDList: pidl,
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: `pidl` is a valid ITEMIDLIST for the call.
    unsafe { ShellExecuteExW(&mut sei).is_ok() }
}

/// Restores the window when minimized and brings it to the foreground. True when it is in front.
pub fn activate(h: HWND) -> bool {
    // SAFETY: window calls on a handle; a stale one only fails.
    unsafe {
        if IsIconic(h).as_bool() {
            let _ = PostMessageW(Some(h), WM_SYSCOMMAND, WPARAM(SC_RESTORE as usize), LPARAM(0));
        }
        if SetForegroundWindow(h).as_bool() && GetForegroundWindow() == h {
            return true;
        }
        // Not allowed by the foreground lock (a command from the bus, not a click): join the
        // foreground thread's input state for the call.
        let fg = GetForegroundWindow();
        let fg_thread = GetWindowThreadProcessId(fg, None);
        let me = GetCurrentThreadId();
        let attached = fg_thread != 0 && fg_thread != me && AttachThreadInput(me, fg_thread, true).as_bool();
        let _ = BringWindowToTop(h);
        let _ = SetForegroundWindow(h);
        if attached {
            let _ = AttachThreadInput(me, fg_thread, false);
        }
        if GetForegroundWindow() != h {
            SwitchToThisWindow(h, true);
        }
        GetForegroundWindow() == h
    }
}

/// Minimizes like the window's own button, so Windows asks the shell hook where to animate.
pub fn minimize(h: HWND) {
    // SAFETY: posts a system command to the window.
    let _ = unsafe { PostMessageW(Some(h), WM_SYSCOMMAND, WPARAM(SC_MINIMIZE as usize), LPARAM(0)) };
}
