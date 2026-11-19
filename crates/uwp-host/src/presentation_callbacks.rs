//! Build-specific callbacks that advance a CoreWindow to presentation readiness.

#[cfg(target_os = "windows")]
pub(crate) fn probe_window_discovery(wrapper: usize, hwnd: usize) -> Result<(), String> {
    let discovered_interface = wrapper
        .checked_sub(0x20)
        .ok_or("wrapper pointer is too small")?;
    probe_window_discovery_interface(discovered_interface, hwnd)
}

#[cfg(target_os = "windows")]
pub(crate) fn probe_view_wrapper_discovery(interface: usize, hwnd: usize) -> Result<(), String> {
    use std::ffi::c_void;
    use windows::Win32::{Foundation::HWND, System::LibraryLoader::GetModuleHandleW};

    if interface == 0
        || hwnd == 0
        || !unsafe {
            windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(HWND(hwnd as *mut c_void)))
        }
        .as_bool()
    {
        return Err("invalid wrapper interface or HWND".into());
    }
    let expected = crate::symbols::address(
        "twinui.pcshell.dll",
        "?WindowDiscoveredFromShellHook@UwpWindowWrapperBase@@UEAAXPEAUHWND__@@@Z",
    )?;
    let vtable = unsafe { (interface as *const *const usize).read() };
    let method = if vtable.is_null() {
        0
    } else {
        unsafe { vtable.add(24).read() }
    };
    if method != expected {
        return Err(format!(
            "unexpected discovery method {method:#x}; expected {expected:#x}"
        ));
    }
    type DiscoverWindow = unsafe extern "system" fn(*mut c_void, *mut c_void);
    let discover: DiscoverWindow = unsafe { std::mem::transmute(method) };
    unsafe { discover(interface as *mut c_void, hwnd as *mut c_void) };
    Ok(())
}

#[cfg(target_os = "windows")]
pub(crate) fn probe_view_wrapper_readiness(interface: usize, hwnd: usize) -> Result<(), String> {
    use std::ffi::c_void;
    use windows::Win32::{Foundation::HWND, System::LibraryLoader::GetModuleHandleW};

    if interface == 0
        || hwnd == 0
        || !unsafe {
            windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(HWND(hwnd as *mut c_void)))
        }
        .as_bool()
    {
        return Err("invalid wrapper interface or HWND".into());
    }
    let expected_visibility = crate::symbols::address(
        "twinui.pcshell.dll",
        "?VisibilityChanged@UwpWindowWrapperBase@@UEAAXW4EventPhase@@W4Visibility@@@Z",
    )?;
    let expected_layout = crate::symbols::address(
        "twinui.pcshell.dll",
        "?HandlePresentationReadinessChange@UwpWindowWrapperBase@@UEAAX_K@Z",
    )?;
    let vtable = unsafe { (interface as *const *const usize).read() };
    if vtable.is_null() {
        return Err("null wrapper vtable".into());
    }
    let visibility_method = unsafe { vtable.add(40).read() };
    let layout_method = unsafe { vtable.add(41).read() };
    if visibility_method != expected_visibility || layout_method != expected_layout {
        return Err(format!(
            "unexpected readiness methods visibility={visibility_method:#x} layout={layout_method:#x}"
        ));
    }
    type VisibilityChanged = unsafe extern "system" fn(*mut c_void, u32, u32);
    type LayoutEvent = unsafe extern "system" fn(*mut c_void, u64);
    let visibility: VisibilityChanged = unsafe { std::mem::transmute(visibility_method) };
    let layout: LayoutEvent = unsafe { std::mem::transmute(layout_method) };
    unsafe {
        visibility(interface as *mut c_void, 1, 1);
        layout(interface as *mut c_void, 0x26);
    }
    Ok(())
}

