//! Decode a foreign clipboard object once per generation, away from the view and destination.
//! An unresponsive provider can delay only its own generation.

use super::{Acknowledge, Arc, FileCut, Pidl, Source, acknowledge, clear, data, shell};
use crate::win::{inbox::Inbox, log};
use std::{
    cell::RefCell,
    sync::{Condvar, Mutex},
    time::{Duration, Instant},
};
use windows::Win32::Foundation::{CLIPBRD_E_CANT_OPEN, E_ABORT, E_INVALIDARG, ERROR_TIMEOUT, S_OK};
use windows::Win32::System::Com::{
    CoCancelCall, CoDisableCallCancellation, CoEnableCallCancellation, IDataObject, TYMED_HGLOBAL,
};
use windows::Win32::System::DataExchange::GetClipboardSequenceNumber;
use windows::Win32::System::Ole::{
    DROPEFFECT_MOVE, OleGetClipboard, OleInitialize, OleUninitialize, ReleaseStgMedium,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Shell::{
    CFSTR_FILEDESCRIPTORA, CFSTR_FILEDESCRIPTORW, CFSTR_PREFERREDDROPEFFECT, DragQueryFileW, HDROP,
    IShellItemArray, SHCreateShellItemArrayFromDataObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MSG, TranslateMessage,
};
use windows_core::Result;

const TIMEOUT: Duration = Duration::from_secs(2);

type Selection = (Vec<Pidl>, bool);

#[derive(Default)]
struct State {
    reader: u32,
    selection: Option<(Instant, Result<Selection>)>,
}

#[derive(Default)]
struct Reply {
    state: Mutex<State>,
    ready: Condvar,
}

pub struct Read {
    sequence: u32,
    reply: Arc<Reply>,
    outcome: Arc<Inbox<Outcome>>,
    cut: Arc<FileCut>,
}

enum Outcome {
    Released,
    Moved { acknowledge: bool },
}

pub struct Pending {
    read: Arc<Read>,
    deadline: Instant,
}

#[cfg(test)]
pub fn same_read(a: &Pending, b: &Pending) -> bool {
    Arc::ptr_eq(&a.read, &b.read)
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<Read>>> = const { RefCell::new(None) };
}

pub fn release() {
    CURRENT.with(|slot| slot.borrow_mut().take());
}

pub fn capture() -> Pending {
    let sequence = unsafe { GetClipboardSequenceNumber() };
    let read = CURRENT.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(read) = slot.as_ref().filter(|read| read.sequence == sequence) {
            if !matches!(
                &read.reply.state.lock().unwrap().selection,
                Some((_, Err(error))) if error.code() == CLIPBRD_E_CANT_OPEN
            ) {
                return read.clone();
            }
        }
        let read = Arc::new(Read::start(sequence));
        *slot = Some(read.clone());
        read
    });
    Pending {
        read,
        deadline: Instant::now() + TIMEOUT,
    }
}

impl Pending {
    pub fn wait(self) -> Result<Source> {
        let reply = &self.read.reply;
        let state = reply.state.lock().unwrap();
        let (mut state, _) = reply
            .ready
            .wait_timeout_while(
                state,
                self.deadline.saturating_duration_since(Instant::now()),
                |state| state.selection.is_none(),
            )
            .unwrap();
        let selection = match &state.selection {
            Some((ready, selection)) if *ready <= self.deadline => selection.clone(),
            _ => {
                let reader = std::mem::take(&mut state.reader);
                if reader != 0 {
                    // Hold the state lock so the STA cannot exit and recycle its thread ID.
                    let _ = unsafe { CoCancelCall(reader, 0) };
                }
                Err(ERROR_TIMEOUT.to_hresult().into())
            }
        };
        drop(state);
        let (items, cut) = selection?;
        Source::new(
            items,
            cut.then_some(&self.read.cut),
            Acknowledge::Foreign(self.read.clone()),
        )
    }
}

impl Read {
    fn start(sequence: u32) -> Self {
        let reply = Arc::new(Reply::default());
        let outcome = Arc::new(Inbox::default());
        let output = reply.clone();
        let done = outcome.clone();
        if std::thread::Builder::new()
            .name("files-clipboard-read".into())
            .spawn(move || run(sequence, &output, &done))
            .is_err()
        {
            reply.state.lock().unwrap().selection = Some((Instant::now(), Err(E_ABORT.into())));
        }
        Self {
            sequence,
            reply,
            outcome,
            cut: Arc::new(FileCut::default()),
        }
    }

