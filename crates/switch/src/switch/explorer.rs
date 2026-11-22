//! Observe and close only Explorer processes on this desktop and in this Windows session.

use super::reopen_list::{self, Folder};
use guard::{explorer_launch::Apartment, explorer_path, log, taskbar};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, ERROR_NO_MORE_FILES, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{
    CLSCTX_LOCAL_SERVER, CoCreateInstance, CoTaskMemFree, IServiceProvider,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Shell::{
    IFolderView, IPersistFolder2, IShellBrowser, IShellItem, IShellWindows, IWebBrowser2,
    SHCreateItemFromIDList, SID_STopLevelBrowser, SIGDN_DESKTOPABSOLUTEPARSING, ShellWindows,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GetWindowThreadProcessId, PostMessageW,
};
use windows::core::{Interface, PCWSTR, w};

const EXIT_EXPLORER: u32 = 0x5B4;
pub const EXIT_BUDGET: Duration = Duration::from_secs(30);

#[derive(Clone, Copy)]
pub struct Process {
    pub pid: u32,
    pub parent: u32,
}

struct Snapshot(HANDLE);

impl Drop for Snapshot {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

pub fn processes(image: &str) -> Result<Vec<Process>, String> {
    let mut session = 0;
    // SAFETY: the snapshot is owned, and outputs are live for each call.
    unsafe {
        ProcessIdToSessionId(std::process::id(), &mut session)
            .map_err(|e| format!("read the Windows session: {e}"))?;
        let snapshot = Snapshot(
            CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
                .map_err(|e| format!("list processes: {e}"))?,
        );
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        Process32FirstW(snapshot.0, &mut entry).map_err(|e| format!("read first process: {e}"))?;
        let mut out = Vec::new();
        loop {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            if String::from_utf16_lossy(&entry.szExeFile[..len])
                .eq_ignore_ascii_case(image)
            {
                let mut process_session = 0;
                if ProcessIdToSessionId(entry.th32ProcessID, &mut process_session).is_ok()
                    && process_session == session
                {
                    out.push(Process {
                        pid: entry.th32ProcessID,
                        parent: entry.th32ParentProcessID,
                    });
                }
            }
            match Process32NextW(snapshot.0, &mut entry) {
                Ok(()) => {}
                Err(e) if e.code() == ERROR_NO_MORE_FILES.to_hresult() => return Ok(out),
                Err(e) => return Err(format!("read next process: {e}")),
            }
        }
    }
}

fn windows(class: PCWSTR) -> Vec<(HWND, u32)> {
    let mut entries = Vec::new();
    let mut after = None;
    // SAFETY: enumerate top-level windows on this thread's desktop.
    unsafe {
        while let Ok(hwnd) = FindWindowExW(None, after, class, None) {
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            entries.push((hwnd, pid));
            after = Some(hwnd);
        }
    }
    entries
}

pub fn file_operations() -> bool {
    !windows(w!("OperationStatusWindow")).is_empty()
}

pub fn folder_windows() -> Vec<(isize, u32)> {
    windows(w!("CabinetWClass"))
        .into_iter()
        .filter(|(_, pid)| taskbar::is_explorer(*pid))
        .map(|(window, pid)| (window.0 as isize, pid))
        .collect()
}

/// Returns None if windows came or went while COM was reading their folders.
pub fn open_folders() -> Result<Option<Vec<Folder>>, String> {
    let open = folder_windows();
    if open.is_empty() {
        return Ok(Some(Vec::new()));
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
        if !reopen_list::same_windows(&entries, open.into_iter()) {
            return Err("one or more Explorer windows could not be saved".into());
        }
        Ok(Some(reopen_list::folders(entries)))
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
pub fn exit_desktop(
    tray: HWND,
    pid: u32,
    arm: impl FnOnce() -> Result<(), String>,
    posted: impl FnOnce() -> Result<(), String>,
) -> Result<bool, String> {
    if taskbar::explorer_window() != Some((tray, pid)) {
        return Err("Explorer's taskbar changed while preparing to switch".into());
    }
    let desktop_owners: Vec<_> = windows(w!("Progman"))
        .into_iter()
        .map(|(_, owner)| owner)
        .filter(|&owner| taskbar::is_explorer(owner))
        .chain([pid])
        .collect();
    arm()?;
    // Keep this check beside PostMessage. Separate folders can take seconds to close.
    if file_operations() {
        log::line("switch: a file operation started before Exit Explorer");
        return Ok(false);
    }
    let started = Instant::now();
    // SAFETY: this is the taskbar's Exit Explorer command, with no pointer arguments.
    unsafe { PostMessageW(Some(tray), EXIT_EXPLORER, WPARAM(0), LPARAM(0)) }
        .map_err(|e| format!("request Exit Explorer: {e}"))?;
    posted()?;
    log::line(&format!("switch: requested Exit Explorer for pid {pid}"));
    loop {
        if let Some((_, owner)) = taskbar::explorer_window()
            && owner != pid
        {
            return Err(format!("Windows restarted Explorer as pid {owner}"));
        }
        let running = processes("explorer.exe")?;
        let pids: Vec<_> = running.iter().map(|process| process.pid).collect();
        let desktop_present = pids.iter().any(|owner| desktop_owners.contains(owner));
        let new_desktop = windows(w!("Progman"))
            .into_iter()
            .any(|(_, owner)| taskbar::is_explorer(owner));
        let desktop_exited = !desktop_present && !new_desktop;
        // Wait until every original Explorer folder window is gone before handing over to Files.
        if desktop_exited && folder_windows().is_empty() {
            log::line(&format!(
                "switch: Explorer desktop exited after {} ms (background Explorer pids: {pids:?})",
                started.elapsed().as_millis()
            ));
            return Ok(true);
        }
        if started.elapsed() >= EXIT_BUDGET {
            if desktop_exited {
                return Err("Explorer folder windows did not close within 30 s".into());
            }
            return Err(format!(
                "Explorer's desktop did not exit within 30 s (remaining pids {pids:?})"
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
