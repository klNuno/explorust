//! Each Files window has its own STA for native view calls. The bus thread dispatches jobs.

use super::{frame, gfx, inbox::Inbox, log, shell, ui_language};
use shell::Pidl;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows::Win32::UI::WindowsAndMessaging::*;

pub enum Destination {
    Home,
    Path(String, bool),
    Folder(Pidl),
}

pub struct Open {
    pub destination: Destination,
    pub position: Option<(i32, i32)>,
    pub requested: Instant,
    pub queued_ms: u32,
}

enum Job {
    OpenHome(Open),
    Close,
}

#[derive(Clone)]
struct Window {
    inbox: Arc<Inbox<Job>>,
    ready: Arc<AtomicBool>,
    folder: Arc<Mutex<Option<String>>>,
}

struct Sessions {
    open: Vec<Window>,
    spare: Option<Window>,
    offset: u8,
    closing: bool,
}

pub struct Windows {
    cmd: isize,
    sessions: Mutex<Sessions>,
}

thread_local! {
    static OWNER: RefCell<Weak<Windows>> = const { RefCell::new(Weak::new()) };
}

pub fn open_folder(place: Pidl, at: Option<(i32, i32)>) {
    OWNER.with(|owner| {
        if let Some(owner) = owner.borrow().upgrade() {
            owner.open(Open { destination: Destination::Folder(place), position: at, requested: Instant::now(), queued_ms: 0 });
        }
    });
}

impl Windows {
    pub fn new(cmd: HWND) -> Arc<Self> {
        Arc::new(Self {
            cmd: cmd.0 as isize,
            sessions: Mutex::new(Sessions { open: Vec::new(), spare: None, offset: 0, closing: false }),
        })
    }

    pub fn prewarm(self: &Arc<Self>) {
        let mut state = self.sessions.lock().unwrap();
        if state.closing || state.spare.as_ref().is_some_and(|spare| !spare.inbox.closed()) {
            return;
        }
        let at = state.position();
        state.spare = self.spawn(at, None);
    }

    pub fn open(self: &Arc<Self>, request: Open) {
        let mut state = self.sessions.lock().unwrap();
        if state.closing {
            return;
        }
        state.open.retain(|window| !window.inbox.closed());
        if matches!(request.destination, Destination::Home)
            && request.position.is_none()
            && let Some(spare) = state.spare.take_if(|spare| spare.ready.load(Ordering::Acquire))
            && spare.inbox.send(Job::OpenHome(Open { destination: Destination::Home, ..request }))
        {
            state.open.push(spare);
            return;
        }
        let at = request.position.unwrap_or_else(|| state.position());
        if let Some(window) = self.spawn(at, Some(request)) {
            state.open.push(window);
        }
    }

    pub fn folders_for_reload(&self) -> Vec<String> {
        let mut state = self.sessions.lock().unwrap();
        state.closing = true;
        state.open.iter().filter(|window| !window.inbox.closed())
            .filter_map(|window| window.folder.lock().unwrap().clone()).collect()
    }

    pub fn shutdown(&self) {
        let mut state = self.sessions.lock().unwrap();
        state.closing = true;
        if let Some(spare) = state.spare.take() {
            spare.inbox.stop(Job::Close);
        }
        for window in state.open.drain(..) {
            window.inbox.stop(Job::Close);
        }
    }

    fn spawn(self: &Arc<Self>, at: (i32, i32), request: Option<Open>) -> Option<Window> {
        let window = Window {
            inbox: Arc::new(Inbox::default()),
            ready: Arc::new(AtomicBool::new(false)),
            folder: Arc::new(Mutex::new(None)),
        };
        let worker = window.clone();
        let owner = Arc::downgrade(self);
        let cmd = self.cmd;
        if let Err(error) = std::thread::Builder::new().name("files-view".into()).spawn(move || {
            run(owner, cmd, worker, request, at);
        }) {
            log::line(&format!("could not start a Files view thread: {error}"));
            return None;
        }
        Some(window)
    }
}

