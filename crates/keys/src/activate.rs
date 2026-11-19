//! The keys host's command window (`bus::Owner::Keys`): it brings the window a part names to the
//! foreground. explorust runs at medium integrity and cannot move the foreground off an elevated
//! window to a UWP frame; explorust-keys.exe has UI access, so the switcher and the taskbar post
//! `Command::Activate` here. The window lives on a thread of its own: a target that hangs in
//! SetForegroundWindow must never stall the keyboard hook.

use std::time::Instant;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT, INPUT_MOUSE, SendInput, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, BringWindowToTop, CreateWindowExW, DefWindowProcW, DispatchMessageW, FindWindowExW, GA_ROOT,
    GetAncestor, GetClassNameW, GetForegroundWindow, GetMessageW, GetWindowTextW, GetWindowThreadProcessId, HWND_MESSAGE,
    IsIconic, IsWindow, MSG, RegisterClassW, SW_RESTORE, SetForegroundWindow, ShowWindow, SwitchToThisWindow, WINDOW_EX_STYLE,
    WINDOW_STYLE, WNDCLASSW,
};
use windows::core::{HSTRING, PCWSTR, w};

/// Starts the command window's thread. Returns once the window exists, or the reason it does not.
pub fn spawn() -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("keys-activate".into())
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
                let hwnd = match hwnd {
                    Ok(h) => h,
                    Err(e) => {
                        let _ = tx.send(Err(format!("activation window not created: {e}")));
                        return;
                    }
                };
                bus::listen(hwnd);
                let _ = tx.send(Ok(()));
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    DispatchMessageW(&msg);
                }
            }
        })
        .map_err(|e| format!("activation thread: {e}"))?;
    rx.recv().map_err(|_| "activation thread ended".to_string())?
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == bus::MSG_COMMAND {
        if let Some((bus::Command::Activate, h)) = bus::decode(wp, lp) {
            let target = HWND(h as _);
            let t = Instant::now();
            let (frame, how) = activate(target);
            crate::hook::log_line(&format!(
                "activate {:?} ({}): {how} in {:.1} ms",
                title(frame),
                class(frame),
                t.elapsed().as_secs_f32() * 1000.0
            ));
        }
        return LRESULT(0);
    }
    // SAFETY: default handling for everything else.
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
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
