//! The switcher's windows and message loop: a message-only command window on the bus, and the
//! panel, a topmost no-activate popup with an acrylic backdrop shown only while Alt is held past
//! `model::SHOW_DELAY_MS`. Thumbnails are registered while the panel shows and released when it
//! hides. Handlers compute under a short borrow of the state and act after it, since activation
//! and window moves re-enter the window procedures.

use crate::layout::{self as L, Layout, Rect};
use crate::scene::{Scene, View};
use crate::windows_list::{self as wl, Entry};
use crate::{activate, log, model};
use render::style::{self, Part, WindowStyle};
use shelldata::Image;
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Instant;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION, DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE,
    DwmQueryThumbnailSourceSize, DwmRegisterThumbnail, DwmUnregisterThumbnail, DwmUpdateThumbnailProperties,
};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromWindow, ValidateRect};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, TME_LEAVE,
    TRACKMOUSEEVENT, TrackMouseEvent, VIRTUAL_KEY, VK_MENU,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{HSTRING, PCWSTR, w};

const PANEL_CLASS: PCWSTR = w!("explorust.switcher.panel");
const WM_ICONS: u32 = WM_APP + 1;
const WM_MOUSELEAVE: u32 = 0x02A3;
const T_SHOW: usize = 1;
const T_ANIM: usize = 2;
const T_REFRESH: usize = 3;
const T_ALT: usize = 4;
const T_VERIFY: usize = 5;
/// An unassigned virtual key pressed while Alt is held, so the window that sees Alt go up does not
/// take it for a lone Alt tap and enter its menu mode (the hook swallows Tab and Esc).
const VK_MASK: u16 = 0xE8;
/// Acrylic tint, ABGR: Windows 11's dark flyout base (#202020) at about 80%.
const TINT: u32 = 0xCC20_2020;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Idle,
    /// Alt+Tab seen, the panel not shown yet.
    Pending,
    Shown,
}

struct Sw {
    cmd: HWND,
    panel: HWND,
    phase: Phase,
    entries: Vec<Entry>,
    sel: usize,
    orig: HWND,
    t0: Instant,
    scale: f32,
    layout: Layout,
    thumbs: Vec<isize>,
    hover: Option<usize>,
    hover_close: bool,
    shown_cursor: POINT,
    ring_from: Rect,
    ring_t0: Option<Instant>,
    refreshes: u32,
    /// The window a commit or cancel activated, checked until it is in front.
    verify: Option<(HWND, Instant, &'static str)>,
    scene: Option<Scene>,
    icons: HashMap<String, Option<Image>>,
}

thread_local! {
    static ST: RefCell<Option<Sw>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Sw) -> R) -> Option<R> {
    ST.with(|c| {
        let mut b = c.try_borrow_mut().ok()?;
        b.as_mut().map(f)
    })
}

