//! Start's windows and message loop: a message-only command window on the bus, and the resident
//! menu window, cloaked until a command shows it. Handlers compute what to do under a short borrow
//! of the state and act after it, since SetForegroundWindow and menus re-enter the window procedure.

use crate::apps::{self, App};
use crate::layout::{self as L, Hit};
use crate::power::{self, Power};
use crate::recent::{self, Recent};
use crate::scene::{RecKind, Start, View};
use crate::{index, log, menu, model, strings};
use std::cell::{Cell, RefCell};
use std::time::Instant;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Globalization::GetUserDefaultUILanguage;
use windows::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_CLOAK, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmFlush, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{ClientToScreen, GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint};
use windows::Win32::Security::Authentication::Identity::{GetUserNameExW, NameDisplay};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, INPUT, INPUT_MOUSE, ReleaseCapture, SendInput, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_APPS, VK_DOWN, VK_ESCAPE, VK_LEFT,
    VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{HSTRING, PCWSTR, PWSTR, w};

mod field;
mod search;

const WM_LOADED: u32 = WM_APP + 1;
const WM_SETTINGS: u32 = WM_APP + 3;
const WM_MOUSELEAVE: u32 = 0x02A3;
const MENU_CLASS: PCWSTR = w!("explorust.start.menu");
/// Acrylic tint, ABGR: Windows 11's dark Start base (#202020) at about 75%.
const TINT: u32 = 0xC020_2020;

struct Loaded {
    apps: Vec<App>,
    recents: Vec<Recent>,
}

thread_local! {
    static ST: RefCell<Option<Start>> = const { RefCell::new(None) };
    static MENU: Cell<isize> = const { Cell::new(0) };
    static TRACKING: Cell<bool> = const { Cell::new(false) };
}

fn with<R>(f: impl FnOnce(&mut Start) -> R) -> Option<R> {
    ST.with(|c| {
        let mut b = c.try_borrow_mut().ok()?;
        b.as_mut().map(f)
    })
}

fn menu_hwnd() -> HWND {
    HWND(MENU.with(|m| m.get()) as _)
}

pub fn run() {
    // SAFETY: COM for this thread only (STA, the UI thread).
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    log::line("start: run");
    menu::dark();
    if let Err(e) = create() {
        log::line(&format!("start: windows not created: {e}"));
    } else {
        let mut msg = MSG::default();
        // SAFETY: a plain message loop on this thread.
        while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
    ST.with(|c| c.borrow_mut().take());
    // SAFETY: destroys our own window if the loop ended without Shutdown.
    unsafe {
        let h = menu_hwnd();
        if !h.is_invalid() && IsWindow(Some(h)).as_bool() {
            let _ = DestroyWindow(h);
        }
    }
    MENU.with(|m| m.set(0));
    log::line("start: stopped");
    if com {
        // SAFETY: balances the CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
}

fn create() -> windows_core::Result<()> {
    // SAFETY: window classes and windows owned by this thread.
    unsafe {
        let inst = GetModuleHandleW(None)?;
        let cmd_class = HSTRING::from(bus::class_of(bus::Owner::Start));
        let cursor = LoadCursorW(None, IDC_ARROW)?;
        let wc = WNDCLASSW { lpfnWndProc: Some(cmd_proc), hInstance: inst.into(), lpszClassName: PCWSTR(cmd_class.as_ptr()), ..Default::default() };
        RegisterClassW(&wc);
        let wc = WNDCLASSW { style: CS_DBLCLKS, lpfnWndProc: Some(menu_proc), hInstance: inst.into(), lpszClassName: MENU_CLASS, hCursor: cursor, ..Default::default() };
        RegisterClassW(&wc);
        let cmd = CreateWindowExW(WINDOW_EX_STYLE(0), &cmd_class, w!(""), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(inst.into()), None)?;
        bus::listen(cmd);
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            MENU_CLASS,
            w!("Start"),
            WS_POPUP,
            0,
            0,
            L::W as i32,
            L::H as i32,
            None,
            None,
            Some(inst.into()),
            None,
        )?;
        MENU.with(|m| m.set(hwnd.0 as isize));
        cloak(hwnd, true);
        // The accent acrylic of `render` (the look of session 1); DWM's transient-window backdrop
        // came out light grey over the black desktop of g1 and is not used.
        render::backdrop::apply(hwnd, render::backdrop::Backdrop::Acrylic { tint_abgr: TINT });
        let round = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &round as *const _ as _, 4);
        let border = 0x003A_3A3Au32;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &border as *const _ as _, 4);
        let scale = GetDpiForWindow(hwnd) as f32 / 96.0;
        let (x, y, w, h) = L::place(0, GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN), scale);
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        let s = strings::for_langid(GetUserDefaultUILanguage());
        let mut start = Start::new(hwnd, scale, s, user_name())?;
        start.draw()?;
        ST.with(|c| *c.borrow_mut() = Some(start));
        log::line(&format!("start: resident window ready, cloaked, scale {scale}, {}", if s.french { "fr" } else { "en" }));
        spawn_loader(hwnd, (L::TILE_ICON * scale).round() as i32);
        search::start(hwnd, (L::LIST_ICON * scale).round() as i32);
    }
    Ok(())
}

