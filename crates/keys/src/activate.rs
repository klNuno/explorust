//! Window actions executed by the authenticated keys pipe. The old message window only logs
//! rejected commands so it cannot grant window control to an unauthenticated sender.

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Instant;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT, INPUT_MOUSE, SendInput, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, BringWindowToTop, CreateWindowExW, DefWindowProcW, DispatchMessageW, FindWindowExW, GA_ROOT,
    GWL_EXSTYLE, GetAncestor, GetClassNameW, GetForegroundWindow, GetMessageW, GetWindowLongW, GetWindowTextW, GetWindowThreadProcessId,
    HWND_MESSAGE, ISMEX_NOSEND, InSendMessageEx, IsHungAppWindow, IsWindowVisible, WS_EX_TOOLWINDOW,
    HWND_TOP, IsIconic, IsWindow, MSG, RegisterClassW, SW_RESTORE, SW_SHOWMINNOACTIVE, SW_SHOWNOACTIVATE, SWP_ASYNCWINDOWPOS,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetForegroundWindow, SetWindowPos, ShowWindow, ShowWindowAsync, SwitchToThisWindow,
    WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSW,
};
use windows::core::{HSTRING, PCWSTR, w};

/// Starts the message-only window that records the rejection of old window commands.
pub fn spawn_refusal_window() -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("keys-legacy-window".into())
        .spawn(move || {
            let class = HSTRING::from(bus::class_of(bus::Owner::Keys));
            // SAFETY: registers a class and creates a message-only window owned by this thread,
            // which then pumps its messages.
            unsafe {
                let inst = GetModuleHandleW(None).unwrap_or_default();
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(proc_),
                    hInstance: inst.into(),
                    lpszClassName: PCWSTR(class.as_ptr()),
                    ..Default::default()
                };
                RegisterClassW(&wc);
                let hwnd = CreateWindowExW(
                    WINDOW_EX_STYLE(0), &class, w!(""), WINDOW_STYLE(0), 0, 0, 0, 0,
                    Some(HWND_MESSAGE), None, Some(inst.into()), None,
                );
                if let Err(e) = hwnd {
                    let _ = tx.send(Err(format!("keys legacy window not created: {e}")));
                    return;
                }
                let _ = tx.send(Ok(()));
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    DispatchMessageW(&msg);
                }
            }
        })
        .map_err(|e| format!("keys legacy window thread: {e}"))?;
    rx.recv().map_err(|_| "keys legacy window thread ended".to_string())?
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    static SEEN: AtomicU8 = AtomicU8::new(0);
    if msg == bus::MSG_COMMAND {
        if bus::decode(wp, lp).is_some_and(|(cmd, _)| cmd.owner() == bus::Owner::Keys) {
            // Window messages expose no authenticated sender. Log once per delivery kind.
            let sent = unsafe { InSendMessageEx(None) } != ISMEX_NOSEND;
            let flag = 1 << u8::from(sent);
            if SEEN.fetch_or(flag, Ordering::Relaxed) & flag == 0 {
                let kind = if sent { "sent" } else { "posted" };
                crate::hook::log_line(&format!("keys: refused {kind} window command"));
            }
        }
        return LRESULT(0);
    }
    // SAFETY: default handling for everything else.
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

pub(crate) fn run(command: bus::Command, window: isize) {
    use bus::Command::{Activate, Minimize, Restore};
    let h = HWND(window as _);
    match command {
        Activate => {
            let start = Instant::now();
            let (frame, how) = activate(h);
            crate::hook::log_line(&format!(
                "activate {:?} ({}): {how} in {:.1} ms",
                title(frame),
                class(frame),
                start.elapsed().as_secs_f32() * 1000.0
            ));
        }
        Minimize | Restore => {
            let min = command == Minimize;
            let action = if min { "minimize" } else { "restore" };
            if let Err(why) = show_desktop_target(h, min) {
                crate::hook::log_line(&format!(
                    "{action} refused for {:?} ({}): {why}", title(h), class(h)
                ));
                return;
            }
            let ok = if min {
                // SAFETY: an async minimize on a checked window.
                unsafe { ShowWindowAsync(h, SW_SHOWMINNOACTIVE) }.as_bool()
            } else {
                restore(h)
            };
            crate::hook::log_line(&format!(
                "{action} {:?} ({}) for show desktop: {ok}", title(h), class(h)
            ));
        }
        _ => {}
    }
}

