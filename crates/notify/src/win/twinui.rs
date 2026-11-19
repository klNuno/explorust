//! Windows' own panels behind Win+N, Win+V and Win+., asked for the way explorer's tray asks for
//! them: through the immersive shell (`CLSID_ImmersiveShell`, run by explorer or by explorust's UWP
//! host) and the private services of `twinui.pcshell.dll`.
//!
//! GUIDs and vtable slots for build 26300 (twinui.pcshell.dll 10.0.26100.9278), read from its
//! public PDB and its code (step notify-2):
//! - `IShellExperienceManagerFactory` 2E8FCB18-A0EE-41AD-8EF8-77FB3A370CA5 (also its service id):
//!   slot 6 `GetExperienceManager(HSTRING, IInspectable**)`.
//! - The notification center is the experience `Windows.Internal.ShellExperience.ActionCenter`
//!   (`CActionCenterExperienceManager`). Its interfaces, from its `GetIids`: `IBaseExperienceManager`
//!   FD8022E5-1099-53F9-B49A-B39BF3F15861 (slot 6 `get_IsVisible`, 7 `put_IsVisible`) and
//!   `IActionCenterExperienceManager` DF65DB57-D504-456E-8BD7-004CE308D8D9 (slot 6 `HotKeyInvoked`).
//! - `TouchKeyboardExperienceManager` (a shell component) runs TextInputHost's panels:
//!   `IClipboardViewLauncher` C9969D8D-C814-50FD-9F55-84A54E9453FC slot 6 `ShowClipboardView()`,
//!   and `ITouchKeyboardExperienceManager` 9516D866-CA0F-40CA-8997-EFA618B50F99 slot 4
//!   `OnShellHotKey(_TouchKeyboardShellHotKeyType)`, where the type maps to a keyboard view
//!   (1 to 2, 2 to 4, 3 to 3, the emoji view).

use std::ffi::c_void;
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, IServiceProvider};
use windows_core::{GUID, HRESULT, HSTRING, IUnknown, Interface, Result};

const CLSID_IMMERSIVE_SHELL: GUID = GUID::from_u128(0xc2f03a33_21f5_47fa_b4bb_156362a2f239);
const IID_EXPERIENCE_FACTORY: GUID = GUID::from_u128(0x2e8fcb18_a0ee_41ad_8ef8_77fb3a370ca5);
const IID_BASE_EXPERIENCE: GUID = GUID::from_u128(0xfd8022e5_1099_53f9_b49a_b39bf3f15861);
const IID_CLIPBOARD_LAUNCHER: GUID = GUID::from_u128(0xc9969d8d_c814_50fd_9f55_84a54e9453fc);
const IID_TOUCH_KEYBOARD: GUID = GUID::from_u128(0x9516d866_ca0f_40ca_8997_efa618b50f99);
const ACTION_CENTER: &str = "Windows.Internal.ShellExperience.ActionCenter";
/// `_TouchKeyboardShellHotKeyType` value explorer passes for Win+. (the emoji view).
pub const HOTKEY_EMOJI: u32 = 3;

/// Slot `i` of a COM object's vtable as a function of type `F`.
///
/// SAFETY: `obj` is a live COM object whose vtable has slot `i` of type `F`.
unsafe fn slot<F: Copy>(obj: &IUnknown, i: usize) -> F {
    unsafe {
        let vtbl = *(obj.as_raw() as *const *const usize);
        std::mem::transmute_copy(&*vtbl.add(i))
    }
}

fn owned(hr: HRESULT, out: *mut c_void) -> Result<IUnknown> {
    hr.ok()?;
    if out.is_null() {
        return Err(HRESULT(0x80004003u32 as i32).into());
    }
    // SAFETY: an owned interface pointer returned by a successful call.
    Ok(unsafe { IUnknown::from_raw(out) })
}

fn shell() -> Result<IServiceProvider> {
    // SAFETY: plain COM activation; the immersive shell lives in the shell process.
    unsafe { CoCreateInstance(&CLSID_IMMERSIVE_SHELL, None, CLSCTX_ALL) }
}

fn service(sid: &GUID, iid: &GUID) -> Result<IUnknown> {
    let sp = shell()?;
    let mut out: *mut c_void = std::ptr::null_mut();
    // SAFETY: QueryService writes an owned pointer of type `iid` on success.
    let hr = unsafe { (Interface::vtable(&sp).QueryService)(sp.as_raw(), sid, iid, &mut out) };
    owned(hr, out)
}