#[cfg(target_os = "windows")]
pub(crate) fn probe_view_wrapper_uncloak(interface: usize) -> Result<(), String> {
    use std::ffi::c_void;
    use windows::{Win32::System::LibraryLoader::GetModuleHandleW, core::HRESULT};

    if interface == 0 {
        return Err("null shell-cloak interface".into());
    }
    let expected = crate::symbols::address(
        "twinui.pcshell.dll",
        "?SetShellCloak@UwpWindowWrapperBase@@UEAAJW4__MIDL___MIDL_itf_privilegedoperations_0000_0002_0001@@@Z",
    )?;
    let vtable = unsafe { (interface as *const *const usize).read() };
    let method = if vtable.is_null() {
        0
    } else {
        unsafe { vtable.add(3).read() }
    };
    if method != expected {
        return Err(format!(
            "unexpected shell-cloak method {method:#x}; expected {expected:#x}"
        ));
    }
    type SetShellCloak = unsafe extern "system" fn(*mut c_void, u32) -> HRESULT;
    let set_shell_cloak: SetShellCloak = unsafe { std::mem::transmute(method) };
    unsafe { set_shell_cloak(interface as *mut c_void, 0) }
        .ok()
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "windows")]
pub(crate) fn probe_view_wrapper_foreground(interface: usize) -> Result<(), String> {
    use std::ffi::c_void;
    use windows::{Win32::System::LibraryLoader::GetModuleHandleW, core::HRESULT};

    if interface == 0 {
        return Err("null foreground interface".into());
    }
    let expected = crate::symbols::address(
        "twinui.pcshell.dll",
        "?SetForegroundWindow@UwpWindowWrapperBase@@UEAAJXZ",
    )?;
    let vtable = unsafe { (interface as *const *const usize).read() };
    let method = if vtable.is_null() {
        0
    } else {
        unsafe { vtable.add(7).read() }
    };
    if method != expected {
        return Err(format!(
            "unexpected foreground method {method:#x}; expected {expected:#x}"
        ));
    }
    type SetForegroundWindow = unsafe extern "system" fn(*mut c_void) -> HRESULT;
    let set_foreground: SetForegroundWindow = unsafe { std::mem::transmute(method) };
    unsafe { set_foreground(interface as *mut c_void) }
        .ok()
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "windows")]
pub(crate) fn probe_view_wrapper_set_frame(
    interface: usize,
    frame_hwnd: usize,
) -> Result<(), String> {
    use std::ffi::c_void;
    use windows::{
        Win32::{Foundation::HWND, System::LibraryLoader::GetModuleHandleW},
        core::HRESULT,
    };

    if interface == 0
        || frame_hwnd == 0
        || !unsafe {
            windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(HWND(frame_hwnd as *mut c_void)))
        }
        .as_bool()
    {
        return Err("invalid frame-window interface or HWND".into());
    }
    let expected = crate::symbols::address(
        "twinui.pcshell.dll",
        "?SetFrameWindow@UwpWindowWrapperBase@@UEAAJPEAUHWND__@@W4__MIDL___MIDL_itf_ntuserviewmanagerinterop_0000_0002_0001@@@Z",
    )?;
    let vtable = unsafe { (interface as *const *const usize).read() };
    let method = if vtable.is_null() {
        0
    } else {
        unsafe { vtable.add(3).read() }
    };
    if method != expected {
        return Err(format!(
            "unexpected SetFrameWindow method {method:#x}; expected {expected:#x}"
        ));
    }
    type SetFrameWindow = unsafe extern "system" fn(*mut c_void, *mut c_void, u32) -> HRESULT;
    let set_frame_window: SetFrameWindow = unsafe { std::mem::transmute(method) };
    unsafe { set_frame_window(interface as *mut c_void, frame_hwnd as *mut c_void, 0) }
        .ok()
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "windows")]
pub(crate) fn probe_view_wrapper_set_size(
    interface: usize,
    frame_hwnd: usize,
) -> Result<(), String> {
    use std::ffi::c_void;
    use windows::{
        Win32::{
            Foundation::{HWND, RECT},
            System::LibraryLoader::GetModuleHandleW,
            UI::WindowsAndMessaging::GetClientRect,
        },
        core::HRESULT,
    };

    if interface == 0 || frame_hwnd == 0 {
        return Err("invalid view wrapper interface or frame HWND".into());
    }
    let expected = crate::symbols::address(
        "twinui.pcshell.dll",
        "?SetSize@UwpWindowWrapperBase@@UEAAJUSize@Foundation@Windows@@@Z",
    )?;
    let vtable = unsafe { (interface as *const *const usize).read() };
    let method = if vtable.is_null() {
        0
    } else {
        unsafe { vtable.add(8).read() }
    };
    if method != expected {
        return Err(format!(
            "unexpected SetSize method {method:#x}; expected {expected:#x}"
        ));
    }
    let mut client = RECT::default();
    unsafe { GetClientRect(HWND(frame_hwnd as *mut c_void), &mut client) }
        .map_err(|error| error.to_string())?;
    let width = (client.right - client.left) as f32;
    let height = (client.bottom - client.top) as f32;
    let size = u64::from(width.to_bits()) | (u64::from(height.to_bits()) << 32);
    type SetSize = unsafe extern "system" fn(*mut c_void, u64) -> HRESULT;
    let set_size: SetSize = unsafe { std::mem::transmute(method) };
    unsafe { set_size(interface as *mut c_void, size) }
        .ok()
        .map_err(|error| error.to_string())?;
    hlog!("phase=view-event-set-size width={width} height={height} result=called");
    Ok(())
}

