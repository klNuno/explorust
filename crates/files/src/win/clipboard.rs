//! Files publishes synchronously on its command window STA so the clipboard outlives any view.
//! Windows owns clipboard ordering; the only local state
//! belongs to the exact IDataObject on the clipboard and tracks completion of its cut.

mod data;
mod foreign;

use super::{
    log,
    shell::{self, Pidl},
};
use crate::cut::{Cut, Reservation};
use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{
        Arc, LazyLock, Mutex, OnceLock, Weak,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
};
use windows::Win32::Foundation::{E_ABORT, ERROR_BUSY, HWND, S_OK, WPARAM};
use windows::Win32::System::Com::IDataObject;
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardSequenceNumber, IsClipboardFormatAvailable,
    OpenClipboard,
};
use windows::Win32::System::Ole::{DROPEFFECT_MOVE, DROPEFFECT_NONE, OleSetClipboard};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Shell::{
    CFSTR_FILEDESCRIPTORA, CFSTR_FILEDESCRIPTORW, CFSTR_PASTESUCCEEDED, CFSTR_PERFORMEDDROPEFFECT,
    CFSTR_SHELLIDLIST,
};
use windows::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, SendMessageW, WM_APP};
use windows_core::{Interface, Result};

windows_core::link!("ole32.dll" "system" fn OleIsCurrentClipboard(data: *mut core::ffi::c_void) -> windows_core::HRESULT);

pub const MSG_RETIRE: u32 = WM_APP + 0x12D;
pub const MSG_COMMAND: u32 = WM_APP + 0x12E;
type FileCut = Cut<Pidl>;

struct Publication {
    data: IDataObject,
    items: Vec<Pidl>,
    cut: Option<Arc<FileCut>>,
}

thread_local! {
    static PUBLISHED: RefCell<Option<Publication>> = const { RefCell::new(None) };
}

fn local<T>(f: impl FnOnce(&Publication) -> T) -> Option<T> {
    // OleIsCurrentClipboard returns S_FALSE for an object we no longer own.
    let data = PUBLISHED.with(|state| state.borrow().as_ref().map(|entry| entry.data.clone()))?;
    if unsafe { OleIsCurrentClipboard(data.as_raw()) } != S_OK {
        return None;
    }
    PUBLISHED.with(|state| {
        state
            .borrow()
            .as_ref()
            .filter(|entry| entry.data == data)
            .map(f)
    })
}

enum Command {
    Publish(Vec<Pidl>, bool),
    Snapshot(mpsc::Sender<Pending>),
}

static REQUESTS: LazyLock<Mutex<HashMap<usize, Command>>> = LazyLock::new(Default::default);
/// The toolbar can see a cut's completion on any view STA, even before Windows sends its update.
static ACTIVE_CUT: Mutex<Option<(u32, Weak<FileCut>)>> = Mutex::new(None);

fn send(window: HWND, command: Command) {
    static NEXT: AtomicUsize = AtomicUsize::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    REQUESTS.lock().unwrap().insert(id, command);
    // A token identifies the request. The window processes it before focus returns.
    unsafe { SendMessageW(window, MSG_COMMAND, Some(WPARAM(id)), None) };
    REQUESTS.lock().unwrap().remove(&id);
}

pub fn execute(id: usize) {
    let command = REQUESTS.lock().unwrap().remove(&id);
    match command {
        Some(Command::Publish(items, cut)) => publish_here(items, cut),
        Some(Command::Snapshot(reply)) => {
            let _ = reply.send(snapshot_here());
        }
        None => {}
    }
}

pub fn publish(window: HWND, items: Vec<Pidl>, cut: bool) {
    send(window, Command::Publish(items, cut));
}

fn publish_here(items: Vec<Pidl>, cut: bool) {
    publish_with(items, cut, data::selection);
}

