//! Shell namespace helpers: ID lists as owned bytes, names, parents, icons, the verbs the command
//! bar invokes, and the dark theme of the hosted view (uxtheme's private dark mode entry points,
//! the ones Explorer itself uses).

use std::ffi::c_void;
use windows::Win32::Foundation::{HWND, LPARAM, POINT};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Controls::SetWindowTheme;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, DestroyMenu, EnumChildWindows, GetClassNameW, SW_SHOWNORMAL, SendMessageW, WM_THEMECHANGED,
};
use windows_core::{BOOL, PCSTR, PCWSTR, PWSTR, Result, w};

/// An ID list held as its bytes, terminator included: comparable, clonable, no COM memory.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Pidl(pub Vec<u8>);

impl Pidl {
    /// Copies and frees a COM-allocated ID list.
    pub fn take(p: *mut ITEMIDLIST) -> Option<Pidl> {
        if p.is_null() {
            return None;
        }
        // SAFETY: `p` is a valid ID list; ILGetSize counts its terminator.
        unsafe {
            let n = ILGetSize(Some(p)) as usize;
            let bytes = std::slice::from_raw_parts(p as *const u8, n).to_vec();
            CoTaskMemFree(Some(p as *const c_void));
            Some(Pidl(bytes))
        }
    }

    /// Copies an ID list the caller keeps.
    pub fn copy(p: *const ITEMIDLIST) -> Option<Pidl> {
        if p.is_null() {
            return None;
        }
        // SAFETY: as above, without freeing.
        unsafe {
            let n = ILGetSize(Some(p)) as usize;
            Some(Pidl(std::slice::from_raw_parts(p as *const u8, n).to_vec()))
        }
    }

    pub fn as_ptr(&self) -> *const ITEMIDLIST {
        self.0.as_ptr() as *const ITEMIDLIST
    }

    pub fn item(&self) -> Result<IShellItem> {
        unsafe { SHCreateItemFromIDList(self.as_ptr()) }
    }

    pub fn of(item: &IShellItem) -> Option<Pidl> {
        unsafe { SHGetIDListFromObject(item).ok().and_then(Pidl::take) }
    }

    /// The parent folder's ID list, `None` for the desktop root.
    pub fn parent(&self) -> Option<Pidl> {
        let mut b = self.0.clone();
        // SAFETY: `b` is a copy of a valid ID list; ILRemoveLastID only shortens it in place.
        unsafe {
            if !ILRemoveLastID(Some(b.as_mut_ptr() as *mut ITEMIDLIST)).as_bool() {
                return None;
            }
            Pidl::copy(b.as_ptr() as *const ITEMIDLIST)
        }
    }

    /// The last item ID (relative to the parent), pointing into `self`.
    pub fn last(&self) -> *const ITEMIDLIST {
        unsafe { ILFindLastID(self.as_ptr()) }
    }

    /// Whether the item is a folder (ZIP folders included).
    pub fn is_folder(&self) -> bool {
        use windows::Win32::System::SystemServices::SFGAO_FOLDER;
        self.item()
            .and_then(|i| unsafe { i.GetAttributes(SFGAO_FOLDER) })
            .map(|a| a.0 & SFGAO_FOLDER.0 != 0)
            .unwrap_or(false)
    }

    /// Its parsing name, for logs.
    pub fn display(&self) -> String {
        self.item().map(|i| name(&i, SIGDN_DESKTOPABSOLUTEPARSING)).unwrap_or_default()
    }

    /// Parses a path, a `shell:` name or a `search-ms:` URI.
    pub fn parse(name: &str) -> Option<Pidl> {
        let w = shelldata::wide(name);
        let mut p = std::ptr::null_mut();
        unsafe { SHParseDisplayName(PCWSTR(w.as_ptr()), None, &mut p, 0, None).ok()? };
        Pidl::take(p)
    }
}

fn take_str(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: a COM-allocated string from the shell.
    unsafe {
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const c_void));
        s
    }
}

pub fn name(item: &IShellItem, sigdn: SIGDN) -> String {
    unsafe { item.GetDisplayName(sigdn).map(take_str).unwrap_or_default() }
}

/// The file system path of a folder, `None` for virtual folders.
pub fn fs_path(item: &IShellItem) -> Option<String> {
    let s = name(item, SIGDN_FILESYSPATH);
    (!s.is_empty()).then_some(s)
}

/// The breadcrumb of `item`: its ancestors below the desktop, then itself, with display names.
pub fn crumbs(item: &IShellItem) -> Vec<(String, Pidl)> {
    let mut out = Vec::new();
    let mut cur = Some(item.clone());
    while let Some(it) = cur {
        let parent = unsafe { it.GetParent().ok() };
        if parent.is_none() && !out.is_empty() {
            // `it` is the desktop root: Explorer does not show it.
            break;
        }
        if let Some(p) = Pidl::of(&it) {
            out.push((name(&it, SIGDN_NORMALDISPLAY), p));
        }
        cur = parent;
    }
    out.reverse();
    out
}