impl Sessions {
    fn position(&mut self) -> (i32, i32) {
        let n = i32::from(self.offset);
        self.offset = (self.offset + 1) % 6;
        (60 + 28 * n, 30 + 28 * n)
    }
}

struct Ole;

impl Drop for Ole {
    fn drop(&mut self) {
        unsafe { OleUninitialize() };
    }
}

fn run(owner: Weak<Windows>, cmd: isize, window: Window, request: Option<Open>, at: (i32, i32)) {
    let jobs = window.inbox.attach();
    if window.inbox.closed() {
        return;
    }
    if let Err(error) = unsafe { OleInitialize(None) } {
        log::line(&format!("Files view OLE initialization failed: {error}"));
        return;
    }
    let _ole = Ole;
    OWNER.with(|slot| *slot.borrow_mut() = owner);
    let start = Instant::now();
    // A blocking provider can only delay its own window.
    let destination = request.as_ref().map_or(&Destination::Home, |request| &request.destination);
    let Some((place, selection, label)) = resolve(destination) else { return };
    if window.inbox.closed() {
        return;
    }
    let sh = match gfx::Shared::new() {
        Ok(sh) => sh,
        Err(error) => {
            log::line(&format!("Files view graphics initialization failed: {error}"));
            return;
        }
    };
    frame::init(sh, crate::text::for_language(&ui_language()), HWND(cmd as _));
    let hwnd = match frame::create(&place, at.0, at.1, request.is_none(), window.folder) {
        Ok(hwnd) => hwnd,
        Err(error) => {
            log::line(&format!("opening {label:?} failed: {error}"));
            frame::clear_thread();
            return;
        }
    };
    if let Some(request) = request {
        show(hwnd, &place, selection, &label, &request, false);
    } else {
        window.ready.store(true, Ordering::Release);
        log::line(&format!("Home spare prepared in {:.1} ms", start.elapsed().as_secs_f64() * 1000.0));
    }
    let mut msg = MSG::default();
    loop {
        match jobs.next() {
            Some(Job::OpenHome(request)) => show(hwnd, &place, None, "Home", &request, true),
            Some(Job::Close) => break,
            None => {}
        }
        if unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 <= 0 {
            break;
        }
        if !frame::middle_click(&msg) && !frame::accelerator(&msg) {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
    frame::clear_thread();
}

fn resolve(destination: &Destination) -> Option<(Pidl, Option<Pidl>, String)> {
    match destination {
        Destination::Home => Some((shell::home()?, None, String::from("Home"))),
        Destination::Folder(folder) => Some((folder.clone(), None, folder.display())),
        Destination::Path(path, select) => {
            let item = match Pidl::parse_result(path) {
                Ok(item) => item,
                Err(error) => {
                    log::line(&format!("open {path:?}: navigation failed (HRESULT {:#010x}): {error}", error.code().0 as u32));
                    return None;
                }
            };
            let parent = (*select || !item.is_folder()).then(|| item.parent()).flatten();
            let (folder, selection) = parent.map_or_else(|| (item.clone(), None), |folder| (folder, Some(item.clone())));
            Some((folder, selection, path.clone()))
        }
    }
}

fn show(hwnd: HWND, place: &Pidl, selection: Option<Pidl>, label: &str, request: &Open, prepared: bool) {
    frame::set_opened(hwnd, request.requested);
    if let Some(item) = selection {
        frame::select_after(hwnd, item);
    }
    let before = request.requested.elapsed().as_secs_f64() * 1000.0;
    let (show_ms, draw_ms, activate_ms) = frame::show(hwnd);
    let presented = before + show_ms + draw_ms;
    let _ = unsafe { DwmFlush() };
    let composed = request.requested.elapsed().as_secs_f64() * 1000.0;
    let origin = if prepared { "prepared window" } else { "new window" };
    log::line(&format!(
        "open {label}: first frame presented {presented:.1} ms after the command ({origin}, queued {} ms; \
         prepare {before:.1}, ShowWindow {show_ms:.1}, draw and Present {draw_ms:.1}), activated after it in \
         {activate_ms:.1} ms, composed by {composed:.1} ms", request.queued_ms
    ));
    frame::announce(hwnd, place);
}
