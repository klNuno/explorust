//! Windows' own desktop view: the desktop IShellFolder's IShellView in Progman, with the desktop
//! folder settings. Icons, the context menu, rename, drag and drop, change notifications and the
//! saved icon layout all come from it.

use crate::browser::Browser;
use crate::layout::{self, Rect};
use crate::{log, reg, screen};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::UI::Controls::{LVM_GETWORKAREAS, LVM_SETWORKAREAS};
use windows::Win32::UI::Shell::{
    FOLDERSETTINGS, FOLDERVIEWMODE, FVM_ICON, IFolderView2, IShellBrowser, IShellView, SHGetDesktopFolder,
};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, SendMessageW};
use windows_core::{ComObject, Interface, w};

const SVUIA_DEACTIVATE: u32 = 0;
const SVUIA_ACTIVATE_NOFOCUS: u32 = 1;
const BAG: &str = r"Software\Microsoft\Windows\Shell\Bags\1\Desktop";

pub struct Desk {
    pub view: IShellView,
    pub browser: ComObject<Browser>,
    /// SHELLDLL_DefView.
    pub hwnd: HWND,
}

pub fn create(progman: HWND, client: Rect) -> windows_core::Result<Desk> {
    let hide = reg::dword(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced", "HideIcons") == Some(1);
    let flags = layout::view_flags(reg::dword(BAG, "FFlags"), hide);
    let fs = FOLDERSETTINGS { ViewMode: FVM_ICON.0 as u32, fFlags: flags };
    let rc = RECT { left: client.x, top: client.y, right: client.right(), bottom: client.bottom() };
    // SAFETY: shell calls on this STA thread; the browser outlives the view window.
    unsafe {
        let folder = SHGetDesktopFolder()?;
        let view: IShellView = folder.CreateViewObject(progman)?;
        let browser = ComObject::new(Browser::new(progman));
        *browser.view.borrow_mut() = Some(view.clone());
        let sb: IShellBrowser = browser.to_interface();
        let hwnd = view.CreateViewWindow(None, &fs, &sb, &rc)?;
        let _ = view.UIActivate(SVUIA_ACTIVATE_NOFOCUS);
        // Right-click, the menu key and Shift+F10 show the Windows 11 menu (crates/menu).
        menu::hook(&view);
        log::line(&format!("view {:?} flags {flags:#x}", hwnd.0));
        if let Ok(fv) = view.cast::<IFolderView2>() {
            let mut mode = FOLDERVIEWMODE(0);
            let mut size = 0;
            let _ = fv.GetViewModeAndIconSize(&mut mode, &mut size);
            let want = reg::dword(BAG, "IconSize").map(|s| s as i32).unwrap_or(48);
            log::line(&format!("view mode {} icon size {size}, saved {want}", mode.0));
            if mode != FVM_ICON || size != want {
                let _ = fv.SetViewModeAndIconSize(FVM_ICON, want);
            }
        }
        Ok(Desk { view, browser, hwnd })
    }
}

impl Desk {
    pub fn destroy(self) {
        // SAFETY: tears down the view window this thread created, then breaks the cycle.
        unsafe {
            let _ = self.view.UIActivate(SVUIA_DEACTIVATE);
            let _ = self.view.DestroyViewWindow();
        }
        self.browser.view.borrow_mut().take();
    }
}

/// Keeps icons inside each monitor's work area (what the taskbar and AppBars leave free).
pub fn apply_work_areas(defview: HWND, origin: (i32, i32)) {
    // SAFETY: window lookups and list-view messages with buffers that outlive the calls.
    unsafe {
        let Ok(list) = FindWindowExW(Some(defview), None, w!("SysListView32"), None) else {
            log::line("work areas: no list view");
            return;
        };
        let mut before = [RECT::default(); 4];
        SendMessageW(list, LVM_GETWORKAREAS, Some(WPARAM(before.len())), Some(LPARAM(before.as_mut_ptr() as isize)));
        let works = layout::work_areas(screen::monitors().into_iter().map(|m| (m.work, m.primary)).collect(), origin);
        let rects: Vec<RECT> =
            works.iter().map(|r| RECT { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() }).collect();
        SendMessageW(list, LVM_SETWORKAREAS, Some(WPARAM(rects.len())), Some(LPARAM(rects.as_ptr() as isize)));
        log::line(&format!("work areas {:?} (view had {:?})", works, screen::rect(&before[0])));
    }
}
