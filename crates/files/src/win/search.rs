//! Search the current folder off the window thread. Both reply types are PIDLs owned by Rust,
//! which the UI can render in the native results view without starting a shell window.

use crate::scope;
use crate::search_delivery::Delivery;
use crate::win::shell::Pidl;
use crate::win::tab::Browser;
use fsindex::client;
use std::sync::{Arc, mpsc};
use std::time::Duration;
use windows::Storage::Search::{FolderDepth, IndexerOption, QueryOptions};
use windows::Storage::StorageFolder;
use windows::Win32::Foundation::{E_ABORT, E_FAIL, HWND, LPARAM, WPARAM};
use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumeInformationW, GetVolumePathNameW};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, IEnumUnknown};
use windows::Win32::System::Search::{IQueryParser, IQueryParserManager, QueryParserManager};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows_core::{HSTRING, PCWSTR, Result, RuntimeType, w};
use windows_future::{AsyncStatus, IAsyncOperation};

pub const MSG_RESULTS: u32 = WM_APP + 0x129;
const PAGE_SIZE: u32 = fsindex::proto::MAX_HITS as u32;

pub struct Request {
    pub tab: usize,
    pub generation: usize,
    pub folder: Pidl,
    pub path: Option<String>,
    pub query: String,
}

pub enum Results {
    Indexed { hits: Vec<Pidl>, first: bool },
    Windows(Pidl),
}

pub struct Response {
    pub tab: usize,
    pub generation: usize,
    pub results: Result<Results>,
}

pub struct Worker {
    requests: mpsc::Sender<(u64, Request)>,
    delivery: Arc<Delivery<Response>>,
}

