//! Read the effective value for dumps and the entire chain for Background ownership checks.

use super::abi::*;
use super::state::BackgroundOwnership;
use windows::Win32::Foundation::ERROR_NOT_FOUND;
use windows_core::HRESULT;

const IS_VALUE_HANDLE: i64 = 0x01;
const IS_VALUE_BINDING_EXPRESSION: i64 = 0x10;
const IS_VALUE_NULL: i64 = 0x20;
const BASE_VALUE_SOURCE_LOCAL: i32 = 4;

pub struct Value {
    pub index: u32,
    pub text: String,
    pub ty: String,
    pub handle: bool,
}

pub struct Property {
    pub value: Value,
    pub ownership: BackgroundOwnership,
}

pub fn get(
    svc: &IVisualTreeService,
    element: InstanceHandle,
    name: &str,
    is_owned: Option<&dyn Fn(InstanceHandle) -> Result<bool, HRESULT>>,
) -> Result<Option<Property>, HRESULT> {
    let (mut ns, mut np) = (0u32, 0u32);
    let mut sources: *mut PropertyChainSource = std::ptr::null_mut();
    let mut values: *mut PropertyChainValue = std::ptr::null_mut();
    // SAFETY: the XAML service writes the four outputs into these locals.
    let hr = unsafe { svc.GetPropertyValuesChain(element, &mut ns, &mut sources, &mut np, &mut values) };
    if hr == ERROR_NOT_FOUND.to_hresult() {
        return Ok(None);
    }
    if hr.is_err() {
        return Err(hr);
    }
    let mut selected: Option<(bool, Value)> = None;
    let mut ownership = BackgroundOwnership::default();
    let mut ownership_error = None;
    // SAFETY: the two arrays and their BSTRs are returned by the service and owned by us.
    unsafe {
        for i in 0..np as usize {
            let v = &*values.add(i);
            if bstr(v.PropertyName) == name {
                let src = v.PropertyChainIndex as usize;
                let local = src < ns as usize && (*sources.add(src)).Source == BASE_VALUE_SOURCE_LOCAL;
                let binding = v.MetadataBits & IS_VALUE_BINDING_EXPRESSION != 0;
                let null = v.MetadataBits & IS_VALUE_NULL != 0;
                let handle = !null && v.MetadataBits & IS_VALUE_HANDLE != 0;
                let text = if null { "null".into() } else { bstr(v.Value) };
                let mut ours = false;
                if local && handle
                    && let Some(owns) = is_owned
                    && let Ok(handle) = text.parse()
                {
                    match owns(handle) {
                        Ok(same) => ours = same,
                        Err(hr) => ownership_error = Some(hr),
                    }
                }
                ownership.observe(local, binding, ours);
                let effective = v.Overridden == 0;
                if selected.as_ref().is_none_or(|(current, _)| !current && effective) {
                    selected = Some((
                        effective,
                        Value {
                            index: v.Index,
                            text,
                            ty: bstr(v.ValueType),
                            handle,
                        },
                    ));
                }
            }
            for s in [v.Type, v.DeclaringType, v.ValueType, v.ItemType, v.Value, v.PropertyName] {
                SysFreeString(s);
            }
        }
        for i in 0..ns as usize {
            let s = &*sources.add(i);
            SysFreeString(s.TargetType);
            SysFreeString(s.Name);
            SysFreeString(s.SrcInfo.FileName as *mut u16);
            SysFreeString(s.SrcInfo.Hash as *mut u16);
        }
        CoTaskMemFree(values as *const _);
        CoTaskMemFree(sources as *const _);
    }
    if let Some(hr) = ownership_error {
        return Err(hr);
    }
    Ok(selected.map(|(_, value)| Property { value, ownership }))
}

pub fn describe(svc: &IVisualTreeService, v: &Value) -> String {
    let short = v.ty.rsplit('.').next().unwrap_or(&v.ty);
    if !v.handle {
        return if short.is_empty() { v.text.clone() } else { format!("{short}({})", v.text) };
    }
    let color = v.text.parse().ok().and_then(|h| get(svc, h, "Color", None).ok().flatten());
    match color {
        Some(c) => format!("{short}({})", c.value.text),
        None => short.into(),
    }
}
