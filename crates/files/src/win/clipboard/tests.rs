use super::*;
use crate::win::{
    inbox::Inbox,
    operations::{Action, PasteTarget, Response, Worker},
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    thread,
    time::{Duration, Instant},
};
use windows::Win32::Foundation::E_NOTIMPL;
use windows::Win32::System::Com::{
    FORMATETC, IAdviseSink, IDataObject_Impl, IEnumFORMATETC, IEnumSTATDATA, STGMEDIUM,
};
use windows::Win32::System::Ole::{OleGetClipboard, OleInitialize, OleUninitialize};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
};
use windows_core::{BOOL, HRESULT, Ref, implement};

static CLIPBOARD: Mutex<()> = Mutex::new(());

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
        release();
        unsafe {
            let _ = OleSetClipboard(None::<&IDataObject>);
            OleUninitialize();
        }
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn parse(path: &Path) -> Pidl {
    Pidl::parse_result(path.to_str().unwrap()).unwrap()
}

fn until(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + timeout;
    loop {
        let mut msg = MSG::default();
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.hwnd.is_invalid() && msg.message == MSG_RETIRE {
                    retire(msg.wParam.0 != 0);
                } else {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
        if predicate() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn submit(worker: &Worker, folder: Pidl) {
    worker.submit(
        1,
        Action::Paste(PasteTarget::Folder(folder), snapshot_here()),
    );
}

#[test]
fn a_native_move_consumes_the_cut_even_when_the_source_path_is_recreated() {
    let _clipboard = CLIPBOARD.lock().unwrap_or_else(|lock| lock.into_inner());
    let fixture = Fixture::new("native-cut");
    let item = fixture.file("native-move.txt");
    let destination = fixture.folder("destination");
    publish_here(vec![item], true);
    assert!(
        can_paste(),
        "Cut did not publish file formats before returning"
    );
    let data = unsafe { OleGetClipboard().unwrap() };
    let original = fixture.0.join("native-move.txt");
    fs::rename(&original, fixture.0.join("moved-by-native.txt")).unwrap();
    data::set_effect(&data, CFSTR_PERFORMEDDROPEFFECT, DROPEFFECT_NONE).unwrap();
    data::set_effect(&data, CFSTR_PASTESUCCEEDED, DROPEFFECT_MOVE).unwrap();
    fs::write(&original, "replacement").unwrap();
    assert_eq!(
        fs::read_to_string(&original).as_deref().ok(),
        Some("replacement")
    );
    assert!(
        !can_paste(),
        "Paste stayed enabled while clipboard retirement was pending"
    );

    let worker = Worker::new(HWND::default()).unwrap();
    submit(&worker, destination.clone());
    worker.submit(1, Action::NewFolder(destination, "after paste"));
    assert!(until(Duration::from_secs(5), || fixture
        .0
        .join("destination/after paste")
        .is_dir()));
    assert_eq!(
        fs::read_to_string(original).as_deref().ok(),
        Some("replacement")
    );
    assert!(!fixture.0.join("destination/native-move.txt").exists());
    assert!(
        !can_paste(),
        "the consumed cut still advertises file formats"
    );
    println!(
        "Native paste consumed the cut, disabled Paste and left the recreated source untouched"
    );
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
    publish_here(vec![item], true);

    let reserved = snapshot_here().wait().unwrap();
    assert!(
        snapshot_here().wait().is_err(),
        "two destinations reserved the same cut"
    );
    drop(reserved);
    assert!(can_paste(), "Cut is available immediately");

    submit(&first, winner);
    submit(&second, loser.clone());
    let left = fixture.0.join("winner/move-only-once.txt");
    assert!(until(Duration::from_secs(5), || left.is_file() && !can_paste()));
    fs::write(fixture.0.join("move-only-once.txt"), "replacement").unwrap();
    submit(&second, loser.clone());
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
    println!(
        "Rapid pastes moved the original to one destination and left the replacement untouched"
    );
}

#[test]
fn a_newer_native_copy_is_not_replaced_by_retiring_an_older_cut() {
    let _clipboard = CLIPBOARD.lock().unwrap_or_else(|lock| lock.into_inner());
    let fixture = Fixture::new("native-copy-wins");
    let old = fixture.file("old-cut.txt");
    let next = fixture.file("native-copy.txt");
    publish_here(vec![old.clone()], true);
    let prior = unsafe { OleGetClipboard().unwrap() };
    let mut producer = None;
    publish_with(vec![old], true, |items, cut| {
        let data = data::selection(items, cut)?;
        producer = Some(ClipboardOwner::new(move || {
            data::selection(&[next], false).unwrap()
        }));
        Ok(data)
    });
    let _producer = producer.unwrap();
    assert!(
        local(|_| ()).is_none(),
        "the old object is still considered current"
    );
    data::set_effect(&prior, CFSTR_PERFORMEDDROPEFFECT, DROPEFFECT_NONE).unwrap();
    data::set_effect(&prior, CFSTR_PASTESUCCEEDED, DROPEFFECT_MOVE).unwrap();
    drop(prior);

    let pending = snapshot_here();
    let (resolved, result) = mpsc::channel();
    thread::spawn(move || {
        unsafe { OleInitialize(None).unwrap() };
        let _ = resolved.send(pending.wait());
        unsafe { OleUninitialize() };
    });
    let mut source = None;
    assert!(until(Duration::from_secs(5), || {
        source = result.try_recv().ok();
        source.is_some()
    }));
    let source = source.unwrap().unwrap();
    assert!(!source.is_cut());
    let paths: Vec<_> = source.items.iter().map(Pidl::display).collect();
    assert!(matches!(paths.as_slice(), [path] if path.ends_with(r"\native-copy.txt")), "{paths:?}");
    assert!(can_paste(), "retiring the old cut cleared a newer source");
    println!("A native copy during Cut preparation remained on the clipboard and supplied the paste selection");
}

#[test]
fn repeated_pastes_from_a_held_provider_do_not_block_the_next_clipboard_owner() {
    let _clipboard = CLIPBOARD.lock().unwrap_or_else(|lock| lock.into_inner());
    let fixture = Fixture::new("clipboard-delay");
    let foreign = fixture.file("foreign-render.txt");
    let next = fixture.file("healthy-native.txt");
    let blocked = fixture.folder("blocked");
    let ready = fixture.folder("ready");
    let (started, observed) = mpsc::channel();
    let provider = ClipboardOwner::new(move || {
        DelayedData {
            inner: shell::data_object(&[foreign]).unwrap(),
            started,
            held: AtomicBool::new(false),
        }
        .into()
    });
    let delayed = Worker::new(HWND::default()).unwrap();
    let other = Worker::new(HWND::default()).unwrap();
    let first = foreign::capture();
    let mut remaining = (0..3).map(|_| foreign::capture()).collect::<Vec<_>>();
    for pending in remaining.drain(..) {
        assert!(foreign::same_read(&first, &pending));
        delayed.submit(
            1,
            Action::Paste(
                PasteTarget::Folder(blocked.clone()),
                Pending::Foreign(pending),
            ),
        );
    }
    delayed.submit(
        1,
        Action::Paste(
            PasteTarget::Folder(blocked.clone()),
            Pending::Foreign(first),
        ),
    );
    delayed.submit(1, Action::NewFolder(blocked, "after timeout"));
    observed
        .recv_timeout(Duration::from_secs(3))
        .expect("foreign GetData was not called");

    let healthy = data::selection(&[next], false).unwrap();
    let switched = Instant::now();
    unsafe { OleSetClipboard(&healthy).unwrap() };
    submit(&other, ready);
    assert!(
        until(Duration::from_secs(4), || fixture
            .0
            .join("ready/healthy-native.txt")
            .is_file()),
        "a healthy external owner did not paste while the previous provider was held"
    );
    assert!(
        switched.elapsed() < Duration::from_secs(8),
        "the healthy producer waited for the held provider"
    );
    assert!(
        until(Duration::from_secs(4), || fixture
            .0
            .join("blocked/after timeout")
            .is_dir()),
        "accepted destination work did not continue after timeout"
    );
    assert_eq!(
        delayed
            .take()
            .iter()
            .filter(|response| matches!(response, Response::ClipboardTimedOut))
            .count(),
        4
    );
    drop(provider);
    assert!(
        !fixture.0.join("blocked/foreign-render.txt").exists(),
        "a late response was applied"
    );
    println!(
        "Four pastes shared one held reader, the next native source pasted, and the destination queue drained"
    );
}

struct ClipboardOwner {
    stop: Arc<Inbox<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl ClipboardOwner {
    fn new(create: impl FnOnce() -> IDataObject + Send + 'static) -> Self {
        let (ready, wait) = mpsc::channel();
        let stop = Arc::new(Inbox::default());
        let input = stop.clone();
        let thread = thread::spawn(move || {
            unsafe { OleInitialize(None).unwrap() };
            let receiver = input.attach();
            let object = create();
            unsafe { OleSetClipboard(&object).unwrap() };
            ready.send(()).unwrap();
            let mut msg = MSG::default();
            unsafe {
                while receiver.next().is_none() && GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            if unsafe { OleIsCurrentClipboard(object.as_raw()) == S_OK } {
                let _ = unsafe { OleSetClipboard(None::<&IDataObject>) };
            }
            drop(object);
            unsafe { OleUninitialize() };
        });
        assert!(until(Duration::from_secs(8), || wait.try_recv().is_ok()));
        Self { stop, thread: Some(thread) }
    }
}

impl Drop for ClipboardOwner {
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
