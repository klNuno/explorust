//! Where the keyboard menu (menu key, Shift+F10) opens: the focused item's rectangle, asked from
//! UI Automation on a thread of its own. UIA calls back into the view's thread for the item, so
//! the view's thread waits for the answer with `CoWaitForMultipleHandles`, which dispatches those
//! calls and sent messages meanwhile.

use super::log;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;
use windows::Win32::Foundation::{CloseHandle, HANDLE, RECT};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoWaitForMultipleHandles,
};
use windows::Win32::System::Threading::{CreateEventW, SetEvent};
use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation};

/// One question: the answer and the event set when it is in. The event closes with the last owner.
struct Ask {
    rect: Mutex<Option<RECT>>,
    done: HANDLE,
}

impl Drop for Ask {
    fn drop(&mut self) {
        // SAFETY: the event this struct made; no owner is left to set it.
        unsafe {
            let _ = CloseHandle(self.done);
        }
    }
}

// SAFETY: an event handle can be set and waited on from any thread.
unsafe impl Send for Ask {}
unsafe impl Sync for Ask {}

fn thread() -> Option<&'static Mutex<Sender<Arc<Ask>>>> {
    static TX: OnceLock<Option<Mutex<Sender<Arc<Ask>>>>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = channel::<Arc<Ask>>();
        std::thread::Builder::new()
            .name("menu-uia".into())
            .spawn(move || {
                // SAFETY: COM for this thread only (UIA clients run in the MTA).
                let uia: Option<IUIAutomation> = unsafe {
                    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                    CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
                        .map_err(|e| log::line(&format!("uia: no automation object ({:?})", e.code())))
                        .ok()
                };
                for ask in rx {
                    // SAFETY: UIA calls on this thread's object; the event is the asker's.
                    unsafe {
                        let r = uia.as_ref().and_then(|u| u.GetFocusedElement().ok()).and_then(|e| e.CurrentBoundingRectangle().ok());
                        *ask.rect.lock().unwrap_or_else(|p| p.into_inner()) = r;
                        let _ = SetEvent(ask.done);
                    }
                }
            })
            .ok()?;
        Some(Mutex::new(tx))
    })
    .as_ref()
}

/// Starts the thread ahead of the first keyboard menu.
pub fn start() {
    let _ = thread();
}

/// The focused element's screen rectangle, or `None` after `wait_ms`.
pub fn focused_rect(wait_ms: u32) -> Option<RECT> {
    let t = Instant::now();
    let tx = thread()?;
    // SAFETY: an unnamed manual-reset event owned by `Ask`.
    let done = unsafe { CreateEventW(None, true, false, windows_core::PCWSTR::null()) }.ok()?;
    let ask = Arc::new(Ask { rect: Mutex::new(None), done });
    tx.lock().ok()?.send(ask.clone()).ok()?;
    // SAFETY: waits on our event; COM calls and sent messages for this thread run meanwhile.
    let r = unsafe { CoWaitForMultipleHandles(0, wait_ms, &[done]) };
    let rect = if r.is_ok() { *ask.rect.lock().unwrap_or_else(|p| p.into_inner()) } else { None };
    log::line(&format!("uia: focused element {rect:?} in {:.1} ms", t.elapsed().as_secs_f64() * 1000.0));
    rect
}
