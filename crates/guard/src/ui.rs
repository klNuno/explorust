//! What the user sees from the guard: task dialogs in the Windows display language (French or
//! English). comctl32's TaskDialogIndirect is looked up at run time: should it be missing, the guard
//! still runs and falls back to a plain message box, so a dialog can never keep a session from
//! getting a shell.

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Globalization::GetUserDefaultUILanguage;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Controls::{
    TASKDIALOG_BUTTON, TASKDIALOG_FLAGS, TASKDIALOG_NOTIFICATIONS, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TD_INFORMATION_ICON, TD_SHIELD_ICON,
    TD_WARNING_ICON, TDCBF_CANCEL_BUTTON, TDCBF_OK_BUTTON, TDF_ALLOW_DIALOG_CANCELLATION, TDF_CALLBACK_TIMER, TDF_SHOW_MARQUEE_PROGRESS_BAR,
    TDF_USE_COMMAND_LINKS, TDM_CLICK_BUTTON, TDM_ENABLE_BUTTON, TDM_SET_PROGRESS_BAR_MARQUEE, TDN_BUTTON_CLICKED, TDN_CREATED, TDN_TIMER,
};
use windows::Win32::UI::WindowsAndMessaging::{
    HWND_TOPMOST, IDNO, IDOK, IDYES, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_OKCANCEL, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO,
    MB_YESNOCANCEL,
    MESSAGEBOX_STYLE, MessageBoxW, SWP_NOMOVE, SWP_NOSIZE, SendMessageW, SetForegroundWindow, SetWindowPos,
};
use windows::core::{HRESULT, HSTRING, PCWSTR, s, w};
use std::sync::atomic::{AtomicBool, Ordering};

/// True when Windows shows its menus in French.
pub fn french() -> bool {
    // SAFETY: a plain query.
    let lang = unsafe { GetUserDefaultUILanguage() };
    lang & 0x3ff == 0x0c
}

/// The French or the English text, after the display language.
pub fn tr(fr: &'static str, en: &'static str) -> &'static str {
    if french() { fr } else { en }
}

#[derive(Clone, Copy)]
pub enum Icon {
    Info,
    Warning,
    Shield,
}

type TaskDialogIndirect = unsafe extern "system" fn(*const TASKDIALOGCONFIG, *mut i32, *mut i32, *mut i32) -> HRESULT;

fn task_dialog() -> Option<TaskDialogIndirect> {
    // SAFETY: comctl32 is a system DLL; the manifest makes this version 6, which exports it.
    unsafe {
        let lib = LoadLibraryW(w!("comctl32.dll")).ok()?;
        let f = GetProcAddress(lib, s!("TaskDialogIndirect"))?;
        Some(std::mem::transmute::<unsafe extern "system" fn() -> isize, TaskDialogIndirect>(f))
    }
}

/// Brings the dialog in front of everything: at logon or after a fallback nothing else owns it.
unsafe extern "system" fn on_event(hwnd: HWND, msg: TASKDIALOG_NOTIFICATIONS, _: WPARAM, _: LPARAM, _: isize) -> HRESULT {
    if msg == TDN_CREATED {
        // SAFETY: the dialog's own window.
        unsafe {
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
            let _ = SetForegroundWindow(hwnd);
        }
    }
    HRESULT(0)
}

