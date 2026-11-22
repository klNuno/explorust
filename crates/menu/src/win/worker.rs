//! The menu's worker: one STA thread per process that builds a selection's `IContextMenu` from the
//! parent folder (`IShellFolder::GetUIObjectOf`), lets its handlers fill an HMENU, reads it, and
//! later fills submenus, runs the command picked and tracks the classic menu. The UI thread shows
//! the static part meanwhile and merges the rows when the reply arrives (a message to its window).
//!
//! Each fill has a generation; the worker keeps the menu of the latest one until the next fill.

use super::{ClipboardRoute, classic, log, read};
use crate::model::Entry;
use std::cell::RefCell;
use std::sync::OnceLock;
use std::time::Instant;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Com::Marshal::CoMarshalInterThreadInterfaceInStream;
use windows::Win32::System::Com::StructuredStorage::CoGetInterfaceAndReleaseStream;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, IStream};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::IObjectWithSite;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{IContextMenu, IShellFolder, IShellView, SHGetDesktopFolder};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{IUnknown, Interface, w};

/// An ID list as bytes, terminator included, so it can cross threads.
pub type Pidl = Vec<u8>;

/// Copies an ID list into bytes.
pub fn pidl_bytes(p: *const ITEMIDLIST) -> Pidl {
    if p.is_null() {
        return vec![0, 0];
    }
    // SAFETY: ILGetSize walks a valid ID list and counts its terminator.
    unsafe {
        let n = windows::Win32::UI::Shell::ILGetSize(Some(p)) as usize;
        std::slice::from_raw_parts(p as *const u8, n.max(2)).to_vec()
    }
}

pub enum Job {
    /// Builds and fills the context menu of `children` (relative ID lists) of folder `folder`
    /// (absolute ID list, empty for the desktop).
    Fill { gn: u64, folder: Pidl, children: Vec<Pidl>, flags: u32, reply: isize, owner: isize, site: Option<usize> },
    /// Fills submenu `menu` (row `row` of level `depth`) and reads it.
    Sub { gn: u64, menu: isize, index: u32, depth: usize, row: usize, reply: isize },
    Invoke { gn: u64, id: u32, owner: isize, at: (i32, i32), shift: bool, ctrl: bool, route: Option<ClipboardRoute> },
    /// Tracks the classic menu of the same HMENU and runs what is picked.
    Classic { gn: u64, owner: isize, at: (i32, i32), route: Option<ClipboardRoute> },
}

pub enum Reply {
    Filled { gn: u64, entries: Vec<Entry>, fill_ms: f64, ok: bool },
    Sub { gn: u64, depth: usize, row: usize, entries: Vec<Entry> },
}

/// The message the UI's root window gets; lParam is a `Box<Reply>`.
pub fn reply_msg() -> u32 {
    static MSG: OnceLock<u32> = OnceLock::new();
    // SAFETY: registers (or finds) a message name.
    *MSG.get_or_init(|| unsafe { RegisterWindowMessageW(w!("explorust.menu.reply")) })
}

fn job_msg() -> u32 {
    WM_APP + 0x31
}

struct Current {
    gn: u64,
    cm: IContextMenu,
    menu: HMENU,
    site: Option<IObjectWithSite>,
}

impl Drop for Current {
    fn drop(&mut self) {
        // SAFETY: releases the site and destroys the menu this thread made.
        unsafe {
            if let Some(s) = &self.site {
                let _ = s.SetSite(None);
            }
            let _ = DestroyMenu(self.menu);
        }
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Current>> = const { RefCell::new(None) };
}

/// The worker's message window, made on first use.
fn window() -> Option<HWND> {
    static HWND_: OnceLock<isize> = OnceLock::new();
    let h = *HWND_.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new().name("menu-worker".into()).spawn(move || run(tx));
        if spawned.is_err() {
            return 0;
        }
        rx.recv().unwrap_or(0)
    });
    (h != 0).then_some(HWND(h as _))
}

/// Starts the worker ahead of the first menu.
pub fn start() {
    let _ = window();
}

pub fn send(job: Job) -> bool {
    let Some(h) = window() else { return false };
    let raw = Box::into_raw(Box::new(job));
    // SAFETY: the worker takes the box back in its window procedure.
    if unsafe { PostMessageW(Some(h), job_msg(), WPARAM(0), LPARAM(raw as isize)) }.is_err() {
        // SAFETY: not posted, so still ours.
        drop(unsafe { Box::from_raw(raw) });
        return false;
    }
    true
}

/// Marshals the view for the worker's menu site; the worker unmarshals it once.
pub fn marshal_site(view: &IShellView) -> Option<usize> {
    // SAFETY: standard cross-apartment marshalling of an interface of this thread.
    match unsafe { CoMarshalInterThreadInterfaceInStream(&IShellView::IID, view) } {
        Ok(s) => Some(s.into_raw() as usize),
        Err(e) => {
            log::line(&format!("worker: the view cannot be marshalled ({:?}), no site", e.code()));
            None
        }
    }
}

fn post(reply: isize, r: Reply) {
    let raw = Box::into_raw(Box::new(r));
    // SAFETY: the UI's window procedure takes the box back.
    if unsafe { PostMessageW(Some(HWND(reply as _)), reply_msg(), WPARAM(0), LPARAM(raw as isize)) }.is_err() {
        // SAFETY: not posted, so still ours.
        drop(unsafe { Box::from_raw(raw) });
    }
}

