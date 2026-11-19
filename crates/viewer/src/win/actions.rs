//! File actions through Windows' own shell: delete to the Recycle Bin with IFileOperation, copy
//! the file's data object to the clipboard, the Open with dialog, and the handler registration.

use super::log;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IDataObject};
use windows::Win32::System::Ole::{OleFlushClipboard, OleSetClipboard};
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Shell::*;
use windows_core::{HSTRING, PCWSTR, Result, w};

fn item(path: &str) -> Result<IShellItem> {
    unsafe { SHCreateItemFromParsingName(&HSTRING::from(path), None) }
}

/// Sends the file to the Recycle Bin; false when the user cancelled or it failed.
pub fn recycle(hwnd: HWND, path: &str) -> bool {
    let r: Result<bool> = unsafe {
        (|| {
            let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER)?;
            op.SetOwnerWindow(hwnd)?;
            op.SetOperationFlags(FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE)?;
            op.DeleteItem(&item(path)?, None)?;
            op.PerformOperations()?;
            Ok(!op.GetAnyOperationsAborted()?.as_bool())
        })()
    };
    log::line(&format!("recycle {path}: {r:?}"));
    r.unwrap_or(false)
}

/// Puts the file on the clipboard as Explorer's Copy does (its shell data object).
pub fn copy(path: &str) -> bool {
    let r: Result<()> = unsafe {
        (|| {
            let data: IDataObject = item(path)?.BindToHandler(None, &BHID_DataObject)?;
            OleSetClipboard(&data)?;
            OleFlushClipboard()
        })()
    };
    log::line(&format!("copy {path}: {r:?}"));
    r.is_ok()
}

pub fn open_with(hwnd: HWND, path: &str) {
    let file = HSTRING::from(path);
    let info = OPENASINFO { pcszFile: PCWSTR(file.as_ptr()), pcszClass: PCWSTR::null(), oaifInFlags: OAIF_ALLOW_REGISTRATION | OAIF_EXEC };
    let r = unsafe { SHOpenWithDialog(Some(hwnd), &info) };
    log::line(&format!("open with {path}: {r:?}"));
}

pub const PROGID: &str = "explorust.Image";
pub const APP: &str = "explorust Viewer";
const CAPS: &str = r"Software\explorust\Viewer\Capabilities";

fn set(key: &str, name: Option<&str>, value: &str) -> bool {
    let k = HSTRING::from(key);
    let n = name.map(HSTRING::from);
    let v: Vec<u16> = value.encode_utf16().chain([0]).collect();
    let np = n.as_ref().map(|n| PCWSTR(n.as_ptr())).unwrap_or(PCWSTR::null());
    unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, &k, np, REG_SZ.0, Some(v.as_ptr().cast()), (v.len() * 2) as u32).is_ok() }
}

fn notify_assoc() {
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) }
}

/// Registers the viewer for the current user: a ProgID, OpenWithProgids on each extension, and
/// Capabilities under RegisteredApplications so Settings > Default apps lists it. Changes no default.
pub fn register() -> bool {
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    let cls = format!(r"Software\Classes\{PROGID}");
    let mut ok = set(&cls, None, "Image (explorust)")
        && set(&format!(r"{cls}\DefaultIcon"), None, &format!("\"{exe}\",0"))
        && set(&format!(r"{cls}\shell\open\command"), None, &format!("\"{exe}\" \"%1\""))
        && set(&format!(r"{cls}\Application"), Some("ApplicationName"), APP)
        && set(CAPS, Some("ApplicationName"), APP)
        && set(CAPS, Some("ApplicationDescription"), "Fast image viewer of the explorust shell")
        && set(r"Software\RegisteredApplications", Some(APP), CAPS);
    for e in crate::view::EXTENSIONS {
        ok &= set(&format!(r"{CAPS}\FileAssociations"), Some(&format!(".{e}")), PROGID);
        ok &= set(&format!(r"Software\Classes\.{e}\OpenWithProgids"), Some(PROGID), "");
    }
    notify_assoc();
    log::line(&format!("register {exe}: {ok}"));
    ok
}

pub fn unregister() -> bool {
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(format!(r"Software\Classes\{PROGID}")));
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, w!(r"Software\explorust\Viewer"));
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, w!(r"Software\RegisteredApplications"), &HSTRING::from(APP));
        for e in crate::view::EXTENSIONS {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(format!(r"Software\Classes\.{e}\OpenWithProgids")), &HSTRING::from(PROGID));
        }
    }
    notify_assoc();
    log::line("unregister done");
    true
}

/// Opens Settings > Default apps on the viewer's page, where the user picks it per type: Windows
/// only accepts a default from the user (UserChoice is hashed), never silently.
pub fn defaults() -> bool {
    // IApplicationAssociationRegistrationUI::LaunchAdvancedAssociationUI answers 0x80070490 on
    // 26300: Settings' own page for a registered app takes its place.
    let uri = format!("ms-settings:defaultapps?registeredAppUser={}", APP.replace(' ', "%20"));
    let r = unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(uri.as_str()), PCWSTR::null(), PCWSTR::null(), windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL) };
    let ok = r.0 as usize > 32;
    log::line(&format!("defaults {uri}: {}", r.0 as usize));
    ok
}
