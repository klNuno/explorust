//! Windows side: the HMENU reader, the classic menu, the Windows 11 popup and the view hook.

mod classic;
mod log;
mod paint;
mod popup;
mod read;

use crate::model;
use std::cell::RefCell;
use std::time::Instant;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Globalization::GetUserDefaultUILanguage;
use windows::Win32::System::Ole::IObjectWithSite;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, SetFocus, VK_SHIFT};
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
/// selection's menu (top row, rename), not the background's; `classic` goes straight to the classic
/// menu with the extended verbs (Shift held).
pub fn show(owner: HWND, cm: &IContextMenu, x: i32, y: i32, items: bool, classic: bool) {
    show_at(owner, cm, POINT { x, y }, items, classic, Instant::now());
}

fn show_at(owner: HWND, cm: &IContextMenu, at: POINT, items: bool, classic: bool, t0: Instant) {
    // SAFETY: a menu this thread creates, fills through the handler and destroys.
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else { return };
    let mut flags = if items { CMF_CANRENAME } else { 0 };
    if classic {
        flags |= CMF_EXTENDEDVERBS;
    }
    let hr = unsafe { cm.QueryContextMenu(menu, 0, FIRST, LAST, flags) };
    let tq = t0.elapsed().as_secs_f64() * 1000.0;
    if classic {
        log::line(&format!("classic menu ({}), handlers filled it in {tq:.1} ms ({hr:?})", if items { "items" } else { "background" }));
        let id = classic::track(cm, menu, at.x, at.y);
        give_back(owner);
        if id >= FIRST {
            classic::invoke(cm, owner, id, FIRST, at);
        }
        let _ = unsafe { DestroyMenu(menu) };
        return;
    }
    let entries = read::entries(cm, menu, FIRST);
    let tr = t0.elapsed().as_secs_f64() * 1000.0;
    let owner_drawn = entries.iter().filter(|e| e.owner_draw).count();
    let verbs: Vec<String> = entries
        .iter()
        .map(|e| match e.kind {
            model::Kind::Separator => "-".into(),
            model::Kind::Submenu => format!("{}>", e.label),
            _ => format!("{}[{}]", e.label, e.verb),
        })
        .collect();
    log::line(&format!(
        "{} menu: handlers filled it in {tq:.1} ms ({hr:?}), read {} entries ({owner_drawn} owner-drawn) by {tr:.1} ms: {}",
        if items { "items" } else { "background" },
        entries.len(),
        verbs.join(", ")
    ));
    // SAFETY: plain query of the UI language.
    let lang = unsafe { GetUserDefaultUILanguage() };
    let (top, list) = model::arrange(&entries, items, model::more_label(lang));
    let pick = popup::run(cm, list, top, owner, at, t0);
    give_back(owner);
    match pick {
        Some(popup::Pick::Command(id)) => classic::invoke(cm, owner, id, FIRST, at),
        Some(popup::Pick::More) => {
            let id = classic::track(cm, menu, at.x, at.y);
            give_back(owner);
            if id >= FIRST {
                classic::invoke(cm, owner, id, FIRST, at);
            }
        }
        None => log::line("dismissed"),
    }
    let _ = unsafe { DestroyMenu(menu) };
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
        popup::prewarm();
        return LRESULT(0);
    }
    match msg {
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
            // Keyboard: at the pointer when it is over the view, else at the view's corner.
            let mut r = RECT::default();
            let _ = GetWindowRect(defview, &mut r);
            let mut p = POINT::default();
            let _ = GetCursorPos(&mut p);
            if p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom {
                (x, y) = (p.x, p.y);
            } else {
                (x, y) = (r.left + 16, r.top + 16);
            }
        }
        let items = view.cast::<IFolderView2>().ok().and_then(|f| f.ItemCount(SVGIO_SELECTION).ok()).unwrap_or(0) > 0;
        let cm: windows_core::Result<IContextMenu> = view.GetItemObject(if items { SVGIO_SELECTION } else { SVGIO_BACKGROUND });
        let Ok(cm) = cm else {
            log::line(&format!("no context menu from the view: {:?}", cm.err()));
            return false;
        };
        let site = cm.cast::<IObjectWithSite>().ok();
        if let Some(s) = &site {
            let _ = s.SetSite(view);
        }
        show_at(defview, &cm, POINT { x, y }, items, shift, t0);
        if let Some(s) = &site {
            let _ = s.SetSite(None);
        }
        true
    }
}

pub use popup::prewarm;
