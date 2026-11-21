//! What the user sees from the guard: task dialogs in the Windows display language (French or
//! English). comctl32's TaskDialogIndirect is looked up at run time: should it be missing, the guard
//! still runs and falls back to a plain message box, so a dialog can never keep a session from
//! getting a shell.

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Globalization::GetUserDefaultUILanguage;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Controls::{
    TASKDIALOG_BUTTON, TASKDIALOG_FLAGS, TASKDIALOG_NOTIFICATIONS, TDN_CREATED, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TD_INFORMATION_ICON, TD_SHIELD_ICON,
    TD_WARNING_ICON, TDCBF_CANCEL_BUTTON, TDF_ALLOW_DIALOG_CANCELLATION, TDF_USE_COMMAND_LINKS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    HWND_TOPMOST, IDYES, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO,
    MESSAGEBOX_STYLE, MessageBoxW, SWP_NOMOVE, SWP_NOSIZE, SetForegroundWindow, SetWindowPos,
};
use windows::core::{HRESULT, HSTRING, PCWSTR, s, w};

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
        return fallback_box(icon, main, content, buttons);
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
    let mut pressed = 0i32;
    // SAFETY: every string and the button array outlive the call.
    let hr = unsafe { td(&cfg, &mut pressed, std::ptr::null_mut(), std::ptr::null_mut()) };
    if hr.is_err() {
        crate::log::line(&format!("task dialog failed ({hr:?}): message box instead"));
        return fallback_box(icon, main, content, buttons);
    }
    pressed
}

/// A dialog with one OK button.
pub fn tell(icon: Icon, main: &str, content: &str, details: Option<&str>) {
    choose(icon, main, content, &[(1, tr("OK", "OK"))], false, details);
}

/// Without task dialogs: Yes is the first button, No the second (or OK for one).
fn fallback_box(icon: Icon, main: &str, content: &str, buttons: &[(i32, &str)]) -> i32 {
    let mut text = format!("{main}\n\n{content}");
    let yes_no = buttons.len() >= 2;
    if yes_no {
        text.push_str(&format!("\n\n{}: {}\n{}: {}", tr("Oui", "Yes"), buttons[0].1, tr("Non", "No"), buttons[1].1));
    }
    let style = match icon {
        Icon::Warning | Icon::Shield => MB_ICONWARNING,
        Icon::Info => MB_ICONINFORMATION,
    };
    let style = MESSAGEBOX_STYLE(style.0 | MB_TOPMOST.0 | MB_SETFOREGROUND.0 | if yes_no { MB_YESNO.0 } else { MB_OK.0 });
    // SAFETY: plain strings that outlive the call.
    let r = unsafe { MessageBoxW(None, &HSTRING::from(text), w!("explorust"), style) };
    match (yes_no, r == IDYES) {
        (true, true) => buttons[0].0,
        (true, false) => buttons[1].0,
        (false, _) => buttons.first().map(|b| b.0).unwrap_or(1),
    }
}
