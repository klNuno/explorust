//! The loader, inside explorust: an out-of-context WinEvent hook sees windows shown by other
//! processes; a target gets its DWM attributes from here when DWM takes them across processes,
//! and the DLL (a `WH_GETMESSAGE` hook on its thread only) for what must run in the host. The hook
//! goes when the window is destroyed or explorust leaves, and the host unloads the DLL with it.

use crate::dark;
use crate::log;
use crate::target::{self, Kind};
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Instant;
use windows::Win32::Foundation::{CloseHandle, FreeLibrary, HINSTANCE, HMODULE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMSBT_MAINWINDOW, DWMSBT_TRANSIENTWINDOW};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, IsWow64Process, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, HSTRING, PCWSTR, PWSTR};

const EVENT_OBJECT_DESTROY: u32 = 0x8001;
const EVENT_OBJECT_SHOW: u32 = 0x8002;
const WINEVENT_OUTOFCONTEXT: u32 = 0;
const WINEVENT_SKIPOWNPROCESS: u32 = 2;
const RETRY_TIMER: usize = 1;
/// bus::MSG_COMMAND with wParam = bus::Command::Shutdown (15): the shell's way to close a part.
const BUS_COMMAND: u32 = 0x8000 + 0x100;
const BUS_SHUTDOWN: usize = 15;

struct Hooked {
    hook: HHOOK,
    windows: Vec<isize>,
}

struct State {
    dll: HMODULE,
    proc: HOOKPROC,
    style_msg: u32,
    done_msg: u32,
    /// Thread hooks by thread id, with the target windows of that thread.
    threads: HashMap<u32, Hooked>,
    /// Windows already handled, so a second show does nothing.
    seen: HashMap<isize, Kind>,
    /// When the show event of a host's target arrived, for the latency log.
    pending: HashMap<u32, (Instant, Kind)>,
    /// Settings CoreWindows not yet inside their frame: retried on a timer.
    retry: Vec<(isize, u8)>,
    hwnd: HWND,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Asks a running loader (this process or another) to remove its hooks and end its thread.
pub fn shutdown() {
    // SAFETY: plain window lookup and a posted message.
    unsafe {
        if let Ok(w) = FindWindowExW(Some(HWND_MESSAGE), None, &HSTRING::from(crate::WINDOW_CLASS), PCWSTR::null()) {
            let _ = PostMessageW(Some(w), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn run() {
    if crate::switched_off() {
        log::line("off (parts-off.txt)");
        return;
    }
    let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) else { return };
    let path = dir.join(crate::DLL_NAME);
    // SAFETY: our own DLL, whose DllMain does nothing; the export has the HOOKPROC signature.
    let (dll, proc) = unsafe {
        let Ok(dll) = LoadLibraryW(&HSTRING::from(path.as_os_str())) else {
            log::line(&format!("{} missing, injector idle", path.display()));
            return;
        };
        let Some(p) = GetProcAddress(dll, windows::core::PCSTR(crate::HOOK_EXPORT.as_ptr())) else {
            log::line("hook export missing, injector idle");
            let _ = FreeLibrary(dll);
            return;
        };
        (dll, Some(std::mem::transmute::<_, unsafe extern "system" fn(i32, WPARAM, LPARAM) -> LRESULT>(p)))
    };
    // SAFETY: the window, the event hook and the thread hooks all live and die on this thread.
    unsafe {
        let inst: HINSTANCE = GetModuleHandleW(None).map(Into::into).unwrap_or_default();
        let class = HSTRING::from(crate::WINDOW_CLASS);
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: inst, lpszClassName: PCWSTR(class.as_ptr()), ..Default::default() };
        RegisterClassW(&wc);
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0), &class, &class, WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(inst), None,
        ) else {
            let _ = FreeLibrary(dll);
            return;
        };
        let state = State {
            dll,
            proc,
            style_msg: RegisterWindowMessageW(&HSTRING::from(crate::MSG_STYLE)),
            done_msg: RegisterWindowMessageW(&HSTRING::from(crate::MSG_DONE)),
            threads: HashMap::new(),
            seen: HashMap::new(),
            pending: HashMap::new(),
            retry: Vec::new(),
            hwnd,
        };
        STATE.with(|s| *s.borrow_mut() = Some(state));
        let ev = SetWinEventHook(
            EVENT_OBJECT_DESTROY, EVENT_OBJECT_SHOW, None, Some(on_event), 0, 0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );
        log::line(&format!("watching (dll {}, {} bytes)", path.display(), std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)));
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = UnhookWinEvent(ev);
        let state = STATE.with(|s| s.borrow_mut().take());
        if let Some(st) = state {
            let n = st.threads.len();
            for (tid, h) in st.threads {
                let _ = UnhookWindowsHookEx(h.hook);
                // A message wakes the thread, so it drops the DLL now rather than at its next input.
                let _ = PostThreadMessageW(tid, WM_NULL, WPARAM(0), LPARAM(0));
            }
            let _ = FreeLibrary(st.dll);
            log::line(&format!("stopped, {n} thread hooks removed"));
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let handled = std::panic::catch_unwind(|| {
        let done = STATE.with(|s| s.borrow().as_ref().map(|st| st.done_msg)).unwrap_or(0);
        if msg != 0 && msg == done {
            on_done(wp.0, lp.0 as u32);
            return true;
        }
        match msg {
            WM_TIMER if wp.0 == RETRY_TIMER => {
                retry();
                true
            }
            BUS_COMMAND if wp.0 == BUS_SHUTDOWN => {
                // SAFETY: our own window.
                unsafe { let _ = DestroyWindow(hwnd); }
                true
            }
            WM_CLOSE => {
                // SAFETY: our own window.
                unsafe { let _ = DestroyWindow(hwnd); }
                true
            }
            WM_DESTROY => {
                // SAFETY: ends this thread's loop.
                unsafe { PostQuitMessage(0) };
                true
            }
            _ => false,
        }
    })
    .unwrap_or(false);
    if handled { LRESULT(0) } else { unsafe { DefWindowProcW(hwnd, msg, wp, lp) } }
}

unsafe extern "system" fn on_event(_: HWINEVENTHOOK, event: u32, hwnd: HWND, obj: i32, child: i32, _: u32, _: u32) {
    if obj != OBJID_WINDOW.0 || child != 0 || hwnd.is_invalid() {
        return;
    }
    let _ = std::panic::catch_unwind(|| match event {
        EVENT_OBJECT_SHOW => consider(hwnd),
        EVENT_OBJECT_DESTROY => forget(hwnd),
        _ => {}
    });
}

fn class_of(h: HWND) -> String {
    let mut buf = [0u16; 128];
    // SAFETY: the buffer outlives the call.
    let n = unsafe { GetClassNameW(h, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n])
}

fn pid_tid(h: HWND) -> (u32, u32) {
    let mut pid = 0u32;
    // SAFETY: writes the pid only.
    let tid = unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    (pid, tid)
}

/// Image path of a process and whether it runs under WOW64 (a 32-bit host the x64 DLL cannot enter).
fn process_info(pid: u32) -> (String, bool) {
    // SAFETY: a limited query handle, closed before returning.
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return (String::new(), false) };
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let path = if QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok() {
            String::from_utf16_lossy(&buf[..len as usize])
        } else {
            String::new()
        };
        let mut wow = BOOL(0);
        let _ = IsWow64Process(h, &mut wow);
        let _ = CloseHandle(h);
        (path, wow.as_bool())
    }
}

