//! The command bar's drop-down menus (sort, view, see more), Win32 popup menus in the dark theme
//! the process asked for, and the clipboard text the edit boxes paste.

use crate::text::Strings;
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND, POINT};
use windows::Win32::Storage::EnhancedStorage::{PKEY_DateModified, PKEY_ItemNameDisplay, PKEY_ItemTypeText, PKEY_Size};
use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW};
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::PCWSTR;

fn popup(hwnd: HWND, items: &[(u32, &str, bool)], at: POINT) -> u32 {
    unsafe {
        let Ok(m) = CreatePopupMenu() else { return 0 };
        for &(id, label, checked) in items {
            if id == 0 {
                let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
            } else {
                let w = shelldata::wide(label);
                let flags = if checked { MF_STRING | MF_CHECKED } else { MF_STRING };
                let _ = AppendMenuW(m, flags, id as usize, PCWSTR(w.as_ptr()));
            }
        }
        let r = TrackPopupMenuEx(m, (TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN).0, at.x, at.y, hwnd, None);
        let _ = DestroyMenu(m);
        r.0 as u32
    }
}

pub fn sort(hwnd: HWND, v: &IFolderView2, s: &Strings, at: POINT) -> windows_core::Result<()> {
    let keys = [PKEY_ItemNameDisplay, PKEY_DateModified, PKEY_ItemTypeText, PKEY_Size];
    let mut cur = [SORTCOLUMN::default()];
    let _ = unsafe { v.GetSortColumns(&mut cur) };
    let mut cur = cur[0];
    if !keys.contains(&cur.propkey) {
        cur = SORTCOLUMN { propkey: keys[0], direction: SORT_ASCENDING };
    }
    let mut items: Vec<(u32, &str, bool)> =
        keys.iter().enumerate().map(|(i, k)| (i as u32 + 1, s.sort_by[i], cur.propkey == *k)).collect();
    items.push((0, "", false));
    items.push((10, s.ascending, cur.direction == SORT_ASCENDING));
    items.push((11, s.descending, cur.direction == SORT_DESCENDING));
    let pick = popup(hwnd, &items, at);
    let col = match pick {
        1..=4 => SORTCOLUMN { propkey: keys[pick as usize - 1], direction: cur.direction },
        10 => SORTCOLUMN { direction: SORT_ASCENDING, ..cur },
        11 => SORTCOLUMN { direction: SORT_DESCENDING, ..cur },
        _ => return Ok(()),
    };
    unsafe { v.SetSortColumns(&[col]) }
}

const VIEWS: [(FOLDERVIEWMODE, i32); 8] = [
    (FVM_ICON, 256),
    (FVM_ICON, 96),
    (FVM_ICON, 48),
    (FVM_SMALLICON, 16),
    (FVM_LIST, 16),
    (FVM_DETAILS, 16),
    (FVM_TILE, 48),
    (FVM_CONTENT, 32),
];

pub fn view(hwnd: HWND, v: &IFolderView2, s: &Strings, at: POINT) -> windows_core::Result<()> {
    let (mut mode, mut size) = (FVM_AUTO, 0);
    let _ = unsafe { v.GetViewModeAndIconSize(&mut mode, &mut size) };
    let current = VIEWS.iter().position(|&(m, sz)| m == mode && (m != FVM_ICON || sz == size));
    let items: Vec<(u32, &str, bool)> =
        (0..VIEWS.len()).map(|i| (i as u32 + 1, s.views[i], current == Some(i))).collect();
    let pick = popup(hwnd, &items, at) as usize;
    if (1..=VIEWS.len()).contains(&pick) {
        let (m, sz) = VIEWS[pick - 1];
        return unsafe { v.SetViewModeAndIconSize(m, sz) };
    }
    Ok(())
}

/// See more: select all, select none, properties.
pub fn more(hwnd: HWND, v: &IFolderView2, s: &Strings, at: POINT) -> windows_core::Result<bool> {
    let items = [(1, s.select_all, false), (2, s.select_none, false), (0, "", false), (3, s.properties, false)];
    match popup(hwnd, &items, at) {
        pick @ (1 | 2) => unsafe {
            let n = v.ItemCount(SVGIO_ALLVIEW)?;
            let flag = if pick == 1 { SVSI_SELECT } else { SVSI_DESELECT };
            for i in 0..n {
                v.SelectItem(i, flag.0 as u32)?;
            }
        },
        3 => return Ok(true),
        _ => {}
    }
    Ok(false)
}

pub fn file_clipboard() -> bool {
    static SHELL_ITEMS: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let shell_items = *SHELL_ITEMS.get_or_init(|| unsafe { RegisterClipboardFormatW(windows_core::w!("Shell IDList Array")) });
    // CF_HDROP is 15. Some shell namespace handlers provide only the native shell item format.
    unsafe { IsClipboardFormatAvailable(15).is_ok() || (shell_items != 0 && IsClipboardFormatAvailable(shell_items).is_ok()) }
}

/// Unicode text on the clipboard.
pub fn clipboard_text(hwnd: HWND) -> Option<String> {
    const CF_UNICODETEXT: u32 = 13;
    unsafe {
        OpenClipboard(Some(hwnd)).ok()?;
        let h: HANDLE = GetClipboardData(CF_UNICODETEXT).unwrap_or_default();
        let mut out = None;
        if !h.is_invalid() {
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u16;
            if !p.is_null() {
                let mut n = 0;
                while *p.add(n) != 0 {
                    n += 1;
                }
                out = Some(String::from_utf16_lossy(std::slice::from_raw_parts(p, n)));
                let _ = GlobalUnlock(g);
            }
        }
        let _ = CloseClipboard();
        out.map(|s| s.replace(['\r', '\n'], ""))
    }
}
