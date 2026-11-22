//! Shell items and app identities for a live taskbar: the pins as shell items, the
//! AppUserModelID of a window (its property store, then its process), process paths, and the
//! AppsFolder item of an AUMID. COM must be initialised on the calling thread.

use crate::wide;
use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Storage::EnhancedStorage::{PKEY_AppUserModel_ID, PKEY_Link_TargetParsingPath};
use windows::Win32::Storage::Packaging::Appx::GetApplicationUserModelId;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RegGetValueW};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
use windows::Win32::UI::Shell::{
    FOLDERID_AppsFolder, IShellItem, IShellItem2, KF_FLAG_DEFAULT, SHCreateItemFromIDList, SHCreateItemInKnownFolder,
    SHParseDisplayName, SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_FILESYSPATH, SIGDN_NORMALDISPLAY,
};
use windows_core::{PCWSTR, PWSTR};
use std::{collections::HashSet, path::Path};

/// A taskbar pin: its identity, display name, PIDL (to launch it) and shell item (for its icon).
pub struct PinItem {
    /// Lowercase AUMID when the item has one, else the shortcut target, else its parsing name.
    pub key: String,
    pub name: String,
    pub pidl: Vec<u8>,
    pub item: IShellItem,
}

/// The pins of the taskbar in their order, from the Taskband blob.
pub fn taskbar_pins() -> Vec<PinItem> {
    let blob = taskband_blob();
    crate::taskband::pin_pidls(&blob)
        .into_iter()
        .filter_map(|pidl| {
            // SAFETY: `pidl` is a validated, zero-terminated ITEMIDLIST that lives for the call.
            let item2: IShellItem2 = unsafe { SHCreateItemFromIDList(pidl.as_ptr() as *const ITEMIDLIST) }.ok()?;
            let key = item_key(&item2)?;
            let name = take(unsafe { item2.GetDisplayName(SIGDN_NORMALDISPLAY) }).unwrap_or_default();
            let item: IShellItem = windows_core::Interface::cast(&item2).ok()?;
            Some(PinItem { key, name, pidl, item })
        })
        .collect()
}

/// Resolves taskband pins in registry order. The shortcut directory supplies the backing
/// files for filesystem PIDLs. Its contents alone cannot distinguish pins from leftovers.
pub fn resolved_taskbar_pins() -> Vec<PinItem> {
    resolved_taskbar_pins_from_blob(&taskband_blob())
}

pub fn resolved_taskbar_pins_from_blob(blob: &[u8]) -> Vec<PinItem> {
    let mut seen = HashSet::new();
    crate::taskband::pin_pidls(blob).into_iter()
        .filter_map(resolve_pin)
        .filter(|pin| seen.insert(pin.key.clone()))
        .collect()
}

fn resolve_pin(mut pidl: Vec<u8>) -> Option<PinItem> {
    // SAFETY: pin_pidls supplies validated ITEMIDLIST buffers.
    let mut item: IShellItem2 = unsafe { SHCreateItemFromIDList(pidl.as_ptr().cast()) }.ok()?;
    // An AppsFolder PIDL has no filesystem path and stays in its original namespace.
    // The parsing-name fallback catches a stale filesystem PIDL even if SIGDN_FILESYSPATH fails.
    let path = take(unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) })
        .or_else(|| take(unsafe { item.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING) }))
        .filter(|path| Path::new(path).is_absolute());
    if let Some(path) = path.as_deref() {
        let path = Path::new(path);
        let name = path.file_name()?.to_str()?;
        let original = crate::taskband::shortcut_name(name)?;
        let shortcut = path.with_file_name(original);
        if !shortcut.is_file() {
            return None;
        }
        if original != name {
            // Reparse the actual link so identity, image and launch all use the same item.
            pidl = parse_pidl(shortcut.to_str()?)?;
            item = unsafe { SHCreateItemFromIDList(pidl.as_ptr().cast()) }.ok()?;
        }
    }
    // The shortcut is the pin. A target may be advertised, on a disconnected mapped drive,
    // or reachable only after the user signs in to a network provider.
    let key = item_key(&item)?;
    let name = take(unsafe { item.GetDisplayName(SIGDN_NORMALDISPLAY) })?;
    let item = windows_core::Interface::cast(&item).ok()?;
    Some(PinItem { key, name, pidl, item })
}