fn has_descendant(root: HWND, class: &str) -> bool {
    struct Find<'a>(&'a str, bool);
    unsafe extern "system" fn each(h: HWND, lp: LPARAM) -> BOOL {
        // SAFETY: lp is the &mut Find below, alive for the whole enumeration.
        let f = unsafe { &mut *(lp.0 as *mut Find) };
        if class_of(h) == f.0 {
            f.1 = true;
            return false.into();
        }
        true.into()
    }
    let mut f = Find(class, false);
    // SAFETY: see `each`.
    unsafe {
        let _ = EnumChildWindows(Some(root), Some(each), LPARAM(&mut f as *mut Find as isize));
    }
    f.1
}

fn consider(hwnd: HWND) {
    let class = class_of(hwnd);
    let interesting = class == target::DIALOG_CLASS || class == target::CORE_WINDOW;
    if !interesting {
        return;
    }
    if STATE.with(|s| s.borrow().as_ref().is_none_or(|st| st.seen.contains_key(&(hwnd.0 as isize)))) {
        return;
    }
    let (pid, _) = pid_tid(hwnd);
    // SAFETY: no arguments.
    if pid == unsafe { GetCurrentProcessId() } {
        return;
    }
    let (exe, wow) = process_info(pid);
    // SAFETY: plain query.
    let top = unsafe { GetAncestor(hwnd, GA_ROOT) } == hwnd;
    let mark = class == target::DIALOG_CLASS && top && has_descendant(hwnd, target::DIALOG_MARK);
    let Some(kind) = target::classify(&class, &exe, mark, false) else { return };
    if crate::switched_off() {
        return;
    }
    let t0 = Instant::now();
    match kind {
        Kind::FileDialog => {
            if wow {
                log::line(&format!("{} file dialog: 32-bit host, skipped", target::exe_name(&exe)));
                return;
            }
            let hr = dark::frame(hwnd, DWMSBT_MAINWINDOW);
            log::line(&format!("{} file dialog {:#x}: frame from outside hr={:#010x}", target::exe_name(&exe), hwnd.0 as usize, hr.0));
            inject(hwnd, kind, t0);
        }
        Kind::Settings => settings(hwnd, t0, 0),
    }
}

