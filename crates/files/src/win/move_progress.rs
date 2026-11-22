//! The Shell reports individual moves even when the batch is later cancelled.

use super::shell::Pidl;
use std::sync::{Arc, Mutex};
use windows::Win32::UI::Shell::{
    COPYENGINE_S_ALREADY_DONE, COPYENGINE_S_NOT_HANDLED, COPYENGINE_S_USER_IGNORED,
    COPYENGINE_S_USER_RETRY, IFileOperationProgressSink, IFileOperationProgressSink_Impl,
    IShellItem,
};
use windows_core::{HRESULT, PCWSTR, Ref, Result, implement};

#[implement(IFileOperationProgressSink, Agile = false)]
pub struct Progress(pub Arc<Mutex<Vec<Pidl>>>);

impl IFileOperationProgressSink_Impl for Progress_Impl {
    fn PostMoveItem(
        &self,
        _: u32,
        source: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        status: HRESULT,
        destination: Ref<IShellItem>,
    ) -> Result<()> {
        // The copy engine can report a successful directory rename as S_DONT_PROCESS_CHILDREN.
        // PostMoveItem's destination is the item it actually created, including on that path.
        let transferred = status.is_ok()
            && !destination.is_null()
            && !matches!(
                status,
                COPYENGINE_S_NOT_HANDLED | COPYENGINE_S_USER_IGNORED | COPYENGINE_S_USER_RETRY
            );
        if (transferred || status == COPYENGINE_S_ALREADY_DONE)
            && let Some(id) = source.as_ref().and_then(Pidl::of)
        {
            self.0.lock().unwrap().push(id);
        }
        Ok(())
    }

    fn StartOperations(&self) -> Result<()> {
        Ok(())
    }
    fn FinishOperations(&self, _: HRESULT) -> Result<()> {
        Ok(())
    }
    fn PreRenameItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> Result<()> {
        Ok(())
    }
    fn PostRenameItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> Result<()> {
        Ok(())
    }
    fn PreMoveItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
    ) -> Result<()> {
        Ok(())
    }
    fn PreCopyItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
    ) -> Result<()> {
        Ok(())
    }
    fn PostCopyItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> Result<()> {
        Ok(())
    }
    fn PreDeleteItem(&self, _: u32, _: Ref<IShellItem>) -> Result<()> {
        Ok(())
    }
    fn PostDeleteItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> Result<()> {
        Ok(())
    }
    fn PreNewItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> Result<()> {
        Ok(())
    }
    fn PostNewItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: &PCWSTR,
        _: u32,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> Result<()> {
        Ok(())
    }
    fn UpdateProgress(&self, _: u32, _: u32) -> Result<()> {
        Ok(())
    }
    fn ResetTimer(&self) -> Result<()> {
        Ok(())
    }
    fn PauseTimer(&self) -> Result<()> {
        Ok(())
    }
    fn ResumeTimer(&self) -> Result<()> {
        Ok(())
    }
}
