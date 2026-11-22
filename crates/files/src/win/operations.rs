//! Filesystem and extension work on an operation STA per window. Only owned paths and PIDLs cross
//! the boundary so a provider cannot block by calling back through the view's COM apartment.

use super::{clipboard, inbox::Inbox, log, shell::{self, Pidl}};
use std::sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}};
use windows::Win32::Foundation::{ERROR_TIMEOUT, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows::Win32::UI::Shell::SHMultiFileProperties;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{Result, w};

pub const MSG_OPERATION: u32 = WM_APP + 0x12B;

pub enum Action {
    Address { text: String, request: usize },
    Delete { items: Vec<Pidl>, results: Option<usize> },
    Paste(Pidl, clipboard::Pending),
    NewFolder(Pidl, &'static str),
    Properties(Vec<Pidl>),
}

pub enum Response {
    Address { tab: usize, request: usize, destination: Result<Option<Pidl>> },
    Refresh { tab: usize, folder: Pidl },
    Deleted { tab: usize, results: usize, items: Vec<Pidl> },
    ClipboardTimedOut,
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

pub struct PasteDestination {
    tab: usize,
    folder: Pidl,
    input: Arc<Inbox<Option<Job>>>,
}

impl PasteDestination {
    pub fn submit(&self) {
        if !self.input.send_with(|| Some(Job {
            tab: self.tab,
            action: Action::Paste(self.folder.clone(), clipboard::snapshot()),
        })) {
            log::line("Files paste destination closed before submission");
        }
    }
}

impl Worker {
    pub fn new(hwnd: HWND) -> std::io::Result<Self> {
        let input = Arc::new(Inbox::default());
        static NEXT_WORKER: AtomicUsize = AtomicUsize::new(1);
        let id = NEXT_WORKER.fetch_add(1, Ordering::Relaxed);
        let output = Arc::new(Mutex::new(Some(Vec::new())));
        let mut bounds = RECT::default();
        let _ = unsafe { GetWindowRect(hwnd, &mut bounds) };
        let hwnd = hwnd.0 as isize;
        let jobs = input.clone();
        let replies = output.clone();
        std::thread::Builder::new().name("files-operations".into()).spawn(move || run(hwnd, bounds, id, jobs, replies))?;
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

    pub fn paste_destination(&self, tab: usize, folder: Pidl) -> PasteDestination {
        PasteDestination { tab, folder, input: self.input.clone() }
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

struct DialogOwner(HWND);

impl DialogOwner {
    fn new(bounds: RECT) -> Result<Self> {
        // Independent, hidden top-level window on the operation STA. Using the frame as an
        // owner would also destroy this window on close, before accepted work has drained.
        unsafe { CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, w!("STATIC"), w!(""), WS_POPUP,
            bounds.left, bounds.top, 0, 0, None, None, None, None).map(Self) }
    }
}

impl Drop for DialogOwner {
    fn drop(&mut self) {
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

fn run(frame: isize, bounds: RECT, id: usize, input: Arc<Inbox<Option<Job>>>, output: Arc<Mutex<Option<Vec<Response>>>>) {
    let receiver = input.attach();
    if let Err(error) = unsafe { OleInitialize(None) } {
        log::line(&format!("Files operation OLE initialization failed: {error}"));
        return;
    }
    let frame = HWND(frame as _);
    let owner = match DialogOwner::new(bounds) {
        Ok(owner) => {
            log::line(&format!("Files operation dialog owner for frame {:#x}: {:#x}", frame.0 as usize, owner.0.0 as usize));
            Some(owner)
        }
        Err(error) => {
            log::line(&format!("Files operation dialog owner failed: {error}"));
            None
        }
    };
    let operation_owner = owner.as_ref().map_or(HWND::default(), |owner| owner.0);
    let mut msg = MSG::default();
    'pump: loop {
        while let Some(job) = receiver.next() {
            let Some(job) = job else { break 'pump };
            if let Some(response) = perform(operation_owner, job) && let Some(responses) = output.lock().unwrap().as_mut() {
                responses.push(response);
                let _ = unsafe { PostMessageW(Some(frame), MSG_OPERATION, WPARAM(id), LPARAM(0)) };
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
    drop(owner);
    unsafe { OleUninitialize() };
}

fn with_dialog_owner<T>(owner: HWND, operation: impl FnOnce() -> T) -> T {
    let foreground = unsafe { GetForegroundWindow() };
    let result = operation();
    // The copy engine can explicitly hand focus to its owner after closing a prompt.
    // Restore the previous window only when the hidden owner actually has focus.
    unsafe {
        if !owner.is_invalid() && foreground != owner && GetForegroundWindow() == owner
            && IsWindowVisible(foreground).as_bool()
        {
            let _ = SetForegroundWindow(foreground);
        }
    }
    result
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
            let result = with_dialog_owner(owner, || shell::delete_items(owner, &items));
            log::line(&format!("command Delete: {result:?} (operation STA)"));
            return results.filter(|_| result.is_ok()).map(|results| Response::Deleted { tab, results, items });
        }
        Action::Paste(folder, source) => {
            let result = match source.wait() {
                Ok(mut source) => with_dialog_owner(owner, || shell::paste(owner, &folder, &mut source)),
                Err(error) if error.code() == ERROR_TIMEOUT.to_hresult() => {
                    log::line("command Paste: clipboard snapshot timed out (operation STA)");
                    return Some(Response::ClipboardTimedOut);
                }
                Err(error) => Err(error),
            };
            ("Paste", result, Some(folder))
        }
        Action::NewFolder(folder, name) => {
            let result = with_dialog_owner(owner, || {
                folder.item().and_then(|item| shell::new_folder(owner, &item, name))
            });
            ("New", result, Some(folder))
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
        let frame = DialogOwner::new(RECT::default()).unwrap();
        let worker = Worker::new(frame.0).unwrap();
        let (release, pending) = clipboard::Pending::fixture();
        worker.submit(1, Action::Paste(target.clone(), pending));
        worker.submit(1, Action::NewFolder(target, "queued folder"));
        drop(worker);
        drop(frame);
        release.send(Ok(clipboard::Source::test_cut(vec![source]))).unwrap();

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
