//! Windows side: the HMENU reader, the classic menu, the Windows 11 popup and the view hook.

mod classic;
mod log;
mod paint;
mod popup;
mod read;
mod tip;
mod uia;
mod worker;

use crate::model;
use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::Common::{ITEMIDLIST, STRRET};
use windows::Win32::UI::Shell::{IPersistFolder2, IShellFolder, SHGDNF, SHGDN_FORPARSING, SHGDN_INFOLDER, SVGIO_ALLVIEW, StrRetToBufW};
use windows::Win32::Foundation::{ERROR_SUCCESS, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Globalization::GetUserDefaultUILanguage;
use windows::Win32::System::Ole::IObjectWithSite;
use windows::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, RegCloseKey, RegOpenKeyExW};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, SetFocus, VK_CONTROL, VK_SHIFT};
use windows::Win32::UI::Shell::{
    DefSubclassProc, IContextMenu, IFolderView2, IShellView, RemoveWindowSubclass, SVGIO_BACKGROUND,
    SVGIO_SELECTION, SetWindowSubclass,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{BOOL, Interface};

/// The first command id given to `QueryContextMenu`.
pub(crate) const FIRST: u32 = 1;
const LAST: u32 = 0x7FFF;
const CMF_CANRENAME: u32 = 0x10;
const CMF_EXTENDEDVERBS: u32 = 0x100;
const SUBCLASS_ID: usize = 0x6D65_6E75;

thread_local! {
    /// The hooked views of this thread: SHELLDLL_DefView and its IShellView.
    static VIEWS: RefCell<Vec<(HWND, IShellView)>> = const { RefCell::new(Vec::new()) };
}

/// Shows the context menu of `cm` at a screen point and runs the command picked. `items` is the
/// selection's menu (top row, rename), not the background's; `classic` requests the extended
/// classic menu explicitly (Shift held).
pub fn show(owner: HWND, cm: &IContextMenu, x: i32, y: i32, items: bool, classic: bool) {
    let prefer_classic = classic || classic_preferred();
    show_at(owner, cm, POINT { x, y }, items, prefer_classic, classic, Instant::now());
}

fn classic_preferred() -> bool {
    let path = windows_core::w!(r"Software\Classes\CLSID\{86ca1aa0-34aa-4e8b-a509-50c905bae2a2}\InprocServer32");
    [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE].into_iter().any(|root| {
        let mut key = HKEY::default();
        unsafe {
            if RegOpenKeyExW(root, path, None, KEY_QUERY_VALUE, &mut key) != ERROR_SUCCESS {
                return false;
            }
            let _ = RegCloseKey(key);
        }
        true
    })
}

/// The menu of `cm`, filled on this thread (after the first frame when a menu of its kind was
/// shown before).
fn show_at(owner: HWND, cm: &IContextMenu, at: POINT, items: bool, classic: bool, extended: bool, t0: Instant) {
    // SAFETY: a menu this thread creates, fills through the handler and destroys.
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else { return };
    let mut flags = if items { CMF_CANRENAME } else { 0 };
    if classic {
        if extended {
            flags |= CMF_EXTENDEDVERBS;
        }
        let hr = unsafe { cm.QueryContextMenu(menu, 0, FIRST, LAST, flags) };
        let tq = t0.elapsed().as_secs_f64() * 1000.0;
        log::line(&format!("classic menu ({}), handlers filled it in {tq:.1} ms ({hr:?})", if items { "items" } else { "background" }));
        let id = classic::track(cm, menu, at.x, at.y);
        give_back(owner);
        if id >= FIRST {
            classic::invoke(cm, owner, id, FIRST, at);
        }
        let _ = unsafe { DestroyMenu(menu) };
        return;
    }
    let key = if items { "items:local".to_string() } else { background_key(owner) };
    let mut fill = || {
        let t = Instant::now();
        // SAFETY: the handlers fill our menu on this thread.
        let hr = unsafe { cm.QueryContextMenu(menu, 0, FIRST, LAST, flags) };
        let tq = t.elapsed().as_secs_f64() * 1000.0;
        let entries = read::entries(cm, menu, FIRST);
        log_entries(items, tq, hr, &entries, t.elapsed().as_secs_f64() * 1000.0);
        entries
    };
    let req = popup::Request {
        src: popup::Src::Local(cm.clone()),
        key,
        items,
        more: more_label(),
        owner,
        at,
        t0,
        fill: Some(&mut fill),
    };
    let pick = popup::run(req);
    give_back(owner);
    match pick {
        Some(popup::Pick::Command(id, _)) => classic::invoke(cm, owner, id, FIRST, at),
        Some(popup::Pick::More) => {
            let id = classic::track(cm, menu, at.x, at.y);
            give_back(owner);
            if id >= FIRST {
                classic::invoke(cm, owner, id, FIRST, at);
            }
        }
        Some(popup::Pick::Fallback) | None => log::line("dismissed"),
    }
    let _ = unsafe { DestroyMenu(menu) };
}

fn log_entries(items: bool, tq: f64, hr: windows_core::HRESULT, entries: &[model::Entry], tr: f64) {
    let owner_drawn = entries.iter().filter(|e| e.owner_draw).count();
    let verbs: Vec<String> = entries
        .iter()
        .map(|e| match e.kind {
            model::Kind::Separator => "-".into(),
            model::Kind::Submenu => format!("{}>[{}]", e.label, e.verb),
            _ => format!("{}[{}]", e.label, e.verb),
        })
        .collect();
    log::line(&format!(
        "{} menu: handlers filled it in {tq:.1} ms ({hr:?}), read {} entries ({owner_drawn} owner-drawn) by {tr:.1} ms: {}",
        if items { "items" } else { "background" },
        entries.len(),
        verbs.join(", ")
    ));
}

pub(crate) fn more_label() -> &'static str {
    // SAFETY: plain query of the UI language.
    model::more_label(unsafe { GetUserDefaultUILanguage() })
}