    pub fn native_data(&self) -> Result<IDataObject> {
        get(self.sequence)
    }

    pub fn moved(&self, acknowledge: bool) {
        self.outcome.send(Outcome::Moved { acknowledge });
    }
}

impl Drop for Read {
    fn drop(&mut self) {
        self.outcome.close(Outcome::Released, |_| true);
    }
}

fn get(sequence: u32) -> Result<IDataObject> {
    unsafe {
        if GetClipboardSequenceNumber() != sequence {
            return Err(E_ABORT.into());
        }
        let data = OleGetClipboard()?;
        if GetClipboardSequenceNumber() != sequence {
            return Err(E_ABORT.into());
        }
        Ok(data)
    }
}

fn run(sequence: u32, reply: &Reply, outcome: &Inbox<Outcome>) {
    let receive = outcome.attach();
    let ole = unsafe { OleInitialize(None) };
    let captured = ole.clone().and_then(|()| {
        let cancellation = unsafe { CoEnableCallCancellation(None) }.is_ok();
        if cancellation {
            reply.state.lock().unwrap().reader = unsafe { GetCurrentThreadId() };
        }
        let result =
            get(sequence).and_then(|object| inspect(&object).map(|selection| (object, selection)));
        reply.state.lock().unwrap().reader = 0;
        if cancellation {
            let _ = unsafe { CoDisableCallCancellation(None) };
        }
        result
    });
    let selection = captured
        .as_ref()
        .map(|(_, selection)| selection.clone())
        .map_err(Clone::clone);
    reply.state.lock().unwrap().selection = Some((Instant::now(), selection));
    reply.ready.notify_all();

    if let Ok((object, (_, true))) = captured.as_ref() {
        let mut msg = MSG::default();
        let outcome = unsafe {
            loop {
                if let Some(moved) = receive.next() {
                    break moved;
                }
                if GetMessageW(&mut msg, None, 0, 0).0 <= 0 {
                    break Outcome::Released;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        };
        if let Outcome::Moved { acknowledge: notify } = outcome
            && unsafe { GetClipboardSequenceNumber() } == sequence
        {
            if notify {
                acknowledge(object);
            }
            log::line(&format!(
                "foreign cut completed: clipboard retired {:?}",
                clear(sequence)
            ));
        }
    }
    drop(captured);
    if ole.is_ok() {
        unsafe { OleUninitialize() };
    }
}

fn inspect(object: &IDataObject) -> Result<Selection> {
    let items = read_items(object).or_else(|error| {
        // Virtual files, for example a mail attachment, are handed to the native drop target.
        [CFSTR_FILEDESCRIPTORW, CFSTR_FILEDESCRIPTORA]
            .into_iter()
            .any(|name| {
                data::format(name)
                    .is_ok_and(|format| unsafe { object.QueryGetData(&format) == S_OK })
            })
            .then(Vec::new)
            .ok_or(error)
    })?;
    let effect = data::effect(object, CFSTR_PREFERREDDROPEFFECT);
    Ok((items, effect == Some(DROPEFFECT_MOVE)))
}

fn read_items(object: &IDataObject) -> Result<Vec<Pidl>> {
    unsafe {
        if let Ok(items) = SHCreateShellItemArrayFromDataObject::<_, IShellItemArray>(object) {
            return shell::item_ids(&items);
        }
        let mut storage = object.GetData(&data::format_id(data::CF_HDROP))?;
        let result = (|| {
            if storage.tymed != TYMED_HGLOBAL.0 as u32 {
                return Err(E_INVALIDARG.into());
            }
            let drop_handle = HDROP(storage.u.hGlobal.0);
            (0..DragQueryFileW(drop_handle, u32::MAX, None))
                .map(|i| {
                    let len = DragQueryFileW(drop_handle, i, None);
                    let mut path = vec![0; len as usize + 1];
                    let written = DragQueryFileW(drop_handle, i, Some(&mut path));
                    if written == 0 {
                        return Err(E_INVALIDARG.into());
                    }
                    Pidl::parse_result(&String::from_utf16_lossy(&path[..written as usize]))
                })
                .collect()
        })();
        ReleaseStgMedium(&mut storage);
        result
    }
}