fn restore(h: HWND) -> bool {
    let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
    // SAFETY: the caller checked this top-level window. Responsive targets are raised before
    // the next command, preserving Z-order across their independent input queues.
    unsafe {
        if IsHungAppWindow(h).as_bool() {
            let queued = ShowWindowAsync(h, SW_SHOWNOACTIVATE).as_bool();
            let raised = SetWindowPos(h, Some(HWND_TOP), 0, 0, 0, 0, flags | SWP_ASYNCWINDOWPOS);
            return queued && raised.is_ok();
        }
        let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
        let raised = SetWindowPos(h, Some(HWND_TOP), 0, 0, 0, 0, flags);
        raised.is_ok() && !IsIconic(h).as_bool()
    }
}

/// A window show desktop may minimize or restore: a visible top-level window, not a tool window,
/// minimized when it is to be restored.
fn show_desktop_target(h: HWND, min: bool) -> Result<(), &'static str> {
    // SAFETY: plain queries on a handle that may be stale.
    unsafe {
        if !IsWindow(Some(h)).as_bool() {
            return Err("not a window");
        }
        if GetAncestor(h, GA_ROOT) != h {
            return Err("not top-level");
        }
        if !IsWindowVisible(h).as_bool() {
            return Err("hidden");
        }
        if GetWindowLongW(h, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
            return Err("tool window");
        }
        if !min && !IsIconic(h).as_bool() {
            return Err("not minimized");
        }
    }
    Ok(())
}

fn class(h: HWND) -> String {
    let mut buf = [0u16; 128];
    // SAFETY: writes at most the buffer's length.
    let n = unsafe { GetClassNameW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn title(h: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: writes at most the buffer's length.
    let n = unsafe { GetWindowTextW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// The window to activate for `h`: a UWP app's CoreWindow is shown through its
/// ApplicationFrameWindow (its root while hosted; found by title while it is detached, minimized).
fn frame_of(h: HWND) -> HWND {
    if class(h) != "Windows.UI.Core.CoreWindow" {
        return h;
    }
    // SAFETY: plain window lookups.
    unsafe {
        let root = GetAncestor(h, GA_ROOT);
        if !root.is_invalid() && class(root) == "ApplicationFrameWindow" {
            return root;
        }
        let name = HSTRING::from(title(h));
        FindWindowExW(None, None, w!("ApplicationFrameWindow"), &name).ok().filter(|f| !f.is_invalid()).unwrap_or(h)
    }
}

fn in_front(h: HWND) -> bool {
    // SAFETY: plain queries.
    unsafe {
        let fg = GetForegroundWindow();
        fg == h || GetAncestor(fg, GA_ROOT) == h
    }
}

/// Restores the window when minimized and makes it the foreground window. Returns the window
/// used (the frame of a CoreWindow) and the way that worked.
pub fn activate(target: HWND) -> (HWND, &'static str) {
    // SAFETY: window manager calls on a handle; a stale one only fails them.
    unsafe {
        if !IsWindow(Some(target)).as_bool() {
            return (target, "gone");
        }
        let h = frame_of(target);
        // An empty mouse input makes this process the last to send input (lifts the foreground lock).
        let input = INPUT { r#type: INPUT_MOUSE, ..Default::default() };
        SendInput(&[input], size_of::<INPUT>() as i32);
        // Lets the target hand the foreground on (a frame to its CoreWindow, an app to a dialog).
        for w in [h, target] {
            let mut pid = 0u32;
            GetWindowThreadProcessId(w, Some(&mut pid));
            let _ = AllowSetForegroundWindow(pid);
        }
        if IsIconic(h).as_bool() {
            if IsHungAppWindow(h).as_bool() {
                let _ = ShowWindowAsync(h, SW_RESTORE);
                return (h, "restore queued (hung)");
            }
            // The window's own restore, with its animation; it activates too.
            let _ = ShowWindow(h, SW_RESTORE);
        }
        if SetForegroundWindow(h).as_bool() && in_front(h) {
            return (h, "SetForegroundWindow");
        }
        let fg = GetForegroundWindow();
        let ft = GetWindowThreadProcessId(fg, None);
        let me = GetCurrentThreadId();
        if ft != 0 && ft != me && AttachThreadInput(me, ft, true).as_bool() {
            let _ = BringWindowToTop(h);
            let _ = SetForegroundWindow(h);
            let _ = SetFocus(Some(h));
            let _ = AttachThreadInput(me, ft, false);
            if in_front(h) {
                return (h, "AttachThreadInput");
            }
        }
        // Finishes asynchronously; the part that asked checks the result.
        SwitchToThisWindow(h, true);
        (h, if in_front(h) { "SwitchToThisWindow" } else { "SwitchToThisWindow (pending)" })
    }
}