pub fn run() {
    // SAFETY: COM for this thread only (STA, the UI thread).
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    log::line("switcher: run");
    match create() {
        Err(e) => log::line(&format!("switcher: windows not created: {e}")),
        Ok((cmd, panel)) => {
            ST.with(|c| *c.borrow_mut() = Some(Sw::new(cmd, panel)));
            if let Err(e) = crate::taskview::create() {
                log::line(&format!("task view window not created: {e}"));
            }
            if let Err(e) = crate::snap::create() {
                log::line(&format!("snap window not created: {e}"));
            }
            let mut msg = MSG::default();
            // SAFETY: a plain message loop on this thread.
            while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            crate::taskview::destroy();
            crate::snap::destroy();
            let state = ST.with(|c| c.borrow_mut().take());
            if let Some(mut s) = state {
                s.unregister();
                // SAFETY: destroys our own windows if the loop ended without Shutdown.
                unsafe {
                    for h in [s.panel, s.cmd] {
                        if IsWindow(Some(h)).as_bool() {
                            let _ = DestroyWindow(h);
                        }
                    }
                }
            }
        }
    }
    log::line("switcher: stopped");
    if com {
        // SAFETY: balances the CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
}

fn create() -> windows_core::Result<(HWND, HWND)> {
    // SAFETY: window classes and windows owned by this thread.
    unsafe {
        let inst = GetModuleHandleW(None)?;
        let cmd_class = HSTRING::from(bus::class_of(bus::Owner::Switcher));
        let wc = WNDCLASSW { lpfnWndProc: Some(cmd_proc), hInstance: inst.into(), lpszClassName: PCWSTR(cmd_class.as_ptr()), ..Default::default() };
        RegisterClassW(&wc);
        let cursor = LoadCursorW(None, IDC_ARROW)?;
        let wc = WNDCLASSW { lpfnWndProc: Some(panel_proc), hInstance: inst.into(), lpszClassName: PANEL_CLASS, hCursor: cursor, ..Default::default() };
        RegisterClassW(&wc);
        let panel = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            PANEL_CLASS,
            w!("Task Switching"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(inst.into()),
            None,
        )?;
        style::bind_with_repaint(panel, Part::Switcher, WindowStyle::popup(TINT, 0x003A_3A3A), Some(|_| {
            with(|s| s.draw());
        }));
        let cmd = CreateWindowExW(WINDOW_EX_STYLE(0), &cmd_class, w!(""), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(inst.into()), None)?;
        bus::listen(cmd);
        log::line("switcher: command window on the bus");
        Ok((cmd, panel))
    }
}

impl Sw {
    fn new(cmd: HWND, panel: HWND) -> Self {
        Sw {
            cmd,
            panel,
            phase: Phase::Idle,
            entries: Vec::new(),
            sel: 0,
            orig: HWND::default(),
            t0: Instant::now(),
            scale: 1.0,
            layout: Layout { w: 0.0, h: 0.0, tiles: Vec::new() },
            thumbs: Vec::new(),
            hover: None,
            hover_close: false,
            shown_cursor: POINT::default(),
            ring_from: Rect::default(),
            ring_t0: None,
            refreshes: 0,
            verify: None,
            scene: None,
            icons: HashMap::new(),
        }
    }

    fn unregister(&mut self) {
        for t in self.thumbs.drain(..) {
            // SAFETY: a thumbnail id we registered.
            let _ = unsafe { DwmUnregisterThumbnail(t) };
        }
    }

    fn ring_target(&self) -> Rect {
        self.layout.tiles.get(self.sel).map(|t| t.rect).unwrap_or_default()
    }

    /// The ring where the animation has it now.
    fn ring_now(&self) -> Rect {
        match self.ring_t0 {
            Some(t0) => {
                let p = model::ease_out(t0.elapsed().as_secs_f32() * 1000.0 / model::RING_MS);
                self.ring_from.lerp(&self.ring_target(), p)
            }
            None => self.ring_target(),
        }
    }

    fn draw(&mut self) {
        if self.phase != Phase::Shown {
            return;
        }
        let ring = self.ring_now();
        let Some(mut scene) = self.scene.take() else { return };
        let view = View {
            scale: self.scale,
            layout: &self.layout,
            titles: self.entries.iter().map(|e| e.title.as_str()).collect(),
            keys: self.entries.iter().map(|e| e.key.as_str()).collect(),
            icons: &self.icons,
            sel: self.sel,
            ring,
            hover: self.hover,
            hover_close: self.hover_close,
        };
        if let Err(e) = scene.draw(&view) {
            log::line(&format!("draw: {e}"));
        }
        self.scene = Some(scene);
    }

    fn select(&mut self, i: usize) {
        if i == self.sel || i >= self.entries.len() {
            return;
        }
        if self.phase == Phase::Shown {
            self.ring_from = self.ring_now();
            self.ring_t0 = Some(Instant::now());
            // SAFETY: a timer on our own window, killed when the glide ends.
            unsafe { SetTimer(Some(self.panel), T_ANIM, 8, None) };
        }
        self.sel = i;
        self.draw();
    }

    /// Registers the thumbnails, lays out, sizes and places the panel. The panel's monitor is the
    /// one of the original foreground window.
    fn arrange(&mut self) {
        self.unregister();
        let mut aspects = Vec::with_capacity(self.entries.len());
        for e in &self.entries {
            // SAFETY: DWM thumbnail of a top-level window, onto our panel.
            let (id, size) = unsafe {
                match DwmRegisterThumbnail(self.panel, e.hwnd) {
                    Ok(id) => (id, DwmQueryThumbnailSourceSize(id).unwrap_or_default()),
                    Err(_) => (0, Default::default()),
                }
            };
            self.thumbs.push(id);
            aspects.push(if size.cy > 0 { size.cx as f32 / size.cy as f32 } else { 16.0 / 10.0 });
        }
        // SAFETY: monitor and DPI queries, then placing our own window.
        unsafe {
            let mon = MonitorFromWindow(if self.orig.is_invalid() { self.panel } else { self.orig }, MONITOR_DEFAULTTOPRIMARY);
            let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(mon, &mut mi);
            let rc = mi.rcMonitor;
            let _ = SetWindowPos(self.panel, None, rc.left, rc.top, 1, 1, SWP_NOACTIVATE | SWP_NOZORDER);
            self.scale = GetDpiForWindow(self.panel).max(96) as f32 / 96.0;
            let s = self.scale;
            let mw = (rc.right - rc.left) as f32 / s;
            let mh = (rc.bottom - rc.top) as f32 / s;
            self.layout = L::layout(&aspects, mw, mh);
            let (x, y) = self.layout.origin(Rect::new(0.0, 0.0, mw, mh));
            let px = |v: f32| (v * s).round() as i32;
            let (width, height) = (px(self.layout.w), px(self.layout.h));
            let _ = SetWindowPos(
                self.panel,
                Some(HWND_TOPMOST),
                rc.left + px(x),
                rc.top + px(y),
                width,
                height,
                SWP_NOACTIVATE,
            );
            if let Some(scene) = &self.scene {
                if let Err(e) = scene.resize(width as u32, height as u32) {
                    log::line(&format!("scene resize: {e}"));
                }
            } else {
                match Scene::new(self.panel, width as u32, height as u32) {
                    Ok(sc) => self.scene = Some(sc),
                    Err(e) => log::line(&format!("scene: {e}")),
                }
            }
            for (t, id) in self.layout.tiles.iter().zip(&self.thumbs) {
                if *id == 0 {
                    continue;
                }
                let r = &t.thumb;
                let props = DWM_THUMBNAIL_PROPERTIES {
                    dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE | DWM_TNP_OPACITY | DWM_TNP_SOURCECLIENTAREAONLY,
                    rcDestination: RECT { left: px(r.x), top: px(r.y), right: px(r.right()), bottom: px(r.bottom()) },
                    opacity: 255,
                    fVisible: true.into(),
                    fSourceClientAreaOnly: false.into(),
                    ..Default::default()
                };
                let _ = DwmUpdateThumbnailProperties(*id, &props);
            }
        }
    }

    fn show(&mut self) {
        if self.phase != Phase::Pending {
            return;
        }
        let t = Instant::now();
        self.phase = Phase::Shown;
        self.hover = None;
        self.hover_close = false;
        self.ring_t0 = None;
        self.arrange();
        self.draw();
        // SAFETY: shows our own window without activating it.
        unsafe {
            let _ = GetCursorPos(&mut self.shown_cursor);
            let _ = ShowWindow(self.panel, SW_SHOWNOACTIVATE);
            let _ = SetWindowPos(self.panel, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
        let tiles: Vec<String> = self
            .layout
            .tiles
            .iter()
            .map(|t| format!("{},{} {}x{}", t.rect.x, t.rect.y, t.rect.w, t.rect.h))
            .collect();
        log::line(&format!("show: tiles {}", tiles.join("; ")));
        log::line(&format!(
            "show: {} windows, selected {} ({:?}), panel {}x{} DIPs at scale {}, {:.1} ms after the first Tab, built in {:.1} ms",
            self.entries.len(),
            self.sel,
            self.entries.get(self.sel).map(|e| e.title.as_str()).unwrap_or(""),
            self.layout.w,
            self.layout.h,
            self.scale,
            self.t0.elapsed().as_secs_f32() * 1000.0,
            t.elapsed().as_secs_f32() * 1000.0
        ));
    }

    /// Hides the panel and ends the session; returns whether it was shown.
    fn close(&mut self) -> bool {
        let shown = self.phase == Phase::Shown;
        self.phase = Phase::Idle;
        // SAFETY: timers and visibility of our own windows.
        unsafe {
            let _ = KillTimer(Some(self.cmd), T_SHOW);
            let _ = KillTimer(Some(self.cmd), T_ALT);
            let _ = KillTimer(Some(self.panel), T_ANIM);
            let _ = KillTimer(Some(self.panel), T_REFRESH);
            if shown {
                let _ = ShowWindow(self.panel, SW_HIDE);
            }
        }
        self.unregister();
        if shown && let Some(scene) = &self.scene && let Err(e) = scene.resize(1, 1) {
            log::line(&format!("hidden scene resize: {e}"));
        }
        shown
    }

    /// Drops the windows that closed or left Alt+Tab; true when the list changed.
    fn refresh(&mut self) -> bool {
        let before = self.entries.len();
        let panel = self.panel;
        let mut i = 0;
        while i < self.entries.len() {
            match wl::entry(self.entries[i].hwnd, panel) {
                Some(e) => {
                    self.entries[i].title = e.title;
                    i += 1;
                }
                None => {
                    self.entries.remove(i);
                    self.sel = model::after_remove(self.sel, i, self.entries.len());
                }
            }
        }
        let changed = self.entries.len() != before;
        if changed {
            self.keep_icons();
        }
        changed
    }

    fn keep_icons(&mut self) {
        self.icons.retain(|key, _| self.entries.iter().any(|entry| entry.key == *key));
        if let Some(scene) = &mut self.scene {
            scene.keep_icons(&self.icons);
        }
    }
}

pub(crate) fn spawn_icons(target: HWND, wanted: Vec<(String, Option<String>, String)>, px: i32) {
    if wanted.is_empty() {
        return;
    }
    let target = target.0 as isize;
    let _ = std::thread::Builder::new().name("switcher-icons".into()).spawn(move || {
        // SAFETY: COM for this worker thread; the boxed result is handed to the UI thread or freed.
        unsafe {
            let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
            let t = Instant::now();
            let out: Vec<(String, Option<Image>)> =
                wanted.into_iter().map(|(key, aumid, exe)| { let i = wl::icon(aumid.as_deref(), &exe, px); (key, i) }).collect();
            log::line(&format!("icons: {} loaded in {} ms", out.len(), t.elapsed().as_millis()));
            let b = Box::into_raw(Box::new(out));
            if PostMessageW(Some(HWND(target as _)), WM_ICONS, WPARAM(0), LPARAM(b as isize)).is_err() {
                drop(Box::from_raw(b));
            }
            if com {
                CoUninitialize();
            }
        }
    });
}

fn on_next(back: bool) {
    let r = with(|s| {
        if s.phase != Phase::Idle {
            let n = model::step(s.sel, s.entries.len(), back);
            s.select(n);
            return None;
        }
        s.t0 = Instant::now();
        mask_alt();
        // SAFETY: plain query.
        s.orig = unsafe { GetForegroundWindow() };
        s.entries = wl::snapshot(s.panel);
        s.keep_icons();
        s.sel = model::first(s.entries.len(), back);
        s.phase = Phase::Pending;
        // SAFETY: timers on our own command window.
        unsafe {
            SetTimer(Some(s.cmd), T_SHOW, model::SHOW_DELAY_MS, None);
            // Over an elevated window GetAsyncKeyState reads Alt as up while it is held, which
            // committed on the first Tab: there only the hook's Alt up commits.
            if activate::key_state_readable(s.orig) {
                SetTimer(Some(s.cmd), T_ALT, 50, None);
            } else {
                log::line("foreground above our integrity level: no Alt polling");
            }
        }
        log::line(&format!(
            "next{}: {} windows in {:.1} ms: {}",
            if back { " (shift)" } else { "" },
            s.entries.len(),
            s.t0.elapsed().as_secs_f32() * 1000.0,
            s.entries.iter().map(|e| format!("{:?}", e.title)).collect::<Vec<_>>().join(", ")
        ));
        // SAFETY: plain DPI query of our own window.
        let px = (16.0 * unsafe { GetDpiForWindow(s.panel) }.max(96) as f32 / 96.0).round() as i32;
        let wanted: Vec<_> = s
            .entries
            .iter()
            .filter(|e| !s.icons.contains_key(&e.key))
            .map(|e| (e.key.clone(), e.aumid.clone(), e.exe.clone()))
            .collect();
        Some((s.panel, wanted, px))
    })
    .flatten();
    if let Some((panel, wanted, px)) = r {
        spawn_icons(panel, wanted, px);
    }
}

/// An arrow key while Alt is held: Left and Right step like Shift+Tab and Tab, Up and Down go to
/// the nearest tile of the next row.
fn on_move(dir: isize) {
    match dir {
        0 => on_next(true),
        1 => on_next(false),
        2 | 3 => {
            with(|s| {
                if s.phase == Phase::Idle {
                    return;
                }
                if let Some(i) = s.layout.vertical(s.sel, dir == 3) {
                    s.select(i);
                }
            });
        }
        _ => {}
    }
}

/// Delete while Alt is held: closes the selected window, as its close button does.
fn close_selected() {
    let Some((h, title)) = with(|s| {
        if s.phase == Phase::Idle {
            return None;
        }
        s.entries.get(s.sel).map(|e| (e.hwnd, e.title.clone()))
    })
    .flatten() else {
        return;
    };
    close_window(h, &title, "delete key");
}

fn close_window(h: HWND, title: &str, why: &str) {
    // SAFETY: asks the window to close, as its own close button does.
    let _ = unsafe { PostMessageW(Some(h), WM_SYSCOMMAND, WPARAM(SC_CLOSE as usize), LPARAM(0)) };
    log::line(&format!("close ({why}): {title:?}"));
    with(|s| {
        s.refreshes = 0;
        // SAFETY: a timer on our own window.
        unsafe { SetTimer(Some(s.panel), T_REFRESH, 150, None) };
    });
}

fn mask_alt() {
    let key = |up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VIRTUAL_KEY(VK_MASK), dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, ..Default::default() },
        },
    };
    // SAFETY: two initialized keyboard inputs.
    unsafe { SendInput(&[key(false), key(true)], size_of::<INPUT>() as i32) };
}