impl Worker {
    pub fn new(hwnd: HWND) -> std::io::Result<Self> {
        let (requests, pending) = mpsc::channel::<(u64, Request)>();
        let delivery = Arc::new(Delivery::default());
        let ready = Arc::clone(&delivery);
        let hwnd = hwnd.0 as isize;
        std::thread::Builder::new().name("files-search".into()).spawn(move || {
            let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok() };
            while let Ok((ticket, job)) = pending.recv() {
                let active = || ready.active(ticket);
                if !active() {
                    continue;
                }
                let post = |results| ready.post(ticket, Response { tab: job.tab, generation: job.generation, results }, || unsafe {
                    PostMessageW(Some(HWND(hwnd as _)), MSG_RESULTS, WPARAM(0), LPARAM(0)).is_ok()
                });
                let result = match &com {
                    Ok(()) => query(&job, &active, |result| post(Ok(result))),
                    Err(error) => Err(error.clone()),
                };
                if let Err(error) = result {
                    post(Err(error));
                }
            }
            if com.is_ok() {
                unsafe { CoUninitialize() };
            }
        })?;
        Ok(Self { requests, delivery })
    }

    pub fn submit(&self, request: Request) {
        let ticket = self.delivery.cancel();
        let _ = self.requests.send((ticket, request));
    }

    pub fn cancel(&self) {
        self.delivery.cancel();
    }

    pub fn take(&self) -> Option<Response> {
        self.delivery.take()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn indexed_volume(path: &str) -> bool {
    // The service lists fixed NTFS drive letters. UNC paths and folder-mounted volumes do not
    // necessarily appear under their letter on the wire.
    if !matches!(path.as_bytes(), [drive, b':', b'\\', ..] if drive.is_ascii_alphabetic()) {
        return false;
    }
    // Alias and junction paths do not refer to the namespace indexed under this drive letter.
    let Ok(final_path) = std::fs::canonicalize(path) else { return false };
    let final_path = final_path.to_string_lossy();
    let Some(final_path) = final_path.strip_prefix(r"\\?\") else { return false };
    if final_path.trim_end_matches('\\').to_lowercase() != path.trim_end_matches('\\').to_lowercase() {
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

fn query(job: &Request, active: &impl Fn() -> bool, mut post: impl FnMut(Results) -> bool) -> Result<()> {
    if let Some(path) = job.path.as_deref().filter(|path| indexed_volume(path)) {
        let mut first = true;
        let mut gaps = Vec::new();
        let status = client::query_pages(&job.query, path, active, |page| {
            if !active() {
                return false;
            }
            if page.gaps {
                gaps.extend(page.reply.hits.into_iter().filter(|hit| hit.path.eq_ignore_ascii_case(path) || scope::contains(path, &hit.path)).map(|hit| hit.path));
                return true;
            }
            let hits = page.reply.hits.iter().take_while(|_| active()).filter_map(|hit| {
                (scope::contains(path, &hit.path) && std::fs::symlink_metadata(&hit.path).is_ok())
                    .then(|| Pidl::parse(&hit.path)).flatten()
            }).collect::<Vec<_>>();
            if first {
                crate::win::log::line(&format!("index search: {} scoped items on the first page, service {} us", hits.len(), page.reply.micros));
            }
            let accepted = (!first && hits.is_empty()) || post(Results::Indexed { hits, first });
            first = false;
            accepted
        });
        if !active() {
            return Ok(());
        }
        match status {
            Ok(true) => {
                for gap in gaps {
                    if let Err(error) = windows_items(&gap, &job.query, active, &mut post) && active() {
                        crate::win::log::line(&format!("Windows Search in {gap:?} failed: {error}"));
                    }
                    if !active() {
                        break;
                    }
                }
                return Ok(());
            }
            Err(error) if !first => {
                crate::win::log::line(&format!("index search ended after a partial reply: {error}"));
                return Ok(());
            }
            _ => {},
        }
    }
    if active() {
        post(Results::Windows(windows_folder(job)?));
    }
    Ok(())
}

fn filename_query(query: &str) -> HSTRING {
    HSTRING::from(format!(r#"System.FileName:~="{}""#, query.replace('"', " ")))
}

fn wait<T: RuntimeType>(op: IAsyncOperation<T>, active: &impl Fn() -> bool) -> Result<T> {
    while active() {
        if op.Status()? != AsyncStatus::Started {
            return op.GetResults();
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = op.Cancel();
    Err(E_ABORT.into())
}

/// Merge Windows Search results for a filesystem subtree the index intentionally left out.
fn windows_items(path: &str, query: &str, active: &impl Fn() -> bool, post: &mut impl FnMut(Results) -> bool) -> Result<()> {
    if !active() {
        return Ok(());
    }
    let folder = wait(StorageFolder::GetFolderFromPathAsync(&HSTRING::from(path))?, active)?;
    let options = QueryOptions::new()?;
    options.SetFolderDepth(FolderDepth::Deep)?;
    options.SetIndexerOption(IndexerOption::UseIndexerWhenAvailable)?;
    options.SetUserSearchFilter(&filename_query(query))?;
    let query = folder.CreateItemQueryWithOptions(&options)?;
    let mut start = 0;
    while active() {
        let page = wait(query.GetItemsAsync(start, PAGE_SIZE)?, active)?;
        let count = page.Size()?;
        let hits = (0..count).take_while(|_| active()).filter_map(|i| {
            let item = page.GetAt(i).ok()?;
            let item_path = item.Path().ok()?.to_string_lossy();
            scope::contains(path, &item_path).then(|| Pidl::parse(&item_path)).flatten()
        }).collect::<Vec<_>>();
        if !hits.is_empty() && !post(Results::Indexed { hits, first: false }) {
            break;
        }
        if count < PAGE_SIZE {
            break;
        }
        start += count;
    }
    Ok(())
}

fn windows_folder(job: &Request) -> Result<Pidl> {
    // ISearchFolderItemFactory builds the shell item that a search-ms URI represents, without
    // invoking the protocol handler. Supplying the original item also preserves virtual scopes.
    unsafe {
        let manager: IQueryParserManager = CoCreateInstance(&QueryParserManager, None, CLSCTX_INPROC_SERVER)?;
        let parser: IQueryParser = manager.CreateLoadedParser(w!("SystemIndex"), 0)?;
        let query = filename_query(&job.query);
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
            Results::Windows(pidl) => browser.browse_results(pidl),
            Results::Indexed { hits, .. } => {
                if browser.folder_view().is_none() {
                    browser.create_index_results()?;
                }
                let view = browser.folder_view().ok_or(E_FAIL)?;
                let results: IResultsFolder = view.GetFolder()?;
                for item in &hits {
                    let _ = results.AddIDList(item.as_ptr(), None);
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn windows_search_returns_only_the_missing_subtree() {
        struct Directory(PathBuf);
        impl Drop for Directory {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        struct Apartment;
        impl Drop for Apartment {
            fn drop(&mut self) {
                unsafe { CoUninitialize() };
            }
        }

        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
        let _apartment = Apartment;
        let root = Directory(std::env::temp_dir().join(format!("explorust-search-{}", std::process::id())));
        let gap = root.0.join("Gap");
        let nested = gap.join("Nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.0.join("needle-outside.txt"), []).unwrap();
        fs::write(nested.join("needle-inside.txt"), []).unwrap();
        fs::write(nested.join("something-else.txt"), []).unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut found = Vec::new();
        windows_items(gap.to_str().unwrap(), "needle", &|| std::time::Instant::now() < deadline, &mut |result| {
            let Results::Indexed { hits, first: false } = result else { panic!("expected an indexed continuation") };
            found.extend(hits.iter().map(Pidl::display));
            true
        }).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].eq_ignore_ascii_case(&nested.join("needle-inside.txt").to_string_lossy()), "{found:?}");
    }
}