/// Shows a dialog with one command link per `(id, text)` (the first is the default) plus Cancel
/// when `cancel` is set. Returns the id pressed, 2 (IDCANCEL) when closed. `details` shows behind a
/// "details" expander.
pub fn choose(icon: Icon, main: &str, content: &str, buttons: &[(i32, &str)], cancel: bool, details: Option<&str>) -> i32 {
    let title = HSTRING::from("explorust");
    let main_w = HSTRING::from(main);
    let content_w = HSTRING::from(content);
    let details_w = details.map(HSTRING::from);
    let labels: Vec<HSTRING> = buttons.iter().map(|(_, t)| HSTRING::from(*t)).collect();
    let tbuttons: Vec<TASKDIALOG_BUTTON> = buttons
        .iter()
        .zip(&labels)
        .map(|((id, _), l)| TASKDIALOG_BUTTON { nButtonID: *id, pszButtonText: PCWSTR(l.as_ptr()) })
        .collect();
    let Some(td) = task_dialog() else {
        return fallback_box(icon, main, content, buttons, cancel);
    };
    let icon_id = match icon {
        Icon::Info => TD_INFORMATION_ICON,
        Icon::Warning => TD_WARNING_ICON,
        Icon::Shield => TD_SHIELD_ICON,
    };
    let more = HSTRING::from(tr("Détails", "Details"));
    let mut flags = TDF_USE_COMMAND_LINKS;
    if cancel {
        flags = TASKDIALOG_FLAGS(flags.0 | TDF_ALLOW_DIALOG_CANCELLATION.0);
    }
    let cfg = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        dwFlags: flags,
        dwCommonButtons: if cancel { TDCBF_CANCEL_BUTTON } else { Default::default() },
        pszWindowTitle: PCWSTR(title.as_ptr()),
        Anonymous1: TASKDIALOGCONFIG_0 { pszMainIcon: icon_id },
        pszMainInstruction: PCWSTR(main_w.as_ptr()),
        pszContent: PCWSTR(content_w.as_ptr()),
        cButtons: tbuttons.len() as u32,
        pButtons: tbuttons.as_ptr(),
        nDefaultButton: buttons.first().map(|b| b.0).unwrap_or(0),
        pszExpandedInformation: details_w.as_ref().map(|d| PCWSTR(d.as_ptr())).unwrap_or(PCWSTR::null()),
        pszCollapsedControlText: if details.is_some() { PCWSTR(more.as_ptr()) } else { PCWSTR::null() },
        pfCallback: Some(on_event),
        ..Default::default()
    };
    crate::log::line(&format!("dialog: {main}"));
    let mut pressed = 0i32;
    // SAFETY: every string and the button array outlive the call.
    let hr = unsafe { td(&cfg, &mut pressed, std::ptr::null_mut(), std::ptr::null_mut()) };
    if hr.is_err() {
        crate::log::line(&format!("task dialog failed ({hr:?}): message box instead"));
        return fallback_box(icon, main, content, buttons, cancel);
    }
    crate::log::line(&format!("dialog: {main}: button {pressed}"));
    pressed
}

/// Runs `work` on a worker thread while a dialog with a moving progress bar says what is going on,
/// so a slow step (the notification package can take a minute or two) never looks frozen. The
/// dialog cannot be closed before `work` ends; it closes itself then. Without task dialogs the work
/// just runs.
pub fn working<T: Send>(main: &str, content: &str, work: impl FnOnce() -> T + Send) -> T {
    let done = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let job = scope.spawn(|| {
            // Set on drop, so a panic in `work` still closes the dialog.
            struct Done<'a>(&'a AtomicBool);
            impl Drop for Done<'_> {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::SeqCst);
                }
            }
            let _done = Done(&done);
            work()
        });
        if let Some(td) = task_dialog() {
            let title = HSTRING::from("explorust");
            let main_w = HSTRING::from(main);
            let content_w = HSTRING::from(content);
            let cfg = TASKDIALOGCONFIG {
                cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
                dwFlags: TASKDIALOG_FLAGS(TDF_SHOW_MARQUEE_PROGRESS_BAR.0 | TDF_CALLBACK_TIMER.0),
                dwCommonButtons: TDCBF_OK_BUTTON,
                pszWindowTitle: PCWSTR(title.as_ptr()),
                pszMainInstruction: PCWSTR(main_w.as_ptr()),
                pszContent: PCWSTR(content_w.as_ptr()),
                pfCallback: Some(on_progress),
                lpCallbackData: &done as *const AtomicBool as isize,
                ..Default::default()
            };
            crate::log::line(&format!("dialog: {main} (progress)"));
            // SAFETY: the strings and `done` outlive the call, which returns before the scope ends.
            let hr = unsafe { td(&cfg, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) };
            if hr.is_err() {
                crate::log::line(&format!("progress dialog failed ({hr:?})"));
            }
        }
        job.join().unwrap_or_else(|p| std::panic::resume_unwind(p))
    })
}

