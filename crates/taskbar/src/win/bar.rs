//! `Shell_TrayWnd` and the command window `explorust.taskbar`: window creation, the shell hook,
//! WinEvent hooks, mouse and keyboard, bus commands. State lives in a thread-local `Bar`; calls
//! that can send messages back to our windows (activation, capture, the work area broadcast)
//! are queued as effects and run once the state is released.

use super::actions::{self, Launch};
use super::pin_worker::{self, PinData, Update};
use super::scene::{RightView, Scene, Slot};
use super::side::{self, Asks, Ctx, Side};
use super::{context, fullscreen, jumplist, menu, popup, settings, trayhost, winx};
use super::taskband::{self as band, Taskband};
use crate::settings::{self as S, Settings};
use super::thumbs::ThumbFlyout;
use crate::jump;
use crate::right::Hit;
use super::{task_lookup, tasks};
use super::workarea::WorkArea;
use super::log;
use super::notification_state;
use crate::geom::{self as G, Row};
use crate::model::{Action, Group, Model};
use crate::pins::{self, Pin};
use crate::theme::Theme;
use render::style::{self, Material, Native, Part, WindowStyle};
use shelldata::items;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint, PAINTSTRUCT,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::System::RemoteDesktop::{NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_ESCAPE, VK_LEFT, VK_RETURN,
    VK_APPS, VK_RIGHT, VK_SHIFT, VK_SPACE,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{HSTRING, PCWSTR, w};

const WM_MOUSELEAVE: u32 = 0x02A3;
/// Coalesced repaint after the model changed.
const WM_APP_DIRTY: u32 = WM_APP + 1;
/// A WinEvent, forwarded from the hook callback: wParam = hwnd, lParam = event.
const WM_APP_EVENT: u32 = WM_APP + 2;
/// The volume or the network changed (posted by their callbacks).
const WM_APP_STATUS: u32 = WM_APP + 3;
/// The flyout lost the activation: close it outside its own message.
const WM_APP_CLOSE_FLYOUT: u32 = WM_APP + 4;
const WM_COPYGLOBALDATA: u32 = 0x0049;
const TIMER_ANIM: usize = 1;
const TIMER_CLOCK: usize = 2;
/// The hover delay before the thumbnails open, and the grace before they close.
const TIMER_THUMB_OPEN: usize = 5;
const TIMER_THUMB_CLOSE: usize = 6;
const THUMB_CLOSE_MS: u32 = 300;
/// Auto-hide: the slide frames, the delay before hiding, and the indeterminate progress frames.
const TIMER_SLIDE: usize = 7;
const TIMER_HIDE: usize = 8;
const TIMER_PROGRESS: usize = 9;
const TIMER_REVEAL: usize = 10;
const REVEAL_CHECK_MS: u32 = 50;
const HSHELL_RUDEAPPACTIVATED: usize = 0x8004;
const HSHELL_FLASH: usize = 0x8006;
const EVENT_MINIMIZESTART: u32 = 0x0016;
const EVENT_MINIMIZEEND: u32 = 0x0017;

thread_local! {
    static BAR: RefCell<Option<Bar>> = const { RefCell::new(None) };
    static BAR_HWND: Cell<isize> = const { Cell::new(0) };
    static PANICKED: Cell<bool> = const { Cell::new(false) };
}

enum Effect {
    Activate(HWND),
    Minimize(HWND),
    Launch(Launch),
    Post(bus::Command),
    Capture(bool),
    FocusBar,
    WorkArea,
    Topmost(bool),
    Reposition,
    Quit,
    /// The Win+X menu above Start.
    WinX,
    /// The jump list of slot n.
    JumpList(usize),
    Context(context::Kind, i32, i32),
}

struct Bar {
    hwnd: HWND,
    notification_session: notification_state::Session,
    cmd: HWND,
    shellhook: u32,
    monitor: RECT,
    h_px: i32,
    theme: Theme,
    scene: Option<Scene>,
    model: Model,
    pins: Vec<Pin<PinData>>,
    pin_inbox: pin_worker::Inbox,
    task_lookup: task_lookup::Lookup,
    /// Exe path and original-case app id of each task window.
    exes: HashMap<isize, String>,
    ids: HashMap<String, String>,
    groups: Vec<Group>,
    hover: Option<usize>,
    pressed: Option<usize>,
    hover_v: Vec<f32>,
    press_v: Vec<f32>,
    last_step: Instant,
    focus: Option<pins::Focus>,
    clock: (String, String),
    work: Option<WorkArea>,
    hooks: Vec<HWINEVENTHOOK>,
    dirty_posted: bool,
    effects: Vec<Effect>,
    side: Side,
    thumbs: Option<ThumbFlyout>,
    /// The slot whose hover delay runs.
    thumb_want: Option<usize>,
    /// Keyboard focus on the right side (Win+B): index in `tray_hits()`.
    tray_focus: Option<usize>,
    taskband: Taskband,
    settings: Settings,
    /// Auto-hide slide: 0 hidden, 1 shown, where it goes, and when the last frame ran.
    shown: f32,
    want_shown: bool,
    /// The first frame was drawn and `bus::PAINTED_PROP` is set for the shell's start check.
    painted: bool,
    slide_at: Instant,
    /// Start of the indeterminate progress animation.
    progress_t0: Instant,
}

pub fn run() {
    let class = HSTRING::from(bus::class_of(bus::Owner::Taskbar));
    if bus::target(bus::Owner::Taskbar).is_some() {
        log::line("another taskbar answers the bus: not starting");
        return;
    }
    // SAFETY: COM for this UI thread, released when the loop ends.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    match create(&class) {
        Ok(()) => {
            let mut msg = MSG::default();
            // SAFETY: a plain message loop on this thread.
            unsafe {
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
        Err(e) => log::line(&format!("not started: {e}")),
    }
    teardown();
    // SAFETY: matches the CoInitializeEx above.
    unsafe { CoUninitialize() };
    if PANICKED.with(|p| p.replace(false)) {
        panic!("taskbar: the window procedure panicked");
    }
    log::line("stopped");
}

fn teardown() {
    let bar = BAR.with(|b| b.borrow_mut().take());
    if let Some(mut bar) = bar {
        for h in bar.hooks.drain(..) {
            // SAFETY: hooks set by this thread.
            unsafe {
                let _ = UnhookWinEvent(h);
            }
        }
        // SAFETY: windows of this thread.
        unsafe {
            set_taskman(HWND::default());
            let _ = DeregisterShellHookWindow(bar.hwnd);
            let _ = WTSUnRegisterSessionNotification(bar.hwnd);
            let _ = DestroyWindow(bar.hwnd);
            let _ = DestroyWindow(bar.cmd);
        }
        bar.scene = None;
        drop(bar.work.take());
    }
    BAR_HWND.with(|h| h.set(0));
}

/// `SetTaskmanWindow` (user32, undocumented, what explorer calls for its bar): Windows asks
/// that window for HSHELL_GETMINRECT when a window minimizes.
fn set_taskman(hwnd: HWND) -> bool {
    use windows::Win32::System::LibraryLoader::GetProcAddress;
    // SAFETY: resolves the user32 export and calls it with its documented-by-use signature.
    unsafe {
        let Ok(user32) = GetModuleHandleW(w!("user32.dll")) else { return false };
        let Some(f) = GetProcAddress(user32, windows_core::s!("SetTaskmanWindow")) else { return false };
        let set: unsafe extern "system" fn(HWND) -> windows_core::BOOL = std::mem::transmute(f);
        set(hwnd).as_bool()
    }
}

fn primary_monitor() -> RECT {
    let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: monitor lookups into a local struct.
    unsafe {
        let m = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let _ = GetMonitorInfoW(m, &mut mi);
    }
    mi.rcMonitor
}

fn reg_dword(path: PCWSTR, name: PCWSTR) -> Option<u32> {
    let mut v = 0u32;
    let mut len = 4u32;
    // SAFETY: reads a DWORD into a local.
    unsafe { RegGetValueW(HKEY_CURRENT_USER, path, name, RRF_RT_REG_DWORD, None, Some(&mut v as *mut _ as _), Some(&mut len)) }
        .ok()
        .ok()
        .map(|_| v)
}

fn current_theme() -> Theme {
    let light = reg_dword(w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"), w!("SystemUsesLightTheme")).unwrap_or(0) != 0;
    let accent = reg_dword(w!(r"Software\Microsoft\Windows\DWM"), w!("AccentColor")).unwrap_or(0xFFD47800);
    Theme::new(light, ((accent & 0xff) as u8, (accent >> 8 & 0xff) as u8, (accent >> 16 & 0xff) as u8))
}

fn style_bar(hwnd: HWND, theme: &Theme) {
    use windows::Win32::Graphics::Dwm::{DWMWA_COLOR_NONE, DWMWCP_DONOTROUND};
    style::bind(hwnd, Part::Taskbar, WindowStyle {
        native: Native::Acrylic(theme.tint_abgr),
        border: DWMWA_COLOR_NONE,
        corner: DWMWCP_DONOTROUND,
        popup: true,
    });
}

fn create(class: &HSTRING) -> windows_core::Result<()> {
    // SAFETY: window creation and hooks on this thread; handles stay owned by it.
    unsafe {
        let inst = GetModuleHandleW(None)?;
        let cursor = LoadCursorW(None, IDC_ARROW)?;
        for name in [w!("Shell_TrayWnd"), PCWSTR(class.as_ptr())] {
            let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: inst.into(), lpszClassName: name, hCursor: cursor, ..Default::default() };
            RegisterClassW(&wc);
        }
        let monitor = primary_monitor();
        let width = monitor.right - monitor.left;
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOREDIRECTIONBITMAP,
            w!("Shell_TrayWnd"),
            w!(""),
            WS_POPUP,
            monitor.left,
            monitor.bottom - G::BAR_H as i32,
            width,
            G::BAR_H as i32,
            None,
            None,
            Some(inst.into()),
            None,
        )?;
        // Tray icons and AppBars arrive as WM_COPYDATA, also from lower-integrity processes.
        for m in [WM_COPYDATA, WM_COPYGLOBALDATA] {
            let _ = ChangeWindowMessageFilterEx(hwnd, m, MSGFLT_ALLOW, None);
        }
        popup::register(Some(wndproc));
        let cmd = CreateWindowExW(WINDOW_EX_STYLE(0), PCWSTR(class.as_ptr()), w!(""), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(inst.into()), None)?;
        bus::listen(cmd);
        BAR_HWND.with(|h| h.set(hwnd.0 as isize));
        let theme = current_theme();
        style_bar(hwnd, &theme);
        let settings = settings::read();
        let (h_px, scene) = place(hwnd, monitor, &theme)?;
        if settings.autohide {
            let _ = SetWindowPos(hwnd, None, monitor.left, S::slide_top(monitor.bottom, h_px, 0.0), 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        } else {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        let work = WorkArea::reserve(monitor, settings.reserved_height(h_px));
        let shellhook = RegisterWindowMessageW(w!("SHELLHOOK"));
        let taskband = Taskband::publish(hwnd, hwnd, shellhook);
        let hooked = RegisterShellHookWindow(hwnd).as_bool();
        let taskman = set_taskman(hwnd);
        let mut task_lookup = task_lookup::Lookup::start(hwnd)
            .map_err(|e| windows_core::Error::new(E_FAIL, format!("task identity worker: {e}")))?;
        let mut hooks = Vec::new();
        for (a, b) in [
            (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
            (EVENT_MINIMIZESTART, EVENT_MINIMIZEEND),
            (EVENT_OBJECT_SHOW, EVENT_OBJECT_HIDE),
            (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE),
            (EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_NAMECHANGE),
            (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
        ] {
            let h = SetWinEventHook(a, b, None, Some(on_winevent), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);
            if !h.is_invalid() {
                hooks.push(h);
            }
        }
        let t0 = Instant::now();
        let mut model = Model::default();
        let mut all = tasks::top_level();
        all.reverse(); // bottom of the z-order first: roughly the oldest windows first
        let mut queued = 0;
        for window in all.into_iter().filter_map(tasks::candidate) {
            task_lookup.request(window);
            queued += 1;
        }
        let fg = GetForegroundWindow();
        model.set_active((!fg.is_invalid()).then_some(fg.0 as isize));
        let dpi = GetDpiForWindow(hwnd);
        log::line(&format!(
            "started: monitor {}x{} dpi {dpi} bar {h_px} px, shell hook {hooked}, taskman {taskman}, {} win hooks, {queued} window lookups queued in {} ms",
            width,
            monitor.bottom - monitor.top,
            hooks.len(),
            t0.elapsed().as_millis()
        ));
        let pin_inbox = pin_worker::Inbox::default();
        let bar = Bar {
            hwnd,
            notification_session: notification_state::Session::default(),
            cmd,
            shellhook,
            monitor,
            h_px,
            theme,
            scene: Some(scene),
            model,
            pins: Vec::new(),
            pin_inbox: pin_inbox.clone(),
            task_lookup,
            exes: HashMap::new(),
            ids: HashMap::new(),
            groups: Vec::new(),
            hover: None,
            pressed: None,
            hover_v: Vec::new(),
            press_v: Vec::new(),
            last_step: Instant::now(),
            focus: None,
            clock: shelldata::clock::now(),
            work: Some(work),
            hooks,
            dirty_posted: false,
            effects: Vec::new(),
            side: Side::new(hwnd, WM_APP_STATUS),
            thumbs: None,
            thumb_want: None,
            tray_focus: None,
            taskband,
            shown: if settings.autohide { 0.0 } else { 1.0 },
            want_shown: !settings.autohide,
            painted: false,
            slide_at: Instant::now(),
            progress_t0: Instant::now(),
            settings,
        };
        BAR.with(|b| *b.borrow_mut() = Some(bar));
        if let Err(e) = WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) {
            log::line(&format!("session notifications unavailable: {e}"));
        } else {
            // Read after registration so transitions after the snapshot are delivered to us.
            let session = notification_state::Session::registered();
            with_bar(|b| b.notification_session = session);
        }
        with_bar(|b| {
            b.fullscreen_changed(fg);
            b.sync_reveal_timer();
            b.rebuild();
            b.paint();
        });
        log::line("initial taskbar paint returned; starting pin resolution");
        pin_worker::start(hwnd, (G::ICON * dpi as f32 / 96.0).round() as i32, &pin_inbox);
        SetTimer(Some(hwnd), TIMER_CLOCK, shelldata::clock::ms_to_next_minute() as u32, None);
        // Running apps add their icons again.
        trayhost::broadcast_created();
        Ok(())
    }
}

/// Sizes the bar for its monitor's DPI and makes its scene: (height px, scene).
fn place(hwnd: HWND, monitor: RECT, theme: &Theme) -> windows_core::Result<(i32, Scene)> {
    // SAFETY: moves our own window.
    unsafe {
        let dpi = GetDpiForWindow(hwnd).max(96);
        let h_px = G::bar_height_px(dpi);
        let width = monitor.right - monitor.left;
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), monitor.left, monitor.bottom - h_px, width, h_px, SWP_NOACTIVATE);
        let scene = Scene::new(hwnd, width as u32, h_px as u32, dpi as f32 / 96.0, theme)?;
        Ok((h_px, scene))
    }
}

/// Runs `f` on the bar state, then the effects it queued. None when there is no state or it is
/// already borrowed (a message sent back to us from inside an effect-free call).
fn with_bar<R>(f: impl FnOnce(&mut Bar) -> R) -> Option<R> {
    let (r, effects) = BAR.with(|b| {
        let mut g = b.try_borrow_mut().ok()?;
        let bar = g.as_mut()?;
        let r = f(bar);
        Some((r, std::mem::take(&mut bar.effects)))
    })?;
    for e in effects {
        run_effect(e);
    }
    Some(r)
}

fn bar_hwnd() -> HWND {
    HWND(BAR_HWND.with(|h| h.get()) as _)
}

fn run_effect(e: Effect) {
    let hwnd = bar_hwnd();
    match e {
        Effect::Activate(h) => {
            let ok = actions::activate(h);
            // Over an elevated window the medium explorust may be refused (a UWP frame above all):
            // explorust-keys.exe, with UI access, activates it then.
            let keys = !ok && bus::post(bus::Command::Activate, h.0 as isize);
            log::line(&format!(
                "activate {:?}: {}",
                h.0,
                if ok { "foreground" } else if keys { "not foreground, asked explorust-keys" } else { "not foreground" }
            ));
        }
        Effect::Minimize(h) => {
            log::line(&format!("minimize {:?}", h.0));
            actions::minimize(h);
        }
        Effect::Launch(l) => actions::launch(l),
        Effect::Post(c) => {
            let ok = bus::post(c, 0);
            log::line(&format!("bus {c:?}: {}", if ok { "posted" } else { "no owner" }));
        }
        // SAFETY: capture of our own window.
        Effect::Capture(on) => unsafe {
            if on {
                SetCapture(hwnd);
            } else {
                let _ = ReleaseCapture();
            }
        },
        Effect::FocusBar => {
            let ok = actions::activate(hwnd);
            log::line(&format!("focus the bar: {ok}"));
        }
        Effect::WorkArea => {
            let _ = with_bar(|b| {
                if b.work.is_some() {
                    let r = b.side.work_area(b.monitor, b.reserved_rect());
                    super::workarea::apply_rect(RECT { left: r.left, top: r.top, right: r.right, bottom: r.bottom });
                }
            });
        }
        // SAFETY: z-order of our own window.
        Effect::Topmost(on) => unsafe {
            let after = if on { HWND_TOPMOST } else { HWND_NOTOPMOST };
            let _ = SetWindowPos(hwnd, Some(after), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            if !on {
                let _ = SetWindowPos(hwnd, Some(HWND_BOTTOM), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
            log::line(&format!("bar topmost {on}"));
        },
        Effect::Reposition => {
            let _ = with_bar(|b| b.reposition());
        }
        Effect::WinX => {
            let at = with_bar(|b| {
                b.close_thumbs();
                let (l, _, _, _) = b.row().slot_rect(0);
                let wr = b.window_rect();
                (b.hwnd, wr.left + (l * b.scale()).round() as i32, wr.top)
            });
            if let Some((owner, x, y)) = at {
                log::line(&format!("win+x menu at {x},{y}"));
                if let Some(c) = winx::show(owner, x, y) {
                    let ok = bus::post(c, 0);
                    log::line(&format!("bus {c:?}: {}", if ok { "posted" } else { "no owner" }));
                }
            }
        }
        Effect::JumpList(i) => {
            let Some((owner, x, y, key, nodes, recent)) = with_bar(|b| b.jump_list(i)).flatten() else { return };
            let id = menu::track(owner, x, y, &nodes, true);
            log::line(&format!("jump list {i}: picked {id}"));
            if let Some(p) = jump::pick(id) {
                let _ = with_bar(|b| b.jump_pick(&key, p, &recent));
            }
        }
        Effect::Context(kind, x, y) => {
            with_bar(|b| b.close_thumbs());
            context::show(hwnd, x, y, kind);
        }
        Effect::Quit => {
            teardown();
            // SAFETY: ends this thread's loop.
            unsafe { PostQuitMessage(0) };
        }
    }
}

unsafe extern "system" fn on_winevent(_: HWINEVENTHOOK, event: u32, hwnd: HWND, id_object: i32, id_child: i32, _: u32, _: u32) {
    if id_object != OBJID_WINDOW.0 || id_child != CHILDID_SELF as i32
        || (hwnd.is_invalid() && event != EVENT_SYSTEM_FOREGROUND) {
        return;
    }
    let bar = bar_hwnd();
    if bar.0.is_null() || hwnd == bar {
        return;
    }
    // A foreground window can enter or leave fullscreen without changing activation.
    if event == EVENT_OBJECT_LOCATIONCHANGE && hwnd != unsafe { GetForegroundWindow() } {
        return;
    }
    // SAFETY: posts to our own window; handled in order with the shell hook messages.
    let _ = unsafe { PostMessageW(Some(bar), WM_APP_EVENT, WPARAM(hwnd.0 as usize), LPARAM(event as isize)) };
}

impl Bar {
    /// The taskbar's work-area reservation stays put during the slide.
    fn reserved_rect(&self) -> crate::appbar::Rect {
        let monitor = super::appbars::to_rect(self.monitor);
        let top = monitor.bottom - self.settings.reserved_height(self.h_px);
        crate::appbar::Rect { top, ..monitor }
    }

    fn row(&self) -> Row {
        Row { width: self.scene.as_ref().map_or(1280.0, |s| s.width), buttons: self.groups.len() + 1, left: self.settings.align_left }
    }

    fn scale(&self) -> f32 {
        self.scene.as_ref().map_or(1.0, |s| s.scale)
    }

    fn dirty(&mut self) {
        if !self.dirty_posted {
            self.dirty_posted = true;
            // SAFETY: posts to our own window.
            let _ = unsafe { PostMessageW(Some(self.hwnd), WM_APP_DIRTY, WPARAM(0), LPARAM(0)) };
        }
    }

    /// Queues an identity read for an eligible window and drops vanished windows immediately.
    fn refresh(&mut self, h: HWND) {
        if let Some(window) = tasks::candidate(h) {
            self.task_lookup.request(window);
        } else {
            self.remove_task(h.0 as isize);
        }
    }

    fn remove_task(&mut self, id: isize) {
        self.task_lookup.forget(id);
        self.exes.remove(&id);
        let band_changed = self.taskband.forget(id);
        if self.model.remove(id) || band_changed {
            self.dirty();
        }
    }

    fn receive_tasks(&mut self) {
        while let Some(result) = self.task_lookup.receive() {
            let id = result.window.handle;
            let hwnd = HWND(id as _);
            if tasks::candidate(hwnd) != Some(result.window) {
                self.remove_task(id);
                self.refresh(hwnd);
                continue;
            }
            if let Some(task) = result.task {
                self.exes.insert(id, task.exe);
                self.ids.insert(task.key.clone(), task.id);
                let new = !self.model.contains(id);
                if self.model.upsert(id, &task.key) {
                    self.dirty();
                }
                if new {
                    log::line(&format!("window {id:#x} {} added", task.key));
                    self.taskband.button_created(id);
                    if result.flash && self.model.set_flash(id, true) {
                        self.dirty();
                    }
                }
            } else {
                self.remove_task(id);
            }
            if result.again {
                self.task_lookup.request(result.window);
            }
        }
    }

    fn set_active(&mut self, h: HWND) {
        if h == self.hwnd {
            return;
        }
        let a = (!h.is_invalid()).then_some(h.0 as isize);
        if a.is_some_and(|a| !self.model.contains(a)) {
            self.refresh(h);
            self.task_lookup.set_flash(h.0 as isize, false);
        }
        if self.model.set_active(a) {
            self.dirty();
        }
    }

    fn rebuild(&mut self) {
        self.groups = self.model.groups();
        if let Some(scene) = &mut self.scene {
            scene.keep_window_icons(&self.groups);
        }
        let n = self.groups.len() + 1;
        self.hover_v.resize(n, 0.0);
        self.press_v.resize(n, 0.0);
        if let Some(slot) = self.focus_slot() {
            // Refresh the key as well as the slot when a pin learns its app identity.
            self.focus = Some(pins::Focus::at(slot, &self.groups, &self.pins));
        } else {
            self.focus = None;
        }
        self.load_icons();
    }

    fn focus_slot(&self) -> Option<usize> {
        self.focus.as_ref()?.slot(&self.groups, &self.pins)
    }

    fn receive_pins(&mut self) {
        let updates = std::mem::take(&mut *self.pin_inbox.lock().unwrap());
        if updates.is_empty() {
            return;
        }
        let mut changed = false;
        let mut icons = Vec::new();
        for update in updates {
            match update {
                Update::Slots(count) => {
                    self.pins.splice(..0, pins::placeholders(count));
                    changed = true;
                }
                Update::Resolved(slot, item) => {
                    if let Some((key, pin)) = &item {
                        log::line(&format!("pin slot {slot}: {} = {key}", pin.name));
                    }
                    changed |= pins::resolve(&mut self.pins, slot, item);
                }
                Update::FileExplorer(slot) => {
                    if let Some(item) = self.pins.iter_mut().find(|pin| pin.source == Some(slot))
                        .and_then(|pin| pin.item.as_mut()) {
                        item.file_explorer = true;
                    }
                }
                Update::Icon(slot, icon) => {
                    if let Some(item) = self.pins.iter_mut().find(|pin| pin.source == Some(slot))
                        .and_then(|pin| pin.item.as_mut()) {
                        item.icon = Some(icon);
                        icons.push(slot);
                    }
                }
            }
        }
        if changed {
            let mut keys = HashSet::new();
            self.pins.retain(|p| keys.insert(p.key.clone()));
            self.model.pins = self.pins.iter().map(|p| p.key.clone()).collect();
            // The pointer may still be down over a slot that now shows a different app.
            self.pressed = None;
            self.rebuild();
            for (i, g) in self.groups.iter().enumerate() {
                log::line(&format!("button {} {} pin={:?} windows={}", i + 1, g.key, g.pin, g.wins.len()));
            }
        }
        if let Some(scene) = self.scene.as_mut() {
            for pin in self.groups.iter().filter_map(|group| group.pin).map(|p| &self.pins[p]) {
                if (changed || pin.source.is_some_and(|slot| icons.contains(&slot)))
                    && let Some(icon) = pin.item.as_ref().and_then(|item| item.icon.clone()) {
                    scene.set_icon(&pin.key, Some(icon));
                }
            }
        }
        self.paint();
    }

    fn load_icons(&mut self) {
        let Some(scene) = self.scene.as_ref() else { return };
        let px = scene.icon_px();
        let missing: Vec<Group> = self.groups.iter().filter(|g| !scene.has_icon(&g.key)).cloned().collect();
        for g in &missing {
            let t = Instant::now();
            let img = self.group_image(g, px);
            if g.pin.is_none() {
                log::line(&format!("icon {} {}: {} ms", g.key, if img.is_some() { "loaded" } else { "missing" }, t.elapsed().as_millis()));
            }
            if let Some(scene) = self.scene.as_mut() {
                scene.set_icon(&g.key, img);
            }
        }
    }

    fn ctx(&self) -> (HWND, RECT, f32) {
        (self.hwnd, self.monitor, self.scale())
    }

    fn asks(&mut self, a: Asks) {
        if a.redraw {
            self.dirty();
        }
        if a.work_area {
            self.effects.push(Effect::WorkArea);
        }
        if let Some(c) = a.post {
            self.effects.push(Effect::Post(c));
        }
        if let Some(t) = a.topmost {
            self.effects.push(Effect::Topmost(t));
        }
        if let Some((kind, x, y)) = a.menu {
            self.effects.push(Effect::Context(kind, x, y));
        }
    }

    /// Lays the right side out for the current clock and icons, and loads new tray icon images.
    fn layout_right(&mut self) {
        let Some(scene) = self.scene.as_mut() else { return };
        let cw = scene.clock_width(&self.clock);
        self.side.relayout(scene.width, scene.width - G::CLOCK_RIGHT, cw);
        let all = self.side.all_hicons();
        let px = scene.tray_px();
        for h in self.side.visible_hicons() {
            if !scene.has_tray_icon(h) {
                scene.set_tray_icon(h, trayhost::icon_image(h, px));
            }
        }
        scene.keep_tray_icons(&all);
    }

    /// DIP point of a mouse lParam, and the right side's hit there.
    fn right_hit(&self, lp: LPARAM) -> Option<crate::right::Hit> {
        let k = self.scale();
        let x = (lp.0 & 0xffff) as i16 as f32 / k;
        let y = (lp.0 >> 16 & 0xffff) as i16 as f32 / k;
        (0.0..G::BAR_H).contains(&y).then(|| self.side.layout.hit(x, y)).flatten()
    }

    fn paint(&mut self) {
        self.layout_right();
        let Some(scene) = self.scene.as_ref() else { return };
        let mut slots = Vec::with_capacity(self.groups.len() + 1);
        let focus = self.focus_slot();
        slots.push(Slot { key: None, pinned: false, running: false, active: false, flashing: false, hover: self.hover_v[0], press: self.press_v[0], focused: focus == Some(0), progress: None, overlay: None });
        let phase = self.progress_t0.elapsed().as_secs_f32();
        for (i, g) in self.groups.iter().enumerate() {
            let progress = g.wins.iter().find_map(|w| self.taskband.progress.get(w).filter(|p| p.shown()).copied());
            let overlay = g.wins.iter().rev().find(|w| self.taskband.overlays.contains_key(w)).copied();
            slots.push(Slot {
                progress: progress.map(|p| (p, phase)),
                overlay,
                key: Some(&g.key),
                pinned: g.pin.is_some(),
                running: !g.wins.is_empty(),
                active: self.model.is_active(g),
                flashing: g.flashing,
                hover: self.hover_v[i + 1],
                press: self.press_v[i + 1],
                focused: focus == Some(i + 1),
            });
        }
        let hicons = self.side.visible_hicons();
        let glyphs = self.side.glyphs();
        let focus = self.tray_focus.and_then(|i| self.tray_hits().get(i).copied());
        let right = RightView { layout: &self.side.layout, icons: &hicons, glyphs: &glyphs, hover: self.side.hover, pressed: self.side.pressed, focus };
        let indeterminate = slots.iter().any(|s| s.progress.is_some_and(|(p, _)| p.state == crate::taskband::ProgressState::Indeterminate));
        let material = style::current(Part::Taskbar).material;
        let dark_theme = (self.theme.light && matches!(material, Material::Oled | Material::Mica)).then(|| {
            let (r, g, b, _) = self.theme.active_ind;
            Theme::new(false, (r, g, b))
        });
        let theme = dark_theme.as_ref().unwrap_or(&self.theme);
        match scene.draw(theme, &slots, &self.clock, &right, self.settings.align_left) {
            Err(e) => log::line(&format!("draw failed: {e}")),
            // Tells the shell's start check the bar is up, also when auto-hide keeps it hidden.
            // SAFETY: a property on our own window.
            Ok(()) if !self.painted => self.painted = unsafe { SetPropW(self.hwnd, &HSTRING::from(bus::PAINTED_PROP), Some(HANDLE(1 as _))) }.is_ok(),
            Ok(()) => {}
        }
        // SAFETY: timers on our own window.
        unsafe {
            if indeterminate {
                SetTimer(Some(self.hwnd), TIMER_PROGRESS, 33, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_PROGRESS);
            }
        }
    }

    /// A taskband message (`crate::taskband`); None when `msg` is not one.
    fn on_taskband(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        let px = (band::OVERLAY * self.scale()).round() as i32;
        match self.taskband.on_message(msg, wp, lp, px)? {
            band::Change::None => {}
            band::Change::Redraw => self.dirty(),
            band::Change::Overlay(h, img) => {
                if let Some(s) = self.scene.as_mut() {
                    s.set_overlay(h, img);
                }
                self.dirty();
            }
            band::Change::Active(h) => self.set_active(HWND(h as _)),
        }
        Some(LRESULT(0))
    }

    /// Auto-hide: starts sliding the bar in or out.
    fn slide_to(&mut self, show: bool) {
        if !self.settings.autohide || self.want_shown == show {
            return;
        }
        self.want_shown = show;
        self.slide_at = Instant::now();
        log::line(&format!("autohide: {}", if show { "show" } else { "hide" }));
        // SAFETY: timers on our own window.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_HIDE);
            SetTimer(Some(self.hwnd), TIMER_SLIDE, 8, None);
        }
        self.sync_reveal_timer();
    }

    fn sync_reveal_timer(&self) {
        // SAFETY: these timers belong to the bar on this thread.
        unsafe {
            if self.settings.autohide && !self.want_shown {
                SetTimer(Some(self.hwnd), TIMER_REVEAL, REVEAL_CHECK_MS, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_REVEAL);
            }
        }
    }

    fn fullscreen_changed(&mut self, foreground: HWND) {
        let (hwnd, monitor, scale) = self.ctx();
        let cx = Ctx { hwnd, monitor, scale, theme: &self.theme };
        let asks = self.side.on_foreground(foreground, &cx);
        self.asks(asks);
        if self.side.appbars.fullscreen {
            self.slide_to(false);
        }
    }

    fn maybe_reveal(&mut self) {
        if !self.settings.autohide || self.want_shown {
            return;
        }
        let mut pt = POINT::default();
        // SAFETY: takes a screen-coordinate snapshot, independent of window hit testing.
        if unsafe { GetCursorPos(&mut pt) }.is_err()
            || !S::at_reveal_edge(super::appbars::to_rect(self.monitor), pt.x, pt.y)
        {
            return;
        }
        // The foreground can enter or leave borderless mode without changing windows.
        self.fullscreen_changed(unsafe { GetForegroundWindow() });
        if !self.side.appbars.fullscreen {
            self.effects.push(Effect::Topmost(true));
            self.slide_to(true);
        }
    }

    /// One slide frame; false once it arrived.
    fn slide_step(&mut self) -> bool {
        let dt = self.slide_at.elapsed().as_secs_f32() * 1000.0;
        self.slide_at = Instant::now();
        let (target, ms) = if self.want_shown { (1.0, S::SHOW_MS) } else { (0.0, S::HIDE_MS) };
        // Linear progress through the slide, eased on placement.
        let step = dt / ms;
        self.shown = if target > self.shown { (self.shown + step).min(1.0) } else { (self.shown - step).max(0.0) };
        self.place_slide();
        if self.shown == target {
            log::line(&format!("autohide: {} at {:?}", if self.want_shown { "shown" } else { "hidden" }, self.window_rect()));
        }
        self.shown != target
    }

    fn place_slide(&self) {
        let k = if self.want_shown { S::ease_out(self.shown) } else { 1.0 - S::ease_out(1.0 - self.shown) };
        let top = if self.settings.autohide { S::slide_top(self.monitor.bottom, self.h_px, k) } else { self.monitor.bottom - self.h_px };
        let visibility = if self.settings.autohide && !self.want_shown && self.shown == 0.0 {
            SWP_HIDEWINDOW
        } else {
            SWP_SHOWWINDOW
        };
        // SAFETY: moves our own window.
        let _ = unsafe { SetWindowPos(self.hwnd, None, self.monitor.left, top, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | visibility) };
    }

    /// Auto-hide: the pointer left; hide unless a flyout or the keyboard keeps the bar.
    fn maybe_hide(&mut self) {
        if !self.settings.autohide {
            return;
        }
        let mut pt = POINT::default();
        // SAFETY: reads the cursor position.
        let cursor_read = unsafe { GetCursorPos(&mut pt) }.is_ok();
        // SAFETY: reads the foreground window.
        let fg = unsafe { GetForegroundWindow() };
        let held_open = self.thumbs.is_some() || self.side.flyout.is_some() || fg == self.hwnd;
        let bar = super::appbars::to_rect(self.window_rect());
        if !cursor_read || !S::should_hide(bar, pt.x, pt.y, held_open) {
            // SAFETY: a timer on our own window.
            unsafe { SetTimer(Some(self.hwnd), TIMER_HIDE, S::HIDE_DELAY_MS, None) };
            return;
        }
        self.slide_to(false);
    }

    /// Settings changed in the registry: true when the layout must be redone.
    fn reload_settings(&mut self) -> bool {
        let s = settings::read();
        if s == self.settings {
            return false;
        }
        let autohide_changed = s.autohide != self.settings.autohide;
        self.settings = s;
        if autohide_changed {
            self.want_shown = !s.autohide;
            self.shown = if s.autohide { 0.0 } else { 1.0 };
            // SAFETY: the new setting starts from a settled position.
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_HIDE);
                let _ = KillTimer(Some(self.hwnd), TIMER_SLIDE);
            }
            self.sync_reveal_timer();
        }
        true
    }

    fn animate(&mut self) {
        self.last_step = Instant::now();
        // SAFETY: a timer on our own window.
        unsafe { SetTimer(Some(self.hwnd), TIMER_ANIM, 16, None) };
    }

    fn step(&mut self) -> bool {
        let dt = self.last_step.elapsed().as_secs_f32() * 1000.0;
        self.last_step = Instant::now();
        let mut moving = false;
        for i in 0..self.hover_v.len() {
            let h = if self.hover == Some(i) { 1.0 } else { 0.0 };
            let p = if self.pressed == Some(i) && self.hover == Some(i) { 1.0 } else { 0.0 };
            moving |= G::fade(&mut self.hover_v[i], h, dt);
            moving |= G::fade(&mut self.press_v[i], p, dt);
        }
        moving
    }

    fn set_hover(&mut self, slot: Option<usize>) {
        if slot != self.hover {
            self.hover = slot;
            self.animate();
        }
    }

    /// What a click (or Win+number, or Enter) on slot `i` does.
    fn activate_slot(&mut self, i: usize, new_instance: bool) {
        if i == 0 {
            self.effects.push(Effect::Post(bus::Command::StartToggle));
            return;
        }
        let Some(g) = self.groups.get(i - 1).cloned() else { return };
        let pin = g.pin.and_then(|p| self.pins[p].item.as_ref());
        if g.pin.is_some() && pin.is_none() {
            return;
        }
        let action = self.model.click(&g, new_instance);
        log::line(&format!("click {} {}: {action:?}", i, g.key));
        match action {
            Action::Activate(h) => self.effects.push(Effect::Activate(HWND(h as _))),
            Action::Minimize(h) => self.effects.push(Effect::Minimize(HWND(h as _))),
            Action::Launch => {
                let explorer = g.key == "microsoft.windows.explorer" || g.key.ends_with("\\explorer.exe")
                    || pin.is_some_and(|pin| pin.file_explorer);
                if explorer {
                    // Starting explorer.exe would make it the shell: File Explorer is our Files part.
                    self.effects.push(Effect::Post(bus::Command::FilesOpen));
                } else if let Some(pin) = pin {
                    self.effects.push(Effect::Launch(pin.launch.clone()));
                } else if !g.key.contains('\\') {
                    let id = self.ids.get(&g.key).cloned().unwrap_or_else(|| g.key.clone());
                    let exe = g.wins.iter().filter_map(|h| self.exes.get(h))
                        .find_map(|path| crate::model::relaunch_exe(path))
                        .map(str::to_owned);
                    self.effects.push(Effect::Launch(Launch::Aumid { id, exe }));
                } else if let Some(exe) = g.wins.first().and_then(|h| self.exes.get(h)) {
                    self.effects.push(Effect::Launch(Launch::Path(exe.clone())));
                }
            }
        }
    }

    fn slot_at(&self, lp: LPARAM) -> (Option<usize>, bool) {
        let x = (lp.0 & 0xffff) as i16 as f32 / self.scale();
        let row = self.row();
        (row.at(x), row.in_clock(x))
    }

    fn reposition(&mut self) {
        self.monitor = primary_monitor();
        self.scene = None;
        match place(self.hwnd, self.monitor, &self.theme) {
            Ok((h, s)) => {
                self.h_px = h;
                self.scene = Some(s);
                self.place_slide();
                // SAFETY: refresh coverage against the new display layout.
                self.fullscreen_changed(unsafe { GetForegroundWindow() });
                if self.side.appbars.fullscreen {
                    self.effects.push(Effect::Topmost(false));
                }
                if let Some(scene) = self.scene.as_mut() {
                    // Badges were drawn at the old scale: redraw them at the new one.
                    for (h, img) in &self.taskband.images {
                        scene.set_overlay(*h, Some(img.clone()));
                    }
                }
                let m = self.monitor;
                // SAFETY: reads our own window's DPI.
                let dpi = unsafe { GetDpiForWindow(self.hwnd) };
                log::line(&format!("reposition: monitor {},{} {}x{} dpi {dpi} bar {h} px at {:?}", m.left, m.top, m.right - m.left, m.bottom - m.top, self.window_rect()));
                self.effects.push(Effect::WorkArea);
                self.rebuild();
                self.paint();
            }
            Err(e) => log::line(&format!("reposition failed: {e}")),
        }
    }

    fn on_shellhook(&mut self, code: usize, lp: LPARAM) -> LRESULT {
        let h = HWND(lp.0 as _);
        if code as u32 != HSHELL_REDRAW {
            log::line(&format!("shell hook {code:#x} {:#x}", lp.0));
        }
        if bus::shell_hook_sink::taskman_only(code)
            && let Some(sink) = bus::shell_hook_sink::get()
        {
            // SAFETY: a posted copy of this message to twinui's window in this process; lParam is
            // a window handle or a plain value for these codes, never a pointer.
            unsafe {
                let _ = PostMessageW(Some(HWND(sink as _)), self.shellhook, WPARAM(code), lp);
            }
        }
        match code as u32 {
            HSHELL_WINDOWCREATED | HSHELL_REDRAW | HSHELL_WINDOWREPLACED => self.refresh(h),
            HSHELL_WINDOWDESTROYED => {
                let a = self.side.prune();
                self.asks(a);
                self.remove_task(lp.0);
                // SAFETY: recheck even when the last foreground window closes without a successor.
                self.fullscreen_changed(unsafe { GetForegroundWindow() });
            }
            HSHELL_WINDOWACTIVATED => self.set_active(h),
            HSHELL_GETMINRECT => return self.min_rect(lp),
            _ => match code {
                HSHELL_RUDEAPPACTIVATED => self.set_active(h),
                HSHELL_FLASH => {
                    if !self.model.contains(lp.0) {
                        self.refresh(h);
                        self.task_lookup.set_flash(lp.0, true);
                    }
                    if self.model.set_flash(lp.0, true) {
                        self.dirty();
                    }
                }
                _ => {}
            },
        }
        LRESULT(0)
    }

    /// HSHELL_GETMINRECT: the button's screen rect, where the minimize animation lands.
    fn min_rect(&mut self, lp: LPARAM) -> LRESULT {
        if lp.0 == 0 {
            return LRESULT(0);
        }
        // SAFETY: for HSHELL_GETMINRECT, lParam points to a SHELLHOOKINFO for this call.
        let info = unsafe { &mut *(lp.0 as *mut SHELLHOOKINFO) };
        let id = info.hwnd.0 as isize;
        let Some(gi) = self.groups.iter().position(|g| g.wins.contains(&id)) else {
            log::line(&format!("getminrect {id:#x}: no button"));
            return LRESULT(0);
        };
        let (l, t, r, b) = self.row().slot_rect(gi + 1);
        let k = self.scale();
        let mut wr = RECT::default();
        // SAFETY: reads our own window's rect.
        let _ = unsafe { GetWindowRect(self.hwnd, &mut wr) };
        info.rc = RECT {
            left: wr.left + (l * k).round() as i32,
            top: wr.top + (t * k).round() as i32,
            right: wr.left + (r * k).round() as i32,
            bottom: wr.top + (b * k).round() as i32,
        };
        log::line(&format!("getminrect {id:#x}: button {} at {},{} {}x{}", gi + 1, info.rc.left, info.rc.top, info.rc.right - info.rc.left, info.rc.bottom - info.rc.top));
        LRESULT(1)
    }

    fn window_rect(&self) -> RECT {
        let mut wr = RECT::default();
        // SAFETY: reads our own window's rect.
        let _ = unsafe { GetWindowRect(self.hwnd, &mut wr) };
        wr
    }

    /// The icon of a group: its pin, its AppsFolder entry, then its exe.
    fn group_image(&self, g: &Group, px: i32) -> Option<shelldata::Image> {
        if let Some(p) = g.pin {
            return self.pins[p].item.as_ref()?.icon.clone();
        }
        let mut img = None;
        if !g.key.contains('\\') {
            let id = self.ids.get(&g.key).cloned().unwrap_or_else(|| g.key.clone());
            img = items::apps_folder_item(&id).and_then(|i| shelldata::icons::icon_for_item(&i, px));
        }
        if img.is_none() {
            let exe = g.wins.first().and_then(|h| self.exes.get(h)).cloned().unwrap_or_else(|| g.key.clone());
            img = shelldata::icons::icon_for_path(&exe, px);
        }
        img
    }

    fn close_thumbs(&mut self) {
        self.thumbs = None;
        self.thumb_want = None;
        // SAFETY: our own timers.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_THUMB_OPEN);
            let _ = KillTimer(Some(self.hwnd), TIMER_THUMB_CLOSE);
        }
    }

    /// The pointer is over `slot` (None: elsewhere): starts, switches or ends the thumbnails.
    fn thumb_hover(&mut self, slot: Option<usize>) {
        let slot = slot.filter(|&i| i > 0 && self.groups.get(i - 1).is_some_and(|g| !g.wins.is_empty()));
        let open = self.thumbs.as_ref().map(|f| f.slot);
        // SAFETY: timers on our own window.
        unsafe {
            match (open, slot) {
                (Some(o), Some(i)) if o == i => {
                    let _ = KillTimer(Some(self.hwnd), TIMER_THUMB_CLOSE);
                }
                (Some(_), Some(i)) => self.open_thumbs(i),
                (Some(_), None) => {
                    SetTimer(Some(self.hwnd), TIMER_THUMB_CLOSE, THUMB_CLOSE_MS, None);
                }
                (None, Some(i)) => {
                    if self.thumb_want != Some(i) {
                        self.thumb_want = Some(i);
                        let mut ms = 400u32;
                        let _ = SystemParametersInfoW(SPI_GETMOUSEHOVERTIME, 0, Some(&mut ms as *mut _ as _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
                        SetTimer(Some(self.hwnd), TIMER_THUMB_OPEN, ms.max(1), None);
                    }
                }
                (None, None) => {
                    self.thumb_want = None;
                    let _ = KillTimer(Some(self.hwnd), TIMER_THUMB_OPEN);
                }
            }
        }
    }

    fn open_thumbs(&mut self, i: usize) {
        self.thumbs = None;
        // SAFETY: our own timer.
        let _ = unsafe { KillTimer(Some(self.hwnd), TIMER_THUMB_CLOSE) };
        let Some(g) = self.groups.get(i - 1).cloned() else { return };
        let wins: Vec<HWND> = g.wins.iter().map(|h| HWND(*h as _)).collect();
        let k = self.scale();
        let icon = self.group_image(&g, (16.0 * k).round() as i32);
        let wr = self.window_rect();
        let anchor = wr.left + (self.row().cx(i) * k).round() as i32;
        let t = Instant::now();
        match ThumbFlyout::open(i, wins, icon, anchor, wr.top, k, &self.theme) {
            Ok(f) => {
                let mut r = RECT::default();
                // SAFETY: reads the flyout's rect.
                let _ = unsafe { GetWindowRect(f.hwnd, &mut r) };
                log::line(&format!("thumbnails open: button {i} {}, {} windows at {},{} {}x{} in {} ms", g.key, g.wins.len(), r.left, r.top, r.right - r.left, r.bottom - r.top, t.elapsed().as_millis()));
                self.thumbs = Some(f);
            }
            Err(e) => log::line(&format!("thumbnails: {e}")),
        }
    }

    /// A mouse message on the thumbnail flyout.
    fn thumbs_mouse(&mut self, msg: u32, x: i32, y: i32) {
        let Some(f) = self.thumbs.as_mut() else { return };
        let hwnd = self.hwnd;
        match msg {
            WM_MOUSEMOVE => {
                let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: f.hwnd, dwHoverTime: 0 };
                // SAFETY: tracks the pointer leaving our flyout; stops its close timer.
                unsafe {
                    let _ = TrackMouseEvent(&mut tme);
                    let _ = KillTimer(Some(hwnd), TIMER_THUMB_CLOSE);
                }
                let h = f.hit(x, y);
                if h != f.hover {
                    f.hover = h;
                    f.draw(&self.theme);
                }
            }
            WM_MOUSELEAVE => {
                f.hover = None;
                f.draw(&self.theme);
                // SAFETY: a timer on our own window.
                unsafe { SetTimer(Some(hwnd), TIMER_THUMB_CLOSE, THUMB_CLOSE_MS, None) };
            }
            WM_LBUTTONUP => {
                let Some((i, close)) = f.hit(x, y) else { return };
                let Some(&w) = f.wins.get(i) else { return };
                if close {
                    log::line(&format!("thumbnail close {:?}", w.0));
                    // SAFETY: asks the window to close, like its own close button.
                    let _ = unsafe { PostMessageW(Some(w), WM_SYSCOMMAND, WPARAM(SC_CLOSE as usize), LPARAM(0)) };
                } else {
                    log::line(&format!("thumbnail click {:?}", w.0));
                    self.effects.push(Effect::Activate(w));
                }
                self.close_thumbs();
            }
            _ => {}
        }
    }

    /// Jump list of slot `i`: (owner, anchor, app key, menu, recent paths).
    #[allow(clippy::type_complexity)]
    fn jump_list(&mut self, i: usize) -> Option<(HWND, i32, i32, String, Vec<menu::Node>, Vec<String>)> {
        self.close_thumbs();
        let g = self.groups.get(i - 1)?.clone();
        let pinned = match g.pin {
            Some(p) => Some(self.pins[p].item.as_ref()?),
            None => None,
        };
        let id = self.ids.get(&g.key).cloned().unwrap_or_else(|| g.key.clone());
        let exe = g.wins.first().and_then(|h| self.exes.get(h)).cloned().unwrap_or_else(|| g.key.clone());
        let t = Instant::now();
        let recent = if g.key.contains('\\') { Vec::new() } else { jumplist::recent(&id, 10) };
        let app = match pinned {
            Some(pin) if !pin.name.is_empty() => pin.name.clone(),
            _ => jumplist::app_name(&id, &exe),
        };
        let input = jump::Input {
            app,
            recent: recent.iter().map(|r| r.0.clone()).collect(),
            pinned_to_bar: g.pin.is_some(),
            windows: g.wins.len(),
            ..Default::default()
        };
        let fr = menu::french();
        let nodes = jump::build(&input, fr)
            .into_iter()
            .map(|e| match e {
                jump::Entry::Header(h) => menu::Node::Header(h.to_string()),
                jump::Entry::Item(id, l) => {
                    let glyph = match id {
                        jump::ID_LAUNCH => '\u{E8A7}',
                        jump::ID_PIN if input.pinned_to_bar => '\u{E77A}',
                        jump::ID_PIN => '\u{E718}',
                        jump::ID_CLOSE => '\u{E711}',
                        jump::ID_PINNED..jump::ID_RECENT => '\u{E718}',
                        jump::ID_RECENT..jump::ID_TASK => '\u{E81C}',
                        _ => '\u{E8A7}',
                    };
                    menu::Node::Item(id, l, glyph)
                }
                jump::Entry::Separator => menu::Node::Separator,
            })
            .collect::<Vec<_>>();
        log::line(&format!("jump list {i} {} (app id {id}): {} recent, {} entries in {} ms", g.key, recent.len(), nodes.len(), t.elapsed().as_millis()));
        let k = self.scale();
        let wr = self.window_rect();
        let x = wr.left + (self.row().cx(i) * k).round() as i32;
        Some((self.hwnd, x, wr.top, g.key, nodes, recent.into_iter().map(|r| r.1).collect()))
    }

    fn jump_pick(&mut self, key: &str, p: jump::Pick, recent: &[String]) {
        let Some(index) = self.groups.iter().position(|g| g.key == key) else { return };
        let g = self.groups[index].clone();
        let i = index + 1;
        log::line(&format!("jump list {} {}: {p:?}", i, g.key));
        match p {
            jump::Pick::Launch => self.activate_slot(i, true),
            jump::Pick::Close => {
                for h in &g.wins {
                    // SAFETY: asks each window to close, like its own close button.
                    let _ = unsafe { PostMessageW(Some(HWND(*h as _)), WM_SYSCOMMAND, WPARAM(SC_CLOSE as usize), LPARAM(0)) };
                }
            }
            jump::Pick::Pin => {
                if let Some(pi) = g.pin {
                    self.pins.remove(pi);
                    self.model.pins.remove(pi);
                    log::line(&format!("unpinned {} (this session; Taskband not written)", g.key));
                } else {
                    let id = self.ids.get(&g.key).cloned().unwrap_or_else(|| g.key.clone());
                    let exe = g.wins.first().and_then(|h| self.exes.get(h)).cloned().unwrap_or_default();
                    if let Some(item) = jumplist::pin_item(&g.key, &id, &exe) {
                        let icon = pin_worker::image(&item, (G::ICON * self.scale()).round() as i32);
                        self.pins.push(Pin {
                            source: None,
                            key: g.key.clone(),
                            item: Some(PinData { name: item.name, launch: Launch::Pidl(item.pidl), file_explorer: false, icon }),
                        });
                        self.model.pins.push(g.key.clone());
                        log::line(&format!("pinned {} (this session; Taskband not written)", g.key));
                    }
                }
                self.rebuild();
                self.paint();
            }
            jump::Pick::Recent(n) => {
                if let Some(path) = recent.get(n) {
                    self.effects.push(Effect::Launch(Launch::Path(path.clone())));
                }
            }
            jump::Pick::Pinned(_) | jump::Pick::Task(_) => {}
        }
    }

    /// What Win+B moves over, left to right.
    fn tray_hits(&self) -> Vec<Hit> {
        let l = &self.side.layout;
        let mut v = Vec::new();
        if l.chevron.is_some() {
            v.push(Hit::Chevron);
        }
        v.extend((0..l.icons.len()).map(Hit::Icon));
        v.push(Hit::QuickSettings);
        v.push(Hit::Clock);
        v
    }

    fn on_command(&mut self, cmd: bus::Command, arg: isize) {
        log::line(&format!("command {cmd:?} {arg}"));
        match cmd {
            bus::Command::TaskbarNth => {
                let n = arg.max(1) as usize;
                if n <= self.groups.len() {
                    self.activate_slot(n, false);
                }
            }
            bus::Command::WinX => self.effects.push(Effect::WinX),
            bus::Command::TrayFocus | bus::Command::TaskbarFocus => {
                self.slide_to(true);
                if cmd == bus::Command::TrayFocus {
                    self.on_command_focus(true);
                } else {
                    self.on_command_focus(false);
                }
            }
            bus::Command::Shutdown => self.effects.push(Effect::Quit),
            _ => {}
        }
    }

    fn on_command_focus(&mut self, tray: bool) {
        match tray {
            true => {
                self.focus = None;
                self.tray_focus = Some(0);
                self.effects.push(Effect::FocusBar);
                self.paint();
            }
            false => {
                self.tray_focus = None;
                self.focus = Some(pins::Focus::at(usize::from(!self.groups.is_empty()), &self.groups, &self.pins));
                self.effects.push(Effect::FocusBar);
                self.paint();
            }
        }
    }

    fn on_tray_key(&mut self, f: usize, vk: u16) -> bool {
        let hits = self.tray_hits();
        let n = hits.len();
        match vk {
            v if v == VK_LEFT.0 => self.tray_focus = Some((f + n - 1) % n),
            v if v == VK_RIGHT.0 => self.tray_focus = Some((f + 1) % n),
            v if v == VK_RETURN.0 || v == VK_SPACE.0 => {
                let hit = hits.get(f).copied();
                log::line(&format!("tray key select {hit:?}"));
                let (hw, m, k) = self.ctx();
                let cx = Ctx { hwnd: hw, monitor: m, scale: k, theme: &self.theme };
                self.side.on_down(hit, false);
                let a = self.side.on_up(hit, false, &cx);
                self.asks(a);
            }
            v if v == VK_ESCAPE.0 => {
                self.tray_focus = None;
                if let Some(a) = self.model.active {
                    self.effects.push(Effect::Activate(HWND(a as _)));
                }
            }
            _ => return false,
        }
        self.paint();
        true
    }

    fn on_key(&mut self, vk: u16) -> bool {
        if let Some(f) = self.tray_focus {
            return self.on_tray_key(f, vk);
        }
        let Some(f) = self.focus_slot() else { return false };
        if vk == VK_APPS.0 {
            self.effects.push(if f == 0 { Effect::WinX } else { Effect::JumpList(f) });
            return true;
        }
        let n = self.groups.len() + 1;
        match vk {
            v if v == VK_LEFT.0 => self.focus = Some(pins::Focus::at((f + n - 1) % n, &self.groups, &self.pins)),
            v if v == VK_RIGHT.0 => self.focus = Some(pins::Focus::at((f + 1) % n, &self.groups, &self.pins)),
            v if v == VK_RETURN.0 || v == VK_SPACE.0 => {
                self.focus = None;
                self.activate_slot(f, false);
            }
            v if v == VK_ESCAPE.0 => {
                self.focus = None;
                if let Some(a) = self.model.active {
                    self.effects.push(Effect::Activate(HWND(a as _)));
                }
            }
            _ => return false,
        }
        self.paint();
        true
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match std::panic::catch_unwind(|| handle(hwnd, msg, wp, lp)) {
        Ok(Some(r)) => r,
        // SAFETY: default handling for messages we do not take.
        Ok(None) => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
        Err(_) => {
            PANICKED.with(|p| p.set(true));
            // SAFETY: ends this thread's loop; run() tears down and panics again for the shell.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
    }
}

fn handle(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    if msg == bus::MSG_COMMAND {
        let (cmd, arg) = bus::decode(wp, lp)?;
        with_bar(|b| b.on_command(cmd, arg))?;
        return Some(LRESULT(0));
    }
    if hwnd != bar_hwnd() {
        return popup_message(hwnd, msg, wp, lp);
    }
    let shellhook = BAR.with(|b| b.try_borrow().ok().and_then(|g| g.as_ref().map(|b| b.shellhook)));
    if shellhook.is_some_and(|m| m == msg) {
        return with_bar(|b| b.on_shellhook(wp.0, lp));
    }
    if (crate::taskband::MSG_ACTIVE_ALT..=crate::taskband::MSG_TAB_PROPERTIES).contains(&msg) {
        if let Some(r) = with_bar(|b| b.on_taskband(msg, wp, lp)).flatten() {
            return Some(r);
        }
    }
    match msg {
        notification_state::QUERY => {
            let state = BAR.with(|bar| {
                let bar = bar.try_borrow().ok()?;
                let bar = bar.as_ref()?;
                Some((bar.notification_session, bar.monitor))
            });
            let (session, bounds) = state.unwrap_or_else(|| (Default::default(), primary_monitor()));
            let notification = notification_state::query(session.inactive(), || {
                // SAFETY: a foreground snapshot also handles a closed app with no successor.
                let fg = unsafe { GetForegroundWindow() };
                fullscreen::covered_monitors(fg).contains(&fullscreen::monitor_at(&bounds))
            });
            Some(LRESULT(notification.0 as isize))
        }
        WM_WTSSESSION_CHANGE => with_bar(|b| {
            b.notification_session.changed(wp.0 as u32);
            LRESULT(0)
        }),
        WM_MOUSEACTIVATE => Some(LRESULT(MA_NOACTIVATE as isize)),
        WM_COPYDATA => with_bar(|b| {
            // SAFETY: for WM_COPYDATA, lParam points to a COPYDATASTRUCT valid for this message.
            let cds = unsafe { &*(lp.0 as *const windows::Win32::System::DataExchange::COPYDATASTRUCT) };
            let (h, m, k) = b.ctx();
            let cx = Ctx { hwnd: h, monitor: m, scale: k, theme: &b.theme };
            let reserved = b.reserved_rect();
            let (r, a) = b.side.on_copydata(cds, &cx, reserved);
            b.asks(a);
            LRESULT(r)
        }),
        WM_APP_STATUS | WM_POWERBROADCAST => with_bar(|b| {
            if b.side.refresh_status() {
                b.paint();
            }
            LRESULT(1)
        }),
        WM_APP_CLOSE_FLYOUT => with_bar(|b| {
            b.side.close_flyout();
            LRESULT(0)
        }),
        WM_RBUTTONDOWN => with_bar(|b| {
            let h = b.right_hit(lp);
            if b.side.on_down(h, true) {
                b.effects.push(Effect::Capture(true));
                b.paint();
            }
            LRESULT(0)
        }),
        WM_RBUTTONUP => with_bar(|b| {
            let h = b.right_hit(lp);
            if h.is_none() && b.side.pressed.is_none() {
                if let (Some(i), _) = b.slot_at(lp) {
                    log::line(&format!("right click on button {i}"));
                    b.effects.push(if i == 0 { Effect::WinX } else { Effect::JumpList(i) });
                    return LRESULT(0);
                }
                let wr = b.window_rect();
                let x = wr.left + (lp.0 & 0xffff) as i16 as i32;
                if (wr.left..wr.right).contains(&x) && (0..wr.bottom - wr.top).contains(&((lp.0 >> 16) as i16 as i32)) {
                    b.effects.push(Effect::Context(context::Kind::Taskbar, x, wr.top));
                }
            }
            let (hw, m, k) = b.ctx();
            let cx = Ctx { hwnd: hw, monitor: m, scale: k, theme: &b.theme };
            let a = b.side.on_up(h, true, &cx);
            b.effects.push(Effect::Capture(false));
            b.asks(a);
            b.paint();
            LRESULT(0)
        }),
        WM_APP_DIRTY => with_bar(|b| {
            b.dirty_posted = false;
            b.rebuild();
            b.paint();
            LRESULT(0)
        }),
        pin_worker::MESSAGE => with_bar(|b| {
            b.receive_pins();
            LRESULT(0)
        }),
        task_lookup::MESSAGE => with_bar(|b| {
            b.receive_tasks();
            LRESULT(0)
        }),
        WM_APP_EVENT => with_bar(|b| {
            let h = HWND(wp.0 as _);
            match lp.0 as u32 {
                EVENT_SYSTEM_FOREGROUND => {
                    b.set_active(h);
                    // SAFETY: the queued event may describe a window that has already lost focus.
                    b.fullscreen_changed(unsafe { GetForegroundWindow() });
                }
                EVENT_OBJECT_LOCATIONCHANGE => {
                    // SAFETY: a queued event can outlive the window's foreground activation.
                    b.fullscreen_changed(unsafe { GetForegroundWindow() });
                }
                EVENT_MINIMIZEEND => b.set_active(h),
                EVENT_MINIMIZESTART => {
                    // SAFETY: a plain query.
                    let fg = unsafe { GetForegroundWindow() };
                    b.set_active(fg);
                }
                _ => b.refresh(h),
            }
            LRESULT(0)
        }),
        WM_MOUSEMOVE => with_bar(|b| {
            let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            // SAFETY: tracks the pointer leaving our own window.
            let _ = unsafe { TrackMouseEvent(&mut tme) };
            if b.settings.autohide && !b.side.appbars.fullscreen {
                // SAFETY: a timer on our own window.
                let _ = unsafe { KillTimer(Some(hwnd), TIMER_HIDE) };
                b.slide_to(true);
            }
            let rh = b.right_hit(lp);
            let (slot, _) = b.slot_at(lp);
            let task_slot = if rh.is_some() { None } else { slot };
            b.set_hover(task_slot);
            b.thumb_hover(task_slot);
            let (hw, m, k) = b.ctx();
            let cx = Ctx { hwnd: hw, monitor: m, scale: k, theme: &b.theme };
            if b.side.on_move(rh, &cx) {
                b.paint();
            }
            LRESULT(0)
        }),
        WM_MOUSELEAVE => with_bar(|b| {
            if b.settings.autohide {
                // SAFETY: a timer on our own window.
                unsafe { SetTimer(Some(hwnd), TIMER_HIDE, S::HIDE_DELAY_MS, None) };
            }
            b.set_hover(None);
            b.thumb_hover(None);
            if b.side.on_leave() {
                b.paint();
            }
            LRESULT(0)
        }),
        WM_LBUTTONDOWN => with_bar(|b| {
            let rh = b.right_hit(lp);
            if b.side.on_down(rh, false) {
                b.effects.push(Effect::Capture(true));
                b.paint();
                return LRESULT(0);
            }
            let (slot, _) = b.slot_at(lp);
            b.close_thumbs();
            b.pressed = slot;
            b.effects.push(Effect::Capture(true));
            b.animate();
            LRESULT(0)
        }),
        WM_LBUTTONUP | WM_MBUTTONUP => with_bar(|b| {
            if msg == WM_LBUTTONUP && b.side.pressed.is_some() {
                let rh = b.right_hit(lp);
                let (hw, m, k) = b.ctx();
                let cx = Ctx { hwnd: hw, monitor: m, scale: k, theme: &b.theme };
                let a = b.side.on_up(rh, false, &cx);
                b.effects.push(Effect::Capture(false));
                b.asks(a);
                b.paint();
                return LRESULT(0);
            }
            let (slot, clock) = b.slot_at(lp);
            let pressed = b.pressed.take();
            b.effects.push(Effect::Capture(false));
            b.animate();
            // SAFETY: reads the key state of this thread's input.
            let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
            if msg == WM_MBUTTONUP {
                if let Some(i) = slot.filter(|i| *i > 0) {
                    b.activate_slot(i, true);
                }
            } else if slot.is_some() && slot == pressed {
                b.activate_slot(slot.unwrap(), shift);
            } else if slot.is_none() && clock {
                b.effects.push(Effect::Post(bus::Command::NotificationCenter));
            }
            LRESULT(0)
        }),
        WM_KEYDOWN => with_bar(|b| b.on_key(wp.0 as u16)).filter(|h| *h).map(|_| LRESULT(0)),
        WM_ACTIVATE => {
            if (wp.0 & 0xffff) as u32 == WA_INACTIVE {
                with_bar(|b| {
                    if b.focus.take().is_some() | b.tray_focus.take().is_some() {
                        b.paint();
                    }
                });
            }
            None
        }
        WM_TIMER => with_bar(|b| {
            match wp.0 {
                TIMER_ANIM => {
                    if !b.step() {
                        // SAFETY: our own timer.
                        let _ = unsafe { KillTimer(Some(hwnd), TIMER_ANIM) };
                    }
                    b.paint();
                }
                TIMER_SLIDE => {
                    if !b.slide_step() {
                        // SAFETY: our own timer.
                        let _ = unsafe { KillTimer(Some(hwnd), TIMER_SLIDE) };
                        if b.want_shown {
                            // SAFETY: a timer on our own window.
                            unsafe { SetTimer(Some(hwnd), TIMER_HIDE, S::HIDE_DELAY_MS, None) };
                        }
                    }
                }
                TIMER_HIDE => {
                    // SAFETY: our own timer.
                    let _ = unsafe { KillTimer(Some(hwnd), TIMER_HIDE) };
                    b.maybe_hide();
                }
                TIMER_REVEAL => b.maybe_reveal(),
                TIMER_PROGRESS => b.paint(),
                TIMER_THUMB_OPEN => {
                    // SAFETY: our own timer.
                    let _ = unsafe { KillTimer(Some(hwnd), TIMER_THUMB_OPEN) };
                    if let Some(i) = b.thumb_want.take().filter(|i| b.hover == Some(*i)) {
                        b.open_thumbs(i);
                    }
                }
                TIMER_THUMB_CLOSE => {
                    if b.thumbs.is_some() {
                        log::line("thumbnails closed");
                    }
                    b.close_thumbs();
                }
                side::TIMER_TIP => {
                    let (hw, m, k) = b.ctx();
                    let cx = Ctx { hwnd: hw, monitor: m, scale: k, theme: &b.theme };
                    b.side.on_tip_timer(&cx);
                }
                TIMER_CLOCK => {
                    let a = b.side.prune();
                    b.asks(a);
                    b.clock = shelldata::clock::now();
                    b.paint();
                    // SAFETY: our own timer.
                    unsafe { SetTimer(Some(hwnd), TIMER_CLOCK, shelldata::clock::ms_to_next_minute() as u32, None) };
                }
                _ => {}
            }
            LRESULT(0)
        }),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            // SAFETY: validates our own window's update region.
            unsafe {
                BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
            }
            with_bar(|b| b.paint());
            Some(LRESULT(0))
        }
        WM_SETTINGCHANGE => {
            let area = PCWSTR(lp.0 as *const u16);
            // SAFETY: lParam of WM_SETTINGCHANGE is null or a NUL-terminated string.
            let name = if area.is_null() { String::new() } else { unsafe { area.to_string() }.unwrap_or_default() };
            with_bar(|b| {
                if name == "ImmersiveColorSet" {
                    b.theme = current_theme();
                    style_bar(b.hwnd, &b.theme);
                    b.effects.push(Effect::Reposition);
                } else if wp.0 as u32 == SPI_SETWORKAREA.0 {
                    b.effects.push(Effect::WorkArea);
                } else if b.reload_settings() {
                    b.effects.push(Effect::Reposition);
                }
            });
            Some(LRESULT(0))
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED => {
            log::line(&format!("{} {:#x} {:#x}", if msg == WM_DPICHANGED { "WM_DPICHANGED" } else { "WM_DISPLAYCHANGE" }, wp.0, lp.0));
            with_bar(|b| b.effects.push(Effect::Reposition));
            Some(LRESULT(0))
        }
        WM_CLOSE => Some(LRESULT(0)),
        _ => None,
    }
}

/// Messages of the bar's popups (the flyout); None leaves them to DefWindowProc.
fn popup_message(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    let is_thumbs = BAR.with(|b| b.try_borrow().ok().and_then(|g| g.as_ref().and_then(|b| b.thumbs.as_ref().map(|f| f.hwnd == hwnd)))) == Some(true);
    if is_thumbs {
        return match msg {
            WM_MOUSEACTIVATE => Some(LRESULT(MA_NOACTIVATE as isize)),
            WM_MOUSEMOVE | WM_MOUSELEAVE | WM_LBUTTONUP => {
                let (x, y) = ((lp.0 & 0xffff) as i16 as i32, (lp.0 >> 16 & 0xffff) as i16 as i32);
                with_bar(|b| {
                    b.thumbs_mouse(msg, x, y);
                    LRESULT(0)
                })
            }
            WM_LBUTTONDOWN => Some(LRESULT(0)),
            _ => None,
        };
    }
    match msg {
        WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP => {
            let (x, y) = ((lp.0 & 0xffff) as i16 as i32, (lp.0 >> 16 & 0xffff) as i16 as i32);
            with_bar(|b| {
                if b.side.is_flyout(hwnd) {
                    if let Some((sx, _)) = b.side.flyout_mouse(msg, x, y, &b.theme) {
                        let y = b.window_rect().top;
                        b.effects.push(Effect::Context(context::Kind::Explorust, sx, y));
                    }
                }
                LRESULT(0)
            })
        }
        WM_ACTIVATE => {
            // SAFETY: a plain post to the bar.
            if (wp.0 & 0xffff) as u32 == WA_INACTIVE && with_bar(|b| b.side.is_flyout(hwnd)) == Some(true) {
                let _ = unsafe { PostMessageW(Some(bar_hwnd()), WM_APP_CLOSE_FLYOUT, WPARAM(0), LPARAM(0)) };
            }
            None
        }
        _ => None,
    }
}