#[cfg(target_os = "windows")]
pub(crate) fn probe_frame_set_position(frame_proxy: usize) -> Result<(), String> {
    use std::ffi::c_void;
    use windows::{
        Win32::{
            Foundation::RECT,
            System::LibraryLoader::GetModuleHandleW,
            UI::WindowsAndMessaging::{SPI_GETWORKAREA, SystemParametersInfoW},
        },
        core::{HRESULT, IUnknown, Interface},
    };

    if frame_proxy == 0 {
        return Err("null frame proxy".into());
    }
    let vtable = unsafe { (frame_proxy as *const *const usize).read() };
    let set_position_method = if vtable.is_null() {
        0
    } else {
        unsafe { vtable.add(4).read() }
    };
    let expected_set_position = crate::symbols::address(
        "twinui.pcshell.dll",
        "?SetPosition@CApplicationFrameWrapper@@UEAAJPEAUIApplicationViewPosition@@@Z",
    )?;
    if set_position_method != expected_set_position {
        return Err(format!(
            "unexpected frame SetPosition method {set_position_method:#x}"
        ));
    }
    let mut work_area = RECT::default();
    unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some((&raw mut work_area).cast()),
            Default::default(),
        )
    }
    .map_err(|error| format!("SPI_GETWORKAREA: {error}"))?;
    let work_width = work_area.right - work_area.left;
    let work_height = work_area.bottom - work_area.top;
    let width = 800.min(work_width).max(320);
    let height = 700.min(work_height).max(480);
    let left = work_area.left + (work_width - width) / 2;
    let top = work_area.top + (work_height - height) / 2;
    let bounds = RECT {
        left,
        top,
        right: left + width,
        bottom: top + height,
    };
    type MakePosition = unsafe extern "system" fn(*mut *mut c_void, *const RECT) -> HRESULT;
    type SetPosition = unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT;
    let make_position_address = crate::symbols::address(
        "twinui.pcshell.dll",
        "??$MakeAndInitialize@VCCommonApplicationViewPosition@@UIApplicationViewPosition@@AEAUtagRECT@@@Details@WRL@Microsoft@@YAJPEAPEAUIApplicationViewPosition@@AEAUtagRECT@@@Z",
    )?;
    let make_position: MakePosition = unsafe { std::mem::transmute(make_position_address) };
    let set_position: SetPosition = unsafe { std::mem::transmute(set_position_method) };
    let mut position = std::ptr::null_mut();
    unsafe { make_position(&mut position, &bounds) }
        .ok()
        .map_err(|error| format!("create position for {bounds:?}: {error}"))?;
    if position.is_null() {
        return Err("position factory returned null".into());
    }
    let status = unsafe { set_position(frame_proxy as *mut c_void, position) };
    unsafe { drop(IUnknown::from_raw(position)) };
    status
        .ok()
        .map_err(|error| format!("set position to {bounds:?}: {error}"))?;
    hlog!("phase=view-event-set-position bounds={bounds:?} result=called");
    Ok(())
}

