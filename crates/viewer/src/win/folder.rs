//! The images of a folder in Explorer's name order: the folder's own IShellFolder enumerates and
//! compares them (CompareIDs on the name column, the natural sort Explorer shows).

use crate::view::is_image;
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::Common::{ITEMIDLIST, STRRET};
use windows::Win32::UI::Shell::*;
use windows_core::{HSTRING, PWSTR};

struct Child(*mut ITEMIDLIST);

impl Drop for Child {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(Some(self.0 as _)) }
    }
}

/// Full paths of the images in `dir`, sorted as Explorer sorts by name.
pub fn images(hwnd: HWND, dir: &str) -> Vec<String> {
    let mut out = Vec::new();
    unsafe {
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        if SHParseDisplayName(&HSTRING::from(dir), None, &mut pidl, 0, None).is_err() {
            return out;
        }
        let folder: windows_core::Result<IShellFolder> = SHBindToObject(None, pidl, None);
        CoTaskMemFree(Some(pidl as _));
        let Ok(folder) = folder else { return out };
        let mut en: Option<IEnumIDList> = None;
        if folder.EnumObjects(hwnd, SHCONTF_NONFOLDERS.0 as u32, &mut en).is_err() {
            return out;
        }
        let Some(en) = en else { return out };
        let mut items: Vec<(Child, String)> = Vec::new();
        loop {
            let mut one = [std::ptr::null_mut(); 1];
            let mut got = 0u32;
            if en.Next(&mut one, Some(&mut got)).is_err() || got == 0 {
                break;
            }
            let child = Child(one[0]);
            let mut sr = STRRET::default();
            if folder.GetDisplayNameOf(child.0, SHGDNF(SHGDN_INFOLDER.0 | SHGDN_FORPARSING.0), &mut sr).is_err() {
                continue;
            }
            let mut name = PWSTR::null();
            if StrRetToStrW(&mut sr, Some(child.0), &mut name).is_err() {
                continue;
            }
            let s = name.to_string().unwrap_or_default();
            CoTaskMemFree(Some(name.0 as _));
            if is_image(&s) {
                items.push((child, s));
            }
        }
        items.sort_by(|a, b| {
            let hr = folder.CompareIDs(LPARAM(0), a.0.0, b.0.0);
            (hr.0 as i16).cmp(&0)
        });
        let base = dir.trim_end_matches('\\');
        out.extend(items.into_iter().map(|(_, n)| format!("{base}\\{n}")));
    }
    out
}