fn publish_with(
    items: Vec<Pidl>,
    cut: bool,
    prepare: impl FnOnce(&[Pidl], bool) -> Result<IDataObject>,
) {
    let verb = if cut { "Cut" } else { "Copy" };
    let result = (|| -> Result<()> {
        let previous = unsafe { GetClipboardSequenceNumber() };
        let inner = prepare(&items, cut)?;
        let state = cut.then(|| Arc::new(Cut::default()));
        let data = match &state {
            Some(cut) => data::CutData::wrap(inner, cut.clone(), unsafe { GetCurrentThreadId() }),
            None => inner,
        };
        // A different app may have published while we were creating the object.
        if unsafe { GetClipboardSequenceNumber() } != previous {
            return Err(E_ABORT.into());
        }
        unsafe { OleSetClipboard(&data)? };
        let sequence = unsafe { GetClipboardSequenceNumber() };
        *ACTIVE_CUT.lock().unwrap() = state
            .as_ref()
            .filter(|_| unsafe { OleIsCurrentClipboard(data.as_raw()) } == S_OK)
            .map(|cut| (sequence, Arc::downgrade(cut)));
        PUBLISHED.with(|slot| {
            *slot.borrow_mut() = Some(Publication {
                data,
                items,
                cut: state,
            });
        });
        foreign::release();
        Ok(())
    })();
    log::line(&format!("command {verb}: {result:?} (Files STA)"));
}

pub fn can_paste() -> bool {
    let sequence = unsafe { GetClipboardSequenceNumber() };
    let local_cut = ACTIVE_CUT
        .lock()
        .unwrap()
        .as_ref()
        .filter(|(published, _)| *published == sequence)
        .and_then(|(_, cut)| cut.upgrade());
    if let Some(cut) = local_cut {
        return !cut.consumed();
    }
    static SHELL_FORMATS: OnceLock<[u32; 3]> = OnceLock::new();
    let formats = SHELL_FORMATS.get_or_init(|| {
        [
            CFSTR_SHELLIDLIST,
            CFSTR_FILEDESCRIPTORW,
            CFSTR_FILEDESCRIPTORA,
        ]
        .map(|name| data::format(name).map_or(0, |format| format.cfFormat as u32))
    });
    (unsafe { IsClipboardFormatAvailable(data::CF_HDROP as u32).is_ok() })
        || formats
            .iter()
            .any(|&format| format != 0 && unsafe { IsClipboardFormatAvailable(format).is_ok() })
}

pub fn release() {
    ACTIVE_CUT.lock().unwrap().take();
    PUBLISHED.with(|slot| slot.borrow_mut().take());
    foreign::release();
}

fn request_retirement(thread: u32, acknowledge: bool) {
    let _ = unsafe {
        PostThreadMessageW(
            thread,
            MSG_RETIRE,
            WPARAM(acknowledge as usize),
            Default::default(),
        )
    };
}

pub fn retire(notify: bool) {
    let sequence = unsafe { GetClipboardSequenceNumber() };
    if let Some(Some(object)) = local(|source| {
        source
            .cut
            .as_ref()
            .filter(|cut| cut.consumed())
            .map(|_| source.data.clone())
    }) {
        // Delayed rendering may have advanced the native sequence since publication.
        if let Some((published, _)) = ACTIVE_CUT.lock().unwrap().as_mut() {
            *published = sequence;
        }
        if notify {
            acknowledge(&object);
        }
        log::line(&format!(
            "cut completed: clipboard retired {:?}",
            clear(sequence)
        ));
    }
}

pub(super) fn clear(sequence: u32) -> Result<bool> {
    unsafe {
        OpenClipboard(None)?;
        let result = if GetClipboardSequenceNumber() == sequence {
            EmptyClipboard().map(|()| true)
        } else {
            Ok(false)
        };
        let _ = CloseClipboard();
        result
    }
}

pub enum Pending {
    Local(Result<Source>),
    Foreign(foreign::Pending),
    #[cfg(test)]
    Fixture(mpsc::Receiver<Source>),
}

