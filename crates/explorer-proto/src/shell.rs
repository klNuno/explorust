//! Folder data from the shell namespace: the items of a folder with Explorer's own display
//! strings (PSFormatForDisplay), the breadcrumb chain, and the navigation pane roots.

use std::time::Instant;
use windows::Win32::Foundation::{FILETIME, HWND, PROPERTYKEY};
use windows::Win32::System::SystemServices::{SFGAO_FOLDER, SFGAO_STREAM};
use windows::Win32::Storage::EnhancedStorage::{
    PKEY_DateModified, PKEY_ItemNameDisplay, PKEY_ItemTypeText, PKEY_Size,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::PropertiesSystem::{PDFF_DEFAULT, PSFormatForDisplayAlloc};
use windows::Win32::UI::Shell::*;
use windows_core::{GUID, Interface, PCWSTR, Result};

pub struct Entry {
    pub name: String,
    pub date: String,
    pub kind: String,
    pub size: String,
    pub modified: u64,
    pub folder: bool,
    /// Parsing name, to load the icon later.
    pub path: String,
}

pub struct Listing {
    pub title: String,
    pub crumbs: Vec<String>,
    pub entries: Vec<Entry>,
    pub list_ms: f64,
}

fn take(p: windows_core::PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    let s = unsafe { p.to_string().unwrap_or_default() };
    unsafe { CoTaskMemFree(Some(p.0 as _)) };
    s
}

fn display(item: &IShellItem2, key: &PROPERTYKEY) -> String {
    unsafe {
        let Ok(v) = item.GetProperty(key) else { return String::new() };
        PSFormatForDisplayAlloc(key, &v, PDFF_DEFAULT).map(take).unwrap_or_default()
    }
}

fn name_of(item: &IShellItem, sigdn: SIGDN) -> String {
    unsafe { item.GetDisplayName(sigdn).map(take).unwrap_or_default() }
}

/// A known folder as a shell item (Downloads, Desktop, ...).
pub fn known(id: &GUID) -> Result<IShellItem> {
    unsafe { SHGetKnownFolderItem(id, KF_FLAG_DEFAULT, None) }
}

pub fn parse(name: &str) -> Result<IShellItem> {
    let w = shelldata::wide(name);
    unsafe { SHCreateItemFromParsingName(PCWSTR(w.as_ptr()), None) }
}

/// Lists `folder` as Explorer's details view sees it: hidden items follow the user's
/// setting, sorted by date modified, newest first (his Downloads view).
pub fn list(folder: &IShellItem) -> Result<Listing> {
    let t0 = Instant::now();
    let mut entries = Vec::new();
    unsafe {
        let sf: IShellFolder = folder.BindToHandler(None, &BHID_SFObject)?;
        let show_hidden = hidden_shown();
        let mut flags = SHCONTF_FOLDERS.0 | SHCONTF_NONFOLDERS.0;
        if show_hidden {
            flags |= SHCONTF_INCLUDEHIDDEN.0;
        }
        let mut en: Option<IEnumIDList> = None;
        let _ = sf.EnumObjects(HWND::default(), flags as u32, &mut en);
        if let Some(en) = en {
            loop {
                let mut pidl = [std::ptr::null_mut::<ITEMIDLIST>(); 1];
                let mut got = 0;
                if en.Next(&mut pidl, Some(&mut got)).is_err() || got == 0 {
                    break;
                }
                let child = pidl[0];
                if let Ok(item) = SHCreateItemWithParent::<_, IShellItem2>(None, &sf, child) {
                    let ft: FILETIME = item.GetFileTime(&PKEY_DateModified).unwrap_or_default();
                    let base: IShellItem = item.cast()?;
                    let attrs = base.GetAttributes(SFGAO_FOLDER | SFGAO_STREAM).unwrap_or_default();
                    let folder = attrs.0 & SFGAO_FOLDER.0 != 0 && attrs.0 & SFGAO_STREAM.0 == 0;
                    entries.push(Entry {
                        name: display(&item, &PKEY_ItemNameDisplay),
                        date: display(&item, &PKEY_DateModified),
                        kind: display(&item, &PKEY_ItemTypeText),
                        size: if folder { String::new() } else { display(&item, &PKEY_Size) },
                        modified: (ft.dwHighDateTime as u64) << 32 | ft.dwLowDateTime as u64,
                        folder,
                        path: name_of(&base, SIGDN_DESKTOPABSOLUTEPARSING),
                    });
                }
                CoTaskMemFree(Some(child as _));
            }
        }
    }
    entries.sort_by(|a, b| b.modified.cmp(&a.modified));
    let list_ms = t0.elapsed().as_secs_f64() * 1000.0;
    Ok(Listing { title: name_of(folder, SIGDN_NORMALDISPLAY), crumbs: crumbs(folder), entries, list_ms })
}

/// Breadcrumb as Explorer shows a file system folder: This PC, then each path prefix
/// (drive, folders) by its display name. The shell parents of a known folder run through
/// the user's profile instead, so the chain is built from the path.
fn crumbs(folder: &IShellItem) -> Vec<String> {
    let path = name_of(folder, SIGDN_FILESYSPATH);
    if path.is_empty() {
        return vec![name_of(folder, SIGDN_NORMALDISPLAY)];
    }
    let mut out = Vec::new();
    if let Ok(pc) = parse("shell:::{20D04FE0-3AEA-1069-A2D8-08002B30309D}") {
        out.push(name_of(&pc, SIGDN_NORMALDISPLAY));
    }
    let mut prefix = String::new();
    for (i, part) in path.trim_end_matches('\\').split('\\').enumerate() {
        prefix.push_str(part);
        prefix.push('\\');
        let p = if i == 0 { prefix.clone() } else { prefix.trim_end_matches('\\').to_string() };
        out.push(parse(&p).map(|it| name_of(&it, SIGDN_NORMALDISPLAY)).unwrap_or_else(|_| part.to_string()));
    }
    out
}

/// Whether the user shows hidden files (Explorer's own setting, read through the shell).
fn hidden_shown() -> bool {
    unsafe {
        let mut s = SHELLSTATEA::default();
        SHGetSetSettings(Some(&mut s), SSF_SHOWALLOBJECTS, false);
        s._bitfield1 & 1 != 0
    }
}

pub struct NavItem {
    pub name: String,
    pub path: String,
    pub pinned: bool,
    pub expandable: bool,
}

/// The navigation pane as his Explorer shows it: Quick access pins, then This PC and
/// Network. The pins come from the Quick access folder's enumeration.
pub fn nav() -> Vec<NavItem> {
    let mut out = Vec::new();
    if let Ok(qa) = parse("shell:::{3936E9E4-D92C-4EEE-A85A-BC16D5EA0819}") {
        unsafe {
            if let Ok(en) = qa.BindToHandler::<_, IEnumShellItems>(None, &BHID_EnumItems) {
                loop {
                    let mut items = [None; 1];
                    let mut got = 0;
                    if en.Next(&mut items, Some(&mut got)).is_err() || got == 0 {
                        break;
                    }
                    if let Some(it) = items[0].take() {
                        out.push(NavItem {
                            name: name_of(&it, SIGDN_NORMALDISPLAY),
                            path: name_of(&it, SIGDN_DESKTOPABSOLUTEPARSING),
                            pinned: true,
                            expandable: false,
                        });
                    }
                }
            }
        }
    }
    for p in ["shell:::{20D04FE0-3AEA-1069-A2D8-08002B30309D}", "shell:::{F02C1A0D-BE21-4350-88B0-7367FC96EF3C}"] {
        if let Ok(it) = parse(p) {
            out.push(NavItem {
                name: name_of(&it, SIGDN_NORMALDISPLAY),
                path: name_of(&it, SIGDN_DESKTOPABSOLUTEPARSING),
                pinned: false,
                expandable: true,
            });
        }
    }
    out
}