fn query(obj: &IUnknown, iid: &GUID) -> Result<IUnknown> {
    let mut out = std::ptr::null_mut();
    // SAFETY: QueryInterface writes an owned pointer of type `iid` on success.
    let hr = unsafe { obj.query(iid, &mut out) };
    owned(hr, out)
}

fn experience(id: &str) -> Result<IUnknown> {
    let factory = service(&IID_EXPERIENCE_FACTORY, &IID_EXPERIENCE_FACTORY)?;
    let id = HSTRING::from(id);
    let mut out = std::ptr::null_mut();
    type Get = unsafe extern "system" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> HRESULT;
    // SAFETY: slot 6 of IShellExperienceManagerFactory is GetExperienceManager (see the module doc).
    let hr = unsafe { slot::<Get>(&factory, 6)(factory.as_raw(), std::mem::transmute_copy(&id), &mut out) };
    owned(hr, out)
}

const IID_ACTION_CENTER: GUID = GUID::from_u128(0xdf65db57_d504_456e_8bd7_004ce308d8d9);

/// Whether Windows' notification center is on screen.
pub fn notification_center_visible() -> Result<bool> {
    let base = query(&experience(ACTION_CENTER)?, &IID_BASE_EXPERIENCE)?;
    type GetVisible = unsafe extern "system" fn(*mut c_void, *mut u8) -> HRESULT;
    let mut visible = 0u8;
    // SAFETY: slot 6 of IBaseExperienceManager is get_IsVisible.
    unsafe { slot::<GetVisible>(&base, 6)(base.as_raw(), &mut visible) }.ok()?;
    Ok(visible != 0)
}

/// `IActionCenterExperienceManager::HotKeyInvoked(kind)`, as explorer's tray calls it.
pub fn notification_center_hotkey(kind: u32) -> Result<()> {
    let ac = query(&experience(ACTION_CENTER)?, &IID_ACTION_CENTER)?;
    type HotKey = unsafe extern "system" fn(*mut c_void, u32) -> HRESULT;
    // SAFETY: slot 6 of IActionCenterExperienceManager is HotKeyInvoked.
    unsafe { slot::<HotKey>(&ac, 6)(ac.as_raw(), kind) }.ok()
}

/// Shows or hides Windows' notification center. Returns whether it is now visible.
pub fn toggle_notification_center() -> Result<bool> {
    let base = query(&experience(ACTION_CENTER)?, &IID_BASE_EXPERIENCE)?;
    type GetVisible = unsafe extern "system" fn(*mut c_void, *mut u8) -> HRESULT;
    type PutVisible = unsafe extern "system" fn(*mut c_void, u8) -> HRESULT;
    let mut visible = 0u8;
    // SAFETY: slots 6 and 7 of IBaseExperienceManager are get_IsVisible and put_IsVisible.
    unsafe {
        slot::<GetVisible>(&base, 6)(base.as_raw(), &mut visible).ok()?;
        let next = u8::from(visible == 0);
        slot::<PutVisible>(&base, 7)(base.as_raw(), next).ok()?;
        Ok(next != 0)
    }
}

/// Opens TextInputHost's clipboard history, as Win+V does.
pub fn show_clipboard_history() -> Result<()> {
    let launcher = service(&IID_TOUCH_KEYBOARD, &IID_CLIPBOARD_LAUNCHER)
        .or_else(|_| service(&IID_CLIPBOARD_LAUNCHER, &IID_CLIPBOARD_LAUNCHER))?;
    type Show = unsafe extern "system" fn(*mut c_void) -> HRESULT;
    // SAFETY: slot 6 of IClipboardViewLauncher is ShowClipboardView.
    unsafe { slot::<Show>(&launcher, 6)(launcher.as_raw()) }.ok()
}

/// Sends one of explorer's touch keyboard hotkeys (`HOTKEY_EMOJI` for Win+.) to TextInputHost.
pub fn shell_hotkey(kind: u32) -> Result<()> {
    let tk = service(&IID_TOUCH_KEYBOARD, &IID_TOUCH_KEYBOARD)?;
    type OnHotKey = unsafe extern "system" fn(*mut c_void, u32) -> HRESULT;
    // SAFETY: slot 4 of ITouchKeyboardExperienceManager is OnShellHotKey.
    unsafe { slot::<OnHotKey>(&tk, 4)(tk.as_raw(), kind) }.ok()
}
