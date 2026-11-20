//! explorust: shell hook messages for twinui's immersive shell hook service.
//!
//! twinui.dll's `CImmersiveShellHookService` gets its shell hook messages (the registered
//! `SHELLHOOK` message) through `CImmersiveWindowMessageService`, whose message window is a
//! hidden `WorkerW` made by `SHCreateWorkerWindowW` in this process. twinui never calls
//! `RegisterShellHookWindow` on that window itself (it only imports `DeregisterShellHookWindow`):
//! explorer does it. Without it, no `UwpWindowEventDispatcher` sink sees HSHELL 0x24 or 0x26, so a
//! frameless shell-experience CoreWindow (the notification center and calendar, the toasts) waits
//! on its readiness flags forever and never shows.
//!
//! The window comes from the service itself: `QueryService(SID_ImmersiveWindowMessageService)`,
//! then the private `IObjectWithWindow` (b482cd93, slot 4 `GetWindow`), as read from twinui's
//! `CImmersiveWindowMessageService` vtable and QueryInterface on 26300.

use std::ffi::c_void;
use windows::{
    Win32::{
        Foundation::HWND,
        System::Com::{CLSCTX_LOCAL_SERVER, CoCreateInstance, IServiceProvider},
        UI::WindowsAndMessaging::{GetWindowThreadProcessId, IsWindow, RegisterShellHookWindow},
    },
    core::{GUID, HRESULT, IUnknown, Interface},
};

const IMMERSIVE_SHELL: GUID = GUID::from_u128(0xc2f03a33_21f5_47fa_b4bb_156362a2f239);
/// `IID_IImmersiveWindowMessageService`, also its service id.
const WINDOW_MESSAGE_SERVICE: GUID = GUID::from_u128(0xfc62ce95_8396_401a_b74f_88df0d75e112);
/// twinui's private `IObjectWithWindow` (SetWindow at slot 3, GetWindow at slot 4).
const OBJECT_WITH_WINDOW: GUID = GUID::from_u128(0xb482cd93_28d0_4f19_b018_d0bd02102dcc);

/// Registers the window message service's window once it exists. `tick` is called from the host
/// timer until it succeeds or gives up.
pub(crate) struct ShellHookBridge {
    window: Option<usize>,
    tries: u32,
}

impl ShellHookBridge {
    pub(crate) fn new() -> Self {
        Self { window: None, tries: 0 }
    }

    pub(crate) fn tick(&mut self) {
        // 40 tries at the host's 250 ms timer: 10 s for the controller to publish the service.
        if self.window.is_some() || self.tries >= 40 {
            return;
        }
        self.tries += 1;
        match message_window() {
            Ok(hwnd) => {
                // SAFETY: the window belongs to this process (checked below) and outlives the call.
                let mut pid = 0;
                unsafe { GetWindowThreadProcessId(HWND(hwnd as *mut c_void), Some(&mut pid)) };
                let ok = unsafe { RegisterShellHookWindow(HWND(hwnd as *mut c_void)) }.as_bool();
                hlog!(
                    "phase=shell-hook window={hwnd:#x} pid={pid} own={} RegisterShellHookWindow={ok} try={}",
                    pid == std::process::id(),
                    self.tries
                );
                if ok {
                    self.window = Some(hwnd);
                }
            }
            Err(error) => {
                if self.tries == 1 || self.tries == 40 {
                    hlog!("phase=shell-hook try={} error={error}", self.tries);
                }
            }
        }
    }
}

fn message_window() -> windows::core::Result<usize> {
    type GetWindow = unsafe extern "system" fn(*mut c_void, *mut HWND) -> HRESULT;
    // SAFETY: runs on the host STA after the controller started; the slots are the ones read from
    // twinui's vtable (see the module comment) and every pointer is checked before use.
    unsafe {
        let shell: IServiceProvider = CoCreateInstance(&IMMERSIVE_SHELL, None, CLSCTX_LOCAL_SERVER)?;
        let mut raw = std::ptr::null_mut();
        (shell.vtable().QueryService)(
            shell.as_raw(),
            &WINDOW_MESSAGE_SERVICE,
            &WINDOW_MESSAGE_SERVICE,
            &mut raw,
        )
        .ok()?;
        if raw.is_null() {
            return Err(HRESULT(0x80004003u32 as i32).into());
        }
        let service = IUnknown::from_raw(raw);
        let mut raw = std::ptr::null_mut();
        service.query(&OBJECT_WITH_WINDOW, &mut raw).ok()?;
        if raw.is_null() {
            return Err(HRESULT(0x80004002u32 as i32).into());
        }
        let object = IUnknown::from_raw(raw);
        let get: GetWindow =
            std::mem::transmute(object.as_raw().cast::<*const usize>().read().add(4).read());
        let mut hwnd = HWND::default();
        get(object.as_raw(), &mut hwnd).ok()?;
        if hwnd.is_invalid() || !IsWindow(Some(hwnd)).as_bool() {
            return Err(HRESULT(0x80070578u32 as i32).into());
        }
        Ok(hwnd.0 as usize)
    }
}