fn background_key(defview: HWND) -> String {
    // The desktop's view sits under Progman or a WorkerW.
    let mut buf = [0u16; 64];
    // SAFETY: class name of our view's top-level window.
    let n = unsafe { GetClassNameW(GetAncestor(defview, GA_ROOT), &mut buf) } as usize;
    let class = String::from_utf16_lossy(&buf[..n]);
    if class == "Progman" || class == "WorkerW" { "bg:desktop".into() } else { "bg:files".into() }
}

const SFGAO_FOLDER: u32 = 0x2000_0000;

static GEN: AtomicU64 = AtomicU64::new(1);

/// The view folder's absolute ID list.
fn folder_of(fv: &IFolderView2) -> Option<worker::Pidl> {
    // SAFETY: COM calls on the view's thread; the ID list is copied and freed.
    unsafe {
        let pf: IPersistFolder2 = fv.GetFolder().ok()?;
        let abs = pf.GetCurFolder().ok()?;
        let folder = worker::pidl_bytes(abs);
        CoTaskMemFree(Some(abs as _));
        Some(folder)
    }
}

/// The selection as the worker needs it: the view folder's ID list, the selected children's ID
/// lists and the `model::Recent` key of the selection's kind.
fn selection(fv: &IFolderView2, only_first: bool) -> Option<(worker::Pidl, Vec<worker::Pidl>, String)> {
    // SAFETY: COM calls on the view's thread; ID lists the view allocates are copied and freed.
    unsafe {
        let folder = folder_of(fv)?;
        let mut children = Vec::new();
        if only_first {
            if fv.ItemCount(SVGIO_ALLVIEW).unwrap_or(0) > 0 {
                if let Ok(p) = fv.Item(0) {
                    children.push(worker::pidl_bytes(p));
                    CoTaskMemFree(Some(p as _));
                }
            }
        } else {
            let mut i = -1;
            loop {
                let next = fv.GetSelectedItem(i + 1);
                match next {
                    Ok(n) if n > i => {
                        i = n;
                        if let Ok(p) = fv.Item(n) {
                            children.push(worker::pidl_bytes(p));
                            CoTaskMemFree(Some(p as _));
                        }
                    }
                    _ => break,
                }
                if children.len() >= 512 {
                    break;
                }
            }
        }
        if children.is_empty() {
            return None;
        }
        let multi = if children.len() > 1 { "+" } else { "" };
        let sf: IShellFolder = fv.GetFolder().ok()?;
        let child = children[0].as_ptr() as *const ITEMIDLIST;
        let mut attrs = SFGAO_FOLDER;
        let _ = sf.GetAttributesOf(&[child], &mut attrs);
        let kind = if attrs & SFGAO_FOLDER != 0 {
            "folder".to_string()
        } else {
            let mut name = STRRET::default();
            let ext = if sf.GetDisplayNameOf(child, SHGDNF(SHGDN_INFOLDER.0 | SHGDN_FORPARSING.0), &mut name).is_ok() {
                let mut out = [0u16; 260];
                let _ = StrRetToBufW(&mut name, Some(child), &mut out);
                let n = out.iter().position(|&c| c == 0).unwrap_or(0);
                let name = String::from_utf16_lossy(&out[..n]);
                name.rsplit_once('.').map(|(_, e)| format!(".{}", e.to_lowercase())).unwrap_or_default()
            } else {
                String::new()
            };
            ext
        };
        Some((folder, children, format!("items:{kind}{multi}")))
    }
}

