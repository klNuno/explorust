//! Reads an HMENU a context menu handler filled: text, verb, state, bitmap and submenus.

use crate::model::{Entry, Kind, parse_label};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, GetDIBits, GetObjectW, HBITMAP,
};
use windows::Win32::UI::Shell::{IContextMenu, IContextMenu2, IContextMenu3};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{Interface, PSTR, PWSTR};

const GCS_VERBW: u32 = 4;

/// The canonical verb of command `offset`, empty when the handler has none.
pub fn verb(cm: &IContextMenu, offset: u32) -> String {
    let mut buf = [0u16; 128];
    // SAFETY: the buffer outlives the call and its length is given in characters.
    let ok = unsafe { cm.GetCommandString(offset as usize, GCS_VERBW, None, PSTR(buf.as_mut_ptr().cast()), buf.len() as u32) };
    if ok.is_err() {
        return String::new();
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
    String::from_utf16_lossy(&buf[..n])
}

/// Asks the handler to fill a submenu (Open with, New, Send to) before it is read.
pub fn init_popup(cm: &IContextMenu, sub: HMENU, index: u32) {
    let (wp, lp) = (WPARAM(sub.0 as usize), LPARAM(index as isize));
    // SAFETY: the handler gets the message the menu manager would send.
    unsafe {
        if let Ok(c3) = cm.cast::<IContextMenu3>() {
            let mut r = windows::Win32::Foundation::LRESULT(0);
            let _ = c3.HandleMenuMsg2(WM_INITMENUPOPUP, wp, lp, Some(&mut r));
        } else if let Ok(c2) = cm.cast::<IContextMenu2>() {
            let _ = c2.HandleMenuMsg(WM_INITMENUPOPUP, wp, lp);
        }
    }
}

/// The entries of `menu`; command ids start at `first`.
pub fn entries(cm: &IContextMenu, menu: HMENU, first: u32) -> Vec<Entry> {
    // SAFETY: reads a menu this thread owns, with buffers that outlive each call.
    unsafe {
        let n = GetMenuItemCount(Some(menu)).max(0) as u32;
        let default = GetMenuDefaultItem(menu, 0, GMDI_USEDISABLED);
        let mut out = Vec::with_capacity(n as usize);
        for i in 0..n {
            let mut text = [0u16; 512];
            let mut mii = MENUITEMINFOW {
                cbSize: size_of::<MENUITEMINFOW>() as u32,
                fMask: MIIM_FTYPE | MIIM_STATE | MIIM_ID | MIIM_SUBMENU | MIIM_STRING | MIIM_BITMAP,
                dwTypeData: PWSTR(text.as_mut_ptr()),
                cch: text.len() as u32 - 1,
                ..Default::default()
            };
            if GetMenuItemInfoW(menu, i, true, &mut mii).is_err() {
                continue;
            }
            if mii.fType.0 & MFT_SEPARATOR.0 != 0 {
                out.push(Entry { kind: Kind::Separator, ..Default::default() });
                continue;
            }
            let owner_draw = mii.fType.0 & MFT_OWNERDRAW.0 != 0;
            let raw = if owner_draw { String::new() } else { String::from_utf16_lossy(&text[..mii.cch as usize]) };
            let (label, mnemonic, shortcut) = parse_label(&raw);
            let sub = !mii.hSubMenu.is_invalid();
            let id = if sub { 0 } else { mii.wID };
            let verb = if !sub && id >= first { verb(cm, id - first) } else { String::new() };
            let bitmap = bitmap(mii.hbmpItem);
            out.push(Entry {
                id,
                label,
                mnemonic,
                shortcut,
                verb,
                kind: if sub { Kind::Submenu } else { Kind::Command },
                enabled: mii.fState.0 & (MFS_DISABLED.0 | MFS_GRAYED.0) == 0,
                checked: mii.fState.0 & MFS_CHECKED.0 != 0,
                radio: mii.fType.0 & MFT_RADIOCHECK.0 != 0,
                default: !sub && default != u32::MAX && default == mii.wID,
                submenu: mii.hSubMenu.0 as isize,
                bitmap,
                owner_draw,
            });
        }
        out
    }
}

/// A menu item bitmap as premultiplied BGRA; `None` for the callback and system values.
fn bitmap(h: HBITMAP) -> Option<(u32, u32, Vec<u8>)> {
    let v = h.0 as isize;
    if (-1..=11).contains(&v) {
        return None;
    }
    // SAFETY: GDI reads of a bitmap the handler owns, into buffers sized from its header.
    unsafe {
        let mut bm = BITMAP::default();
        if GetObjectW(h.into(), size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as _)) == 0 {
            return None;
        }
        let (w, hgt) = (bm.bmWidth.max(0) as u32, bm.bmHeight.unsigned_abs());
        if w == 0 || hgt == 0 || w > 256 || hgt > 256 {
            return None;
        }
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(hgt as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut px = vec![0u8; (w * hgt * 4) as usize];
        let dc = CreateCompatibleDC(None);
        let lines = GetDIBits(dc, h, 0, hgt, Some(px.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS);
        let _ = DeleteDC(dc);
        if lines == 0 {
            return None;
        }
        if bm.bmBitsPixel < 32 || px.chunks(4).all(|p| p[3] == 0) {
            // No alpha channel: opaque.
            for p in px.chunks_mut(4) {
                p[3] = 255;
            }
        }
        Some((w, hgt, px))
    }
}
