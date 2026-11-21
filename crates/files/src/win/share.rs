//! The Windows share panel for the active File Explorer selection.

use super::{log, shell};
use windows::ApplicationModel::DataTransfer::{
    DataRequest, DataRequestDeferral, DataRequestedEventArgs, DataTransferManager,
};
use windows::Foundation::TypedEventHandler;
use windows::Storage::{IStorageItem, StorageFile, StorageFolder};
use windows::Win32::Foundation::{E_INVALIDARG, HWND};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::Shell::{
    IDataTransferManagerInterop, IShellItemArray, IShellView, SIGDN_NORMALDISPLAY, SVGIO_SELECTION,
};
use windows_collections::IIterable;
use windows_core::{Error, HSTRING, Interface, Result};

pub struct Registration {
    manager: DataTransferManager,
    token: i64,
}

impl Drop for Registration {
    fn drop(&mut self) {
        let _ = self.manager.RemoveDataRequested(self.token);
    }
}

/// Registers a snapshot of the selection before asking Windows to show its share panel.
pub fn open(hwnd: HWND, view: &IShellView) -> Result<Registration> {
    // SAFETY: the view belongs to this thread and the share window is visible.
    unsafe {
        let selection: IShellItemArray = view.GetItemObject(SVGIO_SELECTION)?;
        let count = selection.GetCount()?;
        let mut paths = Vec::new();
        let mut title = HSTRING::from("explorust");
        for index in 0..count {
            let item = selection.GetItemAt(index)?;
            let path = shell::fs_path(&item).ok_or_else(|| Error::from_hresult(E_INVALIDARG))?;
            paths.push(path);
            if count == 1 {
                let name = shell::name(&item, SIGDN_NORMALDISPLAY);
                if !name.is_empty() {
                    title = HSTRING::from(name);
                }
            }
        }
        if paths.is_empty() {
            return Err(Error::from_hresult(E_INVALIDARG));
        }
        let interop = windows_core::factory::<DataTransferManager, IDataTransferManagerInterop>()?;
        let manager: DataTransferManager = interop.GetForWindow(hwnd)?;
        let token = manager.DataRequested(&TypedEventHandler::<
            DataTransferManager,
            DataRequestedEventArgs,
        >::new(move |_, args| {
            if let Some(args) = args.as_ref() {
                supply(args.Request()?, &title, paths.clone())?;
            }
            Ok(())
        }))?;
        let registration = Registration { manager, token };
        interop.ShowShareUIForWindow(hwnd)?;
        Ok(registration)
    }
}

fn supply(request: DataRequest, title: &HSTRING, paths: Vec<String>) -> Result<()> {
    let data = request.Data()?;
    data.Properties()?.SetTitle(title)?;
    let deferral = request.GetDeferral()?;
    let pending = deferral.clone();
    let worker_request = request.clone();
    let count = paths.len();
    let run = std::thread::Builder::new()
        .name("files-share".into())
        .spawn(move || {
            // StorageFile and StorageFolder may contact a filesystem provider. Wait on an MTA worker.
            let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok();
            let result = initialized.clone().and_then(|()| {
                storage_items(paths).and_then(|items| data.SetStorageItemsReadOnly(&items))
            });
            complete(&worker_request, &pending, &result);
            drop((data, worker_request, pending));
            if initialized.is_ok() {
                unsafe { CoUninitialize() };
            }
            match result {
                Ok(()) => log::line(&format!("share data supplied: {count} items")),
                Err(error) => log::line(&format!("share data failed: {error}")),
            }
        });
    if let Err(error) = run {
        let result = Err(Error::from(error));
        complete(&request, &deferral, &result);
        return result;
    }
    Ok(())
}

fn storage_items(paths: Vec<String>) -> Result<IIterable<IStorageItem>> {
    let mut items = Vec::with_capacity(paths.len());
    for path in paths {
        let is_dir = std::fs::metadata(&path)?.is_dir();
        let path = HSTRING::from(path);
        let item = if is_dir {
            StorageFolder::GetFolderFromPathAsync(&path)?
                .join()?
                .cast()?
        } else {
            StorageFile::GetFileFromPathAsync(&path)?.join()?.cast()?
        };
        items.push(Some(item));
    }
    Ok(items.into())
}

fn complete(request: &DataRequest, deferral: &DataRequestDeferral, result: &Result<()>) {
    if let Err(error) = result {
        let _ = request.FailWithDisplayText(&HSTRING::from(error.message()));
    }
    let _ = deferral.Complete();
}
