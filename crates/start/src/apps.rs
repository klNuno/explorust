//! Apps from Windows' own list, `shell:AppsFolder` (names, AUMIDs, icons through the item's
//! `IShellItemImageFactory`), launched through the activation manager (packaged apps) or the
//! shell item (desktop apps); and the pins file.

use crate::{log, model};
use shelldata::Image;
use std::path::PathBuf;
use windows::Win32::Foundation::{FILETIME, HWND, SIZE};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits, GetObjectW,
    ReleaseDC,
};
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
use windows::Win32::System::Com::{CLSCTX_LOCAL_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::System::Threading::GetProcessId;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::PropertiesSystem::{GPS_DEFAULT, PSGetNameFromPropertyKey, PSGetPropertyKeyFromName};
use windows::Win32::UI::Shell::{
    AO_NONE, ApplicationActivationManager, BHID_EnumItems, IApplicationActivationManager, IEnumShellItems,
    IShellItem, IShellItem2, IShellItemImageFactory, SEE_MASK_FLAG_NO_UI, SEE_MASK_INVOKEIDLIST,
    SEE_MASK_NOCLOSEPROCESS, SHCreateItemFromParsingName, SHELLEXECUTEINFOW, SHParseDisplayName, SIGDN,
    SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY, ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows_core::{HSTRING, Interface, PCWSTR, PWSTR};

pub struct App {
    pub name: String,
    pub aumid: String,
    pub icon: Option<Image>,
    /// Unix seconds the app was installed, when Windows tells.
    pub installed: Option<i64>,
}

pub fn take_pwstr(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: a NUL-terminated string allocated by the shell, freed once read.
    unsafe {
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as _));
        s
    }
}

fn display(item: &IShellItem, sigdn: SIGDN) -> Option<String> {
    // SAFETY: plain query; the string is freed by take_pwstr.
    unsafe { item.GetDisplayName(sigdn) }.ok().map(take_pwstr).filter(|s| !s.is_empty())
}

pub fn filetime_to_unix(ft: FILETIME) -> i64 {
    let v = ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64;
    (v as i64 - 116_444_736_000_000_000) / 10_000_000
}

/// Every app of `shell:AppsFolder`, sorted as All apps lists them. COM (STA) must be up.
pub fn load(icon_px: i32) -> Vec<App> {
    let mut out = Vec::new();
    // SAFETY: shell namespace calls on this thread's apartment.
    unsafe {
        let Ok(folder) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from("shell:AppsFolder"), None) else {
            log::line("apps: shell:AppsFolder not found");
            return out;
        };
        let Ok(items) = folder.BindToHandler::<_, IEnumShellItems>(None, &BHID_EnumItems) else {
            log::line("apps: shell:AppsFolder not enumerable");
            return out;
        };
        let installed_key = install_time_key();
        let mut logged_props = false;
        loop {
            let mut one = [None];
            let mut n = 0u32;
            if items.Next(&mut one, Some(&mut n)).is_err() || n == 0 {
                break;
            }
            let Some(item) = one[0].take() else { break };
            let Some(name) = display(&item, SIGDN_NORMALDISPLAY) else { continue };
            let item2: Option<IShellItem2> = item.cast().ok();
            let aumid = item2
                .as_ref()
                .and_then(|i| i.GetString(&PKEY_AppUserModel_ID).ok())
                .map(take_pwstr)
                .filter(|s| !s.is_empty())
                .or_else(|| display(&item, SIGDN_PARENTRELATIVEPARSING));
            let Some(aumid) = aumid else { continue };
            if !logged_props && aumid.starts_with("Microsoft.WindowsCalculator") {
                logged_props = true;
                if let Some(i2) = &item2 {
                    log_properties(i2);
                }
            }
            let installed = match (&item2, &installed_key) {
                (Some(i2), Some(k)) => i2.GetFileTime(k).ok().map(filetime_to_unix),
                _ => None,
            };
            let icon = item.cast::<IShellItemImageFactory>().ok().and_then(|f| image(&f, icon_px));
            out.push(App { name, aumid, icon, installed });
        }
    }
    out.sort_by_key(|a| model::sort_key(&a.name));
    out
}

/// The property Windows keeps an app's install time in, by name.
fn install_time_key() -> Option<windows::Win32::Foundation::PROPERTYKEY> {
    for name in ["System.AppUserModel.InstallDate", "System.DateCreated"] {
        let mut k = Default::default();
        // SAFETY: looks a canonical name up in the property schema.
        if unsafe { PSGetPropertyKeyFromName(&HSTRING::from(name), &mut k) }.is_ok() {
            log::line(&format!("apps: install time from {name}"));
            return Some(k);
        }
    }
    None
}

