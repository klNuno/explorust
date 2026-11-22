//! Files orders clipboard commands independently of the OLE publisher and foreign readers.
//! A delayed source can block either OLE call, so local operations use the ordered selection
//! directly while Windows catches up.

use super::{
    inbox::Inbox,
    log,
    shell::{self, Pidl},
};
use crate::cut::{Cut, Reservation};
use std::sync::{
    Arc, LazyLock, Mutex,
    atomic::{AtomicU32, AtomicUsize, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{
    E_ABORT, E_FAIL, E_INVALIDARG, ERROR_BUSY, ERROR_TIMEOUT, GlobalFree, HWND, S_OK,
};
use windows::Win32::System::Com::{
    DVASPECT_CONTENT, FORMATETC, IDataObject, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardOwner, GetClipboardSequenceNumber, OpenClipboard,
    RegisterClipboardFormatW,
};
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::Win32::System::Ole::{
    DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_MOVE, DROPEFFECT_NONE, OleGetClipboard, OleInitialize,
    OleSetClipboard, OleUninitialize, ReleaseStgMedium,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, DispatchMessageW, GetMessageW, GetWindowThreadProcessId, MSG,
    TranslateMessage, WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows_core::{Interface, PCWSTR, Result, w};

// The generated wrapper treats S_FALSE as success. We need the exact ownership result.
windows_core::link!("ole32.dll" "system" fn OleIsCurrentClipboard(data: *mut core::ffi::c_void) -> windows_core::HRESULT);

const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(2);
const FOREIGN_READERS: usize = 4;

pub struct Source {
    pub items: Vec<Pidl>,
    move_request: Option<MoveRequest>,
}

type FileCut = Cut<Pidl>;

impl Source {
    pub fn is_cut(&self) -> bool {
        self.move_request.is_some()
    }

    pub fn finish_move(&mut self, moved: Vec<Pidl>, complete: bool) {
        if let Some(request) = &mut self.move_request {
            request.reservation.take().unwrap().finish(moved, complete);
            request.complete = complete;
        }
    }

    #[cfg(test)]
    pub fn test_cut(mut items: Vec<Pidl>) -> Self {
        let cut = Arc::new(Cut::default());
        let move_request = Some(MoveRequest::local(&cut, &mut items).unwrap());
        Self {
            items,
            move_request,
        }
    }
}

struct MoveRequest {
    reservation: Option<Reservation<Pidl>>,
    outcome: Outcome,
    complete: bool,
}

enum Outcome {
    Local(Arc<FileCut>),
    Foreign(Arc<Inbox<bool>>),
}

impl MoveRequest {
    fn local(cut: &Arc<FileCut>, items: &mut Vec<Pidl>) -> Result<Self> {
        let reservation = cut
            .reserve(items)
            .ok_or_else(|| windows_core::Error::from_hresult(ERROR_BUSY.to_hresult()))?;
        Ok(Self {
            reservation: Some(reservation),
            outcome: Outcome::Local(cut.clone()),
            complete: false,
        })
    }
}

impl Drop for MoveRequest {
    fn drop(&mut self) {
        drop(self.reservation.take());
        match &self.outcome {
            Outcome::Local(cut) if self.complete => publish_native(Publish::Consumed(cut.clone())),
            Outcome::Foreign(result) => {
                result.send(self.complete);
            }
            _ => {}
        }
    }
}

pub struct Pending {
    receive: mpsc::Receiver<Result<Source>>,
    deadline: Instant,
}

impl Pending {
    pub fn wait(self) -> Result<Source> {
        self.receive
            .recv_timeout(self.deadline.saturating_duration_since(Instant::now()))
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => {
                    windows_core::Error::from_hresult(ERROR_TIMEOUT.to_hresult())
                }
                mpsc::RecvTimeoutError::Disconnected => E_ABORT.into(),
            })?
    }

    #[cfg(test)]
    pub fn fixture() -> (mpsc::Sender<Result<Source>>, Self) {
        let (reply, receive) = mpsc::channel();
        (
            reply,
            Self {
                receive,
                deadline: Instant::now() + SNAPSHOT_TIMEOUT,
            },
        )
    }
}

struct Capture {
    sequence: u32,
    deadline: Instant,
    reply: mpsc::Sender<Result<Source>>,
}

impl Capture {
    fn expired(&self) -> bool {
        Instant::now() >= self.deadline
    }

    fn send(self, result: Result<Source>) -> bool {
        let result = if self.expired() {
            Err(ERROR_TIMEOUT.to_hresult().into())
        } else {
            result
        };
        self.reply.send(result).is_ok()
    }
}

struct Publication {
    previous_sequence: u32,
    items: Vec<Pidl>,
    cut: Option<Arc<FileCut>>,
}

impl Publication {
    fn is_preferred(&self, sequence: u32) -> bool {
        self.previous_sequence == sequence
            || unsafe {
                GetClipboardOwner().is_ok_and(|window| {
                    GetWindowThreadProcessId(window, None)
                        == PUBLISHER_THREAD.load(Ordering::Relaxed)
                })
            }
    }

    fn reserve(&self) -> Result<Source> {
        let mut items = self.items.clone();
        let move_request = self
            .cut
            .as_ref()
            .map(|cut| MoveRequest::local(cut, &mut items))
            .transpose()?;
        Ok(Source {
            items,
            move_request,
        })
    }
}

enum Command {
    Publish(Arc<Publication>),
    Snapshot(Capture),
}

enum Publish {
    Set(Arc<Publication>),
    Consumed(Arc<FileCut>),
}

static LATEST: Mutex<Option<Arc<Publication>>> = Mutex::new(None);
static PUBLISHER_THREAD: AtomicU32 = AtomicU32::new(0);

static COMMANDS: LazyLock<Option<Arc<Inbox<Command>>>> = LazyLock::new(|| {
    let input = Arc::new(Inbox::default());
    let worker = input.clone();
    match std::thread::Builder::new()
        .name("files-clipboard".into())
        .spawn(move || run(worker))
    {
        Ok(_) => Some(input),
        Err(error) => {
            log::line(&format!("could not start Files clipboard worker: {error}"));
            None
        }
    }
});

static PUBLISHER: LazyLock<Option<Arc<Inbox<Publish>>>> = LazyLock::new(|| {
    let input = Arc::new(Inbox::default());
    let worker = input.clone();
    match std::thread::Builder::new()
        .name("files-clipboard-owner".into())
        .spawn(move || run_publisher(worker))
    {
        Ok(_) => Some(input),
        Err(error) => {
            log::line(&format!(
                "could not start Files clipboard publisher: {error}"
            ));
            None
        }
    }
});

pub fn publish(items: Vec<Pidl>, cut: bool) {
    let previous_sequence = unsafe { GetClipboardSequenceNumber() };
    send(Command::Publish(Arc::new(Publication {
        previous_sequence,
        items,
        cut: cut.then(|| Arc::new(Cut::default())),
    })));
}

pub fn snapshot() -> Pending {
    let (reply, receive) = mpsc::channel();
    let sequence = unsafe { GetClipboardSequenceNumber() };
    let deadline = Instant::now() + SNAPSHOT_TIMEOUT;
    send(Command::Snapshot(Capture {
        sequence,
        deadline,
        reply,
    }));
    Pending { receive, deadline }
}

fn send(command: Command) {
    if !COMMANDS.as_ref().is_some_and(|input| input.send(command)) {
        log::line("Files clipboard worker is unavailable");
    }
}

fn publish_native(command: Publish) {
    if !PUBLISHER.as_ref().is_some_and(|input| input.send(command)) {
        log::line("Files clipboard publisher is unavailable");
    }
}

fn run(input: Arc<Inbox<Command>>) {
    let receiver = input.attach();
    log::line(&format!("Files clipboard queue ready on thread {}", unsafe {
        GetCurrentThreadId()
    }));
    let mut foreign = (0, Arc::new(Cut::default()));
    pump(|| {
        while let Some(command) = receiver.next() {
            match command {
                Command::Publish(source) => {
                    *LATEST.lock().unwrap() = Some(source.clone());
                    publish_native(Publish::Set(source));
                }
                Command::Snapshot(request) => {
                    let current = unsafe { GetClipboardSequenceNumber() };
                    let latest = LATEST.lock().unwrap().clone();
                    if request.expired() {
                        request.send(Err(ERROR_TIMEOUT.to_hresult().into()));
                    } else if let Some(entry) =
                        latest.as_ref().filter(|entry| entry.is_preferred(current))
                    {
                        request.send(entry.reserve());
                    } else if request.sequence != current {
                        request.send(Err(E_ABORT.into()));
                    } else {
                        if foreign.0 != current {
                            foreign = (current, Arc::new(Cut::default()));
                        }
                        start_foreign(request, foreign.1.clone());
                    }
                }
            }
        }
        None::<()>
    });
}

fn run_publisher(input: Arc<Inbox<Publish>>) {
    let receiver = input.attach();
    if let Err(error) = unsafe { OleInitialize(None) } {
        log::line(&format!(
            "Files clipboard OLE initialization failed: {error}"
        ));
        return;
    }
    PUBLISHER_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::Relaxed);
    // Retain a recognizable owner across EmptyClipboard, so a local Cut queued just
    // before retirement still supersedes the retired version.
    let owner = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("STATIC"),
            w!(""),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            None,
            None,
        )
    }
    .ok();
    let mut published = None::<(Arc<Publication>, IDataObject)>;
    pump(|| {
        while let Some(command) = receiver.next() {
            match command {
                Publish::Set(source) => {
                    let current = unsafe { GetClipboardSequenceNumber() };
                    if !LATEST
                        .lock()
                        .unwrap()
                        .as_ref()
                        .is_some_and(|latest| Arc::ptr_eq(latest, &source))
                        || !source.is_preferred(current)
                    {
                        continue;
                    }
                    if source.cut.as_ref().is_some_and(|cut| cut.consumed()) {
                        // A local paste can finish before OLE has a chance to publish its cut.
                        // In that case clear the clipboard version it was going to replace.
                        let retired = retire_cut(current, owner);
                        log::line(&format!(
                            "cut completed before OLE publication: clipboard retired {retired:?}"
                        ));
                        continue;
                    }
                    let verb = if source.cut.is_some() { "Cut" } else { "Copy" };
                    match put(&source) {
                        Ok(data) => {
                            log::line(&format!("command {verb}: Ok(()) (clipboard STA)"));
                            published = Some((source, data));
                        }
                        Err(error) => {
                            log::line(&format!("command {verb}: {error:?} (clipboard STA)"))
                        }
                    }
                }
                Publish::Consumed(cut) => {
                    let current = unsafe { GetClipboardSequenceNumber() };
                    if let Some((_, data)) = published.as_ref().filter(|(entry, data)| {
                        entry
                            .cut
                            .as_ref()
                            .is_some_and(|before| Arc::ptr_eq(before, &cut))
                            && owns(data)
                    }) {
                        acknowledge_move(data, current, owner);
                    }
                }
            }
        }
        None::<()>
    });
    PUBLISHER_THREAD.store(0, Ordering::Relaxed);
    drop(published);
    if let Some(owner) = owner {
        let _ = unsafe { DestroyWindow(owner) };
    }
    unsafe { OleUninitialize() };
}

