//! A button's jump list data through Windows' APIs: the recent items of the app's automatic
//! destinations (`IApplicationDocumentLists`, by AppUserModelID), the app's display name, and a
//! shell item to pin. Opening a recent item goes through ShellExecute on a short-lived thread.

use super::log;
use shelldata::items::{self, PinItem};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Shell::Common::{IObjectArray, ITEMIDLIST};
use windows::Win32::UI::Shell::{
    ADLT_RECENT, ApplicationDocumentLists, IApplicationDocumentLists, ILGetSize, IShellItem, IShellItem2, IShellLinkW,
    SHCreateItemFromIDList, SHCreateItemFromParsingName, SHGetIDListFromObject, SIGDN_DESKTOPABSOLUTEPARSING,
    SIGDN_NORMALDISPLAY,
};
use windows_core::{HSTRING, Interface};

/// A recent item: (display name, parsing path).
pub type Recent = (String, String);

fn take(p: windows_core::Result<windows_core::PWSTR>) -> Option<String> {
    let p = p.ok()?;
    // SAFETY: a CoTaskMem string from the shell, freed after the copy.
    unsafe {
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as _));
        s
    }
}

fn item_names(item: &IShellItem) -> Option<Recent> {
    // SAFETY: name reads on a live shell item.
    unsafe { Some((take(item.GetDisplayName(SIGDN_NORMALDISPLAY))?, take(item.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING))?)) }
}

/// The app's recent items (at most `max`), newest first. Empty without an AppID or a list.
pub fn recent(app_id: &str, max: u32) -> Vec<Recent> {
    let mut out = Vec::new();
    // SAFETY: COM calls on this STA thread; objects are released when dropped.
    unsafe {
        let Ok(lists) = CoCreateInstance::<_, IApplicationDocumentLists>(&ApplicationDocumentLists, None, CLSCTX_INPROC_SERVER) else {
            return out;
        };
        if lists.SetAppID(&HSTRING::from(app_id)).is_err() {
            return out;
        }
        let arr: IObjectArray = match lists.GetList(ADLT_RECENT, max) {
            Ok(a) => a,
            Err(e) => {
                log::line(&format!("jump list {app_id}: GetList failed: {e}"));
                return out;
            }
        };
        let n = arr.GetCount().unwrap_or(0);
        for i in 0..n {
            if let Ok(item) = arr.GetAt::<IShellItem>(i) {
                if let Some(r) = item_names(&item) {
                    out.push(r);
                }
            } else if let Ok(link) = arr.GetAt::<IShellLinkW>(i) {
                if let Ok(pidl) = link.GetIDList() {
                    if let Ok(item) = SHCreateItemFromIDList::<IShellItem>(pidl as *const ITEMIDLIST) {
                        if let Some(r) = item_names(&item) {
                            out.push(r);
                        }
                    }
                    CoTaskMemFree(Some(pidl as _));
                }
            }
        }
    }
    out
}

/// The shell item of an app: its AppsFolder entry by AUMID, else its exe.
fn app_item(id: &str, exe: &str) -> Option<IShellItem> {
    if !id.contains('\\') {
        if let Some(i) = items::apps_folder_item(id) {
            return Some(i);
        }
    }
    // SAFETY: a parsing-name lookup.
    unsafe { SHCreateItemFromParsingName(&HSTRING::from(exe), None).ok() }
}

/// The app's display name, the jump list's launch entry.
pub fn app_name(id: &str, exe: &str) -> String {
    let name = app_item(id, exe).and_then(|i| item_names(&i)).map(|(n, _)| n);
    let name = name.unwrap_or_else(|| exe.rsplit('\\').next().unwrap_or(exe).to_string());
    name.strip_suffix(".exe").map(str::to_string).unwrap_or(name)
}

/// A pin for the app (kept in the bar's model).
pub fn pin_item(key: &str, id: &str, exe: &str) -> Option<PinItem> {
    let item = app_item(id, exe)?;
    // SAFETY: shell calls on a live item; the PIDL is copied, then freed.
    unsafe {
        let pidl = SHGetIDListFromObject(&item).ok()?;
        let len = ILGetSize(Some(pidl as *const ITEMIDLIST)) as usize;
        let bytes = std::slice::from_raw_parts(pidl as *const u8, len).to_vec();
        CoTaskMemFree(Some(pidl as _));
        let item2: IShellItem2 = item.cast().ok()?;
        let name = take(item2.GetDisplayName(SIGDN_NORMALDISPLAY)).unwrap_or_default();
        Some(PinItem { key: key.to_string(), name, pidl: bytes, item })
    }
}
