//! The UI thread: the command window `explorust.notify`, and one toast card window per toast on
//! screen, stacked above the taskbar's right side.

mod card;
pub mod helper;
mod listen;
mod popup;
mod system;
mod stale;
mod twinui;
pub mod wnf;

use crate::log;
use crate::model::{self, Banner, Hit, Popups, Toast};
use card::Card;
use std::cell::RefCell;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{HSTRING, PCWSTR, w};

const CARD_CLASS: PCWSTR = w!("explorust.notify.toast");
const TIMER_HIDE: usize = 1;
/// Looks for Windows' own popup of the card's toast (`model::POPUP_CHECK_MS`).
const TIMER_CHECK: usize = 2;
const WM_MOUSELEAVE: u32 = 0x02A3;

struct State {
    cmd: HWND,
    /// Newest first.
    cards: Vec<Card>,
    popups: Popups,
}

thread_local! {
    static ST: RefCell<Option<State>> = const { RefCell::new(None) };
}

pub fn run() {
    // SAFETY: COM for this thread only (STA, the UI thread).
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    log::line("notify: run");
    log::line(&format!("notify: {}", stale::end_stale_host()));
    // Without explorer nobody says the shell is up, and Windows' notification center refuses to open.
    log::line(&format!("notify: {}", wnf::ensure_shell_ready()));
    match create() {
        Err(e) => log::line(&format!("notify: windows not created: {e}")),
        Ok(cmd) => {
            ST.with(|c| *c.borrow_mut() = Some(State { cmd, cards: Vec::new(), popups: Popups::Unknown }));
            listen::spawn(cmd);
            let mut msg = MSG::default();
            // SAFETY: a plain message loop on this thread.
            while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            if let Some(s) = ST.with(|c| c.borrow_mut().take()) {
                // SAFETY: destroys our own windows.
                unsafe {
                    for c in &s.cards {
                        let _ = DestroyWindow(c.hwnd);
                    }
                    if IsWindow(Some(s.cmd)).as_bool() {
                        let _ = DestroyWindow(s.cmd);
                    }
                }
            }
        }
    }
    log::line("notify: stopped");
    if com {
        // SAFETY: balances the CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
}

fn create() -> windows_core::Result<HWND> {
    // SAFETY: window classes and a window owned by this thread.
    unsafe {
        let inst = GetModuleHandleW(None)?;
        let cmd_class = HSTRING::from(bus::class_of(bus::Owner::Notify));
        let wc = WNDCLASSW { lpfnWndProc: Some(cmd_proc), hInstance: inst.into(), lpszClassName: PCWSTR(cmd_class.as_ptr()), ..Default::default() };
        RegisterClassW(&wc);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(card_proc),
            hInstance: inst.into(),
            lpszClassName: CARD_CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let cmd = CreateWindowExW(Default::default(), &cmd_class, w!("explorust notify"), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(inst.into()), None)?;
        bus::listen(cmd);
        // explorust.exe runs with uiAccess (a higher integrity than the helper's medium): let the
        // helper's wake-ups through UIPI.
        for m in [listen::MSG_WAKE, listen::MSG_HELPER] {
            let _ = ChangeWindowMessageFilterEx(cmd, m, MSGFLT_ALLOW, None);
        }
        Ok(cmd)
    }
}

unsafe extern "system" fn cmd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        bus::MSG_COMMAND => {
            match bus::decode(wparam, lparam) {
                Some((bus::Command::Shutdown, _)) => {
                    log::line("notify: shutdown");
                    // SAFETY: ends this thread's message loop.
                    unsafe { PostQuitMessage(0) };
                }
                Some((cmd, arg)) => panel(cmd, arg),
                None => {}
            }
            LRESULT(0)
        }
        listen::MSG_ADDED => {
            // SAFETY: the listener thread boxed this toast for us and gave up its ownership.
            let toast = unsafe { Box::from_raw(lparam.0 as *mut Toast) };
            added(*toast);
            LRESULT(0)
        }
        listen::MSG_WAKE => {
            listen::wake();
            LRESULT(0)
        }
        listen::MSG_HELPER => {
            listen::helper_said(wparam.0 == 1);
            LRESULT(0)
        }
        listen::MSG_REMOVED => {
            let id = wparam.0 as u32;
            let hwnd = ST.with(|c| c.borrow().as_ref().and_then(|s| s.cards.iter().find(|k| k.toast.id == id).map(|k| k.hwnd)));
            if let Some(h) = hwnd {
                close(h, "removed from the center");
            }
            LRESULT(0)
        }
        // SAFETY: default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Win+N, Win+V and Win+.: Windows' own panels, asked for through the immersive shell on a
/// thread of their own (a cross-process call to the shell must never stall the UI thread).
fn panel(cmd: bus::Command, arg: isize) {
    let _ = std::thread::Builder::new().name("notify-panel".into()).spawn(move || {
        use windows::Win32::System::Com::COINIT_MULTITHREADED;
        // SAFETY: COM for this short-lived thread only.
        let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
        let t0 = std::time::Instant::now();
        let r = match cmd {
            bus::Command::NotificationCenter if arg > 0 => twinui::notification_center_hotkey(arg as u32 - 1).map(|_| format!("hotkey {} sent", arg - 1)),
            bus::Command::NotificationCenter => twinui::toggle_notification_center().or_else(|e| {
                // E_UNEXPECTED: the center still waits for the shell-ready signal; give it and retry.
                if e.code().0 as u32 != 0x8000FFFF {
                    return Err(e);
                }
                log::line(&format!("notify: center not ready ({e}), {}", wnf::ensure_shell_ready()));
                std::thread::sleep(std::time::Duration::from_millis(300));
                twinui::toggle_notification_center()
            }).map(|v| format!("visible {v}")),
            bus::Command::ClipboardHistory => twinui::show_clipboard_history().map(|_| "shown".into()),
            bus::Command::EmojiPanel => {
                let kind = if arg > 0 { arg as u32 } else { twinui::HOTKEY_EMOJI };
                twinui::shell_hotkey(kind).map(|_| format!("hotkey {kind} sent"))
            }
            _ => Ok("not handled".into()),
        };
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        match r {
            Ok(m) => log::line(&format!("notify: {cmd:?}: {m} in {ms:.1} ms")),
            Err(e) => log::line(&format!("notify: {cmd:?} failed in {ms:.1} ms: {e}")),
        }
        if cmd == bus::Command::NotificationCenter {
            watch_center(t0);
        }
        if com {
            // SAFETY: balances the CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
    });
}

/// Logs `IsVisible` of the notification center every 100 ms for 2 s (each change, with the
/// foreground window's class and ShellExperienceHost's visible windows), to see whether and when
/// the center closes again.
fn watch_center(t0: std::time::Instant) {
    let mut last = None;
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let v = twinui::notification_center_visible().map_err(|e| e.code().0 as u32);
        if last != Some(v) {
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            let (_, host) = popup::windows_toast_visible();
            log::line(&format!("notify: center visible {v:x?} at {ms:.0} ms, foreground {}, ShellExperienceHost windows: {host}", foreground_class()));
            last = Some(v);
        }
    }
}

fn foreground_class() -> String {
    let mut buf = [0u16; 128];
    // SAFETY: reads the class name of the foreground window into a local buffer.
    let n = unsafe { GetClassNameW(GetForegroundWindow(), &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn added(toast: Toast) {
    let state = system::user_state();
    let settings = system::settings(&toast.aumid);
    let decision = model::banner(state, settings);
    log::line(&format!(
        "toast {} from '{}' ({}): {:?} (user state {state:?}, {settings:?}), title '{}'",
        toast.id, toast.app, toast.aumid, decision, toast.title()
    ));
    if decision == Banner::Quiet {
        return;
    }
    let t0 = std::time::Instant::now();
    let hwnd = match card::create_window(CARD_CLASS) {
        Ok(h) => h,
        Err(e) => return log::line(&format!("toast window not created: {e}")),
    };
    let card = match Card::new(hwnd, toast) {
        Ok(c) => c,
        Err(e) => {
            // SAFETY: our window, never shown.
            unsafe { let _ = DestroyWindow(hwnd); }
            return log::line(&format!("toast card not drawn: {e}"));
        }
    };
    let (extra, popups) = ST.with(|c| {
        let mut b = c.borrow_mut();
        let Some(s) = b.as_mut() else { return (None, Popups::Unknown) };
        s.cards.insert(0, card);
        ((s.cards.len() > model::MAX_ON_SCREEN).then(|| s.cards[s.cards.len() - 1].hwnd), s.popups)
    });
    if let Some(old) = extra {
        close(old, "pushed out by a newer toast");
    }
    // SAFETY: arms the check timer of our window.
    unsafe { SetTimer(Some(hwnd), TIMER_CHECK, model::POPUP_CHECK_MS, None) };
    if popups.card_first() {
        show(hwnd);
        log::line(&format!("toast shown in {:.1} ms (popups {popups:?})", t0.elapsed().as_secs_f64() * 1000.0));
    } else {
        log::line("toast card held: Windows showed its own popup last time");
    }
}

/// Puts a card on screen without activating it, arms its hide timer and plays the sound.
fn show(hwnd: HWND) {
    let first = with_card(hwnd, |k| !std::mem::replace(&mut k.shown, true));
    if first != Some(true) {
        return;
    }
    layout();
    // SAFETY: shows our window without activating it, arms its hide timer.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(Some(hwnd), TIMER_HIDE, duration_ms(), None);
    }
    system::play_sound();
}

/// The check timer: Windows' own popup on screen means the card steps aside (closed, and the next
/// cards wait hidden); none means the card shows (late if it was held).
fn check_popup(hwnd: HWND) {
    let t0 = std::time::Instant::now();
    let (seen, list) = popup::windows_toast_visible();
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    let next = ST.with(|c| {
        let mut b = c.borrow_mut();
        let s = b.as_mut()?;
        let (old, next) = (s.popups, s.popups.after_check(seen));
        s.popups = next;
        Some((old, next))
    });
    let Some((old, next)) = next else { return };
    log::line(&format!("popup check in {ms:.1} ms: Windows' toast {seen}, popups {old:?} -> {next:?}; host windows: {list}"));
    if seen {
        close(hwnd, "Windows shows its own popup");
    } else {
        show(hwnd);
    }
}

/// Windows' "Dismiss notifications after this amount of time" (Accessibility), 5 s by default;
/// `EXPLORUST_NOTIFY_HOLD_S` (dev only) holds a card longer, long enough for a capture.
fn duration_ms() -> u32 {
    if let Some(s) = std::env::var("EXPLORUST_NOTIFY_HOLD_S").ok().and_then(|v| v.parse::<u32>().ok()).filter(|s| *s > 0) {
        return s.saturating_mul(1000);
    }
    let mut secs = 0u32;
    // SAFETY: reads one u32.
    let ok = unsafe { SystemParametersInfoW(SPI_GETMESSAGEDURATION, 0, Some(&mut secs as *mut _ as _), Default::default()) }.is_ok();
    if ok && secs > 0 { secs * 1000 } else { 5000 }
}

/// Places the cards, newest at the bottom right of the primary work area.
fn layout() {
    let mut work = RECT::default();
    // SAFETY: reads the work area into a local.
    let _ = unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut work as *mut _ as _), Default::default()) };
    ST.with(|c| {
        let b = c.borrow();
        let Some(s) = b.as_ref() else { return };
        let scale = s.cards.first().map(|k| k.scale).unwrap_or(1.0);
        let cards: Vec<&Card> = s.cards.iter().filter(|k| k.shown).collect();
        let heights: Vec<f32> = cards.iter().map(|k| k.height_dip).collect();
        let mut rects = model::stack((work.left, work.top, work.right, work.bottom), &heights, scale);
        // Dev only: the left side, to compare with Windows' own toast while explorer runs.
        if std::env::var_os("EXPLORUST_NOTIFY_LEFT").is_some() {
            for r in &mut rects {
                r.x = work.left as f32 + work.right as f32 - r.x - r.w;
            }
        }
        for (k, r) in cards.iter().zip(rects) {
            // SAFETY: moves our own window.
            unsafe {
                let _ = SetWindowPos(k.hwnd, Some(HWND_TOPMOST), r.x as i32, r.y as i32, r.w as i32, r.h as i32, SWP_NOACTIVATE);
            }
        }
    });
}