fn user_name() -> String {
    let mut buf = [0u16; 256];
    let mut n = buf.len() as u32;
    // SAFETY: the buffer and its length live through the call.
    if unsafe { GetUserNameExW(NameDisplay, Some(PWSTR(buf.as_mut_ptr())), &mut n) } && n > 0 {
        return String::from_utf16_lossy(&buf[..n as usize]);
    }
    std::env::var("USERNAME").unwrap_or_default()
}

fn spawn_loader(hwnd: HWND, px: i32) {
    let target = hwnd.0 as isize;
    let _ = std::thread::Builder::new().name("start-apps".into()).spawn(move || {
        // SAFETY: COM for this worker thread; the boxed result is handed to the UI thread or freed.
        unsafe {
            let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
            let t = Instant::now();
            let apps = apps::load(px);
            let apps_ms = t.elapsed().as_millis();
            let recents = recent::load(px, L::REC_COLS * L::REC_ROWS);
            log::line(&format!(
                "apps: {} from shell:AppsFolder in {apps_ms} ms ({} with icons), {} recent files",
                apps.len(),
                apps.iter().filter(|a| a.icon.is_some()).count(),
                recents.len()
            ));
            let b = Box::into_raw(Box::new(Loaded { apps, recents }));
            if PostMessageW(Some(HWND(target as _)), WM_LOADED, WPARAM(0), LPARAM(b as isize)).is_err() {
                drop(Box::from_raw(b));
            }
            if com {
                CoUninitialize();
            }
        }
    });
}

fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn on_loaded(l: Loaded) {
    with(|s| {
        s.usage = crate::sources::load_usage();
        s.set_apps(l.apps, l.recents, now_unix());
        let (pins, defaults) = apps::load_pins();
        if defaults {
            s.pins = pins.into_iter().filter(|p| s.apps.iter().any(|a| a.aumid.eq_ignore_ascii_case(p))).collect();
            log::line(&format!("pins: no pins file, Windows' default pins installed here: {}", s.pins.len()));
            apps::save_pins(&s.pins);
        } else {
            s.pins = pins;
        }
        log::line(&format!(
            "pins: {} pinned, {} on the grid, {} pages; recommended {}",
            s.pins.len(),
            s.pinned().len(),
            s.pages(),
            s.recs.len()
        ));
        s.warm();
        let _ = s.draw();
    });
}

/// Class and process of a window, for the deactivation log.
fn describe_window(h: HWND) -> String {
    if h.is_invalid() {
        // SAFETY: plain window queries.
        let fg = unsafe { GetForegroundWindow() };
        return if fg.is_invalid() { "no window".into() } else { format!("foreground {}", describe_window(fg)) };
    }
    let mut class = [0u16; 128];
    let mut pid = 0u32;
    // SAFETY: plain window queries into local buffers.
    let n = unsafe {
        GetWindowThreadProcessId(h, Some(&mut pid));
        GetClassNameW(h, &mut class)
    };
    format!("{} (pid {pid})", String::from_utf16_lossy(&class[..n.max(0) as usize]))
}

