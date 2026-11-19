//! Windows' own virtual desktops, through the private service of `twinui.pcshell.dll` that the UWP
//! host's immersive shell controller runs (no explorer needed). Reached as Windows' tools reach it:
//! `CoCreateInstance(CLSID_ImmersiveShell)`, then `IServiceProvider::QueryService`.
//!
//! GUIDs and vtable slots for build 26300 (twinui.pcshell.dll 10.0.26100.9278), read from its
//! public PDB: the `_GUID_*` symbols below exist in it, and the vftable
//! `CVirtualDesktopManager` for `IVirtualDesktopManagerInternal` lists GetCount (3),
//! MoveViewToDesktop (4), GetCurrentDesktop (6), GetDesktops (7), SwitchDesktop (9),
//! CreateDesktopW (11), RemoveDesktop (13), SwitchDesktopWithAnimation (22); `CVirtualDesktop`
//! lists IsViewVisible (3), GetID (4), GetName (5); `IApplicationViewCollection` GetViewForHwnd (6).
//! The same layout as Windows 11 24H2 in open-source trackers (MScholtes/VirtualDesktop).

use std::ffi::c_void;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CLSCTX_ALL, CLSCTX_LOCAL_SERVER, CoCreateInstance, IServiceProvider};
use windows::Win32::UI::Shell::Common::IObjectArray;
use windows::Win32::UI::Shell::{IVirtualDesktopManager, VirtualDesktopManager};
use windows_core::{GUID, HRESULT, HSTRING, IUnknown, Interface, Result};

const CLSID_IMMERSIVE_SHELL: GUID = GUID::from_u128(0xc2f03a33_21f5_47fa_b4bb_156362a2f239);
/// `SID_VirtualDesktopManager` in the PDB.
const SID_VDM: GUID = GUID::from_u128(0xc5e0cdca_7b6e_41b2_9fc4_d93975cc467b);
/// IVirtualDesktopManagerInternal.
const IID_VDMI: GUID = GUID::from_u128(0x53f5ca0b_158f_4124_900c_057158060b27);
/// IVirtualDesktop.
const IID_VD: GUID = GUID::from_u128(0x3f07f4be_b107_441a_af0f_39d82529072c);
/// IApplicationViewCollection (service and interface).
const IID_VIEWS: GUID = GUID::from_u128(0x1841c6d7_4f9d_42c0_af41_8747538f10e5);

/// One desktop: its COM object, id and name ("Desktop n" when the user gave none).
pub struct Desk {
    obj: IUnknown,
    pub id: GUID,
    pub name: String,
}

pub struct Vd {
    mgr: IUnknown,
    views: IUnknown,
    public: Option<IVirtualDesktopManager>,
}

/// Slot `i` of a COM object's vtable as a function of type `F`.
///
/// SAFETY: `obj` is a live COM object whose vtable has slot `i` of type `F`.
unsafe fn slot<F: Copy>(obj: &IUnknown, i: usize) -> F {
    unsafe {
        let vtbl = *(obj.as_raw() as *const *const usize);
        std::mem::transmute_copy(&*vtbl.add(i))
    }
}

fn check(hr: HRESULT, out: *mut c_void) -> Result<IUnknown> {
    hr.ok()?;
    if out.is_null() {
        return Err(HRESULT(0x80004003u32 as i32).into());
    }
    // SAFETY: an owned interface pointer returned by a successful call.
    Ok(unsafe { IUnknown::from_raw(out) })
}

type Out = unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT;
type One = unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT;
type Two = unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> HRESULT;

