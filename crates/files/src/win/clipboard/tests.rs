use super::*;
use crate::win::operations::{Action, Worker};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    thread,
};
use windows::Win32::Foundation::{E_NOTIMPL, HWND};
use windows::Win32::System::Com::{IAdviseSink, IDataObject_Impl, IEnumFORMATETC, IEnumSTATDATA};
use windows_core::{BOOL, HRESULT, Ref, implement};

static CLIPBOARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Fixture(PathBuf);

impl Fixture {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("explorust-{name}-{}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        unsafe { OleInitialize(None).unwrap() };
        Self(path)
    }

    fn file(&self, name: &str) -> Pidl {
        let path = self.0.join(name);
        fs::write(&path, name).unwrap();
        parse(&path)
    }

    fn folder(&self, name: &str) -> Pidl {
        let path = self.0.join(name);
        fs::create_dir(&path).unwrap();
        parse(&path)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        unsafe { OleUninitialize() };
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn parse(path: &Path) -> Pidl {
    Pidl::parse_result(path.to_str().unwrap()).unwrap()
}

fn until(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + timeout;
    loop {
        if predicate() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_cut_can_only_be_reserved_by_one_destination_at_a_time() {
    let _clipboard = CLIPBOARD.lock().unwrap_or_else(|lock| lock.into_inner());
    let fixture = Fixture::new("cut");
    let item = fixture.file("move-only-once.txt");
    let winner = fixture.folder("winner");
    let loser = fixture.folder("loser");
    let first = Worker::new(HWND::default()).unwrap();
    let second = Worker::new(HWND::default()).unwrap();
    publish(vec![item], true);
    let reserved = snapshot().wait().unwrap();
    let competing = snapshot().wait();
    assert!(competing.is_err(), "two destinations reserved the same cut");
    drop(reserved);
    assert!(until(Duration::from_secs(5), crate::win::menus::file_clipboard));
    let queued_before_retirement = unsafe { GetClipboardSequenceNumber() };

    let move_into =
        |worker: &Worker, folder: &Pidl| worker.paste_destination(1, folder.clone()).submit();
    move_into(&first, &winner);
    move_into(&second, &loser);
    let left = fixture.0.join("winner/move-only-once.txt");
    assert!(until(Duration::from_secs(5), || left.is_file()));
    assert!(until(Duration::from_secs(5), || {
        !crate::win::menus::file_clipboard()
    }));
    fs::write(fixture.0.join("move-only-once.txt"), "replacement").unwrap();
    move_into(&second, &loser);
    second.submit(1, Action::NewFolder(loser, "after paste"));
    assert!(until(Duration::from_secs(5), || fixture
        .0
        .join("loser/after paste")
        .is_dir()));
    assert!(!fixture.0.join("loser/move-only-once.txt").exists());
    assert_eq!(
        fs::read_to_string(fixture.0.join("move-only-once.txt")).unwrap(),
        "replacement"
    );
    let next = fixture.file("copied-after-cut.txt");
    send(Command::Publish(Arc::new(Publication {
        previous_sequence: queued_before_retirement,
        items: vec![next.clone()],
        cut: None,
    })));
    assert_eq!(snapshot().wait().unwrap().items, [next]);
    assert!(until(Duration::from_secs(5), crate::win::menus::file_clipboard));
    println!(
        "Rapid pastes moved the original to winner only, cleared the clipboard, preserved the replacement and kept a newer copy"
    );
}

#[test]
fn a_blocked_provider_does_not_hold_local_pastes_or_destination_mutations() {
    let _clipboard = CLIPBOARD.lock().unwrap_or_else(|lock| lock.into_inner());
    let fixture = Fixture::new("clipboard-delay");
    let foreign = fixture.file("foreign-render.txt");
    let local = fixture.file("local-cut.txt");
    let blocked = fixture.folder("blocked");
    let ready = fixture.folder("ready");
    let (provider, started) = BlockedProvider::new(foreign);
    let delayed = Worker::new(HWND::default()).unwrap();
    let other = Worker::new(HWND::default()).unwrap();
    delayed.paste_destination(1, blocked.clone()).submit();
    delayed.submit(1, Action::NewFolder(blocked, "after timeout"));
    started
        .recv_timeout(Duration::from_secs(3))
        .expect("foreign GetData was not called");
    publish(vec![local], true);
    other.paste_destination(1, ready).submit();
    assert!(
        until(Duration::from_secs(5), || fixture
            .0
            .join("ready/local-cut.txt")
            .is_file()),
        "local Cut and Paste did not finish while the provider was blocked"
    );
    assert!(
        until(Duration::from_secs(4), || fixture
            .0
            .join("blocked/after timeout")
            .is_dir()),
        "New Folder did not drain after the snapshot deadline"
    );
    assert!(delayed.take().into_iter().any(|response| matches!(
        response,
        crate::win::operations::Response::ClipboardTimedOut
    )));
    drop(provider);
    assert!(until(Duration::from_secs(3), || READERS
        .load(Ordering::Relaxed)
        == 0));
    assert!(
        !fixture.0.join("blocked/foreign-render.txt").exists(),
        "late snapshot was applied"
    );
    println!(
        "Local cut arrived while GetData was held, New Folder drained on timeout, and the late clipboard reply was ignored"
    );
}

struct BlockedProvider {
    stop: Arc<Inbox<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl BlockedProvider {
    fn new(item: Pidl) -> (Self, mpsc::Receiver<()>) {
        let (ready, wait) = mpsc::channel();
        let (started, observed) = mpsc::channel();
        let stop = Arc::new(Inbox::default());
        let input = stop.clone();
        let thread = thread::spawn(move || {
            unsafe { OleInitialize(None).unwrap() };
            let receiver = input.attach();
            let inner = shell::data_object(&[item]).unwrap();
            let object: IDataObject = DelayedData {
                inner,
                started,
                held: AtomicBool::new(false),
            }
            .into();
            unsafe { OleSetClipboard(&object).unwrap() };
            ready.send(()).unwrap();
            pump(|| receiver.next());
            if unsafe { OleIsCurrentClipboard(object.as_raw()) == S_OK } {
                let _ = unsafe { OleSetClipboard(None::<&IDataObject>) };
            }
            drop(object);
            unsafe { OleUninitialize() };
        });
        wait.recv_timeout(Duration::from_secs(5)).unwrap();
        (
            Self {
                stop,
                thread: Some(thread),
            },
            observed,
        )
    }
}

impl Drop for BlockedProvider {
    fn drop(&mut self) {
        self.stop.send(());
        let thread = self.thread.take().unwrap();
        thread.thread().unpark();
        thread.join().unwrap();
    }
}

#[implement(IDataObject, Agile = false)]
struct DelayedData {
    inner: IDataObject,
    started: mpsc::Sender<()>,
    held: AtomicBool,
}

impl IDataObject_Impl for DelayedData_Impl {
    fn GetData(&self, format: *const FORMATETC) -> Result<STGMEDIUM> {
        if !self.held.swap(true, Ordering::Relaxed) && self.started.send(()).is_ok() {
            thread::park_timeout(Duration::from_secs(10));
        }
        unsafe { self.inner.GetData(format) }
    }

    fn GetDataHere(&self, _: *const FORMATETC, _: *mut STGMEDIUM) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn QueryGetData(&self, format: *const FORMATETC) -> HRESULT {
        unsafe { self.inner.QueryGetData(format) }
    }
    fn GetCanonicalFormatEtc(&self, _: *const FORMATETC, _: *mut FORMATETC) -> HRESULT {
        E_NOTIMPL
    }
    fn SetData(&self, _: *const FORMATETC, _: *const STGMEDIUM, _: BOOL) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn EnumFormatEtc(&self, direction: u32) -> Result<IEnumFORMATETC> {
        unsafe { self.inner.EnumFormatEtc(direction) }
    }
    fn DAdvise(&self, _: *const FORMATETC, _: u32, _: Ref<IAdviseSink>) -> Result<u32> {
        Err(E_NOTIMPL.into())
    }
    fn DUnadvise(&self, _: u32) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn EnumDAdvise(&self) -> Result<IEnumSTATDATA> {
        Err(E_NOTIMPL.into())
    }
}
