//! OLE's Shell data object with a completion callback for cuts.

use super::{Arc, FileCut, Pidl, request_retirement, shell};
use std::cell::Cell;
use windows::Win32::Foundation::{DV_E_FORMATETC, E_FAIL, GlobalFree};
use windows::Win32::System::Com::*;
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::Win32::System::Ole::{
    DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_MOVE, DROPEFFECT_NONE, ReleaseStgMedium,
};
use windows::Win32::UI::Shell::{
    CFSTR_PASTESUCCEEDED, CFSTR_PERFORMEDDROPEFFECT, CFSTR_PREFERREDDROPEFFECT,
};
use windows_core::{BOOL, HRESULT, PCWSTR, Ref, Result, implement};

pub const CF_HDROP: u16 = 15;

pub fn selection(items: &[Pidl], cut: bool) -> Result<IDataObject> {
    let data = shell::data_object(items)?;
    set_effect(
        &data,
        CFSTR_PREFERREDDROPEFFECT,
        if cut {
            DROPEFFECT_MOVE
        } else {
            DROPEFFECT_COPY
        },
    )?;
    Ok(data)
}

#[implement(IDataObject, Agile = false)]
pub struct CutData {
    inner: IDataObject,
    cut: Arc<FileCut>,
    owner: u32,
    succeeded: u16,
    performed: u16,
    optimized: Cell<bool>,
}

impl CutData {
    pub fn wrap(inner: IDataObject, cut: Arc<FileCut>, owner: u32) -> IDataObject {
        Self {
            inner,
            cut,
            owner,
            succeeded: format(CFSTR_PASTESUCCEEDED).map_or(0, |f| f.cfFormat),
            performed: format(CFSTR_PERFORMEDDROPEFFECT).map_or(0, |f| f.cfFormat),
            optimized: Cell::new(false),
        }
        .into()
    }
}

impl IDataObject_Impl for CutData_Impl {
    fn GetData(&self, format: *const FORMATETC) -> Result<STGMEDIUM> {
        if self.cut.consumed() {
            return Err(DV_E_FORMATETC.into());
        }
        unsafe { self.inner.GetData(format) }
    }

    fn GetDataHere(&self, format: *const FORMATETC, storage: *mut STGMEDIUM) -> Result<()> {
        if self.cut.consumed() {
            return Err(DV_E_FORMATETC.into());
        }
        unsafe { self.inner.GetDataHere(format, storage) }
    }

    fn QueryGetData(&self, format: *const FORMATETC) -> HRESULT {
        if self.cut.consumed() {
            DV_E_FORMATETC
        } else {
            unsafe { self.inner.QueryGetData(format) }
        }
    }

    fn GetCanonicalFormatEtc(&self, input: *const FORMATETC, output: *mut FORMATETC) -> HRESULT {
        unsafe { self.inner.GetCanonicalFormatEtc(input, output) }
    }

    fn SetData(
        &self,
        format: *const FORMATETC,
        storage: *const STGMEDIUM,
        release: BOOL,
    ) -> Result<()> {
        let id = unsafe { format.as_ref() }.map_or(0, |f| f.cfFormat);
        if id == 0 || (id != self.performed && id != self.succeeded) {
            return unsafe { self.inner.SetData(format, storage, release.as_bool()) };
        }
        let effect = unsafe { storage.as_ref() }.and_then(medium_effect);
        if id == self.performed {
            self.optimized.set(effect == Some(DROPEFFECT_NONE));
        }
        let consumed = id == self.succeeded && effect == Some(DROPEFFECT_MOVE);
        if consumed && (self.optimized.get() || self.cut.consumed()) {
            // The target already moved the items. The recipient must release the medium,
            // and must leave any new file at the original path alone.
            if release.as_bool() {
                unsafe { ReleaseStgMedium(&mut storage.read()) };
            }
        } else {
            unsafe { self.inner.SetData(format, storage, release.as_bool())? };
        }
        if consumed && self.cut.consume() {
            request_retirement(self.owner, false);
        }
        Ok(())
    }

    fn EnumFormatEtc(&self, direction: u32) -> Result<IEnumFORMATETC> {
        unsafe { self.inner.EnumFormatEtc(direction) }
    }

    fn DAdvise(&self, format: *const FORMATETC, flags: u32, sink: Ref<IAdviseSink>) -> Result<u32> {
        unsafe { self.inner.DAdvise(format, flags, sink.as_ref()) }
    }

    fn DUnadvise(&self, connection: u32) -> Result<()> {
        unsafe { self.inner.DUnadvise(connection) }
    }

    fn EnumDAdvise(&self) -> Result<IEnumSTATDATA> {
        unsafe { self.inner.EnumDAdvise() }
    }
}

pub fn effect(data: &IDataObject, name: PCWSTR) -> Option<DROPEFFECT> {
    unsafe {
        let mut storage = data.GetData(&format(name).ok()?).ok()?;
        let effect = medium_effect(&storage);
        ReleaseStgMedium(&mut storage);
        effect
    }
}

fn medium_effect(storage: &STGMEDIUM) -> Option<DROPEFFECT> {
    unsafe {
        if storage.tymed != TYMED_HGLOBAL.0 as u32
            || GlobalSize(storage.u.hGlobal) < size_of::<u32>()
        {
            return None;
        }
        let handle = storage.u.hGlobal;
        let value = GlobalLock(handle).cast::<u32>();
        let effect = DROPEFFECT(*value.as_ref()?);
        let _ = GlobalUnlock(handle);
        Some(effect)
    }
}

pub fn set_effect(data: &IDataObject, name: PCWSTR, effect: DROPEFFECT) -> Result<()> {
    unsafe {
        let format = format(name)?;
        let h = GlobalAlloc(GMEM_MOVEABLE, size_of::<u32>())?;
        let value = GlobalLock(h).cast::<u32>();
        if value.is_null() {
            let _ = GlobalFree(Some(h));
            return Err(E_FAIL.into());
        }
        *value = effect.0;
        let _ = GlobalUnlock(h);
        let storage = STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 { hGlobal: h },
            ..Default::default()
        };
        if let Err(error) = data.SetData(&format, &storage, true) {
            let _ = GlobalFree(Some(h));
            return Err(error);
        }
        Ok(())
    }
}

pub fn format(name: PCWSTR) -> Result<FORMATETC> {
    let id = unsafe { RegisterClipboardFormatW(name) };
    if id == 0 {
        Err(windows_core::Error::from_thread())
    } else {
        Ok(format_id(id as u16))
    }
}

pub fn format_id(id: u16) -> FORMATETC {
    FORMATETC {
        cfFormat: id,
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
        ..Default::default()
    }
}