fn cloak(hwnd: HWND, on: bool) {
    let v: i32 = on as i32;
    // SAFETY: DWMWA_CLOAK takes a BOOL.
    let _ = unsafe { DwmSetWindowAttribute(hwnd, DWMWA_CLOAK, &v as *const _ as _, 4) };
}

/// Takes the foreground for the menu; the thread-input attach covers a foreground lock.
fn foreground(h: HWND) -> bool {
    // SAFETY: plain window manager calls on our own window.
    unsafe {
        // An empty mouse input makes this process the last to send input, which lifts the
        // foreground lock (what PowerToys Run does).
        let input = INPUT { r#type: INPUT_MOUSE, ..Default::default() };
        SendInput(&[input], size_of::<INPUT>() as i32);
        if SetForegroundWindow(h).as_bool() && GetForegroundWindow() == h {
            return true;
        }
        let fg = GetForegroundWindow();
        let ft = GetWindowThreadProcessId(fg, None);
        let me = GetCurrentThreadId();
        if ft != 0 && ft != me {
            let _ = AttachThreadInput(me, ft, true);
            let _ = SetForegroundWindow(h);
            let _ = SetFocus(Some(h));
            let _ = AttachThreadInput(me, ft, false);
        }
        GetForegroundWindow() == h
    }
}

fn toggle(search: bool, msg_time: i32) {
    let shown = with(|s| (s.shown, s.search_focus)).unwrap_or((false, false));
    match shown {
        (true, focus) if search && !focus => {
            with(|s| {
                s.search_focus = true;
                let _ = s.draw();
            });
        }
        (true, _) => hide(true, "toggled"),
        (false, _) => open(search, msg_time),
    }
}

fn open(search: bool, msg_time: i32) {
    let t0 = Instant::now();
    search::reset_input();
    // SAFETY: tick count and window queries.
    let (queued, prev) = unsafe { ((GetTickCount() as i32).wrapping_sub(msg_time), GetForegroundWindow()) };
    let Some((hwnd, scale, drew)) = with(|s| {
        s.reset();
        if search {
            s.search_focus = true;
            s.focus = Hit::Search;
            s.dirty = true;
        }
        let drew = s.dirty;
        if drew {
            let _ = s.draw();
        }
        s.shown = true;
        s.prev_fg = prev;
        (s.hwnd, s.scale, drew)
    }) else {
        return;
    };
    // SAFETY: positions, uncloaks and focuses our own window.
    unsafe {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTOPRIMARY);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let rc = mi.rcMonitor;
        let (x, y, w, h) = L::place(rc.left, rc.right, rc.bottom, scale);
        let slide = (L::SLIDE * scale).round();
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y + slide as i32, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        cloak(hwnd, false);
        let fg = foreground(hwnd);
        let _ = DwmFlush();
        let first = t0.elapsed().as_secs_f32() * 1000.0;
        let a = Instant::now();
        let mut frames = 0;
        loop {
            let p = model::open_progress(a.elapsed().as_secs_f32() * 1000.0, L::OPEN_MS);
            let dy = ((1.0 - p) * slide).round() as i32;
            let _ = SetWindowPos(hwnd, None, x, y + dy, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            if p >= 1.0 {
                break;
            }
            let _ = DwmFlush();
            frames += 1;
        }
        log::line(&format!(
            "open{}: key to handler {queued} ms (message time), handler to first presented frame {first:.1} ms (redraw {drew}), foreground {fg}, slide {frames} frames in {:.0} ms",
            if search { " on search" } else { "" },
            a.elapsed().as_secs_f32() * 1000.0
        ));
    }
}

fn hide(restore: bool, why: &str) {
    let Some(Some(prev)) = with(|s| {
        if !s.shown {
            return None;
        }
        s.shown = false;
        s.menu_open = false;
        s.pending_search_keys = None;
        Some(s.prev_fg)
    }) else {
        return;
    };
    let hwnd = menu_hwnd();
    end_search_drag();
    cloak(hwnd, true);
    // SAFETY: hands the foreground back to the window that had it, when we still hold it. On a
    // helper thread: in g1 the call never returned for a UWP frame (ApplicationFrameWindow) and
    // hung Start's UI thread.
    unsafe {
        if restore && GetForegroundWindow() == hwnd && !prev.is_invalid() && prev != hwnd && IsWindow(Some(prev)).as_bool() {
            let p = prev.0 as isize;
            let _ = std::thread::Builder::new().name("start-restore".into()).spawn(move || {
                let t = Instant::now();
                let ok = SetForegroundWindow(HWND(p as _)).as_bool();
                log::line(&format!("hide: foreground back to {} {ok} in {} ms", describe_window(HWND(p as _)), t.elapsed().as_millis()));
            });
        }
    }
    log::line(&format!("hide: {why}"));
    with(|s| {
        s.reset();
        if s.dirty {
            let _ = s.draw();
        }
    });
}

enum Target {
    App(String),
    File(String),
}

fn launch(t: Target, from: &'static str) {
    if let Target::App(a) = &t {
        if let Some(u) = with(|s| s.count_use(a)) {
            crate::sources::save_usage(&u);
        }
    }
    let _ = std::thread::Builder::new().name("start-launch".into()).spawn(move || {
        // SAFETY: COM for this worker thread.
        let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
        let t0 = Instant::now();
        let (what, r) = match &t {
            Target::App(a) => (a.clone(), apps::launch(a)),
            Target::File(f) => (f.clone(), apps::open_path(f)),
        };
        let ms = t0.elapsed().as_millis();
        match r {
            Ok(pid) => log::line(&format!("launch from {from}: {what}: pid {pid} in {ms} ms")),
            Err(e) => log::line(&format!("launch from {from}: {what}: failed: {e}")),
        }
        if com {
            // SAFETY: balances the call above.
            unsafe { CoUninitialize() };
        }
    });
    hide(false, "launched");
}

fn dips(lp: LPARAM, scale: f32) -> (f32, f32) {
    let x = (lp.0 & 0xFFFF) as i16 as f32;
    let y = ((lp.0 >> 16) & 0xFFFF) as i16 as f32;
    (x / scale, y / scale)
}

fn screen_point(hwnd: HWND, x: f32, y: f32, scale: f32) -> (i32, i32) {
    let mut p = POINT { x: (x * scale) as i32, y: (y * scale) as i32 };
    // SAFETY: converts a point of our own window.
    let _ = unsafe { ClientToScreen(hwnd, &mut p) };
    (p.x, p.y)
}

fn click(x: f32, y: f32) {
    if let Some(hit) = with(|s| s.hit(x, y)) {
        activate(hit);
    }
}

fn end_search_drag() -> bool {
    if with(|s| std::mem::take(&mut s.search_drag)) != Some(true) {
        return false;
    }
    // SAFETY: releases this thread's capture after dropping the state borrow.
    let _ = unsafe { ReleaseCapture() };
    true
}

fn activate(hit: Hit) {
    enum Act {
        None,
        Launch(Target, &'static str),
        PowerMenu,
        UserMenu,
        Search(usize),
    }
    let act = with(|s| {
        match hit {
            Hit::Search => {
                s.search_focus = true;
                s.dirty = true;
                Act::None
            }
            Hit::AllApps => {
                s.view = View::AllApps;
                s.scroll = 0.0;
                s.dirty = true;
                Act::None
            }
            Hit::Back => {
                s.view = View::Main;
                s.dirty = true;
                Act::None
            }
            Hit::PageDot(p) => {
                s.page = p;
                s.dirty = true;
                Act::None
            }
            Hit::Tile(i) => s.tile_app(i).map_or(Act::None, |a| Act::Launch(Target::App(s.apps[a].aumid.clone()), "grid")),
            Hit::Row(i) if s.view == View::Search => {
                s.found.sel = Some(i);
                Act::Search(0)
            }
            Hit::Action(i) => Act::Search(i),
            Hit::Row(i) => {
                let from = if s.view == View::AllApps { "all apps" } else { "search" };
                s.row_app(i).map_or(Act::None, |a| Act::Launch(Target::App(s.apps[a].aumid.clone()), from))
            }
            Hit::Rec(i) => match s.recs.get(i).map(|r| &r.kind) {
                Some(RecKind::File(f)) => Act::Launch(Target::File(f.clone()), "recommended"),
                Some(RecKind::App(a)) => Act::Launch(Target::App(a.clone()), "recommended"),
                None => Act::None,
            },
            Hit::Power => Act::PowerMenu,
            Hit::User => Act::UserMenu,
            Hit::None => Act::None,
        }
    });
    match act {
        Some(Act::Launch(t, from)) => launch(t, from),
        Some(Act::PowerMenu) => power_menu(),
        Some(Act::UserMenu) => user_menu(),
        Some(Act::Search(i)) => search::run_action(i),
        _ => redraw(),
    }
}

fn redraw() {
    with(|s| {
        if s.dirty {
            let _ = s.draw();
        }
    });
}

/// Runs a popup menu with the state released, marking it open so deactivation keeps Start up.
fn popup(x: f32, y: f32, items: &[(u32, &str)], up: bool) -> u32 {
    let Some((hwnd, scale)) = with(|s| {
        s.menu_open = true;
        (s.hwnd, s.scale)
    }) else {
        return 0;
    };
    let (sx, sy) = screen_point(hwnd, x, y, scale);
    let id = menu::pick(hwnd, sx, sy, items, up);
    with(|s| s.menu_open = false);
    id
}

fn power_menu() {
    let Some(st) = with(|s| s.s) else { return };
    let p = L::POWER;
    let id = popup(p.x, p.y - 4.0, &[(10, st.sleep), (11, st.shutdown), (12, st.restart)], true);
    let action = match id {
        10 => Power::Sleep,
        11 => Power::Shutdown,
        12 => Power::Restart,
        _ => return,
    };
    hide(false, "power");
    power::run(action);
}

fn user_menu() {
    let Some(st) = with(|s| s.s) else { return };
    let u = L::USER;
    let id = popup(u.x, u.y - 4.0, &[(20, st.lock), (21, st.sign_out)], true);
    let action = match id {
        20 => Power::Lock,
        21 => Power::SignOut,
        _ => return,
    };
    hide(false, "session");
    power::run(action);
}

fn context(x: f32, y: f32) {
    if let Some(hit) = with(|s| s.hit(x, y)) {
        context_on(hit, x, y);
    }
}

fn context_on(hit: Hit, x: f32, y: f32) {
    if let Hit::Row(i) = hit {
        if with(|s| s.view == View::Search) == Some(true) {
            search::context(i, x, y);
            return;
        }
    }
    let Some((app, pinned_pos, st)) = with(|s| {
        let app = match hit {
            Hit::Tile(i) => s.tile_app(i),
            Hit::Row(i) => s.row_app(i),
            _ => None,
        }?;
        let aumid = s.apps[app].aumid.clone();
        let pos = s.pins.iter().position(|p| p.eq_ignore_ascii_case(&aumid));
        Some((aumid, pos, s.s))
    })
    .flatten() else {
        return;
    };
    let items: Vec<(u32, &str)> = match pinned_pos {
        Some(0) => vec![(1, st.unpin)],
        Some(_) => vec![(1, st.unpin), (2, st.to_front)],
        None => vec![(3, st.pin)],
    };
    let id = popup(x, y, &items, false);
    with(|s| {
        let before = s.pins.len();
        match id {
            1 => s.pins.retain(|p| !p.eq_ignore_ascii_case(&app)),
            2 => {
                s.pins.retain(|p| !p.eq_ignore_ascii_case(&app));
                s.pins.insert(0, app.clone());
            }
            3 => s.pins.push(app.clone()),
            _ => return,
        }
        log::line(&format!("pins: {} {app} ({before} -> {})", match id { 1 => "unpinned", 2 => "moved to front", _ => "pinned" }, s.pins.len()));
        apps::save_pins(&s.pins);
        s.page = s.page.min(s.pages() - 1);
        s.dirty = true;
        let _ = s.draw();
    });
}

fn key(vk: u16) {
    if field::key(vk) {
        return;
    }
    enum Act {
        None,
        Hide,
        Activate(Hit),
        Context(Hit),
        Search(usize),
        SearchContext,
    }
    let act = with(|s| {
        if !s.shown {
            return Act::None;
        }
        match vk {
            v if v == VK_ESCAPE.0 => Act::Hide,
            v if v == VK_TAB.0 => {
                // SAFETY: reads the Shift state of this key event.
                let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                s.tab(shift);
                Act::None
            }
            v if v == VK_RETURN.0 && !matches!(s.focus, Hit::None | Hit::Search) => Act::Activate(s.focus),
            v if v == VK_APPS.0 && matches!(s.focus, Hit::Tile(_) | Hit::Row(_)) => Act::Context(s.focus),
            v if (v == VK_LEFT.0 || v == VK_RIGHT.0 || v == VK_UP.0 || v == VK_DOWN.0) && s.view != View::Search => {
                let (dx, dy) = match v {
                    _ if v == VK_LEFT.0 => (-1, 0),
                    _ if v == VK_RIGHT.0 => (1, 0),
                    _ if v == VK_UP.0 => (0, -1),
                    _ => (0, 1),
                };
                s.arrow(dx, dy);
                Act::None
            }
            v if v == VK_RETURN.0 && s.view == View::Search => Act::Search(match s.focus {
                Hit::Action(i) => i,
                _ => 0,
            }),
            v if v == VK_APPS.0 && s.view == View::Search => Act::SearchContext,
            v if (v == VK_DOWN.0 || v == VK_UP.0) && s.view == View::Search => {
                s.focus = Hit::None;
                s.move_selection(if v == VK_DOWN.0 { 1 } else { -1 });
                Act::None
            }
            v if v == VK_NEXT.0 && s.view == View::Main => {
                s.page = (s.page + 1).min(s.pages() - 1);
                s.dirty = true;
                Act::None
            }
            v if v == VK_PRIOR.0 && s.view == View::Main => {
                s.page = s.page.saturating_sub(1);
                s.dirty = true;
                Act::None
            }
            _ => Act::None,
        }
    });
    match act {
        Some(Act::Hide) => hide(true, "esc"),
        Some(Act::Search(i)) => search::run_action(i),
        Some(Act::SearchContext) => {
            if let Some((i, b)) = with(|s| s.found.sel.and_then(|i| s.result_box(i).map(|b| (i, b)))).flatten() {
                search::context(i, b.x + 40.0, b.y + b.h / 2.0);
            }
        }
        Some(Act::Activate(h)) => activate(h),
        Some(Act::Context(h)) => {
            if let Some(b) = with(|s| s.box_of(h)).flatten() {
                context_on(h, b.x + b.w / 2.0, b.y + b.h / 2.0);
            }
        }
        // A key whose character TranslateMessage already queued draws with that character
        // (WM_CHAR), so a frame left dirty by a coalesced answer is not shown first.
        _ if char_follows() => {}
        _ => redraw(),
    }
}

/// A WM_CHAR from the key being handled waits in this thread's queue.
fn char_follows() -> bool {
    let Some(hwnd) = with(|s| s.hwnd) else { return false };
    let mut msg = MSG::default();
    // SAFETY: looks at the first queued character without removing it.
    unsafe { PeekMessageW(&mut msg, Some(hwnd), WM_CHAR, WM_CHAR, PM_NOREMOVE | PM_NOYIELD) }.as_bool()
}

fn wheel(delta: i16) {
    with(|s| {
        match s.view {
            View::Main if s.pages() > 1 => {
                s.page = if delta < 0 { (s.page + 1).min(s.pages() - 1) } else { s.page.saturating_sub(1) };
            }
            View::Main => return,
            _ => s.scroll = (s.scroll - delta as f32 / 120.0 * 2.0 * L::ROW_H).clamp(0.0, s.max_scroll()),
        }
        s.dirty = true;
        let _ = s.draw();
    });
}

fn hover(x: Option<(f32, f32)>) {
    with(|s| {
        let h = x.map_or(Hit::None, |(x, y)| s.hit(x, y));
        if h != s.hover {
            s.hover = h;
            s.dirty = true;
            let _ = s.draw();
        }
    });
}

unsafe extern "system" fn cmd_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == bus::MSG_COMMAND {
        // SAFETY: the time of the message being handled.
        let mt = unsafe { GetMessageTime() };
        match bus::decode(wp, lp) {
            Some((bus::Command::StartToggle, _)) => toggle(false, mt),
            Some((bus::Command::Search, _)) => toggle(true, mt),
            Some((bus::Command::Shutdown, _)) => {
                log::line("start: shutdown asked");
                ST.with(|c| {
                    if let Ok(mut b) = c.try_borrow_mut() {
                        b.take();
                    }
                });
                // SAFETY: our own windows; the loop ends on WM_QUIT.
                unsafe {
                    let _ = DestroyWindow(menu_hwnd());
                    let _ = DestroyWindow(h);
                    PostQuitMessage(0);
                }
            }
            _ => {}
        }
        return LRESULT(0);
    }
    // SAFETY: default handling.
    unsafe { DefWindowProcW(h, msg, wp, lp) }
}

unsafe extern "system" fn menu_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let scale = with(|s| s.scale).unwrap_or(1.0);
    match msg {
        index::WM_FILES => {
            // SAFETY: made by Box::into_raw in the index worker and posted once.
            let b = unsafe { Box::from_raw(lp.0 as *mut index::Batch) };
            search::files(*b);
            return LRESULT(0);
        }
        WM_SETTINGS => {
            // SAFETY: made by Box::into_raw in the settings loader and posted once.
            let b = unsafe { Box::from_raw(lp.0 as *mut Vec<crate::sources::Setting>) };
            search::settings_loaded(*b);
            return LRESULT(0);
        }
        WM_LOADED => {
            // SAFETY: the pointer was made by Box::into_raw in the loader and is posted once.
            let l = unsafe { Box::from_raw(lp.0 as *mut Loaded) };
            on_loaded(*l);
            return LRESULT(0);
        }
        WM_ACTIVATE => {
            if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE && with(|s| s.shown && !s.menu_open) == Some(true) {
                hide(false, &format!("deactivated, to {}", describe_window(HWND(lp.0 as _))));
            }
        }
        WM_KEYDOWN => {
            key(wp.0 as u16);
            return LRESULT(0);
        }
        WM_CHAR => {
            search::ch(wp.0 as u16);
            return LRESULT(0);
        }
        WM_DWMCOLORIZATIONCOLORCHANGED => {
            with(|s| s.refresh_accent());
            if with(|s| s.shown) == Some(true) {
                redraw();
            }
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            let (x, y) = dips(lp, scale);
            if field::press(x, y, msg == WM_LBUTTONDBLCLK) {
                if msg == WM_LBUTTONDOWN {
                    // SAFETY: capture our own window to follow a selection outside the field.
                    unsafe { SetCapture(h) };
                }
                return LRESULT(0);
            }
        }
        WM_CAPTURECHANGED => {
            with(|s| s.search_drag = false);
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            let (x, y) = dips(lp, scale);
            if field::drag(x) {
                return LRESULT(0);
            }
            if !TRACKING.with(|t| t.replace(true)) {
                let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: h, dwHoverTime: 0 };
                // SAFETY: asks for WM_MOUSELEAVE on our window.
                let _ = unsafe { TrackMouseEvent(&mut tme) };
            }
            hover(Some((x, y)));
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            TRACKING.with(|t| t.set(false));
            hover(None);
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            let (x, y) = dips(lp, scale);
            field::drag(x);
            if end_search_drag() {
                return LRESULT(0);
            }
            click(x, y);
            return LRESULT(0);
        }
        WM_RBUTTONUP => {
            let (x, y) = dips(lp, scale);
            context(x, y);
            return LRESULT(0);
        }
        WM_MOUSEWHEEL => {
            wheel(((wp.0 >> 16) & 0xFFFF) as u16 as i16);
            return LRESULT(0);
        }
        _ => {}
    }
    // SAFETY: default handling.
    unsafe { DefWindowProcW(h, msg, wp, lp) }
}
