//! Search the current folder off the window thread. Both reply types are PIDLs owned by Rust,
//! which the UI can render in the native results view without starting a shell window.

use crate::scope;
use crate::win::shell::Pidl;
use crate::win::tab::Browser;
use fsindex::client::Client;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, mpsc};
use windows::Win32::Foundation::{E_FAIL, HWND, LPARAM, WPARAM};
use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumeInformationW, GetVolumePathNameW};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, IEnumUnknown};
use windows::Win32::System::Search::{IQueryParser, IQueryParserManager, QueryParserManager};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows_core::{HSTRING, IUnknown, PCWSTR, Result, w};

pub const MSG_RESULTS: u32 = WM_APP + 0x129;
const INDEX_LIMIT: u16 = 200;

pub struct Request {
    pub tab: usize,
    pub generation: usize,
    pub folder: Pidl,
    pub path: Option<String>,
    pub query: String,
}

pub enum Results {
    Indexed(Vec<Pidl>),
    Windows(Pidl),
}

pub struct Response {
    pub tab: usize,
    pub generation: usize,
    pub results: Result<Results>,
}

pub struct Worker {
    requests: mpsc::Sender<Request>,
    responses: Arc<Mutex<Vec<Response>>>,
}

impl Worker {
    pub fn new(hwnd: HWND) -> std::io::Result<Self> {
        let (requests, pending) = mpsc::channel::<Request>();
        let responses = Arc::new(Mutex::new(Vec::new()));
        let ready = Arc::clone(&responses);
        let hwnd = hwnd.0 as isize;
        std::thread::Builder::new().name("files-search".into()).spawn(move || {
            let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok() };
            let mut index = Client::default();
            let mut queued = VecDeque::<Request>::new();
            loop {
                if queued.is_empty() {
                    let Ok(job) = pending.recv() else { break };
                    queued.push_back(job);
                }
                for job in pending.try_iter() {
                    if let Some(earlier) = queued.iter_mut().find(|earlier| earlier.tab == job.tab) {
                        *earlier = job;
                    } else {
                        queued.push_back(job);
                    }
                }
                let job = queued.pop_front().expect("received a search request");
                let results = match &com {
                    Ok(()) => query(&job, &mut index),
                    Err(error) => Err(error.clone()),
                };
                ready.lock().unwrap().push(Response { tab: job.tab, generation: job.generation, results });
                unsafe {
                    let _ = PostMessageW(Some(HWND(hwnd as _)), MSG_RESULTS, WPARAM(0), LPARAM(0));
                }
            }
            if com.is_ok() {
                unsafe { CoUninitialize() };
            }
        })?;
        Ok(Self { requests, responses })
    }

    pub fn submit(&self, request: Request) {
        let _ = self.requests.send(request);
    }

    pub fn take(&self) -> Vec<Response> {
        std::mem::take(&mut *self.responses.lock().unwrap())
    }
}

fn indexed_volume(path: &str) -> bool {
    // The service lists fixed NTFS drive letters. UNC paths and folder-mounted volumes do not
    // necessarily appear under their letter on the wire.
    if !matches!(path.as_bytes(), [drive, b':', b'\\', ..] if drive.is_ascii_alphabetic()) {
        return false;
    }
    let path = HSTRING::from(path);
    let mut root = [0; 260];
    let mut format = [0; 16];
    unsafe {
        if GetVolumePathNameW(&path, &mut root).is_err() || root[3] != 0 || GetDriveTypeW(PCWSTR(root.as_ptr())) != 3 {
            return false;
        }
        GetVolumeInformationW(PCWSTR(root.as_ptr()), None, None, None, None, Some(&mut format)).is_ok()
            && format.starts_with(&[b'N' as u16, b'T' as u16, b'F' as u16, b'S' as u16, 0])
    }
}

fn query(job: &Request, index: &mut Client) -> Result<Results> {
    if let Some(path) = job.path.as_deref().filter(|path| indexed_volume(path)) {
        if let Ok(reply) = index.query(&job.query, INDEX_LIMIT) {
            // The service's list is global. If it reaches its cap the relevant matches may have
            // been cut off, so let Windows Search run the query in its actual scope.
            if reply.names > 0 && reply.hits.len() < INDEX_LIMIT.into() {
                let hits = reply.hits.iter().filter_map(|hit| {
                    (scope::contains(path, &hit.path) && std::fs::symlink_metadata(&hit.path).is_ok())
                        .then(|| Pidl::parse(&hit.path)).flatten()
                }).collect();
                return Ok(Results::Indexed(hits));
            }
        }
    }
    windows_folder(job).map(Results::Windows)
}

fn windows_folder(job: &Request) -> Result<Pidl> {
    // ISearchFolderItemFactory builds the shell item that a search-ms URI represents, without
    // invoking the protocol handler. Supplying the original item also preserves virtual scopes.
    unsafe {
        let manager: IQueryParserManager = CoCreateInstance(&QueryParserManager, None, CLSCTX_INPROC_SERVER)?;
        let parser: IQueryParser = manager.CreateLoadedParser(w!("SystemIndex"), 0)?;
        let query = HSTRING::from(format!(r#"System.FileName:~="{}""#, job.query.replace('"', " ")));
        let solution = parser.Parse(&query, None::<&IEnumUnknown>)?;
        let mut condition = None;
        solution.GetQuery(Some(&mut condition), None)?;
        let factory: ISearchFolderItemFactory = CoCreateInstance(&SearchFolderItemFactory, None, CLSCTX_INPROC_SERVER)?;
        let scope: IShellItemArray = SHCreateShellItemArrayFromShellItem(&job.folder.item()?)?;
        factory.SetScope(&scope)?;
        factory.SetCondition(condition.as_ref().ok_or(E_FAIL)?)?;
        factory.SetDisplayName(&HSTRING::from(&job.query))?;
        Pidl::take(factory.GetIDList()?).ok_or_else(|| E_FAIL.into())
    }
}

pub fn present(browser: &Browser, results: Results) -> Result<()> {
    unsafe {
        match results {
            Results::Windows(pidl) => browser.eb.BrowseToIDList(pidl.as_ptr(), SBSP_SAMEBROWSER | SBSP_ACTIVATE_NOFOCUS),
            Results::Indexed(hits) => {
                if browser.folder_view().is_none() {
                    browser.eb.FillFromObject(None::<&IUnknown>, EBF_NODROPTARGET)?;
                }
                let view = browser.folder_view().ok_or(E_FAIL)?;
                let results: IResultsFolder = view.GetFolder()?;
                results.RemoveAll()?;
                for item in &hits {
                    results.AddIDList(item.as_ptr(), None)?;
                }
                crate::win::log::line(&format!("index search: {} scoped items", hits.len()));
                Ok(())
            }
        }
    }
}