#[cfg(target_os = "windows")]
pub(crate) fn probe_frame_set_presented_window(
    frame_proxy: usize,
    core_hwnd: usize,
    app_id: &str,
) -> Result<usize, String> {
    use std::ffi::c_void;
    use windows::{
        Win32::{Foundation::HWND, System::LibraryLoader::GetModuleHandleW},
        core::HRESULT,
    };

    if frame_proxy == 0
        || core_hwnd == 0
        || !unsafe {
            windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(HWND(core_hwnd as *mut c_void)))
        }
        .as_bool()
    {
        return Err("invalid frame proxy or CoreWindow".into());
    }
    let vtable = unsafe { (frame_proxy as *const *const usize).read() };
    if vtable.is_null() {
        return Err("null frame proxy vtable".into());
    }
    let get_frame_method = unsafe { vtable.add(3).read() };
    let set_presented_method = unsafe { vtable.add(6).read() };
    let set_application_id_method = unsafe { vtable.add(9).read() };
    let expected_get_frame = crate::symbols::address(
        "twinui.pcshell.dll",
        "?GetFrameWindow@CApplicationFrameWrapper@@UEAAJPEAPEAUHWND__@@@Z",
    )?;
    let expected_set_presented = crate::symbols::address(
        "twinui.pcshell.dll",
        "?SetPresentedWindow@CApplicationFrameWrapper@@UEAAJPEAUHWND__@@@Z",
    )?;
    let expected_set_application_id = crate::symbols::address(
        "twinui.pcshell.dll",
        "?SetApplicationId@CApplicationFrameWrapper@@UEAAJPEBGH@Z",
    )?;
    if get_frame_method != expected_get_frame
        || set_presented_method != expected_set_presented
        || set_application_id_method != expected_set_application_id
    {
        return Err(format!(
            "unexpected frame proxy methods get={get_frame_method:#x} set-presented={set_presented_method:#x} set-app-id={set_application_id_method:#x}"
        ));
    }
    type GetFrameWindow = unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT;
    type SetPresentedWindow = unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT;
    type SetApplicationId = unsafe extern "system" fn(*mut c_void, *const u16, i32) -> HRESULT;
    let get_frame: GetFrameWindow = unsafe { std::mem::transmute(get_frame_method) };
    let set_presented: SetPresentedWindow = unsafe { std::mem::transmute(set_presented_method) };
    let set_application_id: SetApplicationId =
        unsafe { std::mem::transmute(set_application_id_method) };
    let mut frame_hwnd = std::ptr::null_mut();
    unsafe { get_frame(frame_proxy as *mut c_void, &mut frame_hwnd) }
        .ok()
        .map_err(|error| format!("GetFrameWindow: {error}"))?;
    if frame_hwnd.is_null() {
        return Err("GetFrameWindow returned null".into());
    }
    let wide_app_id: Vec<u16> = app_id.encode_utf16().chain([0]).collect();
    unsafe { set_application_id(frame_proxy as *mut c_void, wide_app_id.as_ptr(), 0) }
        .ok()
        .map_err(|error| format!("SetApplicationId: {error}"))?;
    unsafe { set_presented(frame_proxy as *mut c_void, core_hwnd as *mut c_void) }
        .ok()
        .map_err(|error| format!("SetPresentedWindow: {error}"))?;
    Ok(frame_hwnd as usize)
}

#[cfg(target_os = "windows")]
pub(crate) fn probe_frame_fit_to_work_area(frame_proxy: usize) -> Result<(), String> {
    use std::ffi::c_void;
    use windows::{Win32::System::LibraryLoader::GetModuleHandleW, core::HRESULT};

    if frame_proxy == 0 {
        return Err("null frame proxy".into());
    }
    let expected = crate::symbols::address(
        "twinui.pcshell.dll",
        "?FitToWorkArea@CApplicationFrameWrapper@@UEAAJXZ",
    )?;
    let vtable = unsafe { (frame_proxy as *const *const usize).read() };
    let method = if vtable.is_null() {
        0
    } else {
        unsafe { vtable.add(12).read() }
    };
    if method != expected {
        return Err(format!(
            "unexpected FitToWorkArea method {method:#x}; expected {expected:#x}"
        ));
    }
    type FitToWorkArea = unsafe extern "system" fn(*mut c_void) -> HRESULT;
    let fit_to_work_area: FitToWorkArea = unsafe { std::mem::transmute(method) };
    unsafe { fit_to_work_area(frame_proxy as *mut c_void) }
        .ok()
        .map_err(|error| error.to_string())
}

/// explorust: the address of `symbol` once it is found in the vtable of the wrapper object at
/// `object` (the `UwpWindowWrapper` primary interface, the one `find_wrapper` validated).
/// Replaces upstream's check of the first ten bytes of each function, which broke on 26300.9974
/// when the compiler changed HandlePresentationReadinessChange's prologue (it now saves rbp and
/// rsi too). A vtable match proves the object really dispatches to this PDB address, whatever the
/// function's code looks like. The slot is logged once per symbol.
#[cfg(target_os = "windows")]
fn wrapper_method(object: usize, symbol: &str) -> Result<usize, String> {
    use std::{collections::HashMap, sync::Mutex};
    static SLOTS: Mutex<Option<HashMap<String, usize>>> = Mutex::new(None);
    const MAX_SLOTS: usize = 96;

    let address = crate::symbols::address("twinui.pcshell.dll", symbol)?;
    // SAFETY: find_wrapper validated `object` as a live UwpWindowWrapper in this process whose
    // first field is its vtable, a table of at least MAX_SLOTS entries in twinui's .rdata.
    let vtable = unsafe { (object as *const *const usize).read() };
    if vtable.is_null() {
        return Err("null wrapper vtable".into());
    }
    let slot = (0..MAX_SLOTS).find(|&i| unsafe { vtable.add(i).read() } == address);
    let Some(slot) = slot else {
        return Err(format!("{symbol} at {address:#x} is not in the wrapper vtable {vtable:p}"));
    };
    let mut slots = SLOTS.lock().unwrap();
    let slots = slots.get_or_insert_with(HashMap::new);
    if slots.insert(symbol.to_owned(), slot) != Some(slot) {
        hlog!("phase=wrapper-method slot={slot} address={address:#x} symbol={symbol}");
    }
    Ok(address)
}