/// Checks the activated window every 25 ms for half a second (SwitchToThisWindow and a restore
/// finish asynchronously) and logs when it is in front.
fn verify(h: HWND, what: &'static str) {
    with(|s| {
        s.verify = Some((h, Instant::now(), what));
        // SAFETY: a timer on our own command window.
        unsafe { SetTimer(Some(s.cmd), T_VERIFY, 25, None) };
    });
}

fn on_verify(hwnd: HWND) {
    let Some(Some((h, t, what))) = with(|s| s.verify) else { return };
    let ms = t.elapsed().as_secs_f32() * 1000.0;
    let front = activate::in_front(h);
    if front || ms > 500.0 {
        // SAFETY: our own timer.
        let _ = unsafe { KillTimer(Some(hwnd), T_VERIFY) };
        with(|s| s.verify = None);
        // SAFETY: plain query.
        let fg = wl::title(unsafe { GetForegroundWindow() });
        if front {
            log::line(&format!("{what}: {:?} in front {ms:.0} ms after activation", wl::title(h)));
        } else {
            log::line(&format!("{what}: {:?} not in front after {ms:.0} ms, foreground {fg:?}", wl::title(h)));
        }
    }
}

fn commit(why: &str) {
    let Some(Some((h, title, shown, ms))) = with(|s| {
        if s.phase == Phase::Idle {
            return None;
        }
        let ms = s.t0.elapsed().as_secs_f32() * 1000.0;
        let shown = s.close();
        let e = s.entries.get(s.sel)?;
        Some((e.hwnd, e.title.clone(), shown, ms))
    }) else {
        return;
    };
    let t = Instant::now();
    let how = activate::request(h);
    log::line(&format!(
        "commit ({why}): {title:?} via {how} in {:.1} ms, {ms:.0} ms after the first Tab, {}",
        t.elapsed().as_secs_f32() * 1000.0,
        if shown { "panel shown" } else { "quick switch, no panel" }
    ));
    verify(h, "commit");
}

