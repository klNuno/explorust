//! Filesystem and extension work on an operation STA per window. Only owned paths and PIDLs cross
//! the boundary so a provider cannot block by calling back through the view's COM apartment.

use super::{clipboard, inbox::Inbox, log, shell::{self, Pidl}};
use std::sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}, mpsc};
use windows::Win32::Foundation::{E_ABORT, HWND, LPARAM, WPARAM};
use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows::Win32::UI::Shell::SHMultiFileProperties;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::Result;

pub const MSG_OPERATION: u32 = WM_APP + 0x12B;

pub enum Action {
    Address { text: String, request: usize },
    Delete { items: Vec<Pidl>, results: Option<usize> },
    Paste(Pidl, mpsc::Receiver<Result<clipboard::Source>>),
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
    pub id: usize,
    input: Arc<Inbox<Option<Job>>>,
    output: Arc<Mutex<Option<Vec<Response>>>>,
}

impl Worker {
    pub fn new(hwnd: HWND) -> std::io::Result<Self> {
        let input = Arc::new(Inbox::default());
        static NEXT_WORKER: AtomicUsize = AtomicUsize::new(1);
        let id = NEXT_WORKER.fetch_add(1, Ordering::Relaxed);
        let output = Arc::new(Mutex::new(Some(Vec::new())));
        let hwnd = hwnd.0 as isize;
        let jobs = input.clone();
        let replies = output.clone();
        std::thread::Builder::new().name("files-operations".into()).spawn(move || run(hwnd, id, jobs, replies))?;
        Ok(Self { id, input, output })
    }

    pub fn submit(&self, tab: usize, action: Action) {
        if !self.input.send(Some(Job { tab, action })) {
            log::line("Files operation worker is unavailable");
        }
    }

    pub fn take(&self) -> Vec<Response> {
        self.output.lock().unwrap().as_mut().map(std::mem::take).unwrap_or_default()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.output.lock().unwrap().take();
        // Keep accepted mutations in FIFO order. Work that only serves the closed frame can go.
        self.input.close(None, |job| job.as_ref().is_some_and(|job| matches!(job.action,
            Action::Delete { .. } | Action::Paste(..) | Action::NewFolder(..))));
    }
}

fn run(owner: isize, id: usize, input: Arc<Inbox<Option<Job>>>, output: Arc<Mutex<Option<Vec<Response>>>>) {
    let receiver = input.attach();
    if let Err(error) = unsafe { OleInitialize(None) } {
        log::line(&format!("Files operation OLE initialization failed: {error}"));
        return;
    }
    let owner = HWND(owner as _);
    let mut msg = MSG::default();
    'pump: loop {
        while let Some(job) = receiver.next() {
            let Some(job) = job else { break 'pump };
            let operation_owner = if output.lock().unwrap().is_some() { owner } else { HWND::default() };
            if let Some(response) = perform(operation_owner, job) && let Some(responses) = output.lock().unwrap().as_mut() {
                responses.push(response);
                let _ = unsafe { PostMessageW(Some(owner), MSG_OPERATION, WPARAM(id), LPARAM(0)) };
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
    unsafe { OleUninitialize() };
}

fn perform(owner: HWND, job: Job) -> Option<Response> {
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
        Action::Delete { items, results } => {
            let result = shell::delete_items(owner, &items);
            log::line(&format!("command Delete: {result:?} (operation STA)"));
            return results.filter(|_| result.is_ok()).map(|results| Response::Deleted { tab, results, items });
        }
        Action::Paste(folder, source) => {
            let result = source.recv().unwrap_or_else(|_| Err(E_ABORT.into()))
                .and_then(|source| shell::paste(owner, &folder, &source));
            ("Paste", result, Some(folder))
        }
        Action::NewFolder(folder, name) => {
            ("New", folder.item().and_then(|item| shell::new_folder(owner, &item, name)), Some(folder))
        }
        Action::Properties(items) => {
            let result = shell::data_object(&items).and_then(|data| unsafe { SHMultiFileProperties(&data, 0) });
            ("Properties", result, None)
        }
    };
    log::line(&format!("command {verb}: {result:?} (operation STA)"));
    refresh.filter(|_| result.is_ok()).map(|folder| Response::Refresh { tab, folder })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf, time::{Duration, Instant}};

    struct Fixture(PathBuf);

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn accepted_paste_and_new_folder_finish_after_window_closes() {
        let dir = std::env::temp_dir().join(format!("explorust-files-drain-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let fixture = Fixture(dir);
        let source_path = fixture.0.join("move.txt");
        let target_path = fixture.0.join("target");
        fs::write(&source_path, "move me").unwrap();
        fs::create_dir(&target_path).unwrap();
        let (source, target) = unsafe {
            OleInitialize(None).unwrap();
            let source = Pidl::parse_result(source_path.to_str().unwrap());
            let target = Pidl::parse_result(target_path.to_str().unwrap());
            OleUninitialize();
            (source.unwrap(), target.unwrap())
        };
        let worker = Worker::new(HWND::default()).unwrap();
        let (release, pending) = mpsc::channel();
        worker.submit(1, Action::Paste(target.clone(), pending));
        worker.submit(1, Action::NewFolder(target, "queued folder"));
        drop(worker);
        release.send(Ok(clipboard::Source { items: vec![source], cut: true })).unwrap();

        let moved = target_path.join("move.txt");
        let created = target_path.join("queued folder");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !created.is_dir() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(created.is_dir(), "queued New Folder did not finish");
        assert!(!source_path.exists(), "Paste did not move the accepted selection");
        assert_eq!(fs::read_to_string(moved).unwrap(), "move me");
    }
}
