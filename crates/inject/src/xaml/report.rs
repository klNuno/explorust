//! The TAP's log: lines go to the loader's message-only window in explorust by `WM_COPYDATA` (the
//! loader writes them to `inject.log`), since a packaged host's AppData writes may be redirected.
//! Settings' threads only queue them: one thread of ours sends, so a slow or absent loader never
//! stalls Settings' UI thread; a full queue drops lines and says how many.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, HWND_MESSAGE, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_COPYDATA};
use windows::core::{HSTRING, PCWSTR};

/// `COPYDATASTRUCT::dwData` of a TAP log batch: UTF-8 lines.
pub const COPYDATA_TAG: usize = 0x5841_4d4c;
/// Bytes per `WM_COPYDATA` at most; the loader refuses bigger ones.
pub const MAX_MESSAGE: usize = 64 * 1024;
/// Batches waiting for the sender at most.
const QUEUE: usize = 64;

static TX: OnceLock<Option<SyncSender<String>>> = OnceLock::new();
static DROPPED: AtomicUsize = AtomicUsize::new(0);

/// Queues lines (joined by `\n`) for the loader; never waits.
pub fn send(lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    let tx = TX.get_or_init(|| {
        let (tx, rx) = sync_channel(QUEUE);
        std::thread::Builder::new().name("explorust-xaml-log".into()).spawn(move || sender(rx)).ok().map(|_| tx)
    });
    let queued = tx.as_ref().is_some_and(|tx| tx.try_send(lines.join("\n")).is_ok());
    if !queued {
        DROPPED.fetch_add(lines.len(), Ordering::Relaxed);
    }
}

pub fn line(msg: String) {
    send(&[msg]);
}

/// Joins what waits into messages of at most `MAX_MESSAGE` bytes and sends each one.
fn sender(rx: Receiver<String>) {
    while let Ok(first) = rx.recv() {
        let mut text = first;
        while text.len() < MAX_MESSAGE
            && let Ok(more) = rx.try_recv()
        {
            text.push('\n');
            text.push_str(&more);
        }
        let dropped = DROPPED.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            text.push_str(&format!("\n{dropped} lines dropped (log queue full)"));
        }
        let mut rest = text.as_str();
        while !rest.is_empty() {
            let mut cut = rest.len().min(MAX_MESSAGE);
            while !rest.is_char_boundary(cut) {
                cut -= 1;
            }
            // A cut inside a line ends it there; the rest starts a new line in the log.
            if cut < rest.len()
                && let Some(nl) = rest[..cut].rfind('\n')
            {
                cut = nl + 1;
            }
            deliver(&rest[..cut]);
            rest = &rest[cut..];
        }
    }
}

fn deliver(text: &str) {
    // SAFETY: a window lookup and a synchronous send whose buffer outlives the call.
    unsafe {
        let Ok(to) = FindWindowExW(Some(HWND_MESSAGE), None, &HSTRING::from(crate::WINDOW_CLASS), PCWSTR::null()) else { return };
        let cds = COPYDATASTRUCT { dwData: COPYDATA_TAG, cbData: text.len() as u32, lpData: text.as_ptr() as *mut _ };
        let _ = SendMessageTimeoutW(to, WM_COPYDATA, WPARAM(0), LPARAM(&cds as *const _ as isize), SMTO_ABORTIFHUNG, 500, None);
    }
}
