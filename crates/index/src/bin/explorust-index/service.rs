//! The service: dispatcher entry, control handler, and the install and uninstall that
//! `explorust-guard --install` and `--restore` run.

use crate::{log, pipe, server};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{ERROR_CALL_NOT_IMPLEMENTED, HANDLE, NO_ERROR};
use windows::Win32::UI::WindowsAndMessaging::{DBT_DEVTYP_DEVICEINTERFACE, DEV_BROADCAST_DEVICEINTERFACE_W, DEVICE_NOTIFY_SERVICE_HANDLE, RegisterDeviceNotificationW};
use windows_core::GUID;
use windows::Win32::System::Services::{
    ChangeServiceConfig2W, ChangeServiceConfigW, CloseServiceHandle, SC_ACTION, SC_ACTION_RESTART, SERVICE_ACCEPT_SESSIONCHANGE, SERVICE_CONFIG_FAILURE_ACTIONS,
    SERVICE_CONTROL_DEVICEEVENT, SERVICE_CONTROL_SESSIONCHANGE, SERVICE_FAILURE_ACTIONSW, ControlService, CreateServiceW, DeleteService, OpenSCManagerW, OpenServiceW, QueryServiceStatus,
    RegisterServiceCtrlHandlerExW, SC_HANDLE, SC_MANAGER_ALL_ACCESS, SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP, SERVICE_ALL_ACCESS,
    SERVICE_AUTO_START, SERVICE_CONTROL_INTERROGATE, SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP, SERVICE_ERROR_NORMAL, SERVICE_RUNNING,
    SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE, SERVICE_STATUS_HANDLE, SERVICE_STOPPED, SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS,
    SetServiceStatus, StartServiceCtrlDispatcherW, StartServiceW,
};
use windows_core::{HSTRING, PCWSTR, PWSTR};

static STATUS: OnceLock<isize> = OnceLock::new();

fn report(state: SERVICE_STATUS_CURRENT_STATE) {
    let Some(&h) = STATUS.get() else { return };
    let s = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING { SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN | SERVICE_ACCEPT_SESSIONCHANGE } else { 0 },
        ..Default::default()
    };
    // SAFETY: the handle came from RegisterServiceCtrlHandlerExW.
    let _ = unsafe { SetServiceStatus(SERVICE_STATUS_HANDLE(h as _), &s) };
}

/// GUID_DEVINTERFACE_VOLUME: volume arrivals reach the handler as device events.
const VOLUME_INTERFACE: GUID = GUID::from_u128(0x53f5630d_b6bf_11d0_94f2_00a0c91efb8b);
const DBT_DEVICEARRIVAL: u32 = 0x8000;

unsafe extern "system" fn handler(ctrl: u32, event: u32, _: *mut core::ffi::c_void, _: *mut core::ffi::c_void) -> u32 {
    match ctrl {
        SERVICE_CONTROL_SESSIONCHANGE => {
            let _ = std::thread::Builder::new().name("index-session".into()).spawn(pipe::refresh);
            NO_ERROR.0
        }
        SERVICE_CONTROL_DEVICEEVENT => {
            if event == DBT_DEVICEARRIVAL {
                // The letter comes a moment after the volume interface.
                let _ = std::thread::Builder::new().name("index-arrival".into()).spawn(|| {
                    for _ in 0..5 {
                        std::thread::sleep(Duration::from_secs(2));
                        if server::rescan() > 0 {
                            break;
                        }
                    }
                });
            }
            NO_ERROR.0
        }
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            log::line("service: stop");
            report(SERVICE_STOPPED);
            std::process::exit(0);
        }
        SERVICE_CONTROL_INTERROGATE => NO_ERROR.0,
        _ => ERROR_CALL_NOT_IMPLEMENTED.0,
    }
}

unsafe extern "system" fn service_main(_: u32, _: *mut PWSTR) {
    // SAFETY: registers the handler above for this service name.
    match unsafe { RegisterServiceCtrlHandlerExW(&HSTRING::from(fsindex::SERVICE), Some(handler), None) } {
        Ok(h) => {
            let _ = STATUS.set(h.0 as isize);
        }
        Err(e) => {
            log::line(&format!("service: RegisterServiceCtrlHandlerExW: {e}"));
            return;
        }
    }
    log::line("service: started");
    let filter = DEV_BROADCAST_DEVICEINTERFACE_W {
        dbcc_size: size_of::<DEV_BROADCAST_DEVICEINTERFACE_W>() as u32,
        dbcc_devicetype: DBT_DEVTYP_DEVICEINTERFACE.0,
        dbcc_classguid: VOLUME_INTERFACE,
        ..Default::default()
    };
    if let Some(&h) = STATUS.get() {
        // SAFETY: the filter is a local the call copies; the status handle stands in for a window.
        match unsafe { RegisterDeviceNotificationW(HANDLE(h as _), &filter as *const _ as *const core::ffi::c_void, DEVICE_NOTIFY_SERVICE_HANDLE) } {
            Ok(_) => log::line("service: volume arrivals registered"),
            Err(e) => log::line(&format!("service: RegisterDeviceNotificationW: {e}")),
        }
    }
    server::run(Some(&|| report(SERVICE_RUNNING)));
}