fn parse_pidl(path: &str) -> Option<Vec<u8>> {
    use windows::Win32::UI::Shell::ILGetSize;
    let path = wide(path);
    let mut pidl = std::ptr::null_mut();
    // SAFETY: the shell returns a CoTaskMemAlloc PIDL. Copy it before releasing it.
    unsafe {
        SHParseDisplayName(PCWSTR(path.as_ptr()), None, &mut pidl, 0, None).ok()?;
        let bytes = (!pidl.is_null()).then(|| {
            std::slice::from_raw_parts(pidl.cast::<u8>(), ILGetSize(Some(pidl)) as usize).to_vec()
        });
        CoTaskMemFree(Some(pidl.cast()));
        bytes
    }
}

/// The identity a pin is matched on (lowercase).
pub fn item_key(item: &IShellItem2) -> Option<String> {
    // SAFETY: property reads on a live shell item.
    unsafe {
        let s = take(item.GetString(&PKEY_AppUserModel_ID))
            .or_else(|| take(item.GetString(&PKEY_Link_TargetParsingPath)))
            .or_else(|| take(item.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING)))?;
        Some(s.to_lowercase())
    }
}

/// The raw Taskband `Favorites` value (empty when absent).
pub fn taskband_blob() -> Vec<u8> {
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Taskband");
    let val = wide("Favorites");
    // SAFETY: registry reads into a buffer of the size the first call returns.
    unsafe {
        let mut len = 0u32;
        let k = PCWSTR(key.as_ptr());
        let v = PCWSTR(val.as_ptr());
        if RegGetValueW(HKEY_CURRENT_USER, k, v, RRF_RT_REG_BINARY, None, None, Some(&mut len)).is_err() {
            return Vec::new();
        }
        let mut buf = vec![0u8; len as usize];
        if RegGetValueW(HKEY_CURRENT_USER, k, v, RRF_RT_REG_BINARY, None, Some(buf.as_mut_ptr().cast()), Some(&mut len)).is_err() {
            return Vec::new();
        }
        buf.truncate(len as usize);
        buf
    }
}

/// The AppUserModelID a window sets in its property store (UWP frames, Edge, apps with
/// explicit IDs).
pub fn window_app_id(hwnd: HWND) -> Option<String> {
    // SAFETY: property store of a window, read and freed here.
    unsafe {
        let store: IPropertyStore = SHGetPropertyStoreForWindow(hwnd).ok()?;
        let pv = store.GetValue(&PKEY_AppUserModel_ID).ok()?;
        let p = PropVariantToStringAlloc(&pv).ok()?;
        take_pwstr(p).filter(|s| !s.is_empty())
    }
}

/// The AUMID of a packaged process, None for a plain desktop process.
pub fn process_app_id(pid: u32) -> Option<String> {
    // SAFETY: a limited-information handle, closed before returning.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let r = GetApplicationUserModelId(h, &mut len, Some(PWSTR(buf.as_mut_ptr())));
        let _ = CloseHandle(h);
        if r.0 != 0 || len == 0 {
            return None;
        }
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..n])).filter(|s| !s.is_empty())
    }
}

/// Full image path of a process.
pub fn process_path(pid: u32) -> Option<String> {
    // SAFETY: a limited-information handle, closed before returning.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        r.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// The `shell:AppsFolder` item of an AUMID (packaged apps and Start menu apps).
pub fn apps_folder_item(aumid: &str) -> Option<IShellItem> {
    let w = wide(aumid);
    // SAFETY: a known-folder child lookup by name.
    unsafe { SHCreateItemInKnownFolder(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, PCWSTR(w.as_ptr())).ok() }
}

/// Whether DWM hides the window (another virtual desktop, a suspended UWP frame).
pub fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    // SAFETY: reads a 4-byte attribute into a local.
    let _ = unsafe { DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut _ as _, 4) };
    cloaked != 0
}

fn take(r: windows_core::Result<PWSTR>) -> Option<String> {
    r.ok().and_then(|p| unsafe { take_pwstr(p) })
}

/// Copies and frees a CoTaskMem string.
unsafe fn take_pwstr(p: PWSTR) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: `p` is a NUL-terminated string the callee allocated with CoTaskMemAlloc.
    unsafe {
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as _));
        s.filter(|s| !s.is_empty())
    }
}