thread_local! {
    /// Families this thread prewarmed (one fill each per thread).
    static WARMED: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
}

/// After a view's first frame: the root window, the worker, and one fill per family (a file of the
/// view, the folder's background) so the first menu of a kind has a static part to show.
fn prewarm_view(defview: HWND, view: &IShellView) {
    popup::prewarm();
    worker::start();
    uia::start();
    let Some(reply) = popup::reply_window() else { return };
    let Ok(fv) = view.cast::<IFolderView2>() else { return };
    let first = WARMED.with(|w| {
        let mut w = w.borrow_mut();
        let todo: Vec<&'static str> = ["items", "bg"].into_iter().filter(|f| !w.contains(f)).collect();
        w.extend(todo.iter().copied());
        todo
    });
    for family in first {
        let job = if family == "items" {
            selection(&fv, true).map(|(folder, children, key)| (folder, children, key, CMF_CANRENAME))
        } else {
            folder_of(&fv).map(|folder| (folder, Vec::new(), "bg:prewarm".to_string(), 0))
        };
        if let Some((folder, children, key, flags)) = job {
            let gn = GEN.fetch_add(1, Ordering::Relaxed);
            popup::expect_prewarm(gn, &key);
            worker::send(worker::Job::Fill { gn, folder, children, flags, reply: reply.0 as isize, owner: defview.0 as isize, site: None });
        } else {
            WARMED.with(|w| w.borrow_mut().retain(|f| *f != family));
        }
    }
}

/// The selection's menu built and filled on the worker; the first frame shows the static part.
/// False when the selection cannot be given to the worker.
fn show_worker(defview: HWND, view: &IShellView, fv: &IFolderView2, at: POINT, t0: Instant) -> bool {
    let Some((folder, children, key)) = selection(fv, false) else { return false };
    let Some(reply) = popup::reply_window() else { return false };
    let gn = GEN.fetch_add(1, Ordering::Relaxed);
    let first_child = children[0].clone();
    let site = worker::marshal_site(view);
    let sent = worker::send(worker::Job::Fill {
        gn,
        folder,
        children,
        flags: CMF_CANRENAME,
        reply: reply.0 as isize,
        owner: defview.0 as isize,
        site,
    });
    if !sent {
        return false;
    }
    let req = popup::Request { src: popup::Src::Worker(gn), key, items: true, more: more_label(), owner: defview, at, t0, fill: None };
    let pick = popup::run(req);
    give_back(defview);
    // SAFETY: modifier state of this thread's input.
    let (shift, ctrl) = unsafe { (GetKeyState(VK_SHIFT.0 as i32) < 0, GetKeyState(VK_CONTROL.0 as i32) < 0) };
    match pick {
        Some(popup::Pick::Command(_, verb)) if verb.eq_ignore_ascii_case("rename") => {
            // In-place rename is the view's: select the item for editing, as the view's own menu does.
            const SVSI_EDIT: u32 = 3;
            const SVSI_DESELECTOTHERS: u32 = 4;
            const SVSI_ENSUREVISIBLE: u32 = 8;
            const SVSI_FOCUSED: u32 = 0x10;
            // SAFETY: the view's own call with an ID list that outlives it.
            let r = unsafe {
                view.SelectItem(first_child.as_ptr() as *const ITEMIDLIST, SVSI_EDIT | SVSI_DESELECTOTHERS | SVSI_ENSUREVISIBLE | SVSI_FOCUSED)
            };
            log::line(&format!("rename through the view: {:?}", r.err().map(|e| e.code())));
        }
        Some(popup::Pick::Command(id, verb)) => {
            log::line(&format!("pick id {id} [{verb}]: the worker runs it"));
            worker::send(worker::Job::Invoke { gn, id, owner: defview.0 as isize, at: (at.x, at.y), shift, ctrl });
        }
        Some(popup::Pick::More) => {
            worker::send(worker::Job::Classic { gn, owner: defview.0 as isize, at: (at.x, at.y) });
        }
        Some(popup::Pick::Fallback) => {
            log::line("worker fill failed: the view's own menu fills here");
            return false;
        }
        None => log::line("dismissed"),
    }
    true
}