/// The Home folder of Windows 11, else This PC.
pub fn home() -> Option<Pidl> {
    Pidl::parse("shell:::{f874310e-b6b7-47dc-bc84-b9e6b38f5903}").or_else(|| Pidl::parse("shell:MyComputerFolder"))
}

const CMIC_MASK_UNICODE: u32 = 0x4000;

/// Invokes a canonical verb ("cut", "copy", "delete", "paste", "NewFolder", ...) on a context menu.
pub fn invoke(menu: &IContextMenu, hwnd: HWND, verb: &str) -> Result<()> {
    let a: Vec<u8> = verb.bytes().chain(std::iter::once(0)).collect();
    let wv = shelldata::wide(verb);
    unsafe {
        let hmenu = CreatePopupMenu()?;
        let _ = menu.QueryContextMenu(hmenu, 0, 1, 0x7fff, CMF_NORMAL);
        let info = CMINVOKECOMMANDINFOEX {
            cbSize: size_of::<CMINVOKECOMMANDINFOEX>() as u32,
            fMask: CMIC_MASK_UNICODE,
            hwnd,
            lpVerb: PCSTR(a.as_ptr()),
            lpVerbW: PCWSTR(wv.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ptInvoke: POINT::default(),
            ..Default::default()
        };
        let r = menu.InvokeCommand(&info as *const _ as *const CMINVOKECOMMANDINFO);
        let _ = DestroyMenu(hmenu);
        r
    }
}

/// The context menu of the selected items of `view`, `None` when nothing is selected.
pub fn selection_menu(view: &IFolderView2) -> Option<IContextMenu> {
    unsafe {
        let items = view.GetSelection(false).ok()?;
        if items.GetCount().ok()? == 0 {
            return None;
        }
        items.BindToHandler(None, &BHID_SFUIObject).ok()
    }
}

pub fn has_selection(view: &IFolderView2) -> bool {
    unsafe { view.GetSelection(false).ok().and_then(|a| a.GetCount().ok()).unwrap_or(0) > 0 }
}

/// The folder background's context menu (paste, new folder).
pub fn background_menu(view: &IShellView) -> Option<IContextMenu> {
    unsafe { view.GetItemObject(SVGIO_BACKGROUND).ok() }
}

/// uxtheme ordinals: 133 AllowDarkModeForWindow, 135 SetPreferredAppMode, 136 FlushMenuThemes.
fn uxtheme(ordinal: usize) -> Option<unsafe extern "system" fn() -> isize> {
    unsafe {
        let m = LoadLibraryW(w!("uxtheme.dll")).ok()?;
        GetProcAddress(m, PCSTR(ordinal as *const u8))
    }
}

/// Dark menus and dark common controls for the whole process (`SetPreferredAppMode(ForceDark)`).
pub fn dark_process() {
    unsafe {
        if let Some(f) = uxtheme(135) {
            let set: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(f);
            set(2);
        }
        if let Some(f) = uxtheme(136) {
            let flush: unsafe extern "system" fn() = std::mem::transmute(f);
            flush();
        }
    }
}

pub fn allow_dark(hwnd: HWND) {
    unsafe {
        if let Some(f) = uxtheme(133) {
            let allow: unsafe extern "system" fn(HWND, BOOL) -> BOOL = std::mem::transmute(f);
            let _ = allow(hwnd, true.into());
        }
    }
}

unsafe extern "system" fn dark_child(h: HWND, _: LPARAM) -> BOOL {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(h, &mut buf) } as usize;
    let class = String::from_utf16_lossy(&buf[..n]);
    let theme = match class.as_str() {
        "DirectUIHWND" => Some(w!("DarkMode_ItemsView")),
        "SysListView32" => Some(w!("DarkMode_ItemsView")),
        "SysTreeView32" | "SysHeader32" | "ScrollBar" | "NamespaceTreeControl" => Some(w!("DarkMode_Explorer")),
        _ => None,
    };
    allow_dark(h);
    if let Some(t) = theme {
        unsafe {
            let _ = SetWindowTheme(h, t, PCWSTR::null());
        }
    }
    unsafe {
        if class == "SysTreeView32" {
            // TVM_SETBKCOLOR, TVM_SETTEXTCOLOR: the dark navigation pane of Explorer.
            SendMessageW(h, 0x111D, None, Some(LPARAM(0x0019_1919)));
            SendMessageW(h, 0x111E, None, Some(LPARAM(0x00FF_FFFF)));
        }
        SendMessageW(h, WM_THEMECHANGED, None, None)
    };
    true.into()
}

/// Dark theme on every window the browser created under `root`.
pub fn dark_children(root: HWND) {
    unsafe {
        let _ = EnumChildWindows(Some(root), Some(dark_child), LPARAM(0));
    }
}

/// The icon of `item` as premultiplied BGRA at `px`.
pub fn icon(item: &IShellItem, px: i32) -> Option<(u32, u32, Vec<u8>)> {
    let img = shelldata::icons::icon_for_item(item, px)?;
    Some((img.width, img.height, img.to_premultiplied_bgra()))
}

