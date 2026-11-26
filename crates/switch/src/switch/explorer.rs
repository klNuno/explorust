//! Observe and close only Explorer processes on this desktop and in this Windows session.

use super::reopen_list::{self, Folder, FolderWindow};
use guard::{explorer_launch::Apartment, explorer_path, log, taskbar};
use std::collections::HashSet;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{
    CLSCTX_LOCAL_SERVER, CoCreateInstance, CoTaskMemFree, IServiceProvider,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Shell::{
    IFolderView, IPersistFolder2, IShellBrowser, IShellItem, IShellWindows, IWebBrowser2,
    SHCreateItemFromIDList, SID_STopLevelBrowser, SIGDN_DESKTOPABSOLUTEPARSING, ShellWindows,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GetWindowThreadProcessId, IsWindowVisible, PostMessageW,
};
use windows::core::{Interface, PCWSTR, w};

const EXIT_EXPLORER: u32 = 0x5B4;
pub const CLOSE_BUDGET: Duration = Duration::from_secs(30);

/// Top-level windows of `class` with their owning process and visibility.
fn windows(class: PCWSTR) -> Vec<(HWND, u32, bool)> {
    let mut entries = Vec::new();
    let mut after = None;
    // SAFETY: enumerate top-level windows on this thread's desktop.
    unsafe {
        while let Ok(hwnd) = FindWindowExW(None, after, class, None) {
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            entries.push((hwnd, pid, IsWindowVisible(hwnd).as_bool()));
            after = Some(hwnd);
        }
    }
    entries
}

pub use guard::taskbar::explorer_file_operations as file_operations;

fn explorer_folder_windows() -> Vec<FolderWindow> {
    windows(w!("CabinetWClass"))
        .into_iter()
        .filter(|(_, pid, _)| taskbar::is_explorer(*pid))
        .map(|(window, owner, visible)| FolderWindow { hwnd: window.0 as isize, owner, visible })
        .collect()
}

/// Every Explorer folder window, hidden ones included.
pub fn folder_windows() -> Vec<(isize, u32)> {
    explorer_folder_windows().into_iter().map(|window| (window.hwnd, window.owner)).collect()
}

/// Hidden preloaded folder windows excused at capture.
pub type Preloaded = HashSet<(isize, u32)>;

/// The folder windows a snapshot must cover.
pub fn user_windows(preloaded: &Preloaded) -> Vec<(isize, u32)> {
    reopen_list::user_windows(explorer_folder_windows(), preloaded)
}

/// Returns None if windows came or went while COM was reading their folders.
pub fn open_folders() -> Result<Option<(Vec<Folder>, Preloaded)>, String> {
    let windows = explorer_folder_windows();
    let open: Vec<_> = windows.iter().map(|window| (window.hwnd, window.owner)).collect();
    if open.is_empty() {
        return Ok(Some((Vec::new(), Preloaded::new())));
    }
    let _apartment = Apartment::new().map_err(|e| format!("initialize ShellWindows COM: {e}"))?;
    // SAFETY: the worker owns the apartment and drops all interfaces before uninitializing it.
    unsafe {
        let list: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER)
            .map_err(|e| format!("get ShellWindows: {e}"))?;
        let count = list
            .Count()
            .map_err(|e| format!("count ShellWindows: {e}"))?;
        let mut entries = Vec::new();
        let mut listed = HashSet::new();
        for i in 0..count {
            let Ok(browser) = list
                .Item(&VARIANT::from(i))
                .and_then(|item| item.cast::<IWebBrowser2>())
            else {
                continue;
            };
            let Ok(hwnd) = browser.HWND() else { continue };
            let Some(&(_, owner)) = open.iter().find(|(h, _)| *h == hwnd.0) else {
                continue;
            };
            listed.insert((hwnd.0, owner));
            match active_folder(&browser) {
                Ok(folder) if !folder.is_empty() && !folder.contains('\0') => {
                    entries.push(Folder { hwnd: hwnd.0, owner, path: folder })
                }
                Ok(_) => log::line(&format!(
                    "switch: folder window {:#x} has no parsing name",
                    hwnd.0
                )),
                Err(e) => log::line(&format!(
                    "switch: cannot read folder window {:#x}: {e}",
                    hwnd.0
                )),
            }
        }
        let current = folder_windows();
        if current.len() != open.len() || current.iter().any(|window| !open.contains(window)) {
            return Ok(None);
        }
        let preloaded = reopen_list::preloaded(&windows, &listed);
        if !reopen_list::same_windows(&entries, reopen_list::user_windows(windows, &preloaded).into_iter()) {
            return Err("one or more Explorer windows could not be saved".into());
        }
        Ok(Some((reopen_list::folders(entries), preloaded)))
    }
}