fn cancel() {
    let Some(orig) = with(|s| {
        if s.phase == Phase::Idle {
            return None;
        }
        s.close();
        Some(s.orig)
    })
    .flatten() else {
        return;
    };
    // SAFETY: plain query.
    let fg = unsafe { GetForegroundWindow() };
    let how = if fg == orig || orig.is_invalid() { "unchanged" } else { activate::request(orig) };
    log::line(&format!("cancel: original foreground {:?} {how}", wl::title(orig)));
    verify(orig, "cancel");
}

unsafe extern "system" fn cmd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        bus::MSG_COMMAND => {
            let cmd = bus::decode(wp, lp);
            if let Some((c, a)) = cmd.filter(|(c, _)| !matches!(c, bus::Command::SwitcherNext)) {
                log::line(&format!("command {c:?} ({a})"));
            }
            match cmd {
                Some((bus::Command::SwitcherNext, arg)) => on_next(arg == 1),
                Some((bus::Command::SwitcherCommit, _)) => commit("alt up"),
                Some((bus::Command::SwitcherCancel, _)) => cancel(),
                Some((bus::Command::SwitcherMove, dir)) => on_move(dir),
                Some((bus::Command::SwitcherClose, _)) => close_selected(),
                Some((bus::Command::TaskView, _)) => crate::taskview::toggle(),
                Some((bus::Command::SnapLayouts, _)) => crate::snap::toggle(),
                Some((bus::Command::DesktopNew, _)) => crate::desktops::new_desktop(),
                Some((bus::Command::DesktopSwitch, arg)) => crate::desktops::switch(arg),
                Some((bus::Command::DesktopClose, _)) => crate::desktops::close_current(),
                Some((bus::Command::Shutdown, _)) => {
                    with(|s| s.close());
                    log::line("switcher: shutdown");
                    // SAFETY: ends this thread's message loop.
                    unsafe { PostQuitMessage(0) };
                }
                Some((c, _)) => log::line(&format!("command {c:?}: not handled yet")),
                None => {}
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == T_SHOW => {
            // SAFETY: a one-shot use of our own timer.
            let _ = unsafe { KillTimer(Some(hwnd), T_SHOW) };
            with(|s| s.show());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == T_VERIFY => {
            on_verify(hwnd);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == T_ALT => {
            // A lost Alt up (the hook missed it) would leave the panel up: commit when Alt is up.
            // SAFETY: plain key state query.
            let alt = unsafe { GetAsyncKeyState(VK_MENU.0 as i32) } as u16 & 0x8000 != 0;
            if !alt {
                commit("alt seen up");
            }
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

fn dips(lp: LPARAM, scale: f32) -> (f32, f32) {
    let x = (lp.0 & 0xFFFF) as i16 as f32;
    let y = ((lp.0 >> 16) & 0xFFFF) as i16 as f32;
    (x / scale, y / scale)
}

fn on_mouse_move(hwnd: HWND, lp: LPARAM) {
    with(|s| {
        if s.phase != Phase::Shown {
            return;
        }
        let mut pt = POINT::default();
        // SAFETY: plain query; tracking a leave on our own window.
        unsafe {
            let _ = GetCursorPos(&mut pt);
            let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            let _ = TrackMouseEvent(&mut tme);
        }
        // The panel appearing under a still cursor sends a move: it must not steal the selection.
        if pt.x == s.shown_cursor.x && pt.y == s.shown_cursor.y {
            return;
        }
        s.shown_cursor = POINT { x: i32::MIN, y: i32::MIN };
        let (x, y) = dips(lp, s.scale);
        let hit = s.layout.hit(x, y);
        let on_close = hit.is_some_and(|i| s.layout.tiles[i].close().contains(x, y));
        if hit != s.hover || on_close != s.hover_close {
            s.hover = hit;
            s.hover_close = on_close;
            match hit {
                Some(i) if i != s.sel => s.select(i),
                _ => s.draw(),
            }
        }
    });
}

fn on_click(lp: LPARAM) {
    enum Act {
        None,
        Commit,
        Close(HWND, String),
    }
    let act = with(|s| {
        if s.phase != Phase::Shown {
            return Act::None;
        }
        let (x, y) = dips(lp, s.scale);
        let Some(i) = s.layout.hit(x, y) else { return Act::None };
        if s.layout.tiles[i].close().contains(x, y) {
            let e = &s.entries[i];
            return Act::Close(e.hwnd, e.title.clone());
        }
        s.sel = i;
        Act::Commit
    })
    .unwrap_or(Act::None);
    match act {
        Act::None => {}
        Act::Commit => commit("click"),
        Act::Close(h, title) => close_window(h, &title, "close button"),
    }
}

fn on_refresh() {
    let empty = with(|s| {
        s.refreshes += 1;
        if s.refresh() {
            log::line(&format!("refresh: {} windows left", s.entries.len()));
            if !s.entries.is_empty() {
                s.ring_t0 = None;
                s.hover = None;
                s.arrange();
                s.draw();
            }
            // SAFETY: our own timer.
            let _ = unsafe { KillTimer(Some(s.panel), T_REFRESH) };
        } else if s.refreshes > 20 {
            // SAFETY: our own timer; the window kept running (a save prompt, a refusal).
            let _ = unsafe { KillTimer(Some(s.panel), T_REFRESH) };
        }
        s.entries.is_empty()
    });
    if empty == Some(true) {
        cancel();
    }
}

unsafe extern "system" fn panel_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_MOUSEMOVE => {
            on_mouse_move(hwnd, lp);
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            with(|s| {
                if s.hover.is_some() {
                    s.hover = None;
                    s.hover_close = false;
                    s.draw();
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            on_click(lp);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == T_ANIM => {
            with(|s| {
                let done = s.ring_t0.is_none_or(|t| t.elapsed().as_secs_f32() * 1000.0 >= model::RING_MS);
                if done {
                    s.ring_t0 = None;
                    // SAFETY: our own timer.
                    let _ = unsafe { KillTimer(Some(hwnd), T_ANIM) };
                }
                s.draw();
            });
            LRESULT(0)
        }
        WM_TIMER if wp.0 == T_REFRESH => {
            on_refresh();
            LRESULT(0)
        }
        WM_ICONS => {
            // SAFETY: the box was made by `spawn_icons` and is ours now.
            let got = unsafe { Box::from_raw(lp.0 as *mut Vec<(String, Option<Image>)>) };
            with(|s| {
                s.icons.extend(got.into_iter().filter(|(key, _)| s.entries.iter().any(|entry| entry.key == *key)));
                s.draw();
            });
            LRESULT(0)
        }
        WM_PAINT => {
            // SAFETY: nothing to paint by GDI; the frame comes from the swap chain.
            let _ = unsafe { ValidateRect(Some(hwnd), None) };
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}