pub fn snapshot(window: HWND) -> Pending {
    let (reply, result) = mpsc::channel();
    send(window, Command::Snapshot(reply));
    result
        .try_recv()
        .unwrap_or(Pending::Local(Err(E_ABORT.into())))
}

fn snapshot_here() -> Pending {
    let thread = unsafe { GetCurrentThreadId() };
    if let Some((items, cut)) = local(|source| (source.items.clone(), source.cut.clone())) {
        Pending::Local(Source::new(items, cut.as_ref(), Acknowledge::Local(thread)))
    } else {
        Pending::Foreign(foreign::capture())
    }
}

impl Pending {
    pub fn wait(self) -> Result<Source> {
        match self {
            Self::Local(source) => source,
            Self::Foreign(source) => source.wait(),
            #[cfg(test)]
            Self::Fixture(receive) => receive
                .recv_timeout(std::time::Duration::from_secs(2))
                .map_err(|_| E_ABORT.into()),
        }
    }
}

enum Acknowledge {
    Local(u32),
    Foreign(Arc<foreign::Read>),
}

pub struct Source {
    pub items: Vec<Pidl>,
    reservation: Option<Reservation<Pidl>>,
    acknowledge: Acknowledge,
}

impl Source {
    fn new(
        mut items: Vec<Pidl>,
        cut: Option<&Arc<FileCut>>,
        acknowledge: Acknowledge,
    ) -> Result<Self> {
        let reservation = cut
            .map(|cut| {
                cut.reserve(&mut items)
                    .ok_or_else(|| windows_core::Error::from_hresult(ERROR_BUSY.to_hresult()))
            })
            .transpose()?;
        Ok(Self {
            items,
            reservation,
            acknowledge,
        })
    }

    pub fn is_cut(&self) -> bool {
        self.reservation.is_some()
    }

    pub fn for_drop(&self) -> Result<IDataObject> {
        match &self.acknowledge {
            Acknowledge::Foreign(read) => read.native_data(),
            Acknowledge::Local(thread) => {
                let data = data::selection(&self.items, self.is_cut())?;
                Ok(match &self.reservation {
                    Some(cut) => data::CutData::wrap(data, cut.cut().clone(), *thread),
                    None => data,
                })
            }
        }
    }

    pub fn drop_completed(&mut self, object: &IDataObject) {
        if let Acknowledge::Foreign(read) = &self.acknowledge
            && self.is_cut()
            && data::effect(object, CFSTR_PASTESUCCEEDED) == Some(DROPEFFECT_MOVE)
        {
            self.reservation.take().unwrap().finish(Vec::new(), true);
            read.moved(false);
        }
    }

    pub fn finish_move(&mut self, moved: Vec<Pidl>, complete: bool) {
        if let Some(reservation) = self.reservation.take() {
            reservation.finish(moved, complete);
            if complete {
                match &self.acknowledge {
                    Acknowledge::Local(thread) => request_retirement(*thread, true),
                    Acknowledge::Foreign(read) => read.moved(true),
                }
            }
        }
    }

    #[cfg(test)]
    pub fn test_cut(items: Vec<Pidl>) -> Self {
        Self::new(
            items,
            Some(&Arc::new(Cut::default())),
            Acknowledge::Local(0),
        )
        .unwrap()
    }
}

fn acknowledge(data: &IDataObject) {
    // The copy engine has already performed the move. The source must not delete by filename.
    let performed = data::set_effect(data, CFSTR_PERFORMEDDROPEFFECT, DROPEFFECT_NONE);
    let succeeded = data::set_effect(data, CFSTR_PASTESUCCEEDED, DROPEFFECT_MOVE);
    log::line(&format!(
        "cut completed: performed {performed:?}, paste succeeded {succeeded:?}"
    ));
}

#[cfg(test)]
mod tests;