impl Vd {
    /// Connects to the service; fails when no immersive shell runs or the interfaces differ.
    pub fn connect() -> Result<Vd> {
        // SAFETY: COM calls on an initialized apartment; out pointers are checked.
        unsafe {
            let shell: IServiceProvider = CoCreateInstance(&CLSID_IMMERSIVE_SHELL, None, CLSCTX_LOCAL_SERVER)?;
            let mut raw = std::ptr::null_mut();
            let hr = (shell.vtable().QueryService)(shell.as_raw(), &SID_VDM, &IID_VDMI, &mut raw);
            let mgr = check(hr, raw)?;
            let mut raw = std::ptr::null_mut();
            let hr = (shell.vtable().QueryService)(shell.as_raw(), &IID_VIEWS, &IID_VIEWS, &mut raw);
            let views = check(hr, raw)?;
            let public: Option<IVirtualDesktopManager> = CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_ALL).ok();
            Ok(Vd { mgr, views, public })
        }
    }

    pub fn count(&self) -> Result<u32> {
        let mut n = 0u32;
        // SAFETY: slot 3, GetCount(UINT*).
        unsafe {
            let f: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT = slot(&self.mgr, 3);
            f(self.mgr.as_raw(), &mut n).ok()?;
        }
        Ok(n)
    }

    fn desk(obj: IUnknown, index: usize) -> Desk {
        let mut id = GUID::zeroed();
        let mut name = HSTRING::new();
        // SAFETY: IVirtualDesktop slots 4 GetID(GUID*) and 5 GetName(HSTRING*).
        unsafe {
            let get_id: unsafe extern "system" fn(*mut c_void, *mut GUID) -> HRESULT = slot(&obj, 4);
            let _ = get_id(obj.as_raw(), &mut id);
            let get_name: unsafe extern "system" fn(*mut c_void, *mut HSTRING) -> HRESULT = slot(&obj, 5);
            let _ = get_name(obj.as_raw(), &mut name);
        }
        let name = if name.is_empty() { format!("Desktop {}", index + 1) } else { name.to_string() };
        Desk { obj, id, name }
    }

    /// The desktops in their strip order.
    pub fn desktops(&self) -> Result<Vec<Desk>> {
        // SAFETY: slot 7, GetDesktops(IObjectArray**); items are IVirtualDesktop.
        unsafe {
            let f: Out = slot(&self.mgr, 7);
            let mut raw = std::ptr::null_mut();
            let arr = check(f(self.mgr.as_raw(), &mut raw), raw)?;
            let arr: IObjectArray = arr.cast()?;
            let n = arr.GetCount()?;
            let mut out = Vec::with_capacity(n as usize);
            for i in 0..n {
                let d: IUnknown = arr.GetAt(i)?;
                let mut raw = std::ptr::null_mut();
                let d = check(d.query(&IID_VD, &mut raw), raw)?;
                out.push(Self::desk(d, i as usize));
            }
            Ok(out)
        }
    }

    pub fn current_id(&self) -> Result<GUID> {
        // SAFETY: slot 6, GetCurrentDesktop(IVirtualDesktop**).
        unsafe {
            let f: Out = slot(&self.mgr, 6);
            let mut raw = std::ptr::null_mut();
            let d = check(f(self.mgr.as_raw(), &mut raw), raw)?;
            Ok(Self::desk(d, 0).id)
        }
    }

    /// Index of the current desktop in `desks`.
    pub fn current_index(&self, desks: &[Desk]) -> Option<usize> {
        let cur = self.current_id().ok()?;
        desks.iter().position(|d| d.id == cur)
    }

    /// Creates a desktop at the end of the strip (does not switch to it).
    pub fn create(&self) -> Result<Desk> {
        let n = self.count().unwrap_or(0) as usize;
        // SAFETY: slot 11, CreateDesktopW(IVirtualDesktop**).
        unsafe {
            let f: Out = slot(&self.mgr, 11);
            let mut raw = std::ptr::null_mut();
            let d = check(f(self.mgr.as_raw(), &mut raw), raw)?;
            Ok(Self::desk(d, n))
        }
    }

    /// Switches to `d` and checks that it became current: the animated call first, then the
    /// plain one. In g1 (no explorer) the animated call returned S_OK and left the current
    /// desktop as it was, so the result is checked rather than trusted.
    pub fn switch(&self, d: &Desk) -> Result<&'static str> {
        // SAFETY: slots 22 SwitchDesktopWithAnimation and 9 SwitchDesktop, both (IVirtualDesktop*).
        unsafe {
            let anim: One = slot(&self.mgr, 22);
            let a = anim(self.mgr.as_raw(), d.obj.as_raw());
            if a.is_ok() && self.current_id().ok() == Some(d.id) {
                return Ok("SwitchDesktopWithAnimation");
            }
            let plain: One = slot(&self.mgr, 9);
            plain(self.mgr.as_raw(), d.obj.as_raw()).ok()?;
        }
        if self.current_id().ok() == Some(d.id) {
            Ok("SwitchDesktop")
        } else {
            Ok("SwitchDesktop (returned S_OK, current unchanged)")
        }
    }

    /// Removes `d`; its windows go to `fallback`.
    pub fn remove(&self, d: &Desk, fallback: &Desk) -> Result<()> {
        // SAFETY: slot 13, RemoveDesktop(IVirtualDesktop*, IVirtualDesktop* fallback).
        unsafe {
            let f: Two = slot(&self.mgr, 13);
            f(self.mgr.as_raw(), d.obj.as_raw(), fallback.obj.as_raw()).ok()
        }
    }

    fn view(&self, hwnd: HWND) -> Result<IUnknown> {
        // SAFETY: IApplicationViewCollection slot 6, GetViewForHwnd(HWND, IApplicationView**).
        unsafe {
            let f: unsafe extern "system" fn(*mut c_void, HWND, *mut *mut c_void) -> HRESULT = slot(&self.views, 6);
            let mut raw = std::ptr::null_mut();
            check(f(self.views.as_raw(), hwnd, &mut raw), raw)
        }
    }

    /// Moves a top-level window of any process to `d`.
    pub fn move_window(&self, hwnd: HWND, d: &Desk) -> Result<()> {
        let view = self.view(hwnd)?;
        // SAFETY: slot 4, MoveViewToDesktop(IApplicationView*, IVirtualDesktop*).
        unsafe {
            let f: Two = slot(&self.mgr, 4);
            f(self.mgr.as_raw(), view.as_raw(), d.obj.as_raw()).ok()
        }
    }

    /// The desktop a window is on (documented IVirtualDesktopManager), when the shell knows it.
    pub fn window_desktop(&self, hwnd: HWND) -> Option<GUID> {
        let p = self.public.as_ref()?;
        // SAFETY: documented COM call on a live object.
        unsafe { p.GetWindowDesktopId(hwnd) }.ok().filter(|g| *g != GUID::zeroed())
    }
}
