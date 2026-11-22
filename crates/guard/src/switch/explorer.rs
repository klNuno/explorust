//! Observe and close only Explorer processes on this desktop and in this Windows session.

use super::reopen_list;
use crate::{log, taskbar};
use std::path::Path;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, ERROR_NO_MORE_FILES, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{
    CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize, IServiceProvider,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Shell::{
    CSIDL_DESKTOP, IFolderView, IPersistFolder2, IShellBrowser, IShellDispatch2,
    IShellFolderViewDual, IShellItem, IShellWindows, IWebBrowser2, SHCreateItemFromIDList,
    SID_STopLevelBrowser, SIGDN_DESKTOPABSOLUTEPARSING, SVGIO_BACKGROUND, SWC_DESKTOP,
    SWFO_NEEDDISPATCH, ShellWindows,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
};
use windows::core::{BSTR, Interface, PCWSTR, w};

const EXIT_EXPLORER: u32 = 0x5B4;

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

pub fn processes() -> Result<Vec<Process>, String> {
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
                .eq_ignore_ascii_case("explorer.exe")
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

fn folder_windows() -> Vec<(HWND, u32)> {
    windows(w!("CabinetWClass"))
        .into_iter()
        .filter(|(_, pid)| taskbar::is_explorer(*pid))
        .collect()
}

struct Apartment;

impl Apartment {
    fn new() -> windows::core::Result<Self> {
        // SAFETY: this guard balances CoInitializeEx once its thread is finished with COM.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok() }?;
        Ok(Self)
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: paired with this thread's successful CoInitializeEx.
        unsafe { CoUninitialize() };
    }
}

/// Ask the desktop Explorer to launch the installed switch in its own user context.
pub fn launch_switch(guard: &Path) -> Result<(), String> {
    if taskbar::explorer_window().is_none() {
        return Err("no Explorer taskbar in this session".into());
    }
    let path = BSTR::from(guard.to_string_lossy().as_ref());
    let Some(dir) = guard.parent() else {
        return Err("the installed guard has no working directory".into());
    };
    let result = (|| {
        let _apartment = Apartment::new()?;
        // SAFETY: all automation methods run on this COM apartment, with live arguments.
        unsafe {
            let list: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER)?;
            let mut hwnd = 0;
            let desktop = list.FindWindowSW(
                &VARIANT::from(CSIDL_DESKTOP as i32),
                &VARIANT::default(),
                SWC_DESKTOP,
                &mut hwnd,
                SWFO_NEEDDISPATCH,
            )?;
            let browser: IShellBrowser = desktop
                .cast::<IServiceProvider>()?
                .QueryService(&SID_STopLevelBrowser)?;
            let folder: IShellFolderViewDual = browser
                .QueryActiveShellView()?
                .GetItemObject(SVGIO_BACKGROUND)?;
            let app: IShellDispatch2 = folder.Application()?.cast()?;
            app.ShellExecute(
                &path,
                &VARIANT::from(BSTR::from("--switch")),
                &VARIANT::from(BSTR::from(dir.to_string_lossy().as_ref())),
                &VARIANT::from(BSTR::from("open")),
                &VARIANT::from(1),
            )
        }
    })();
    result.map_err(|e| format!("launch the switch through Explorer: {e}"))
}

pub fn open_folders() -> Result<Vec<String>, String> {
    let open = folder_windows();
    if open.is_empty() {
        return Ok(Vec::new());
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
            if !open.iter().any(|(h, _)| h.0 as isize == hwnd.0) {
                continue;
            }
            match active_folder(&browser) {
                Ok(folder) if !folder.is_empty() && !folder.contains('\0') => {
                    entries.push((hwnd.0, folder))
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
        if folder_windows()
            .iter()
            .any(|(h, _)| !entries.iter().any(|(hwnd, _)| h.0 as isize == *hwnd))
        {
            return Err("one or more Explorer windows could not be saved".into());
        }
        Ok(reopen_list::folders(entries))
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

pub fn exit(tray: HWND, pid: u32) -> Result<(), String> {
    if taskbar::explorer_window() != Some((tray, pid)) {
        return Err("Explorer's taskbar changed while preparing to switch".into());
    }
    // SAFETY: this is the taskbar's Exit Explorer command, with no pointer arguments.
    unsafe { PostMessageW(Some(tray), EXIT_EXPLORER, WPARAM(0), LPARAM(0)) }
        .map_err(|e| format!("request Exit Explorer: {e}"))?;
    log::line(&format!("switch: requested Exit Explorer for pid {pid}"));
    for (folder, owner) in folder_windows() {
        if owner != pid {
            // SAFETY: standard close to a folder window in a separate Explorer process.
            if let Err(e) = unsafe { PostMessageW(Some(folder), WM_CLOSE, WPARAM(0), LPARAM(0)) } {
                log::line(&format!(
                    "switch: could not close the folder window in Explorer pid {owner}: {e}"
                ));
            }
        }
    }
    let started = Instant::now();
    loop {
        let running = processes()?;
        if running.is_empty() {
            log::line(&format!(
                "switch: all Explorer processes in this session exited after {} ms",
                started.elapsed().as_millis()
            ));
            return Ok(());
        }
        if let Some((_, owner)) = taskbar::explorer_window()
            && owner != pid
        {
            return Err(format!("Windows restarted Explorer as pid {owner}"));
        }
        if started.elapsed() >= Duration::from_secs(30) {
            let pids: Vec<_> = running.iter().map(|process| process.pid).collect();
            return Err(format!(
                "Explorer did not exit within 30 s (remaining pids {pids:?})"
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