fn run(tx: std::sync::mpsc::Sender<isize>) {
    // SAFETY: COM for this thread only; a message window of a class registered here.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        // Copy and Cut run here (Job::Invoke): the shell puts the files on the clipboard with
        // OleSetClipboard, which needs OLE on the calling thread, and the data object lives as
        // long as this thread does.
        let ole = windows::Win32::System::Ole::OleInitialize(None);
        super::log::line(&format!("worker: OLE {ole:?}"));
        let inst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSW { lpfnWndProc: Some(proc_), hInstance: inst.into(), lpszClassName: w!("explorust.menu.worker"), ..Default::default() };
        RegisterClassW(&wc);
        let h = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("explorust.menu.worker"),
            w!(""),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(inst.into()),
            None,
        )
        .unwrap_or_default();
        let _ = tx.send(h.0 as isize);
        if h.is_invalid() {
            return;
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn proc_(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == job_msg() {
        // SAFETY: posted by `send` as a Box<Job>.
        let job = unsafe { Box::from_raw(lp.0 as *mut Job) };
        handle(*job);
        return LRESULT(0);
    }
    // SAFETY: default handling.
    unsafe { DefWindowProcW(h, msg, wp, lp) }
}

fn handle(job: Job) {
    match job {
        Job::Fill { gn, folder, children, flags, reply, owner, site } => {
            let t = Instant::now();
            CURRENT.with(|c| c.borrow_mut().take());
            let (entries, ok) = match fill(gn, &folder, &children, flags, HWND(owner as _), site) {
                Ok(e) => (e, true),
                Err(e) => {
                    log::line(&format!("worker: fill {gn} failed: {e}"));
                    (Vec::new(), false)
                }
            };
            let fill_ms = t.elapsed().as_secs_f64() * 1000.0;
            post(reply, Reply::Filled { gn, entries, fill_ms, ok });
        }
        Job::Sub { gn, menu, index, depth, row, reply } => {
            let entries = CURRENT.with(|c| {
                let c = c.borrow();
                let cur = c.as_ref().filter(|c| c.gn == gn)?;
                let sub = HMENU(menu as _);
                read::init_popup(&cur.cm, sub, index);
                Some(read::entries(&cur.cm, sub, super::FIRST))
            });
            post(reply, Reply::Sub { gn, depth, row, entries: entries.unwrap_or_default() });
        }
        Job::Invoke { gn, id, owner, at, shift, ctrl, route } => {
            let cm = CURRENT.with(|c| c.borrow().as_ref().filter(|c| c.gn == gn).map(|c| c.cm.clone()));
            match cm {
                Some(cm) => classic::invoke_with(&cm, HWND(owner as _), id, super::FIRST, POINT { x: at.0, y: at.1 }, shift, ctrl, route.as_ref()),
                None => log::line(&format!("worker: invoke {id} of stale menu {gn}")),
            }
        }
        Job::Classic { gn, owner, at, route } => {
            let cur = CURRENT.with(|c| c.borrow().as_ref().filter(|c| c.gn == gn).map(|c| (c.cm.clone(), c.menu)));
            let Some((cm, menu)) = cur else { return };
            let id = classic::track(&cm, menu, at.0, at.1);
            super::give_back(HWND(owner as _));
            if id >= super::FIRST {
                classic::invoke_with(&cm, HWND(owner as _), id, super::FIRST, POINT { x: at.0, y: at.1 }, false, false, route.as_ref());
            }
        }
    }
}

fn fill(gn: u64, folder: &[u8], children: &[Pidl], flags: u32, owner: HWND, site: Option<usize>) -> windows_core::Result<Vec<Entry>> {
    // SAFETY: COM calls on this STA; ID lists point into byte vectors that outlive the calls.
    unsafe {
        let desktop = SHGetDesktopFolder()?;
        let sf: IShellFolder =
            if folder.len() <= 2 { desktop } else { desktop.BindToObject(folder.as_ptr() as *const ITEMIDLIST, None)? };
        let apidl: Vec<*const ITEMIDLIST> = children.iter().map(|c| c.as_ptr() as *const ITEMIDLIST).collect();
        // No children: the folder's own background menu (prewarm only; the view's has more rows).
        let cm: IContextMenu = if apidl.is_empty() { sf.CreateViewObject(owner)? } else { sf.GetUIObjectOf(owner, &apidl, None)? };
        let site = site.and_then(|s| {
            let stream = IStream::from_raw(s as *mut _);
            let view: windows_core::Result<IUnknown> = CoGetInterfaceAndReleaseStream(&stream);
            std::mem::forget(stream);
            let ows = cm.cast::<IObjectWithSite>().ok()?;
            match view {
                Ok(v) => {
                    let _ = ows.SetSite(&v);
                    Some(ows)
                }
                Err(e) => {
                    log::line(&format!("worker: site unmarshal failed {:?}", e.code()));
                    None
                }
            }
        });
        let menu = CreatePopupMenu()?;
        let t = Instant::now();
        let hr = cm.QueryContextMenu(menu, 0, super::FIRST, super::LAST, flags);
        let tq = t.elapsed().as_secs_f64() * 1000.0;
        let entries = read::entries(&cm, menu, super::FIRST);
        log::line(&format!(
            "worker: fill {gn}: QueryContextMenu {tq:.1} ms ({hr:?}), {} entries read by {:.1} ms, site {}",
            entries.len(),
            t.elapsed().as_secs_f64() * 1000.0,
            site.is_some()
        ));
        CURRENT.with(|c| *c.borrow_mut() = Some(Current { gn, cm, menu, site }));
        Ok(entries)
    }
}