/// Gives the foreground and the focus back to the view the menu came from.
fn give_back(owner: HWND) {
    // SAFETY: window calls on our own windows.
    unsafe {
        let root = GetAncestor(owner, GA_ROOT);
        let _ = SetForegroundWindow(root);
        let _ = SetFocus(Some(owner));
    }
}

/// Makes the right-click, the menu key and Shift+F10 on `view` show this menu. Call it on the
/// view's thread; when the view has no window yet, it is tried again on a thread timer. The hook
/// goes away with the window.
pub fn hook(view: &IShellView) {
    if !try_hook(view) {
        PENDING.with(|p| p.borrow_mut().push((view.clone(), 0)));
        // SAFETY: a thread timer; its callback runs from this thread's message loop.
        unsafe {
            SetTimer(None, 0, 50, Some(retry));
        }
    }
}

thread_local! {
    /// Views whose window did not exist yet, with the number of tries.
    static PENDING: RefCell<Vec<(IShellView, u32)>> = const { RefCell::new(Vec::new()) };
}

unsafe extern "system" fn retry(_: HWND, _: u32, id: usize, _: u32) {
    let left = PENDING.with(|p| {
        let mut p = p.borrow_mut();
        let views: Vec<(IShellView, u32)> = p.drain(..).collect();
        for (v, n) in views {
            if !try_hook(&v) && n < 40 {
                p.push((v, n + 1));
            } else if n >= 40 {
                log::line("hook: gave up on a view without a window");
            }
        }
        p.len()
    });
    if left == 0 {
        // SAFETY: stops our own thread timer.
        unsafe {
            let _ = KillTimer(None, id);
        }
    }
}

fn try_hook(view: &IShellView) -> bool {
    // SAFETY: the view's own window, subclassed on its thread.
    unsafe {
        let hwnd = match view.GetWindow() {
            Ok(h) if !h.is_invalid() => h,
            other => {
                log::line(&format!("hook: no view window yet ({:?})", other.err().map(|e| e.code())));
                return false;
            }
        };
        VIEWS.with(|v| {
            let mut v = v.borrow_mut();
            v.retain(|(h, _)| *h != hwnd);
            v.push((hwnd, view.clone()));
        });
        let _ = SetWindowSubclass(hwnd, Some(subclass), SUBCLASS_ID, hwnd.0 as usize);
        hook_children(hwnd);
        // The fonts and the root window are made once per thread, after the view's first frame.
        let _ = PostMessageW(Some(hwnd), prewarm_msg(), WPARAM(0), LPARAM(0));
        log::line(&format!("hooked view {:?}", hwnd.0));
        true
    }
}

fn prewarm_msg() -> u32 {
    static MSG: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    // SAFETY: registers (or finds) a message name, once per process.
    *MSG.get_or_init(|| unsafe { RegisterWindowMessageW(windows_core::w!("explorust.menu.prewarm")) })
}

unsafe extern "system" fn child(h: HWND, lp: LPARAM) -> BOOL {
    let mut buf = [0u16; 64];
    // SAFETY: class name into a local buffer, then a subclass on our thread's window.
    unsafe {
        let n = GetClassNameW(h, &mut buf) as usize;
        let class = String::from_utf16_lossy(&buf[..n]);
        if class == "DirectUIHWND" || class == "SysListView32" {
            let _ = SetWindowSubclass(h, Some(subclass), SUBCLASS_ID, lp.0 as usize);
        }
    }
    true.into()
}

fn hook_children(defview: HWND) {
    // SAFETY: enumerates the view's children on its thread.
    unsafe {
        let _ = EnumChildWindows(Some(defview), Some(child), LPARAM(defview.0 as isize));
    }
}