pub fn dispatch() -> u32 {
    let mut name: Vec<u16> = fsindex::SERVICE.encode_utf16().chain([0]).collect();
    let table = [SERVICE_TABLE_ENTRYW { lpServiceName: PWSTR(name.as_mut_ptr()), lpServiceProc: Some(service_main) }, SERVICE_TABLE_ENTRYW::default()];
    // SAFETY: the table and name outlive the dispatcher, which returns when the service stops.
    match unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } {
        Ok(()) => 0,
        Err(e) => {
            log::line(&format!("service: StartServiceCtrlDispatcherW: {e} (run --console outside the SCM)"));
            1
        }
    }
}

struct Sc(SC_HANDLE);
impl Drop for Sc {
    fn drop(&mut self) {
        // SAFETY: closes a handle opened below.
        let _ = unsafe { CloseServiceHandle(self.0) };
    }
}

fn stop(s: &Sc) {
    let mut st = SERVICE_STATUS::default();
    // SAFETY: control and status queries on an open service handle.
    unsafe {
        if ControlService(s.0, SERVICE_CONTROL_STOP, &mut st).is_err() {
            return;
        }
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(10) {
            if QueryServiceStatus(s.0, &mut st).is_err() || st.dwCurrentState == SERVICE_STOPPED {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn say(s: &str) {
    println!("{s}");
    log::line(s);
}

pub fn install() -> u32 {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            say(&format!("install: {e}"));
            return 1;
        }
    };
    let cmd = HSTRING::from(format!("\"{}\" --service", exe.display()));
    let name = HSTRING::from(fsindex::SERVICE);
    let display = HSTRING::from("explorust file index");
    // SAFETY: SCM calls with handles closed by Sc.
    let r = unsafe {
        (|| -> windows_core::Result<&'static str> {
            let scm = Sc(OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_ALL_ACCESS)?);
            let (svc, what) = match OpenServiceW(scm.0, &name, SERVICE_ALL_ACCESS) {
                Ok(h) => {
                    let s = Sc(h);
                    stop(&s);
                    ChangeServiceConfigW(s.0, SERVICE_WIN32_OWN_PROCESS, SERVICE_AUTO_START, SERVICE_ERROR_NORMAL, &cmd, PCWSTR::null(), None, PCWSTR::null(), PCWSTR::null(), PCWSTR::null(), &display)?;
                    (s, "updated")
                }
                Err(_) => (
                    Sc(CreateServiceW(scm.0, &name, &display, SERVICE_ALL_ACCESS, SERVICE_WIN32_OWN_PROCESS, SERVICE_AUTO_START, SERVICE_ERROR_NORMAL, &cmd, PCWSTR::null(), None, PCWSTR::null(), PCWSTR::null(), PCWSTR::null())?),
                    "created",
                ),
            };
            // Restart on a crash: after 1 s, 5 s, then 30 s; the count resets after a day.
            let mut actions = [
                SC_ACTION { Type: SC_ACTION_RESTART, Delay: 1000 },
                SC_ACTION { Type: SC_ACTION_RESTART, Delay: 5000 },
                SC_ACTION { Type: SC_ACTION_RESTART, Delay: 30000 },
            ];
            let fa = SERVICE_FAILURE_ACTIONSW { dwResetPeriod: 86400, lpRebootMsg: PWSTR::null(), lpCommand: PWSTR::null(), cActions: actions.len() as u32, lpsaActions: actions.as_mut_ptr() };
            ChangeServiceConfig2W(svc.0, SERVICE_CONFIG_FAILURE_ACTIONS, Some(&fa as *const _ as *const core::ffi::c_void))?;
            StartServiceW(svc.0, None)?;
            Ok(what)
        })()
    };
    match r {
        Ok(what) => {
            say(&format!("install: service {} {what} and started: {cmd}", fsindex::SERVICE));
            0
        }
        Err(e) => {
            say(&format!("install: service {}: {e}", fsindex::SERVICE));
            1
        }
    }
}

pub fn uninstall() -> u32 {
    let name = HSTRING::from(fsindex::SERVICE);
    // SAFETY: SCM calls with handles closed by Sc.
    let r = unsafe {
        (|| -> windows_core::Result<()> {
            let scm = Sc(OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_ALL_ACCESS)?);
            let s = Sc(OpenServiceW(scm.0, &name, SERVICE_ALL_ACCESS)?);
            stop(&s);
            DeleteService(s.0)
        })()
    };
    match r {
        Ok(()) => {
            say(&format!("uninstall: service {} stopped and deleted", fsindex::SERVICE));
            0
        }
        // Absent, or already deleted and waiting for its last handle: nothing left to do.
        Err(e) if e.code().0 as u32 & 0xffff == 1060 || e.code().0 as u32 & 0xffff == 1072 => {
            say(&format!("uninstall: service {} already gone ({e})", fsindex::SERVICE));
            0
        }
        Err(e) => {
            say(&format!("uninstall: service {}: {e}", fsindex::SERVICE));
            1
        }
    }
}