fn owns(data: &IDataObject) -> bool {
    unsafe { OleIsCurrentClipboard(data.as_raw()) == S_OK }
}

// Dispatch normal and COM window messages until the inbox reports completion.
fn pump<T>(mut next: impl FnMut() -> Option<T>) -> Option<T> {
    let mut msg = MSG::default();
    loop {
        if let Some(value) = next() {
            return Some(value);
        }
        unsafe {
            if GetMessageW(&mut msg, None, 0, 0).0 <= 0 {
                return None;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

struct ReaderPermit;

impl ReaderPermit {
    fn take() -> Option<Self> {
        READERS
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |live| {
                (live < FOREIGN_READERS).then_some(live + 1)
            })
            .ok()
            .map(|_| Self)
    }
}

static READERS: AtomicUsize = AtomicUsize::new(0);

impl Drop for ReaderPermit {
    fn drop(&mut self) {
        READERS.fetch_sub(1, Ordering::Relaxed);
    }
}

fn start_foreign(request: Capture, cut: Arc<FileCut>) {
    let Some(permit) = ReaderPermit::take() else {
        request.send(Err(ERROR_TIMEOUT.to_hresult().into()));
        return;
    };
    if let Err(error) = std::thread::Builder::new()
        .name("files-clipboard-reader".into())
        .spawn(move || {
            let _permit = permit;
            match unsafe { OleInitialize(None) } {
                Ok(()) => {
                    read_foreign(request, &cut);
                    unsafe { OleUninitialize() };
                }
                Err(error) => {
                    request.send(Err(error));
                }
            }
        })
    {
        log::line(&format!("could not start Files clipboard reader: {error}"));
    }
}

fn read_foreign(request: Capture, cut: &Arc<FileCut>) {
    let sequence = request.sequence;
    let captured = unsafe {
        (|| {
            if GetClipboardSequenceNumber() != sequence {
                return Err(E_ABORT.into());
            }
            let data = OleGetClipboard()?;
            let items = read_items(&data)?;
            if items.is_empty() {
                return Err(E_INVALIDARG.into());
            }
            let moving = read_cut(&data)?;
            if GetClipboardSequenceNumber() != sequence {
                return Err(E_ABORT.into());
            }
            Ok((data, items, moving))
        })()
    };
    let (data, mut items, moving) = match captured {
        Ok(source) => source,
        Err(error) => {
            request.send(Err(error));
            return;
        }
    };
    if !moving {
        request.send(Ok(Source {
            items,
            move_request: None,
        }));
        return;
    }
    if request.expired() {
        request.send(Err(ERROR_TIMEOUT.to_hresult().into()));
        return;
    }
    let Some(reservation) = cut.reserve(&mut items) else {
        request.send(Err(ERROR_BUSY.to_hresult().into()));
        return;
    };
    let outcome = Arc::new(Inbox::default());
    let receive = outcome.attach();
    let move_request = MoveRequest {
        reservation: Some(reservation),
        outcome: Outcome::Foreign(outcome.clone()),
        complete: false,
    };
    if request.send(Ok(Source {
        items,
        move_request: Some(move_request),
    })) && pump(|| receive.next()) == Some(true)
    {
        acknowledge_move(&data, sequence, None);
    }
}

fn acknowledge_move(data: &IDataObject, sequence: u32, owner: Option<HWND>) {
    unsafe {
        // IFileOperation already moved the source. NONE tells a source that the move was
        // optimized, so it must not delete the original paths itself.
        if GetClipboardSequenceNumber() != sequence {
            return;
        }
        let performed = set_effect(data, CFSTR_PERFORMEDDROPEFFECT, DROPEFFECT_NONE);
        if GetClipboardSequenceNumber() != sequence {
            return;
        }
        let succeeded = set_effect(data, CFSTR_PASTESUCCEEDED, DROPEFFECT_MOVE);
        let retired = retire_cut(sequence, owner);
        log::line(&format!(
            "cut completed: performed {performed:?}, paste succeeded {succeeded:?}, clipboard retired {retired:?}"
        ));
    }
}

fn retire_cut(sequence: u32, owner: Option<HWND>) -> Result<bool> {
    for delay in [2, 4, 8, 16] {
        match clear_cut(sequence, owner) {
            Err(error)
                if error.code() == windows::Win32::Foundation::ERROR_ACCESS_DENIED.to_hresult() =>
            {
                std::thread::sleep(Duration::from_millis(delay));
            }
            result => return result,
        }
    }
    clear_cut(sequence, owner)
}

fn clear_cut(sequence: u32, owner: Option<HWND>) -> Result<bool> {
    unsafe {
        if GetClipboardSequenceNumber() != sequence {
            return Ok(false);
        }
        OpenClipboard(owner)?;
        // Compare and clear under the clipboard lock, so a newer publication survives.
        let result = if GetClipboardSequenceNumber() == sequence {
            EmptyClipboard().map(|()| true)
        } else {
            Ok(false)
        };
        let _ = CloseClipboard();
        result
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
        let items = (0..DragQueryFileW(drop_handle, u32::MAX, None))
            .map(|i| {
                let len = DragQueryFileW(drop_handle, i, None);
                let mut path = vec![0; len as usize + 1];
                let written = DragQueryFileW(drop_handle, i, Some(&mut path));
                if written == 0 {
                    return Err(E_FAIL.into());
                }
                Pidl::parse_result(&String::from_utf16_lossy(&path[..written as usize]))
            })
            .collect();
        ReleaseStgMedium(&mut storage);
        items
    }
}

unsafe fn read_cut(data: &IDataObject) -> Result<bool> {
    unsafe {
        let Ok(mut storage) = data.GetData(&effect_format(CFSTR_PREFERREDDROPEFFECT)?) else {
            return Ok(false);
        };
        if storage.tymed != TYMED_HGLOBAL.0 as u32 {
            ReleaseStgMedium(&mut storage);
            return Err(E_FAIL.into());
        }
        let h = storage.u.hGlobal;
        let effect = if GlobalSize(h) >= size_of::<u32>() {
            let value = GlobalLock(h).cast::<u32>();
            if value.is_null() {
                None
            } else {
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

fn put(source: &Publication) -> Result<IDataObject> {
    let data = shell::data_object(&source.items)?;
    let effect = if source.cut.is_some() {
        DROPEFFECT_MOVE
    } else {
        DROPEFFECT_COPY
    };
    set_effect(&data, CFSTR_PREFERREDDROPEFFECT, effect)?;
    unsafe { OleSetClipboard(&data)? };
    Ok(data)
}

fn set_effect(data: &IDataObject, name: PCWSTR, effect: DROPEFFECT) -> Result<()> {
    unsafe {
        let format = effect_format(name)?;
        let h = GlobalAlloc(GMEM_MOVEABLE, size_of::<u32>())?;
        let value = GlobalLock(h).cast::<u32>();
        if value.is_null() {
            let _ = GlobalFree(Some(h));
            return Err(E_FAIL.into());
        }
        *value = effect.0;
        let _ = GlobalUnlock(h);
        let storage = STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 { hGlobal: h },
            ..Default::default()
        };
        if let Err(error) = data.SetData(&format, &storage, true) {
            let _ = GlobalFree(Some(h));
            return Err(error);
        }
        Ok(())
    }
}

fn effect_format(name: PCWSTR) -> Result<FORMATETC> {
    let id = unsafe { RegisterClipboardFormatW(name) };
    if id == 0 {
        Err(windows_core::Error::from_thread())
    } else {
        Ok(format(id as u16))
    }
}

fn format(id: u16) -> FORMATETC {
    FORMATETC {
        cfFormat: id,
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests;
