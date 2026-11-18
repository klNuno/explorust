//! One-line shell actions the hotkeys start (Win+R, Win+I). They run on a thread of their own so the
//! keyboard hook returns at once.

use crate::log;
use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};
use std::sync::Mutex;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCSTR, w};

#[derive(Clone, Copy, Debug)]
pub enum Action {
    /// Win+R: Windows' own Run dialog (shell32 `RunFileDlg`).
    RunDialog,
    /// A URI handed to ShellExecute: Win+I Settings, Win+Shift+S screen clip, and the like.
    Uri(&'static str),
}

static TX: OnceLock<Mutex<Sender<Action>>> = OnceLock::new();

pub fn spawn() {
    let (tx, rx) = channel::<Action>();
    let _ = TX.set(Mutex::new(tx));
    let _ = std::thread::Builder::new().name("actions".into()).spawn(move || {
        // SAFETY: COM for this thread, needed by the Run dialog and ShellExecute.
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        for a in rx {
            log::line(&format!("action {a:?}"));
            run(a);
        }
    });
}

/// Queues an action. False before `spawn`.
pub fn send(a: Action) -> bool {
    TX.get().and_then(|m| m.lock().ok()).is_some_and(|tx| tx.send(a).is_ok())
}

fn run(a: Action) {
    match a {
        Action::RunDialog => run_dialog(),
        // SAFETY: plain ShellExecute of a URI.
        Action::Uri(uri) => unsafe {
            ShellExecuteW(None, w!("open"), &HSTRING::from(uri), None, None, SW_SHOWNORMAL);
        },
    }
}

type RunFileDlg = unsafe extern "system" fn(isize, isize, isize, isize, isize, u32) -> i32;

fn run_dialog() {
    // SAFETY: shell32 exports RunFileDlg as ordinal 61 since Windows 95; called with no owner,
    // default icon, directory, title and prompt. It is modal, which is fine on this thread.
    unsafe {
        let Ok(shell32) = LoadLibraryW(w!("shell32.dll")) else { return };
        let Some(f) = GetProcAddress(shell32, PCSTR(61 as *const u8)) else {
            log::line("RunFileDlg not found");
            return;
        };
        let f: RunFileDlg = std::mem::transmute(f);
        f(0, 0, 0, 0, 0, 0);
    }
}