/// Logs the property names an AppsFolder item carries, for the next steps (search keywords).
fn log_properties(item: &IShellItem2) {
    // SAFETY: reads the item's property store.
    unsafe {
        let Ok(store) = item.GetPropertyStore::<windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore>(GPS_DEFAULT)
        else {
            return;
        };
        let n = store.GetCount().unwrap_or(0);
        let mut names = Vec::new();
        for i in 0..n {
            let mut k = Default::default();
            if store.GetAt(i, &mut k).is_ok() {
                names.push(PSGetNameFromPropertyKey(&k).map(take_pwstr).unwrap_or_else(|_| format!("{:?}/{}", k.fmtid, k.pid)));
            }
        }
        log::line(&format!("apps: Calculator item properties: {}", names.join(", ")));
    }
}

/// The item's icon at `px`, straight-alpha BGRA.
pub fn image(factory: &IShellItemImageFactory, px: i32) -> Option<Image> {
    // SAFETY: GDI reads of a bitmap the factory hands over, deleted before returning.
    unsafe {
        let hbmp = factory.GetImage(SIZE { cx: px, cy: px }, SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK).ok()?;
        let mut bm = BITMAP::default();
        GetObjectW(hbmp.into(), size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _));
        let (bw, bh) = (bm.bmWidth, bm.bmHeight.abs());
        let mut info = BITMAPINFO::default();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: bw,
            biHeight: -bh,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let mut bgra = vec![0u8; (bw * bh * 4) as usize];
        let dc = GetDC(None);
        let lines = GetDIBits(dc, hbmp, 0, bh as u32, Some(bgra.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS);
        ReleaseDC(None, dc);
        let _ = DeleteObject(hbmp.into());
        if lines == 0 {
            return None;
        }
        for px in bgra.chunks_exact_mut(4) {
            let a = px[3] as u32;
            if a > 0 && a < 255 {
                for c in &mut px[..3] {
                    *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
        Some(Image { width: bw as u32, height: bh as u32, bgra })
    }
}

/// Starts an app by AUMID: packaged apps through IApplicationActivationManager (works with no
/// explorer), the others through their `shell:AppsFolder` item. Returns the pid when known.
pub fn launch(aumid: &str) -> Result<u32, String> {
    if aumid.contains('!') {
        // SAFETY: standard creation and call of the activation manager.
        unsafe {
            let m: IApplicationActivationManager = CoCreateInstance(&ApplicationActivationManager, None, CLSCTX_LOCAL_SERVER)
                .map_err(|e| format!("activation manager: {e}"))?;
            return m
                .ActivateApplication(&HSTRING::from(aumid), &HSTRING::new(), AO_NONE)
                .map_err(|e| format!("ActivateApplication: {e}"));
        }
    }
    let path = HSTRING::from(format!("shell:AppsFolder\\{aumid}"));
    let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
    // SAFETY: parses the item, invokes its default verb, frees the ID list.
    unsafe {
        SHParseDisplayName(&path, None, &mut pidl, 0, None).map_err(|e| format!("parse {aumid}: {e}"))?;
        let r = execute(PCWSTR::null(), pidl.cast());
        CoTaskMemFree(Some(pidl as _));
        r
    }
}

/// Opens a file or shortcut with its default verb.
pub fn open_path(path: &str) -> Result<u32, String> {
    let w = HSTRING::from(path);
    // SAFETY: ShellExecuteEx on a path string that outlives the call.
    unsafe { execute(PCWSTR(w.as_ptr()), std::ptr::null_mut()) }
}

unsafe fn execute(file: PCWSTR, pidl: *mut core::ffi::c_void) -> Result<u32, String> {
    let mut sei = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI | if pidl.is_null() { 0 } else { SEE_MASK_INVOKEIDLIST },
        hwnd: HWND::default(),
        lpFile: file,
        lpIDList: pidl,
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: the struct and its strings live through the call.
    unsafe {
        ShellExecuteExW(&mut sei).map_err(|e| format!("ShellExecuteEx: {e}"))?;
        if sei.hProcess.is_invalid() {
            return Ok(0);
        }
        let pid = GetProcessId(sei.hProcess);
        let _ = windows::Win32::Foundation::CloseHandle(sei.hProcess);
        Ok(pid)
    }
}

pub fn pins_path() -> PathBuf {
    log::dir().join("start-pins.json")
}

/// Our pins, or Windows' default ones when the file does not exist yet (true in the second field).
pub fn load_pins() -> (Vec<String>, bool) {
    match std::fs::read_to_string(pins_path()).ok().and_then(|t| model::pins_from_json(&t)) {
        Some(p) => (p, false),
        None => (model::DEFAULT_PINS.iter().map(|s| s.to_string()).collect(), true),
    }
}

pub fn save_pins(pins: &[String]) {
    let _ = std::fs::create_dir_all(log::dir());
    match std::fs::write(pins_path(), model::pins_to_json(pins)) {
        Ok(()) => log::line(&format!("pins: saved {} to {}", pins.len(), pins_path().display())),
        Err(e) => log::line(&format!("pins: not saved: {e}")),
    }
}
