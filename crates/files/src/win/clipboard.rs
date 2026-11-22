//! One OLE apartment orders clipboard commands across all Files windows. A paste captures
//! owned shell item IDs here, before the transfer enters the destination's operation queue.

use super::{inbox::Inbox, log, shell::{self, Pidl}};
use std::sync::{Arc, LazyLock, mpsc};
use windows::Win32::Foundation::{E_ABORT, E_FAIL, E_INVALIDARG, GlobalFree, S_OK};
use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, IDataObject, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL};
use windows::Win32::System::DataExchange::{GetClipboardSequenceNumber, RegisterClipboardFormatW};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::{DROPEFFECT_COPY, DROPEFFECT_MOVE, OleGetClipboard, OleInitialize, OleSetClipboard, OleUninitialize, ReleaseStgMedium};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetMessageW, MSG, TranslateMessage};
use windows_core::{Interface, Result};

// The generated wrapper treats S_FALSE as success. We need the exact ownership result.
windows_core::link!("ole32.dll" "system" fn OleIsCurrentClipboard(data: *mut core::ffi::c_void) -> windows_core::HRESULT);

#[derive(Clone)]
pub struct Source {
    pub items: Vec<Pidl>,
    pub cut: bool,
}

enum Command {
    Publish(Source),
    Snapshot { sequence: u32, reply: mpsc::Sender<Result<Source>> },
}

static COMMANDS: LazyLock<Option<Arc<Inbox<Command>>>> = LazyLock::new(|| {
    let input = Arc::new(Inbox::default());
    let worker = input.clone();
    match std::thread::Builder::new().name("files-clipboard".into()).spawn(move || run(worker)) {
        Ok(_) => Some(input),
        Err(error) => {
            log::line(&format!("could not start Files clipboard worker: {error}"));
            None
        }
    }
});

pub fn publish(items: Vec<Pidl>, cut: bool) {
    send(Command::Publish(Source { items, cut }));
}

pub fn snapshot() -> mpsc::Receiver<Result<Source>> {
    let (reply, receive) = mpsc::channel();
    let sequence = unsafe { GetClipboardSequenceNumber() };
    send(Command::Snapshot { sequence, reply });
    receive
}

fn send(command: Command) {
    if !COMMANDS.as_ref().is_some_and(|input| input.send(command)) {
        log::line("Files clipboard worker is unavailable");
    }
}