fn close(hwnd: HWND, why: &str) {
    let removed = ST.with(|c| {
        let mut b = c.borrow_mut();
        let s = b.as_mut()?;
        let i = s.cards.iter().position(|k| k.hwnd == hwnd)?;
        Some(s.cards.remove(i))
    });
    if let Some(card) = removed {
        log::line(&format!("toast {} closed: {why}", card.toast.id));
        drop(card);
        // SAFETY: our own window.
        unsafe { let _ = DestroyWindow(hwnd); }
        layout();
    }
}

fn with_card<R>(hwnd: HWND, f: impl FnOnce(&mut Card) -> R) -> Option<R> {
    ST.with(|c| c.borrow_mut().as_mut().and_then(|s| s.cards.iter_mut().find(|k| k.hwnd == hwnd).map(f)))
}

unsafe extern "system" fn card_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_MOUSEMOVE => {
            let (x, y) = point(lparam);
            let started = with_card(hwnd, |k| {
                let first = !k.hover;
                k.hover = true;
                let over = model::hit(x / k.scale, y / k.scale) == Hit::Close;
                if over != k.hover_close {
                    k.hover_close = over;
                    k.paint();
                }
                first
            });
            if started == Some(true) {
                // SAFETY: our window; the hide timer waits while the pointer is on the card.
                unsafe {
                    let _ = KillTimer(Some(hwnd), TIMER_HIDE);
                    let mut t = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                    let _ = TrackMouseEvent(&mut t);
                }
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            with_card(hwnd, |k| {
                k.hover = false;
                if k.hover_close {
                    k.hover_close = false;
                    k.paint();
                }
            });
            // SAFETY: rearms the hide timer of our window.
            unsafe { SetTimer(Some(hwnd), TIMER_HIDE, duration_ms(), None) };
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = point(lparam);
            let Some((hit, toast)) = with_card(hwnd, |k| (model::hit(x / k.scale, y / k.scale), k.toast.clone())) else { return LRESULT(0) };
            match hit {
                Hit::Close => close(hwnd, "close button"),
                Hit::Body => {
                    log::line(&format!("toast {} clicked: activating {}", toast.id, toast.aumid));
                    // SAFETY: lets the app we start take the foreground (we got the click).
                    unsafe { let _ = AllowSetForegroundWindow(ASFW_ANY); }
                    system::activate(&toast.aumid);
                    // Windows removes a clicked toast from the notification center too.
                    if let Ok(l) = windows::UI::Notifications::Management::UserNotificationListener::Current() {
                        let _ = l.RemoveNotification(toast.id);
                    }
                    close(hwnd, "clicked");
                }
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_CHECK => {
            // SAFETY: our timer.
            unsafe { let _ = KillTimer(Some(hwnd), TIMER_CHECK); }
            check_popup(hwnd);
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_HIDE => {
            // SAFETY: our timer.
            unsafe { let _ = KillTimer(Some(hwnd), TIMER_HIDE); }
            close(hwnd, "timed out");
            LRESULT(0)
        }
        // SAFETY: default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn point(lparam: LPARAM) -> (f32, f32) {
    ((lparam.0 & 0xFFFF) as i16 as f32, ((lparam.0 >> 16) & 0xFFFF) as i16 as f32)
}
