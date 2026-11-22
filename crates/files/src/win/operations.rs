//! Filesystem and extension work on an operation STA per window. Only owned paths and PIDLs cross
//! the boundary so a provider cannot block by calling back through the view's COM apartment.

use super::{inbox::Inbox, log, shell::{self, Pidl}};
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::{E_FAIL, E_INVALIDARG, GlobalFree, HWND, LPARAM, S_OK, WPARAM};
use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, IDataObject, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{DROPEFFECT_COPY, DROPEFFECT_MOVE, OleFlushClipboard, OleInitialize, OleSetClipboard, OleUninitialize};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{Interface, Result};

// The generated wrapper treats S_FALSE as success. Preserve the distinction so a closing view
// flushes its own delayed clipboard data without touching another application's clipboard.
windows_core::link!("ole32.dll" "system" fn OleIsCurrentClipboard(data: *mut core::ffi::c_void) -> windows_core::HRESULT);

pub const MSG_OPERATION: u32 = WM_APP + 0x12B;

pub enum Action {
    Address { text: String, request: usize },
    Clipboard(Vec<Pidl>, bool),
    Delete { items: Vec<Pidl>, results: Option<usize> },
    Paste(Pidl),
    NewFolder(Pidl, &'static str),
    Properties(Vec<Pidl>),
}

pub enum Response {
    Address { tab: usize, request: usize, destination: Result<Option<Pidl>> },
    Refresh { tab: usize, folder: Pidl },
    Deleted { tab: usize, results: usize, items: Vec<Pidl> },
}

struct Job {
    tab: usize,
    action: Action,
}

pub struct Worker {
    input: Arc<Inbox<Option<Job>>>,
    output: Arc<Mutex<Vec<Response>>>,
}

impl Worker {
    pub fn new(hwnd: HWND) -> std::io::Result<Self> {
        let input = Arc::new(Inbox::default());
        let output = Arc::new(Mutex::new(Vec::new()));
        let hwnd = hwnd.0 as isize;
        let jobs = input.clone();
        let replies = output.clone();
        std::thread::Builder::new().name("files-operations".into()).spawn(move || run(hwnd, jobs, replies))?;
        Ok(Self { input, output })
    }

    pub fn submit(&self, tab: usize, action: Action) {
        if !self.input.send(Some(Job { tab, action })) {
            log::line("Files operation worker is unavailable");
        }
    }

    pub fn take(&self) -> Vec<Response> {
        std::mem::take(&mut *self.output.lock().unwrap())
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.input.stop(None);
    }
}

fn run(owner: isize, input: Arc<Inbox<Option<Job>>>, output: Arc<Mutex<Vec<Response>>>) {
    let receiver = input.attach();
    if let Err(error) = unsafe { OleInitialize(None) } {
        log::line(&format!("Files operation OLE initialization failed: {error}"));
        return;
    }
    let owner = HWND(owner as _);
    let mut clipboard = None;
    let mut msg = MSG::default();
    'pump: loop {
        while let Some(job) = receiver.next() {
            let Some(job) = job else { break 'pump };
            if let Some(response) = perform(owner, job, &mut clipboard) && !input.closed() {
                output.lock().unwrap().push(response);
                let _ = unsafe { PostMessageW(Some(owner), MSG_OPERATION, WPARAM(0), LPARAM(0)) };
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
    unsafe {
        if let Some(data) = clipboard {
            if OleIsCurrentClipboard(data.as_raw()) == S_OK {
                let _ = OleFlushClipboard();
            }
        }
        OleUninitialize();
    }
}

fn perform(owner: HWND, job: Job, clipboard: &mut Option<IDataObject>) -> Option<Response> {
    let Job { tab, action } = job;
    let (verb, result, refresh) = match action {
        Action::Address { text, request } => {
            let destination = Pidl::parse_result(&text).and_then(|item| {
                if let Some(folder) = shell::folder_target(&item.item()?) {
                    Ok(Some(folder))
                } else {
                    shell::open_item(owner, &item).map(|()| None)
                }
            });
            if let Err(error) = &destination {
                log::line(&format!("address: navigation failed for {text:?} (HRESULT {:#010x}): {error}", error.code().0 as u32));
            }
            return Some(Response::Address { tab, request, destination });
        }
        Action::Clipboard(items, cut) => {
            let result = put_clipboard(&items, cut).map(|data| *clipboard = Some(data));
            (if cut { "Cut" } else { "Copy" }, result, None)
        }
        Action::Delete { items, results } => {
            let result = shell::delete_items(owner, &items);
            log::line(&format!("command Delete: {result:?} (operation STA)"));
            return results.filter(|_| result.is_ok()).map(|results| Response::Deleted { tab, results, items });
        }
        Action::Paste(folder) => ("Paste", shell::paste(owner, &folder), Some(folder)),
        Action::NewFolder(folder, name) => {
            ("New", folder.item().and_then(|item| shell::new_folder(owner, &item, name)), Some(folder))
        }
        Action::Properties(items) => {
            let result = data_object(&items).and_then(|data| unsafe { SHMultiFileProperties(&data, 0) });
            ("Properties", result, None)
        }
    };
    log::line(&format!("command {verb}: {result:?} (operation STA)"));
    refresh.filter(|_| result.is_ok()).map(|folder| Response::Refresh { tab, folder })
}

fn data_object(items: &[Pidl]) -> Result<IDataObject> {
    if items.is_empty() {
        return Err(E_INVALIDARG.into());
    }
    let children: Vec<_> = items.iter().map(Pidl::as_ptr).collect();
    // Absolute item IDs are relative to the desktop ID list. Keep the paths in the shell's
    // IDataObject so paste can use the original namespace, including mixed-parent results.
    let desktop = Pidl(vec![0, 0]);
    unsafe { SHCreateDataObject(Some(desktop.as_ptr()), Some(&children), None::<&IDataObject>) }
}

fn put_clipboard(items: &[Pidl], cut: bool) -> Result<IDataObject> {
    let data = data_object(items)?;
    unsafe {
        let format = RegisterClipboardFormatW(CFSTR_PREFERREDDROPEFFECT);
        if format == 0 {
            return Err(windows_core::Error::from_thread());
        }
        let h = GlobalAlloc(GMEM_MOVEABLE, size_of::<u32>())?;
        let value = GlobalLock(h).cast::<u32>();
        if value.is_null() {
            let _ = GlobalFree(Some(h));
            return Err(E_FAIL.into());
        }
        *value = if cut { DROPEFFECT_MOVE } else { DROPEFFECT_COPY }.0;
        let _ = GlobalUnlock(h);
        let format = FORMATETC { cfFormat: format as u16, dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32, ..Default::default() };
        let storage = STGMEDIUM { tymed: TYMED_HGLOBAL.0 as u32, u: STGMEDIUM_0 { hGlobal: h }, ..Default::default() };
        if let Err(error) = data.SetData(&format, &storage, true) {
            let _ = GlobalFree(Some(h));
            return Err(error);
        }
        OleSetClipboard(&data)?;
    }
    Ok(data)
}