fn run(input: Arc<Inbox<Command>>) {
    let receiver = input.attach();
    if let Err(error) = unsafe { OleInitialize(None) } {
        log::line(&format!("Files clipboard OLE initialization failed: {error}"));
        return;
    }
    let mut published = None;
    let mut failed = None;
    let mut msg = MSG::default();
    loop {
        while let Some(command) = receiver.next() {
            match command {
                Command::Publish(source) => {
                    let verb = if source.cut { "Cut" } else { "Copy" };
                    match put(&source) {
                        Ok(data) => {
                            log::line(&format!("command {verb}: Ok(()) (clipboard STA)"));
                            published = Some((data, source));
                            failed = None;
                        }
                        Err(error) => {
                            log::line(&format!("command {verb}: {error:?} (clipboard STA)"));
                            failed = Some((unsafe { GetClipboardSequenceNumber() }, error));
                        }
                    }
                }
                Command::Snapshot { sequence, reply } => {
                    let _ = reply.send(capture(sequence, published.as_ref(), failed.as_ref()));
                }
            }
        }
        unsafe {
            if GetMessageW(&mut msg, None, 0, 0).0 <= 0 {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    drop(published);
    unsafe { OleUninitialize() };
}

fn capture(sequence: u32, published: Option<&(IDataObject, Source)>, failed: Option<&(u32, windows_core::Error)>) -> Result<Source> {
    unsafe {
        let current = GetClipboardSequenceNumber();
        if let Some((_, error)) = failed.filter(|(at, _)| *at == current) {
            return Err(error.clone());
        }
        if let Some((_, source)) = published.filter(|(data, _)| OleIsCurrentClipboard(data.as_raw()) == S_OK) {
            return Ok(source.clone());
        }
        if sequence != current {
            return Err(E_ABORT.into());
        }
        let data = OleGetClipboard()?;
        let items = read_items(&data)?;
        if items.is_empty() {
            return Err(E_INVALIDARG.into());
        }
        let cut = read_cut(&data)?;
        if GetClipboardSequenceNumber() != current {
            return Err(E_ABORT.into());
        }
        Ok(Source { items, cut })
    }
}

unsafe fn read_items(data: &IDataObject) -> Result<Vec<Pidl>> {
    unsafe {
        if let Ok(items) = SHCreateShellItemArrayFromDataObject::<_, IShellItemArray>(data) {
            return shell::item_ids(&items);
        }
        // A Win32 clipboard producer can provide CF_HDROP without a shell ID list.
        const CF_HDROP: u16 = 15;
        let mut storage = data.GetData(&format(CF_HDROP))?;
        if storage.tymed != TYMED_HGLOBAL.0 as u32 {
            ReleaseStgMedium(&mut storage);
            return Err(E_FAIL.into());
        }
        let drop_handle = HDROP(storage.u.hGlobal.0);
        let items = (0..DragQueryFileW(drop_handle, u32::MAX, None)).map(|i| {
            let len = DragQueryFileW(drop_handle, i, None);
            let mut path = vec![0; len as usize + 1];
            let written = DragQueryFileW(drop_handle, i, Some(&mut path));
            if written == 0 {
                return Err(E_FAIL.into());
            }
            Pidl::parse_result(&String::from_utf16_lossy(&path[..written as usize]))
        }).collect();
        ReleaseStgMedium(&mut storage);
        items
    }
}

unsafe fn read_cut(data: &IDataObject) -> Result<bool> {
    unsafe {
        let Ok(mut storage) = data.GetData(&preferred_effect()?) else { return Ok(false) };
        if storage.tymed != TYMED_HGLOBAL.0 as u32 {
            ReleaseStgMedium(&mut storage);
            return Err(E_FAIL.into());
        }
        let h = storage.u.hGlobal;
        let effect = if GlobalSize(h) >= size_of::<u32>() {
            let value = GlobalLock(h).cast::<u32>();
            if value.is_null() { None } else {
                let effect = *value;
                let _ = GlobalUnlock(h);
                Some(effect)
            }
        } else {
            None
        };
        ReleaseStgMedium(&mut storage);
        Ok(effect == Some(DROPEFFECT_MOVE.0))
    }
}

fn put(source: &Source) -> Result<IDataObject> {
    let data = shell::data_object(&source.items)?;
    unsafe {
        let format = preferred_effect()?;
        let h = GlobalAlloc(GMEM_MOVEABLE, size_of::<u32>())?;
        let value = GlobalLock(h).cast::<u32>();
        if value.is_null() {
            let _ = GlobalFree(Some(h));
            return Err(E_FAIL.into());
        }
        *value = if source.cut { DROPEFFECT_MOVE } else { DROPEFFECT_COPY }.0;
        let _ = GlobalUnlock(h);
        let storage = STGMEDIUM { tymed: TYMED_HGLOBAL.0 as u32, u: STGMEDIUM_0 { hGlobal: h }, ..Default::default() };
        if let Err(error) = data.SetData(&format, &storage, true) {
            let _ = GlobalFree(Some(h));
            return Err(error);
        }
        OleSetClipboard(&data)?;
    }
    Ok(data)
}

fn preferred_effect() -> Result<FORMATETC> {
    let id = unsafe { RegisterClipboardFormatW(CFSTR_PREFERREDDROPEFFECT) };
    if id == 0 {
        Err(windows_core::Error::from_thread())
    } else {
        Ok(format(id as u16))
    }
}

fn format(id: u16) -> FORMATETC {
    FORMATETC { cfFormat: id, dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32, ..Default::default() }
}
