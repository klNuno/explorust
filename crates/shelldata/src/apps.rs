//! Taskbar buttons from real data: pinned shortcuts in the taskbar's own order, then
//! running apps that are not pinned, as Explorer groups them by executable.

use crate::{Image, icons::icon_for_path, wide};
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, MAX_PATH};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile, STGM_READ};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RegGetValueW};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GWL_EXSTYLE, GetForegroundWindow, GetWindow, GetWindowLongW, GetWindowTextLengthW,
    GetWindowThreadProcessId, IsWindowVisible, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};
use windows_core::{BOOL, Interface, PCWSTR};

pub struct TaskbarApp {
    pub name: String,
    /// Lowercase target or exe path.
    pub path: String,
    pub icon: Option<Image>,
    pub running: bool,
    pub active: bool,
}

/// Pinned apps (resolved targets that exist) then unpinned running ones. COM must be initialised.
pub fn taskbar_apps(icon_px: i32) -> Vec<TaskbarApp> {
    let windows = top_level_app_windows();
    let fg = unsafe { GetForegroundWindow() };
    let fg_exe = windows.iter().find(|w| w.0 == fg.0 as isize).map(|w| w.1.clone());
    let mut out = Vec::new();
    let mut pinned_exes = Vec::new();
    for lnk in pinned_shortcuts() {
        let Some(target) = shortcut_target(&lnk) else { continue };
        if !target.as_os_str().is_empty() && !target.exists() {
            continue; // Explorer hides pins whose target is gone.
        }
        let exe = target.to_string_lossy().to_lowercase();
        let running = windows.iter().any(|w| same_app(&exe, &w.1));
        out.push(TaskbarApp {
            name: lnk.file_stem().unwrap_or_default().to_string_lossy().into_owned(),
            path: exe.clone(),
            icon: icon_for_path(&lnk.to_string_lossy(), icon_px),
            running,
            active: fg_exe.as_deref().is_some_and(|f| same_app(&exe, f)),
        });
        pinned_exes.push(exe);
    }
    let mut seen: Vec<String> = Vec::new();
    for (_, exe) in &windows {
        if pinned_exes.iter().any(|p| same_app(p, exe)) || seen.contains(exe) || exe.ends_with("\\explorer.exe") {
            continue;
        }
        seen.push(exe.clone());
        out.push(TaskbarApp {
            name: Path::new(exe).file_stem().unwrap_or_default().to_string_lossy().into_owned(),
            path: exe.clone(),
            icon: icon_for_path(exe, icon_px),
            running: true,
            active: fg_exe.as_deref() == Some(exe.as_str()),
        });
    }
    out
}

/// Whether a running exe belongs to a pin: same file, or inside the pin target's folder
/// (launcher stubs such as Discord's, which start a versioned exe one level down).
fn same_app(pin_target: &str, exe: &str) -> bool {
    if pin_target == exe {
        return true;
    }
    let Some(dir) = Path::new(pin_target).parent() else { return false };
    let d = dir.to_string_lossy();
    d.len() > 3 && !d.starts_with("c:\\windows") && !d.starts_with("c:\\program files") && Path::new(exe).starts_with(dir)
}

/// Shortcut files of the taskbar pins, ordered as the Taskband blob lists them.
fn pinned_shortcuts() -> Vec<PathBuf> {
    let dir = PathBuf::from(std::env::var("APPDATA").unwrap_or_default())
        .join(r"Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|it| it.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "lnk")).collect())
        .unwrap_or_default();
    let blob = taskband_blob();
    files.sort_by_key(|p| {
        let name: Vec<u16> = p.file_name().unwrap_or_default().to_string_lossy().encode_utf16().collect();
        find_utf16(&blob, &name).unwrap_or(usize::MAX)
    });
    files
}

fn taskband_blob() -> Vec<u8> {
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Taskband");
    let val = wide("Favorites");
    unsafe {
        let mut len = 0u32;
        let k = PCWSTR(key.as_ptr());
        let v = PCWSTR(val.as_ptr());
        if RegGetValueW(HKEY_CURRENT_USER, k, v, RRF_RT_REG_BINARY, None, None, Some(&mut len)).is_err() {
            return Vec::new();
        }
        let mut buf = vec![0u8; len as usize];
        let _ = RegGetValueW(HKEY_CURRENT_USER, k, v, RRF_RT_REG_BINARY, None, Some(buf.as_mut_ptr().cast()), Some(&mut len));
        buf
    }
}

/// Byte offset of a UTF-16LE string at any alignment.
fn find_utf16(hay: &[u8], needle: &[u16]) -> Option<usize> {
    let bytes: Vec<u8> = needle.iter().flat_map(|c| c.to_le_bytes()).collect();
    hay.windows(bytes.len()).position(|w| w.eq_ignore_ascii_case(&bytes))
}

fn shortcut_target(lnk: &Path) -> Option<PathBuf> {
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let file: IPersistFile = link.cast().ok()?;
        let w = wide(&lnk.to_string_lossy());
        file.Load(PCWSTR(w.as_ptr()), STGM_READ).ok()?;
        let mut buf = [0u16; MAX_PATH as usize];
        let _ = link.GetPath(&mut buf, std::ptr::null_mut(), 0);
        let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
        Some(PathBuf::from(String::from_utf16_lossy(&buf[..n])))
    }
}

/// (hwnd, lowercase exe path) of windows that get a taskbar button.
fn top_level_app_windows() -> Vec<(isize, String)> {
    unsafe extern "system" fn cb(hwnd: HWND, lp: LPARAM) -> BOOL {
        unsafe {
            let out = &mut *(lp.0 as *mut Vec<(isize, String)>);
            if !IsWindowVisible(hwnd).as_bool() || GetWindowTextLengthW(hwnd) == 0 {
                return true.into();
            }
            let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
            let owned = GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid());
            let app = ex & WS_EX_APPWINDOW.0 != 0;
            if (ex & WS_EX_TOOLWINDOW.0 != 0 || owned) && !app {
                return true.into();
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if let Some(exe) = process_path(pid) {
                out.push((hwnd.0 as isize, exe.to_lowercase()));
            }
            true.into()
        }
    }
    let mut out: Vec<(isize, String)> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut out as *mut _ as isize));
    }
    out
}

fn process_path(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows_core::PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        r.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}