/// Settings' CoreWindow: style the frame it sits in, or wait for it to get one.
fn settings(core: HWND, t0: Instant, tries: u8) {
    // SAFETY: plain query.
    let frame = unsafe { GetAncestor(core, GA_ROOT) };
    if frame == core || frame.is_invalid() {
        if tries < 10 {
            STATE.with(|s| {
                if let Some(st) = s.borrow_mut().as_mut() {
                    st.retry.push((core.0 as isize, tries + 1));
                    // SAFETY: our own window.
                    unsafe { SetTimer(Some(st.hwnd), RETRY_TIMER, 300, None) };
                }
            });
        }
        return;
    }
    let fclass = class_of(frame);
    let hr = dark::frame(frame, DWMSBT_TRANSIENTWINDOW);
    let (fpid, _) = pid_tid(frame);
    let fexe = target::exe_name(&process_info(fpid).0);
    log::line(&format!("settings frame {:#x} ({fclass}, {fexe}): acrylic and dark caption from outside hr={:#010x}", frame.0 as usize, hr.0));
    STATE.with(|s| {
        if let Some(st) = s.borrow_mut().as_mut() {
            st.seen.insert(core.0 as isize, Kind::Settings);
            st.seen.insert(frame.0 as isize, Kind::Settings);
        }
    });
    if hr.is_err() {
        inject(frame, Kind::Settings, t0);
    } else {
        log::line(&format!("settings styled without injection in {} us", t0.elapsed().as_micros()));
    }
}

fn retry() {
    let list = STATE.with(|s| {
        let mut b = s.borrow_mut();
        let st = b.as_mut()?;
        // SAFETY: our own window.
        unsafe { let _ = KillTimer(Some(st.hwnd), RETRY_TIMER); }
        Some(std::mem::take(&mut st.retry))
    });
    for (h, tries) in list.unwrap_or_default() {
        let h = HWND(h as *mut _);
        // SAFETY: plain query.
        if unsafe { IsWindow(Some(h)) }.as_bool() {
            settings(h, Instant::now(), tries);
        }
    }
}

/// Puts the DLL's hook on the window's thread (once per thread) and asks it to style the window.
fn inject(hwnd: HWND, kind: Kind, t0: Instant) {
    let (pid, tid) = pid_tid(hwnd);
    STATE.with(|s| {
        let mut b = s.borrow_mut();
        let Some(st) = b.as_mut() else { return };
        st.seen.insert(hwnd.0 as isize, kind);
        if !st.threads.contains_key(&tid) {
            // SAFETY: the hook procedure lives in the DLL, which this process keeps loaded.
            match unsafe { SetWindowsHookExW(WH_GETMESSAGE, st.proc, Some(st.dll.into()), tid) } {
                Ok(hook) => {
                    st.threads.insert(tid, Hooked { hook, windows: Vec::new() });
                }
                Err(e) => {
                    log::line(&format!("pid {pid}: hook refused: {e}"));
                    return;
                }
            }
        }
        if let Some(h) = st.threads.get_mut(&tid) {
            h.windows.push(hwnd.0 as isize);
        }
        st.pending.insert(pid, (t0, kind));
        // SAFETY: a registered message the host ignores unless the DLL's hook takes it.
        unsafe {
            let _ = PostMessageW(Some(hwnd), st.style_msg, WPARAM(kind as usize), LPARAM(0));
        }
    });
}

fn on_done(us: usize, pid: u32) {
    let p = STATE.with(|s| s.borrow_mut().as_mut().and_then(|st| st.pending.remove(&pid)));
    match p {
        Some((t0, kind)) => log::line(&format!(
            "pid {pid} {kind:?}: styled in the host in {us} us, {} us from the show event",
            t0.elapsed().as_micros()
        )),
        None => log::line(&format!("pid {pid}: styled in the host in {us} us")),
    }
}

/// A destroyed window leaves the lists; its thread's hook goes with the last target of that thread.
fn forget(hwnd: HWND) {
    let key = hwnd.0 as isize;
    STATE.with(|s| {
        let mut b = s.borrow_mut();
        let Some(st) = b.as_mut() else { return };
        if st.seen.remove(&key).is_none() {
            return;
        }
        let mut empty = None;
        for (tid, h) in st.threads.iter_mut() {
            if let Some(i) = h.windows.iter().position(|w| *w == key) {
                h.windows.remove(i);
                if h.windows.is_empty() {
                    empty = Some(*tid);
                }
                break;
            }
        }
        if let Some(tid) = empty
            && let Some(h) = st.threads.remove(&tid)
        {
            // SAFETY: our hook; the posted WM_NULL lets the thread unload the DLL at once.
            unsafe {
                let _ = UnhookWindowsHookEx(h.hook);
                let _ = PostThreadMessageW(tid, WM_NULL, WPARAM(0), LPARAM(0));
            }
            log::line(&format!("thread {tid}: last target gone, hook removed"));
        }
    });
}
