//! The loader, inside explorust: an out-of-context WinEvent hook sees windows shown by other
//! processes; a target gets its DWM attributes from here when DWM takes them across processes,
//! and the DLL (a `WH_GETMESSAGE` hook on its thread only) for what must run in the host. The hook
//! goes when the window is destroyed or explorust leaves, and the host unloads the DLL with it.

use crate::dark;
use crate::log;
use crate::target::{self, Kind, Phase};
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Instant;
use crate::host;
use std::collections::HashSet;
use windows::Win32::Foundation::{FreeLibrary, HINSTANCE, HMODULE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMSBT_MAINWINDOW, DWMSBT_TRANSIENTWINDOW};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, HSTRING, PCWSTR};

const EVENT_OBJECT_CREATE: u32 = 0x8000;
const EVENT_OBJECT_DESTROY: u32 = 0x8001;
const EVENT_OBJECT_SHOW: u32 = 0x8002;
const WINEVENT_OUTOFCONTEXT: u32 = 0;
const WINEVENT_SKIPOWNPROCESS: u32 = 2;
const RETRY_TIMER: usize = 1;
/// Re-applies file dialog captions for a second after their show (`dark::recaption`).
const CAPTION_TIMER: usize = 2;
/// bus::MSG_COMMAND with wParam = bus::Command::Shutdown (15): the shell's way to close a part.
const BUS_COMMAND: u32 = 0x8000 + 0x100;
const BUS_SHUTDOWN: usize = 15;

struct Hooked {
    hook: HHOOK,
    windows: Vec<isize>,
}

/// When the first event of a host's target arrived, its kind, and the phases the DLL reported.
type Pending = (Instant, Kind, Vec<(Phase, u64)>);

struct State {
    dll: HMODULE,
    proc: HOOKPROC,
    style_msg: u32,
    done_msg: u32,
    /// Thread hooks by thread id, with the target windows of that thread.
    threads: HashMap<u32, Hooked>,
    /// Windows already handled, so a second show does nothing.
    seen: HashMap<isize, Kind>,
    /// When the first event of a host's target arrived, for the latency log, and the phases the
    /// DLL reported so far.
    pending: HashMap<u32, Pending>,
    /// Dialogs hooked at their creation, not shown yet: at the show they are kept (a file dialog)
    /// or let go.
    created: HashSet<isize>,
    /// Settings CoreWindows not yet inside their frame: retried on a timer.
    retry: Vec<(isize, u8)>,
    /// File dialogs whose caption is checked again: (window, ticks left).
    captions: Vec<(isize, u8)>,
    /// The DLL's path, for the Settings TAP.
    dll_path: std::path::PathBuf,
    /// Settings processes the TAP was sent to.
    xaml_pids: HashSet<u32>,
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
        (dll, Some(std::mem::transmute::<unsafe extern "system" fn() -> isize, unsafe extern "system" fn(i32, WPARAM, LPARAM) -> LRESULT>(p)))
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
            created: HashSet::new(),
            retry: Vec::new(),
            captions: Vec::new(),
            dll_path: path.clone(),
            xaml_pids: HashSet::new(),
            hwnd,
        };
        STATE.with(|s| *s.borrow_mut() = Some(state));
        let ev = SetWinEventHook(
            EVENT_OBJECT_CREATE, EVENT_OBJECT_SHOW, None, Some(on_event), 0, 0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );
        log::line(&format!("watching (dll {}, {} bytes)", path.display(), std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)));
        scan_existing();
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
            on_done(wp.0 as u64, lp.0 as u32);
            return true;
        }
        match msg {
            WM_COPYDATA => {
                // SAFETY: WM_COPYDATA's lParam is a COPYDATASTRUCT valid for the send.
                unsafe { tap_lines(lp) };
                true
            }
            WM_TIMER if wp.0 == RETRY_TIMER => {
                retry();
                true
            }
            WM_TIMER if wp.0 == CAPTION_TIMER => {
                recaptions();
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
    if handled { LRESULT(if msg == WM_COPYDATA { 1 } else { 0 }) } else { unsafe { DefWindowProcW(hwnd, msg, wp, lp) } }
}

/// Lines the Settings TAP sent (`xaml::report`), into inject.log.
unsafe fn tap_lines(lp: LPARAM) {
    // SAFETY: the caller's contract.
    let cds = unsafe { &*(lp.0 as *const windows::Win32::System::DataExchange::COPYDATASTRUCT) };
    if cds.dwData != crate::xaml::report::COPYDATA_TAG || cds.lpData.is_null() || cds.cbData as usize > crate::xaml::report::MAX_MESSAGE {
        return;
    }
    // SAFETY: cbData bytes at lpData, valid during the send.
    let bytes = unsafe { std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize) };
    log::lines("settings xaml: ", String::from_utf8_lossy(bytes).lines());
}