pub fn restore_desktop_folders(folders: &[Folder], desktop: u32) {
    let existing = folder_windows();
    for folder in folders.iter().filter(|folder| folder.owner == desktop && !existing.contains(&(folder.hwnd, folder.owner))) {
        reopen_folder(&folder.path);
    }
}

pub fn reopen_folder(folder: &str) {
    match std::process::Command::new(explorer_path()).arg(folder).spawn() {
        Ok(_) => log::line(&format!("switch: reopened Explorer folder {folder}")),
        Err(e) => log::line(&format!("switch: could not reopen Explorer folder {folder}: {e}")),
    }
}

unsafe fn active_folder(browser: &IWebBrowser2) -> windows::core::Result<String> {
    // SAFETY: the COM interfaces are live in this apartment; shell-allocated outputs are freed.
    unsafe {
        let shell: IShellBrowser = browser
            .cast::<IServiceProvider>()?
            .QueryService(&SID_STopLevelBrowser)?;
        let folder = shell
            .QueryActiveShellView()?
            .cast::<IFolderView>()?
            .GetFolder::<IPersistFolder2>()?;
        let pidl = folder.GetCurFolder()?;
        let item: windows::core::Result<IShellItem> = SHCreateItemFromIDList(pidl);
        CoTaskMemFree(Some(pidl.cast()));
        let name = item?.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING)?;
        let path = name.to_string();
        CoTaskMemFree(Some(name.0.cast()));
        path.map_err(Into::into)
    }
}

/// Returns false when the desktop exit has to wait for a new file operation.
pub fn post_exit(
    tray: HWND,
    pid: u32,
    arm: impl FnOnce() -> Result<(), String>,
) -> Result<bool, String> {
    if taskbar::explorer_window_for(pid) != Some(tray) {
        return Err("Explorer's taskbar changed while preparing to switch".into());
    }
    // Separate folders can take seconds to close. Check again at the posting boundary.
    if file_operations() {
        log::line("switch: a file operation started before Exit Explorer");
        return Ok(false);
    }
    arm()?;
    // SAFETY: this is the taskbar's Exit Explorer command, with no pointer arguments.
    unsafe { PostMessageW(Some(tray), EXIT_EXPLORER, WPARAM(0), LPARAM(0)) }
        .map_err(|e| format!("request Exit Explorer: {e}"))?;
    Ok(true)
}

pub fn wait_for_desktop_windows(preloaded: &Preloaded) -> Result<(), String> {
    let started = Instant::now();
    loop {
        if let Some((_, owner)) = taskbar::explorer_window() {
            return Err(format!("Windows restarted Explorer as pid {owner}"));
        }
        let desktop_window = windows(w!("Progman"))
            .into_iter()
            .any(|(_, owner, _)| taskbar::is_explorer(owner));
        // Wait until every original Explorer folder window is gone before handing over to Files.
        if !desktop_window && user_windows(preloaded).is_empty() {
            return Ok(());
        }
        if started.elapsed() >= CLOSE_BUDGET {
            return Err("Explorer windows stayed open after the original desktop process exited".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
