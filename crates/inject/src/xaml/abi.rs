//! The XAML diagnostics ABI (`xamlom.h` of the Windows SDK), declared by hand: the `windows` crate
//! 0.62 has no `Win32::UI::Xaml::Diagnostics` module. Only what the TAP calls or implements.

#![allow(non_snake_case, non_camel_case_types, clippy::upper_case_acronyms)]

use std::ffi::c_void;
use windows_core::{HRESULT, IUnknown, IUnknown_Vtbl, interface};

/// An element of the target's tree, as the diagnostics service names it.
pub type InstanceHandle = u64;

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VisualMutationType(pub i32);
pub const ADD: VisualMutationType = VisualMutationType(0);

/// BSTRs here belong to the caller: read, never freed.
#[repr(C)]
pub struct SourceInfo {
    pub FileName: *const u16,
    pub LineNumber: u32,
    pub ColumnNumber: u32,
    pub CharPosition: u32,
    pub Hash: *const u16,
}

#[repr(C)]
pub struct ParentChildRelation {
    pub Parent: InstanceHandle,
    pub Child: InstanceHandle,
    pub ChildIndex: u32,
}

#[repr(C)]
pub struct VisualElement {
    pub Handle: InstanceHandle,
    pub SrcInfo: SourceInfo,
    pub Type: *const u16,
    pub Name: *const u16,
    pub NumChildren: u32,
}

/// Returned in a CoTaskMem array; its BSTRs are ours to free.
#[repr(C)]
pub struct PropertyChainSource {
    pub Handle: InstanceHandle,
    pub TargetType: *mut u16,
    pub Name: *mut u16,
    pub Source: i32,
    pub SrcInfo: SourceInfo,
}

/// Returned in a CoTaskMem array; its BSTRs are ours to free.
#[repr(C)]
pub struct PropertyChainValue {
    pub Index: u32,
    pub Type: *mut u16,
    pub DeclaringType: *mut u16,
    pub ValueType: *mut u16,
    pub ItemType: *mut u16,
    pub Value: *mut u16,
    pub Overridden: i32,
    pub MetadataBits: i64,
    pub PropertyName: *mut u16,
    pub PropertyChainIndex: u32,
}

#[interface("aa7a8931-80e4-4fec-8f3b-553f87b4966e")]
pub unsafe trait IVisualTreeServiceCallback: IUnknown {
    pub fn OnVisualTreeChange(&self, relation: ParentChildRelation, element: VisualElement, mutation: VisualMutationType) -> HRESULT;
}

#[interface("bad9eb88-ae77-4397-b948-5fa2db0a19ea")]
pub unsafe trait IVisualTreeServiceCallback2: IVisualTreeServiceCallback {
    pub fn OnElementStateChanged(&self, element: InstanceHandle, state: i32, context: *const u16) -> HRESULT;
}

#[interface("a593b11a-d17f-48bb-8f66-83910731c8a5")]
pub unsafe trait IVisualTreeService: IUnknown {
    pub fn AdviseVisualTreeChange(&self, callback: *mut c_void) -> HRESULT;
    pub fn UnadviseVisualTreeChange(&self, callback: *mut c_void) -> HRESULT;
    pub fn GetEnums(&self, count: *mut u32, enums: *mut *mut c_void) -> HRESULT;
    pub fn CreateInstance(&self, type_name: *const u16, value: *const u16, handle: *mut InstanceHandle) -> HRESULT;
    pub fn GetPropertyValuesChain(
        &self,
        handle: InstanceHandle,
        source_count: *mut u32,
        sources: *mut *mut PropertyChainSource,
        property_count: *mut u32,
        values: *mut *mut PropertyChainValue,
    ) -> HRESULT;
    pub fn SetProperty(&self, handle: InstanceHandle, value: InstanceHandle, index: u32) -> HRESULT;
    pub fn ClearProperty(&self, handle: InstanceHandle, index: u32) -> HRESULT;
}

#[interface("18c9e2b6-3f43-4116-9f2b-ff935d7770d2")]
pub unsafe trait IXamlDiagnostics: IUnknown {
    pub fn GetDispatcher(&self, dispatcher: *mut *mut c_void) -> HRESULT;
    pub fn GetUiLayer(&self, layer: *mut *mut c_void) -> HRESULT;
    pub fn GetApplication(&self, application: *mut *mut c_void) -> HRESULT;
    pub fn GetIInspectableFromHandle(&self, handle: InstanceHandle, instance: *mut *mut c_void) -> HRESULT;
    pub fn GetHandleFromIInspectable(&self, instance: *mut c_void, handle: *mut InstanceHandle) -> HRESULT;
    pub fn HitTest(&self, rect: windows::Win32::Foundation::RECT, count: *mut u32, handles: *mut *mut InstanceHandle) -> HRESULT;
    pub fn RegisterInstance(&self, instance: *mut c_void, handle: *mut InstanceHandle) -> HRESULT;
    pub fn GetInitializationData(&self, data: *mut *mut u16) -> HRESULT;
}

#[link(name = "oleaut32")]
unsafe extern "system" {
    pub fn SysFreeString(s: *mut u16);
    pub fn SysStringLen(s: *const u16) -> u32;
    pub fn SysAllocString(s: *const u16) -> *mut u16;
}

#[link(name = "ole32")]
unsafe extern "system" {
    pub fn CoTaskMemFree(p: *const c_void);
}

/// A BSTR the caller owns, as a Rust string (empty for null).
pub fn bstr(s: *const u16) -> String {
    if s.is_null() {
        return String::new();
    }
    // SAFETY: a BSTR carries its length; the slice stays inside it.
    unsafe { String::from_utf16_lossy(std::slice::from_raw_parts(s, SysStringLen(s) as usize)) }
}

/// A BSTR for an [in] argument, freed on drop.
pub struct Bstr(pub *mut u16);

impl Bstr {
    pub fn new(s: &str) -> Bstr {
        let w: Vec<u16> = s.encode_utf16().chain(Some(0)).collect();
        // SAFETY: a NUL-terminated buffer, copied by the call.
        Bstr(unsafe { SysAllocString(w.as_ptr()) })
    }
}

impl Drop for Bstr {
    fn drop(&mut self) {
        // SAFETY: allocated by SysAllocString; null is allowed.
        unsafe { SysFreeString(self.0) }
    }
}
