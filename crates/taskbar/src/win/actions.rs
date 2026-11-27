//! What a button does to windows and apps: activate, minimize, launch through the shell.

use super::{log, winx};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentProcessId, GetCurrentThreadId, OpenProcess, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, QueryFullProcessImageNameW, TerminateProcess,
};
use windows::core::{PWSTR, w};
use windows::Win32::UI::Shell::{
    SEE_MASK_FLAG_NO_UI, SEE_MASK_INVOKEIDLIST, SHELLEXECUTEINFOW, SHGetIDListFromObject, ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, FindWindowExW, GetForegroundWindow, GetWindowThreadProcessId, IsIconic, PostMessageW, SC_MINIMIZE, SC_RESTORE,
    SW_SHOWNORMAL, SetForegroundWindow, SwitchToThisWindow, WM_SYSCOMMAND,
};

/// How to start an app.
#[derive(Clone, Debug)]
pub enum Launch {
    /// A pin's absolute PIDL (AppsFolder item or shortcut).
    Pidl(Vec<u8>),
    /// An AUMID, with the running application's executable as a fallback.
    Aumid { id: String, exe: Option<String> },
    /// An exe path.
    Path(String),
    /// An exe path with a fixed command-line argument.
    Command(String, &'static str),
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
            Launch::Aumid { id, exe } => exec_aumid(id)
                || exe.as_deref().is_some_and(|path| winx::open(path, None, None)),
            Launch::Path(path) => winx::open(path, None, None),
            Launch::Command(path, arg) => winx::open(path, None, Some(arg)),
        }
    }
}

unsafe fn exec_aumid(id: &str) -> bool {
    let Some(item) = shelldata::items::apps_folder_item(id) else { return false };
    // SAFETY: the returned PIDL is valid until freed with the COM allocator.
    unsafe {
        let Ok(pidl) = SHGetIDListFromObject(&item) else { return false };
        let ok = exec_idlist(pidl as _);
        CoTaskMemFree(Some(pidl as _));
        ok
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

/// The process that owns `h`'s content: a UWP frame's hosted CoreWindow, else the window's own.
fn content_pid(h: HWND) -> u32 {
    let mut pid = 0u32;
    // SAFETY: window queries on a handle; a stale one gives 0.
    unsafe {
        if let Ok(core) = FindWindowExW(Some(h), None, w!("Windows.UI.Core.CoreWindow"), None) {
            GetWindowThreadProcessId(core, Some(&mut pid));
        }
        if pid == 0 {
            GetWindowThreadProcessId(h, Some(&mut pid));
        }
    }
    pid
}

/// Ends the processes behind a button's windows at once, like Windows' "End task" (Settings >
/// System > For developers), which also stops an app that no longer answers. Never ends this
/// shell or explorer.exe.
pub fn end_task(wins: &[isize]) {
    // SAFETY: plain process id query.
    let me = unsafe { GetCurrentProcessId() };
    let mut pids: Vec<u32> = wins.iter().map(|h| content_pid(HWND(*h as _))).filter(|p| *p != 0 && *p != me).collect();
    pids.sort_unstable();
    pids.dedup();
    for pid in pids {
        // SAFETY: a handle we open, query and close here.
        unsafe {
            let h = match OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                Ok(h) => h,
                Err(e) => {
                    log::line(&format!("end task: cannot open pid {pid}: {e}"));
                    continue;
                }
            };
            let mut buf = [0u16; 260];
            let mut n = buf.len() as u32;
            let path = if QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut n).is_ok() {
                String::from_utf16_lossy(&buf[..n as usize])
            } else {
                String::new()
            };
            let name = path.rsplit('\\').next().unwrap_or("");
            if name.eq_ignore_ascii_case("explorer.exe") {
                log::line(&format!("end task: kept {path} (pid {pid})"));
            } else {
                match TerminateProcess(h, 1) {
                    Ok(()) => log::line(&format!("end task: ended {path} (pid {pid})")),
                    Err(e) => log::line(&format!("end task: {path} (pid {pid}): {e}")),
                }
            }
            let _ = CloseHandle(h);
        }
    }
}
