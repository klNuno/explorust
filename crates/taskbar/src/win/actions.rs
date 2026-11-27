//! What a button does to windows and apps: activate, minimize, launch through the shell.

use super::{log, winx};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WPARAM};
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
    BringWindowToTop, FindWindowExW, GetClassNameW, GetForegroundWindow, GetWindowThreadProcessId, IsIconic, PostMessageW, SC_CLOSE, SC_MINIMIZE, SC_RESTORE,
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
/// None for a frame whose app cannot be found (ending its host would close other apps' frames).
fn content_pid(h: HWND) -> Option<u32> {
    let mut pid = 0u32;
    let mut class = [0u16; 64];
    // SAFETY: window queries on a handle; a stale one gives 0.
    unsafe {
        let n = GetClassNameW(h, &mut class);
        if String::from_utf16_lossy(&class[..n.max(0) as usize]) == "ApplicationFrameWindow" {
            let core = FindWindowExW(Some(h), None, w!("Windows.UI.Core.CoreWindow"), None).ok()?;
            GetWindowThreadProcessId(core, Some(&mut pid));
        } else {
            GetWindowThreadProcessId(h, Some(&mut pid));
        }
    }
    (pid != 0).then_some(pid)
}

/// The full image path of an open process, None when Windows does not give it.
fn image_path(h: HANDLE) -> Option<String> {
    let mut buf = vec![0u16; 1024];
    let mut n = buf.len() as u32;
    // SAFETY: fills a local buffer of `n` units.
    unsafe { QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut n) }.ok()?;
    Some(String::from_utf16_lossy(&buf[..n as usize]))
}

/// Ends the processes behind a button's windows at once, like Windows' "End task" (Settings >
/// System > For developers), which also stops an app that no longer answers. Never ends this
/// shell, explorer.exe, a UWP frame host or a process unknown by name. A process that also owns
/// a window of another button (`others`) is not ended: this button's windows get a close instead.
pub fn end_task(wins: &[isize], others: &[isize]) {
    // SAFETY: plain process id query.
    let me = unsafe { GetCurrentProcessId() };
    let shared: Vec<u32> = others.iter().filter_map(|h| content_pid(HWND(*h as _))).collect();
    let mut pids = Vec::new();
    for &w in wins {
        let h = HWND(w as _);
        match content_pid(h) {
            Some(pid) if shared.contains(&pid) => {
                log::line(&format!("end task: pid {pid} also owns another button's window, closing {w:#x} instead"));
                // SAFETY: asks the window to close, like its own close button.
                let _ = unsafe { PostMessageW(Some(h), WM_SYSCOMMAND, WPARAM(SC_CLOSE as usize), LPARAM(0)) };
            }
            Some(pid) if pid != me => pids.push(pid),
            Some(_) => {}
            None => log::line(&format!("end task: no app process for window {w:#x}, skipped")),
        }
    }
    pids.sort_unstable();
    pids.dedup();
    for pid in pids {
        // SAFETY: a handle we open, query, use and close here.
        unsafe {
            let h = match OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                Ok(h) => h,
                Err(e) => {
                    log::line(&format!("end task: cannot open pid {pid}: {e}"));
                    continue;
                }
            };
            match image_path(h) {
                None => log::line(&format!("end task: pid {pid} has no image path, kept")),
                Some(path) => {
                    let name = path.rsplit('\\').next().unwrap_or("");
                    if ["explorer.exe", "ApplicationFrameHost.exe"].iter().any(|k| name.eq_ignore_ascii_case(k)) {
                        log::line(&format!("end task: kept {path} (pid {pid})"));
                    } else {
                        match TerminateProcess(h, 1) {
                            Ok(()) => log::line(&format!("end task: ended {path} (pid {pid})")),
                            Err(e) => log::line(&format!("end task: {path} (pid {pid}): {e}")),
                        }
                    }
                }
            }
            let _ = CloseHandle(h);
        }
    }
}