#[cfg(target_os = "windows")]
fn probe_window_discovery_interface(
    discovered_interface: usize,
    hwnd: usize,
) -> Result<(), String> {
    use std::ffi::c_void;
    use windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::IsWindow};

    if discovered_interface == 0
        || hwnd == 0
        || !unsafe { IsWindow(Some(HWND(hwnd as *mut c_void))) }.as_bool()
    {
        return Err("invalid wrapper or HWND".into());
    }
    let address = wrapper_method(
        discovered_interface,
        "?WindowDiscoveredFromShellHook@UwpWindowWrapperBase@@UEAAXPEAUHWND__@@@Z",
    )?;
    type DiscoverWindow = unsafe extern "system" fn(*mut c_void, *mut c_void);
    // SAFETY: The address is the wrapper's own vtable entry for this PDB-named method.
    let discover: DiscoverWindow = unsafe { std::mem::transmute(address) };
    unsafe { discover(discovered_interface as *mut c_void, hwnd as *mut c_void) };
    hlog!("phase=window-discovered object={discovered_interface:#x} core={hwnd:#x}");
    Ok(())
}

#[cfg(target_os = "windows")]
pub(crate) fn probe_window_visibility(wrapper: usize, hwnd: usize) -> Result<(), String> {
    use std::ffi::c_void;
    use windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::IsWindow};

    let visibility_interface = wrapper
        .checked_sub(0x20)
        .ok_or("wrapper pointer is too small")?;
    if wrapper == 0 || hwnd == 0 || !unsafe { IsWindow(Some(HWND(hwnd as *mut c_void))) }.as_bool()
    {
        return Err("invalid wrapper or HWND".into());
    }
    let client_hwnd = unsafe { ((wrapper + 0x138) as *const usize).read() };
    if client_hwnd != hwnd {
        return Err(format!(
            "wrapper client HWND {client_hwnd:#x} differs from {hwnd:#x}"
        ));
    }
    let address = wrapper_method(
        visibility_interface,
        "?VisibilityChanged@UwpWindowWrapperBase@@UEAAXW4EventPhase@@W4Visibility@@@Z",
    )?;
    type VisibilityChanged = unsafe extern "system" fn(*mut c_void, u32, u32);
    // SAFETY: The address is the wrapper's own vtable entry for this PDB-named method.
    let visibility_changed: VisibilityChanged = unsafe { std::mem::transmute(address) };
    unsafe { visibility_changed(visibility_interface as *mut c_void, 1, 1) };
    Ok(())
}

#[cfg(target_os = "windows")]
pub(crate) fn probe_window_layout(wrapper: usize, hwnd: usize) -> Result<(), String> {
    use std::ffi::c_void;
    use windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::IsWindow};

    let event_interface = wrapper
        .checked_sub(0x20)
        .ok_or("wrapper pointer is too small")?;
    if wrapper == 0 || hwnd == 0 || !unsafe { IsWindow(Some(HWND(hwnd as *mut c_void))) }.as_bool()
    {
        return Err("invalid wrapper or HWND".into());
    }
    let client_hwnd = unsafe { ((wrapper + 0x138) as *const usize).read() };
    if client_hwnd != hwnd {
        return Err(format!(
            "wrapper client HWND {client_hwnd:#x} differs from {hwnd:#x}"
        ));
    }
    let address = wrapper_method(
        event_interface,
        "?HandlePresentationReadinessChange@UwpWindowWrapperBase@@UEAAX_K@Z",
    )?;
    type LayoutEvent = unsafe extern "system" fn(*mut c_void, u64);
    // SAFETY: The address is the wrapper's own vtable entry for this PDB-named method.
    let layout_event: LayoutEvent = unsafe { std::mem::transmute(address) };
    let flags = (wrapper + 0x140) as *const u32;
    let before = unsafe { flags.read() };
    unsafe { layout_event(event_interface as *mut c_void, 0x26) };
    let after = unsafe { flags.read() };
    if before != after {
        hlog!("phase=layout-wait-flags core={hwnd:#x} before={before:#x} after={after:#x}");
    }
    Ok(())
}
