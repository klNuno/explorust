//! Windows 8+ application resolver. Its flags distinguish an explicit AppUserModelID
//! from a synthesized identity, which would break pins keyed by a program's path.

use std::ffi::c_void;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows_core::{BOOL, GUID, HRESULT, IUnknown, IUnknown_Vtbl, PWSTR, interface};

const CLSID_APP_RESOLVER: GUID = GUID::from_u128(0x660b90c8_73a9_4b58_8cae_355b7f55341b);

// ABI reference: System Informer, phlib/include/appresolverp.h, IApplicationResolver2.
// The Windows 7 IID has one fewer shortcut method ahead of the window and process calls.
#[interface("de25675a-72de-44b4-9373-05170450c140")]
unsafe trait IApplicationResolver: IUnknown {
    fn app_id_for_shortcut(&self, item: *mut c_void, id: *mut PWSTR) -> HRESULT;
    fn app_id_for_shortcut_object(
        &self,
        link: *mut c_void,
        item: *mut c_void,
        id: *mut PWSTR,
    ) -> HRESULT;
    fn app_id_for_window(
        &self,
        hwnd: HWND,
        id: *mut PWSTR,
        prevent_pinning: *mut BOOL,
        explicit: *mut BOOL,
        embedded: *mut BOOL,
    ) -> HRESULT;
    fn app_id_for_process(
        &self,
        pid: u32,
        id: *mut PWSTR,
        prevent_pinning: *mut BOOL,
        explicit: *mut BOOL,
        embedded: *mut BOOL,
    ) -> HRESULT;
}

pub(super) struct Resolver(IApplicationResolver);

impl Resolver {
    /// Created and used on the task lookup worker's COM apartment.
    pub fn new() -> windows_core::Result<Self> {
        // SAFETY: the active COM apartment owns the IID-specific interface returned here.
        unsafe { CoCreateInstance(&CLSID_APP_RESOLVER, None, CLSCTX_INPROC_SERVER) }.map(Self)
    }

    pub fn explicit_id(&self, pid: u32, hwnd: Option<HWND>) -> Option<String> {
        self.read_explicit(|id, explicit| {
            // SAFETY: uses writable local outputs; the other two outputs are optional.
            unsafe {
                self.0.app_id_for_process(
                    pid,
                    id,
                    std::ptr::null_mut(),
                    explicit,
                    std::ptr::null_mut(),
                )
            }
        })
        .or_else(|| {
            let hwnd = hwnd?;
            self.read_explicit(|id, explicit| {
                // SAFETY: same outputs as above; HWND is passed by value.
                unsafe {
                    self.0.app_id_for_window(
                        hwnd,
                        id,
                        std::ptr::null_mut(),
                        explicit,
                        std::ptr::null_mut(),
                    )
                }
            })
        })
    }

    fn read_explicit(&self, get: impl FnOnce(&mut PWSTR, &mut BOOL) -> HRESULT) -> Option<String> {
        let mut id = PWSTR::null();
        let mut explicit = BOOL(0);
        let hr = get(&mut id, &mut explicit);
        // SAFETY: success with a non-null output yields a terminated COM-allocated string.
        let s = unsafe {
            (hr.is_ok() && explicit.as_bool() && !id.is_null())
                .then(|| id.to_string().ok())
                .flatten()
        };
        // SAFETY: frees the returned allocation, or does nothing for a null output.
        unsafe { CoTaskMemFree(Some(id.0.cast())) };
        s.filter(|s| !s.is_empty())
    }
}