unsafe extern "system" fn on_event(_: HWINEVENTHOOK, event: u32, hwnd: HWND, obj: i32, child: i32, _: u32, _: u32) {
    if obj != OBJID_WINDOW.0 || child != 0 || hwnd.is_invalid() {
        return;
    }
    let _ = std::panic::catch_unwind(|| match event {
        EVENT_OBJECT_CREATE => created(hwnd),
        EVENT_OBJECT_SHOW => consider(hwnd),
        EVENT_OBJECT_DESTROY => forget(hwnd),
        _ => {}
    });
}

/// Windows shown before the loader started (explorust restarted with Settings or a file dialog
/// open) get what a new show would give them.
fn scan_existing() {
    unsafe extern "system" fn each(h: HWND, list: LPARAM) -> BOOL {
        // SAFETY: lParam is the Vec below, alive for the enumeration.
        unsafe { (*(list.0 as *mut Vec<HWND>)).push(h) };
        BOOL(1)
    }
    let mut tops: Vec<HWND> = Vec::new();
    // SAFETY: the callback only pushes into the Vec passed as lParam.
    unsafe { let _ = EnumWindows(Some(each), LPARAM(&mut tops as *mut _ as isize)); }
    for h in tops {
        // SAFETY: plain queries; the handle may be gone.
        if !unsafe { IsWindowVisible(h) }.as_bool() {
            continue;
        }
        if class_of(h) == target::FRAME_WINDOW {
            // SAFETY: plain query.
            if let Ok(core) = unsafe { FindWindowExW(Some(h), None, &HSTRING::from(target::CORE_WINDOW), PCWSTR::null()) } {
                consider(core);
            }
        } else {
            consider(h);
        }
    }
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

/// A top-level `#32770` of another eligible process was just created: hook its thread now, so the
/// DLL sees the dialog being built and shown. Most dialogs are not file dialogs; their hook goes at
/// their show.
fn created(hwnd: HWND) {
    if class_of(hwnd) != target::DIALOG_CLASS {
        return;
    }
    // SAFETY: plain query.
    if unsafe { GetAncestor(hwnd, GA_ROOT) } != hwnd {
        return;
    }
    let (pid, _) = pid_tid(hwnd);
    // SAFETY: no arguments.
    if pid == unsafe { GetCurrentProcessId() } || crate::switched_off() {
        return;
    }
    if STATE.with(|s| s.borrow().as_ref().is_none_or(|st| st.seen.contains_key(&(hwnd.0 as isize)))) {
        return;
    }
    let t0 = Instant::now();
    let h = host::host(pid);
    if let Some(why) = h.refused {
        log::line(&format!("{} dialog {:#x}: {why}, skipped", target::exe_name(&h.exe), hwnd.0 as usize));
        STATE.with(|s| {
            if let Some(st) = s.borrow_mut().as_mut() {
                st.seen.insert(hwnd.0 as isize, Kind::FileDialog);
            }
        });
        return;
    }
    if hook(hwnd, Kind::FileDialog, t0) {
        STATE.with(|s| {
            if let Some(st) = s.borrow_mut().as_mut() {
                st.created.insert(hwnd.0 as isize);
            }
        });
    }
}

fn consider(hwnd: HWND) {
    let class = class_of(hwnd);
    let interesting = class == target::DIALOG_CLASS || class == target::CORE_WINDOW;
    if !interesting {
        return;
    }
    let key = hwnd.0 as isize;
    let early = STATE.with(|s| s.borrow_mut().as_mut().is_some_and(|st| st.created.remove(&key)));
    if !early && STATE.with(|s| s.borrow().as_ref().is_none_or(|st| st.seen.contains_key(&key))) {
        return;
    }
    let (pid, _) = pid_tid(hwnd);
    // SAFETY: no arguments.
    if pid == unsafe { GetCurrentProcessId() } {
        return;
    }
    // SAFETY: plain query.
    let top = unsafe { GetAncestor(hwnd, GA_ROOT) } == hwnd;
    let mark = class == target::DIALOG_CLASS && top && has_descendant(hwnd, target::DIALOG_MARK);
    let h = host::host(pid);
    let kind = target::classify(&class, &h.exe, mark, false);
    if early && kind != Some(Kind::FileDialog) {
        // Hooked at creation, but not a file dialog: let it go.
        forget(hwnd);
        return;
    }
    let Some(kind) = kind else { return };
    if crate::switched_off() {
        return;
    }
    let t0 = Instant::now();
    match kind {
        Kind::FileDialog => {
            if let Some(why) = h.refused {
                log::line(&format!("{} file dialog: {why}, skipped", target::exe_name(&h.exe)));
                return;
            }
            let hr = dark::frame(hwnd, DWMSBT_MAINWINDOW);
            log::line(&format!(
                "{} file dialog {:#x}: frame from outside hr={:#010x}, hooked {}",
                target::exe_name(&h.exe), hwnd.0 as usize, hr.0, if early { "at creation" } else { "at show" }
            ));
            STATE.with(|s| {
                if let Some(st) = s.borrow_mut().as_mut() {
                    st.captions.retain(|(w, _)| *w != hwnd.0 as isize);
                    st.captions.push((hwnd.0 as isize, 4));
                    // SAFETY: our own window.
                    unsafe { SetTimer(Some(st.hwnd), CAPTION_TIMER, 250, None) };
                }
            });
            if early {
                // The DLL styled it at WM_SHOWWINDOW already, or does now (it ignores a repeat).
                request(hwnd, kind);
            } else {
                inject(hwnd, kind, t0);
            }
        }
        Kind::Settings => settings(hwnd, t0, 0),
    }
}

/// One tick of the caption checks: each file dialog gets its dark caption back if its host reset
/// it, four times 250 ms apart; the timer stops with the list.
fn recaptions() {
    let due = STATE.with(|s| {
        let mut b = s.borrow_mut();
        let Some(st) = b.as_mut() else { return Vec::new() };
        let due: Vec<isize> = st.captions.iter().map(|(w, _)| *w).collect();
        st.captions.retain_mut(|(_, left)| {
            *left -= 1;
            *left > 0
        });
        if st.captions.is_empty() {
            // SAFETY: our own window and timer.
            unsafe { let _ = KillTimer(Some(st.hwnd), CAPTION_TIMER); }
        }
        due
    });
    for w in due {
        let hwnd = HWND(w as _);
        // SAFETY: plain query; the handle may be gone.
        if unsafe { IsWindow(Some(hwnd)) }.as_bool() && !dark::recaption(hwnd, DWMSBT_MAINWINDOW) {
            log::line(&format!("file dialog {w:#x}: caption still light after a reset"));
        }
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
    let fexe = target::exe_name(&host::host(fpid).exe);
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
    let (pid, _) = pid_tid(core);
    let dll = STATE.with(|s| {
        let mut b = s.borrow_mut();
        let st = b.as_mut()?;
        st.xaml_pids.insert(pid).then(|| st.dll_path.clone())
    });
    if let Some(dll) = dll {
        use crate::xaml::attach;
        let rules = if attach::switched_off() { None } else { attach::rules_text() };
        match rules {
            Some(rules) => attach::attach(pid, &dll, rules),
            // A TAP an earlier explorust loaded keeps styling a live Settings: an empty set of
            // rules makes it undo that.
            None if attach::loaded_in(pid, &dll) => {
                log::line("settings xaml: off or no settings-style.txt, the TAP loaded before undoes its styling");
                attach::attach(pid, &dll, String::new());
            }
            None if attach::switched_off() => log::line("settings xaml: off (parts-off.txt)"),
            None => log::line("settings xaml: no settings-style.txt, Settings left as Windows draws it"),
        }
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

/// Puts the DLL's hook on the window's thread (once per thread). False when it was refused.
fn hook(hwnd: HWND, kind: Kind, t0: Instant) -> bool {
    let (pid, tid) = pid_tid(hwnd);
    STATE.with(|s| {
        let mut b = s.borrow_mut();
        let Some(st) = b.as_mut() else { return false };
        st.seen.insert(hwnd.0 as isize, kind);
        if !st.threads.contains_key(&tid) {
            // SAFETY: the hook procedure lives in the DLL, which this process keeps loaded.
            match unsafe { SetWindowsHookExW(WH_CALLWNDPROC, st.proc, Some(st.dll.into()), tid) } {
                Ok(hook) => {
                    st.threads.insert(tid, Hooked { hook, windows: Vec::new() });
                }
                Err(e) => {
                    log::line(&format!("pid {pid}: hook refused: {e}"));
                    return false;
                }
            }
        }
        if let Some(h) = st.threads.get_mut(&tid) {
            h.windows.push(hwnd.0 as isize);
        }
        st.pending.entry(pid).or_insert((t0, kind, Vec::new()));
        true
    })
}

/// Asks the DLL to style the window: a sent (not posted) message, so the `WH_CALLWNDPROC` hook sees
/// it; `SendNotifyMessageW` does not wait for the host.
fn request(hwnd: HWND, kind: Kind) {
    let m = STATE.with(|s| s.borrow().as_ref().map(|st| st.style_msg)).unwrap_or(0);
    // SAFETY: a registered message the host ignores unless the DLL's hook takes it.
    unsafe {
        let _ = SendNotifyMessageW(hwnd, m, WPARAM(kind as usize), LPARAM(0));
    }
}

fn inject(hwnd: HWND, kind: Kind, t0: Instant) {
    if hook(hwnd, kind, t0) {
        request(hwnd, kind);
    }
}

fn on_done(v: u64, pid: u32) {
    let (phase, us) = target::unpack(v);
    let Some(phase) = phase else { return };
    let line = STATE.with(|s| {
        let mut b = s.borrow_mut();
        let st = b.as_mut()?;
        let e = st.pending.entry(pid).or_insert((Instant::now(), Kind::FileDialog, Vec::new()));
        if phase != Phase::Total {
            e.2.push((phase, us));
            return None;
        }
        let (t0, kind, phases) = st.pending.remove(&pid)?;
        let list: Vec<String> = phases.iter().map(|(p, u)| format!("{p:?} {u}")).collect();
        Some(format!(
            "pid {pid} {kind:?}: styled in the host in {us} us ({}), {} us from the first event",
            list.join(", "),
            t0.elapsed().as_micros()
        ))
    });
    if let Some(l) = line {
        log::line(&l);
    }
}

/// A destroyed window leaves the lists; its thread's hook goes with the last target of that thread.
fn forget(hwnd: HWND) {
    let key = hwnd.0 as isize;
    STATE.with(|s| {
        let mut b = s.borrow_mut();
        let Some(st) = b.as_mut() else { return };
        st.created.remove(&key);
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