/// The progress dialog: marquee and a greyed OK at first, then OK clicked by itself once the work
/// is done; a click or Escape before that is ignored.
unsafe extern "system" fn on_progress(hwnd: HWND, msg: TASKDIALOG_NOTIFICATIONS, _: WPARAM, _: LPARAM, data: isize) -> HRESULT {
    // SAFETY: `data` is the `AtomicBool` that `working` keeps alive for the whole dialog.
    let done = unsafe { &*(data as *const AtomicBool) }.load(Ordering::SeqCst);
    // SAFETY: messages to the dialog's own window.
    unsafe {
        if msg == TDN_CREATED {
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
            let _ = SetForegroundWindow(hwnd);
            SendMessageW(hwnd, TDM_SET_PROGRESS_BAR_MARQUEE.0 as u32, Some(WPARAM(1)), Some(LPARAM(30)));
            SendMessageW(hwnd, TDM_ENABLE_BUTTON.0 as u32, Some(WPARAM(IDOK.0 as usize)), Some(LPARAM(0)));
        } else if msg == TDN_TIMER && done {
            SendMessageW(hwnd, TDM_CLICK_BUTTON.0 as u32, Some(WPARAM(IDOK.0 as usize)), Some(LPARAM(0)));
        } else if msg == TDN_BUTTON_CLICKED && !done {
            return HRESULT(1);
        }
    }
    HRESULT(0)
}

/// A dialog with one OK button.
pub fn tell(icon: Icon, main: &str, content: &str, details: Option<&str>) {
    choose(icon, main, content, &[(1, tr("OK", "OK"))], false, details);
}

/// Without task dialogs: the first button is Yes (OK when alone), the second No; with `cancel`, a
/// Cancel button, and a box that closes or fails to open returns 2 (IDCANCEL) so it never counts
/// as a yes.
fn fallback_box(icon: Icon, main: &str, content: &str, buttons: &[(i32, &str)], cancel: bool) -> i32 {
    let mut text = format!("{main}\n\n{content}");
    let yes_no = buttons.len() >= 2;
    if yes_no {
        text.push_str(&format!("\n\n{}: {}\n{}: {}", tr("Oui", "Yes"), buttons[0].1, tr("Non", "No"), buttons[1].1));
    } else if cancel && let Some(b) = buttons.first() {
        text.push_str(&format!("\n\nOK: {}", b.1));
    }
    let style = match icon {
        Icon::Warning | Icon::Shield => MB_ICONWARNING,
        Icon::Info => MB_ICONINFORMATION,
    };
    let kind = match (yes_no, cancel) {
        (true, true) => MB_YESNOCANCEL,
        (true, false) => MB_YESNO,
        (false, true) => MB_OKCANCEL,
        (false, false) => MB_OK,
    };
    let style = MESSAGEBOX_STYLE(style.0 | MB_TOPMOST.0 | MB_SETFOREGROUND.0 | kind.0);
    crate::log::line(&format!("dialog: {main} (message box)"));
    // SAFETY: plain strings that outlive the call.
    let r = unsafe { MessageBoxW(None, &HSTRING::from(text), w!("explorust"), style) };
    let picked = if r == IDYES || r == IDOK {
        buttons.first().map(|b| b.0).unwrap_or(1)
    } else if r == IDNO && yes_no {
        buttons[1].0
    } else {
        2
    };
    crate::log::line(&format!("dialog: {main}: button {picked}"));
    picked
}
