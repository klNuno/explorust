//! Ask the current desktop Explorer to launch a switch worker in the signed-in user context.

use crate::{account, taskbar};
use std::path::Path;
use windows::Win32::System::Com::{
    CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize, IDispatch, IServiceProvider,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Shell::{
    CSIDL_DESKTOP, IShellBrowser, IShellDispatch2, IShellFolderViewDual, IShellWindows,
    SID_STopLevelBrowser, SVGIO_BACKGROUND, SWC_DESKTOP, SWFO_NEEDDISPATCH, ShellWindows,
};
use windows::Win32::UI::WindowsAndMessaging::{GetShellWindow, GetWindowThreadProcessId};
use windows::core::{BSTR, Interface};

pub struct Apartment;

impl Apartment {
    pub fn new() -> windows::core::Result<Self> {
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

pub fn can_launch_switch() -> Result<(), String> {
    if taskbar::explorer_window().is_none() {
        return Err("no Explorer taskbar in this session".into());
    }
    let mut pid = 0;
    // SAFETY: this queries the current desktop window and copies its process ID.
    unsafe {
        let desktop = GetShellWindow();
        if desktop.is_invalid() {
            return Err("no Explorer desktop in this session".into());
        }
        GetWindowThreadProcessId(desktop, Some(&mut pid));
    }
    if !taskbar::is_explorer(pid) {
        return Err("the desktop is not owned by Windows Explorer".into());
    }
    account::check_unelevated_process(pid)
}

/// Ask the desktop Explorer to launch the installed switch in its own user context.
pub fn launch_switch(switch: &Path) -> Result<(), String> {
    can_launch_switch()?;
    let path = BSTR::from(switch.to_string_lossy().as_ref());
    let Some(dir) = switch.parent() else {
        return Err("the installed switch has no working directory".into());
    };
    let _apartment =
        Apartment::new().map_err(|e| format!("initialize Explorer automation: {e}"))?;
    // SAFETY: all automation methods run on this COM apartment, with live arguments.
    unsafe {
        let list: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER)
            .map_err(|e| format!("get ShellWindows: {e}"))?;
        let mut hwnd = 0;
        let desktop = list
            .FindWindowSW(
                &VARIANT::from(CSIDL_DESKTOP as i32),
                &VARIANT::default(),
                SWC_DESKTOP,
                &mut hwnd,
                SWFO_NEEDDISPATCH,
            )
            .map_err(|e| format!("find Explorer's desktop: {e}"))?;
        let browser: IShellBrowser = desktop
            .cast::<IServiceProvider>()
            .and_then(|services| services.QueryService(&SID_STopLevelBrowser))
            .map_err(|e| format!("get Explorer's desktop browser: {e}"))?;
        // Background automation is exposed through IDispatch by the shell view.
        let folder: IShellFolderViewDual = browser
            .QueryActiveShellView()
            .and_then(|view| view.GetItemObject::<IDispatch>(SVGIO_BACKGROUND))
            .and_then(|dispatch| dispatch.cast())
            .map_err(|e| format!("get Explorer's desktop folder automation: {e}"))?;
        let app: IShellDispatch2 = folder
            .Application()
            .and_then(|app| app.cast())
            .map_err(|e| format!("get Explorer's application automation: {e}"))?;
        app.ShellExecute(
            &path,
            &VARIANT::from(BSTR::from("--switch-worker")),
            &VARIANT::from(BSTR::from(dir.to_string_lossy().as_ref())),
            &VARIANT::from(BSTR::from("open")),
            &VARIANT::from(1),
        )
        .map_err(|e| format!("launch the switch through Explorer: {e}"))
    }
}