unsafe extern "system" fn subclass(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _id: usize, data: usize) -> LRESULT {
    let defview = HWND(data as _);
    if msg == prewarm_msg() && h == defview {
        if let Some(view) = VIEWS.with(|v| v.borrow().iter().find(|(w, _)| *w == defview).map(|(_, s)| s.clone())) {
            prewarm_view(defview, &view);
        }
        return LRESULT(0);
    }
    match msg {
        WM_CONTEXTMENU if !from_view(HWND(wp.0 as _)) => {}
        WM_CONTEXTMENU => {
            let t0 = Instant::now();
            let view = VIEWS.with(|v| v.borrow().iter().find(|(w, _)| *w == defview).map(|(_, s)| s.clone()));
            if let Some(view) = view {
                let mut buf = [0u16; 64];
                let n = unsafe { GetClassNameW(h, &mut buf) } as usize;
                log::line(&format!("WM_CONTEXTMENU on {} at {:#x}", String::from_utf16_lossy(&buf[..n]), lp.0));
                if on_context(defview, &view, lp, t0) {
                    return LRESULT(0);
                }
            }
        }
        WM_SIZE | WM_PARENTNOTIFY if h == defview => hook_children(defview),
        WM_NCDESTROY => {
            // SAFETY: removes our subclass from the window going away.
            unsafe {
                let _ = RemoveWindowSubclass(h, Some(subclass), SUBCLASS_ID);
            }
            if h == defview {
                VIEWS.with(|v| v.borrow_mut().retain(|(w, _)| *w != h));
            }
        }
        _ => {}
    }
    // SAFETY: the next handler in the chain.
    unsafe { DefSubclassProc(h, msg, wp, lp) }
}

/// Shows the menu for a WM_CONTEXTMENU of a hooked view; false leaves it to the view.
fn on_context(defview: HWND, view: &IShellView, lp: LPARAM, t0: Instant) -> bool {
    // SAFETY: COM calls on the view's thread, window queries on our windows.
    unsafe {
        let shift = GetKeyState(VK_SHIFT.0 as i32) < 0;
        let (mut x, mut y) = ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32);
        if x == -1 && y == -1 {
            // Keyboard: at the focused item (UI Automation), else at the pointer when it is over
            // the view, else at the view's corner.
            let mut r = RECT::default();
            let _ = GetWindowRect(defview, &mut r);
            let mut p = POINT::default();
            let _ = GetCursorPos(&mut p);
            let item = uia::focused_rect(150)
                .and_then(|f| model::keyboard_anchor((f.left, f.top, f.right, f.bottom), (r.left, r.top, r.right, r.bottom)));
            if let Some((ix, iy)) = item {
                (x, y) = (ix, iy);
            } else if p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom {
                (x, y) = (p.x, p.y);
            } else {
                (x, y) = (r.left + 16, r.top + 16);
            }
        }
        let fv = view.cast::<IFolderView2>().ok();
        let items = fv.as_ref().and_then(|f| f.ItemCount(SVGIO_SELECTION).ok()).unwrap_or(0) > 0;
        let classic = shift || classic_preferred();
        if items && !classic {
            if let Some(fv) = &fv {
                if show_worker(defview, view, fv, POINT { x, y }, t0) {
                    return true;
                }
                log::line("selection not given to the worker: the view's menu fills here");
            }
        }
        let cm: windows_core::Result<IContextMenu> = view.GetItemObject(if items { SVGIO_SELECTION } else { SVGIO_BACKGROUND });
        let Ok(cm) = cm else {
            log::line(&format!("no context menu from the view: {:?}", cm.err()));
            return false;
        };
        let site = cm.cast::<IObjectWithSite>().ok();
        if let Some(s) = &site {
            let _ = s.SetSite(view);
        }
        show_at(defview, &cm, POINT { x, y }, items, classic, shift, t0);
        if let Some(s) = &site {
            let _ = s.SetSite(None);
        }
        true
    }
}

pub use popup::prewarm;
pub use popup::track_commands;

/// True when a WM_CONTEXTMENU came from the view itself (its list or item area), not from a
/// child with its own menu such as the details view's column header.
fn from_view(src: HWND) -> bool {
    let mut buf = [0u16; 64];
    // SAFETY: class name into a local buffer.
    let n = unsafe { GetClassNameW(src, &mut buf) } as usize;
    let class = String::from_utf16_lossy(&buf[..n]);
    !matches!(class.as_str(), "SysHeader32" | "CtrlNotifySink" | "ScrollBar")
}
