//! `Shell_TrayWnd` and the command window `explorust.taskbar`: window creation, the shell hook,
//! WinEvent hooks, mouse and keyboard, bus commands. State lives in a thread-local `Bar`; calls
//! that can send messages back to our windows (activation, capture, the work area broadcast)
//! are queued as effects and run once the state is released.

use super::actions::{self, Launch};
use super::scene::{Scene, Slot};
use super::tasks;
use super::workarea::WorkArea;
use super::log;
use crate::geom::{self as G, Row};
use crate::model::{Action, Group, Model};
use crate::theme::Theme;
use shelldata::items::{self, PinItem};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::time::Instant;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint, PAINTSTRUCT,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_ESCAPE, VK_LEFT, VK_RETURN,
    VK_RIGHT, VK_SHIFT, VK_SPACE,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{HSTRING, PCWSTR, w};

const WM_MOUSELEAVE: u32 = 0x02A3;
/// Coalesced repaint after the model changed.
const WM_APP_DIRTY: u32 = WM_APP + 1;
/// A WinEvent, forwarded from the hook callback: wParam = hwnd, lParam = event.
const WM_APP_EVENT: u32 = WM_APP + 2;
const TIMER_ANIM: usize = 1;
const TIMER_CLOCK: usize = 2;
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
    Reposition,
    Quit,
}

struct Bar {
    hwnd: HWND,
    cmd: HWND,
    shellhook: u32,
    monitor: RECT,
    h_px: i32,
    theme: Theme,
    scene: Option<Scene>,
    model: Model,
    pins: Vec<PinItem>,
    /// Exe path and original-case app id of each task window.
    exes: HashMap<isize, String>,
    ids: HashMap<String, String>,
    groups: Vec<Group>,
    hover: Option<usize>,
    pressed: Option<usize>,
    hover_v: Vec<f32>,
    press_v: Vec<f32>,
    last_step: Instant,
    focus: Option<usize>,
    clock: (String, String),
    work: Option<WorkArea>,
    hooks: Vec<HWINEVENTHOOK>,
    dirty_posted: bool,
    effects: Vec<Effect>,
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
            let _ = DeregisterShellHookWindow(bar.hwnd);
            let _ = DestroyWindow(bar.hwnd);
            let _ = DestroyWindow(bar.cmd);
        }
        bar.scene = None;
        drop(bar.work.take());
    }
    BAR_HWND.with(|h| h.set(0));
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
        let cmd = CreateWindowExW(WINDOW_EX_STYLE(0), PCWSTR(class.as_ptr()), w!(""), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(inst.into()), None)?;
        bus::listen(cmd);
        BAR_HWND.with(|h| h.set(hwnd.0 as isize));
        let theme = current_theme();
        render::backdrop::apply(hwnd, render::backdrop::Backdrop::Acrylic { tint_abgr: theme.tint_abgr });
        let (h_px, scene) = place(hwnd, monitor, &theme)?;
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let work = WorkArea::reserve(monitor, h_px);
        let shellhook = RegisterWindowMessageW(w!("SHELLHOOK"));
        let hooked = RegisterShellHookWindow(hwnd).as_bool();
        let mut hooks = Vec::new();
        for (a, b) in [
            (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
            (EVENT_MINIMIZESTART, EVENT_MINIMIZEEND),
            (EVENT_OBJECT_SHOW, EVENT_OBJECT_HIDE),
            (EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_NAMECHANGE),
            (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
        ] {
            let h = SetWinEventHook(a, b, None, Some(on_winevent), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);
            if !h.is_invalid() {
                hooks.push(h);
            }
        }
        let t0 = Instant::now();
        let pins = items::taskbar_pins();
        let mut model = Model::default();
        model.pins = pins.iter().map(|p| p.key.clone()).collect();
        let mut exes = HashMap::new();
        let mut ids = HashMap::new();
        let mut all = tasks::top_level();
        all.reverse(); // bottom of the z-order first: roughly the oldest windows first
        for h in all {
            if let Some(t) = tasks::task_of(h) {
                model.upsert(h.0 as isize, &t.key);
                exes.insert(h.0 as isize, t.exe);
                ids.insert(t.key, t.id);
            }
        }
        let fg = GetForegroundWindow();
        model.set_active((!fg.is_invalid()).then_some(fg.0 as isize));
        let dpi = GetDpiForWindow(hwnd);
        log::line(&format!(
            "started: monitor {}x{} dpi {dpi} bar {h_px} px, shell hook {hooked}, {} win hooks, {} pins, {} windows in {} ms",
            width,
            monitor.bottom - monitor.top,
            hooks.len(),
            pins.len(),
            model.wins.len(),
            t0.elapsed().as_millis()
        ));
        for p in &pins {
            log::line(&format!("pin {} = {}", p.name, p.key));
        }
        let bar = Bar {
            hwnd,
            cmd,
            shellhook,
            monitor,
            h_px,
            theme,
            scene: Some(scene),
            model,
            pins,
            exes,
            ids,
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
        };
        BAR.with(|b| *b.borrow_mut() = Some(bar));
        with_bar(|b| {
            b.rebuild();
            b.paint();
            b.log_groups();
        });
        SetTimer(Some(hwnd), TIMER_CLOCK, shelldata::clock::ms_to_next_minute() as u32, None);
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
            log::line(&format!("activate {:?}: {}", h.0, if ok { "foreground" } else { "not foreground" }));
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
                if let Some(w) = &b.work {
                    w.apply(b.monitor, b.h_px)
                }
            });
        }
        Effect::Reposition => {
            let _ = with_bar(|b| b.reposition());
        }
        Effect::Quit => {
            teardown();
            // SAFETY: ends this thread's loop.
            unsafe { PostQuitMessage(0) };
        }
    }
}

unsafe extern "system" fn on_winevent(_: HWINEVENTHOOK, event: u32, hwnd: HWND, id_object: i32, id_child: i32, _: u32, _: u32) {
    if id_object != OBJID_WINDOW.0 || id_child != CHILDID_SELF as i32 || hwnd.is_invalid() {
        return;
    }
    let bar = bar_hwnd();
    if bar.0.is_null() || hwnd == bar {
        return;
    }
    // SAFETY: posts to our own window; handled in order with the shell hook messages.
    let _ = unsafe { PostMessageW(Some(bar), WM_APP_EVENT, WPARAM(hwnd.0 as usize), LPARAM(event as isize)) };
}

impl Bar {
    fn row(&self) -> Row {
        Row { width: self.scene.as_ref().map_or(1280.0, |s| s.width), buttons: self.groups.len() + 1 }
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

    /// Re-reads one window: adds, re-keys or drops its button.
    fn refresh(&mut self, h: HWND) {
        let id = h.0 as isize;
        let changed = match tasks::task_of(h) {
            Some(t) => {
                self.exes.insert(id, t.exe);
                self.ids.insert(t.key.clone(), t.id);
                let new = !self.model.contains(id);
                let c = self.model.upsert(id, &t.key);
                if new {
                    log::line(&format!("window {id:#x} {} added", t.key));
                }
                c
            }
            None => {
                self.exes.remove(&id);
                self.model.remove(id)
            }
        };
        if changed {
            self.dirty();
        }
    }

    fn set_active(&mut self, h: HWND) {
        if h == self.hwnd {
            return;
        }
        let a = (!h.is_invalid()).then_some(h.0 as isize);
        if a.is_some_and(|a| !self.model.contains(a)) {
            self.refresh(h);
        }
        if self.model.set_active(a) {
            self.dirty();
        }
    }

    fn rebuild(&mut self) {
        self.groups = self.model.groups();
        let n = self.groups.len() + 1;
        self.hover_v.resize(n, 0.0);
        self.press_v.resize(n, 0.0);
        if self.focus.is_some_and(|f| f >= n) {
            self.focus = Some(n - 1);
        }
        self.load_icons();
    }

    fn load_icons(&mut self) {
        let Some(scene) = self.scene.as_mut() else { return };
        let px = scene.icon_px();
        for g in &self.groups {
            if scene.has_icon(&g.key) {
                continue;
            }
            let t = Instant::now();
            let mut img = g.pin.and_then(|p| shelldata::icons::icon_for_item(&self.pins[p].item, px));
            if img.is_none() && !g.key.contains('\\') {
                let id = self.ids.get(&g.key).cloned().unwrap_or_else(|| g.key.clone());
                img = items::apps_folder_item(&id).and_then(|i| shelldata::icons::icon_for_item(&i, px));
            }
            if img.is_none() {
                let exe = g.wins.first().and_then(|h| self.exes.get(h)).cloned().unwrap_or_else(|| g.key.clone());
                img = shelldata::icons::icon_for_path(&exe, px);
            }
            log::line(&format!("icon {} {}: {} ms", g.key, if img.is_some() { "loaded" } else { "missing" }, t.elapsed().as_millis()));
            scene.set_icon(&g.key, img);
        }
    }

    fn log_groups(&self) {
        for (i, g) in self.groups.iter().enumerate() {
            log::line(&format!("button {} {} pin={:?} windows={}", i + 1, g.key, g.pin, g.wins.len()));
        }
    }

    fn paint(&mut self) {
        let Some(scene) = self.scene.as_ref() else { return };
        let mut slots = Vec::with_capacity(self.groups.len() + 1);
        slots.push(Slot { key: None, running: false, active: false, flashing: false, hover: self.hover_v[0], press: self.press_v[0], focused: self.focus == Some(0) });
        for (i, g) in self.groups.iter().enumerate() {
            slots.push(Slot {
                key: Some(&g.key),
                running: !g.wins.is_empty(),
                active: self.model.is_active(g),
                flashing: g.flashing,
                hover: self.hover_v[i + 1],
                press: self.press_v[i + 1],
                focused: self.focus == Some(i + 1),
            });
        }
        if let Err(e) = scene.draw(&self.theme, &slots, &self.clock) {
            log::line(&format!("draw failed: {e}"));
        }
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
        let action = self.model.click(&g, new_instance);
        log::line(&format!("click {} {}: {action:?}", i, g.key));
        match action {
            Action::Activate(h) => self.effects.push(Effect::Activate(HWND(h as _))),
            Action::Minimize(h) => self.effects.push(Effect::Minimize(HWND(h as _))),
            Action::Launch => {
                let explorer = g.key == "microsoft.windows.explorer" || g.key.ends_with("\\explorer.exe");
                if explorer {
                    // Starting explorer.exe would make it the shell: File Explorer is our Files part.
                    self.effects.push(Effect::Post(bus::Command::FilesOpen));
                } else if let Some(p) = g.pin {
                    self.effects.push(Effect::Launch(Launch::Pidl(self.pins[p].pidl.clone())));
                } else if !g.key.contains('\\') {
                    let id = self.ids.get(&g.key).cloned().unwrap_or_else(|| g.key.clone());
                    self.effects.push(Effect::Launch(Launch::Aumid(id)));
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
                self.effects.push(Effect::WorkArea);
                self.rebuild();
                self.paint();
            }
            Err(e) => log::line(&format!("reposition failed: {e}")),
        }
    }

    fn on_shellhook(&mut self, code: usize, lp: LPARAM) -> LRESULT {
        let h = HWND(lp.0 as _);
        match code as u32 {
            HSHELL_WINDOWCREATED | HSHELL_REDRAW | HSHELL_WINDOWREPLACED => self.refresh(h),
            HSHELL_WINDOWDESTROYED => {
                self.exes.remove(&(lp.0));
                if self.model.remove(lp.0) {
                    self.dirty();
                }
            }
            HSHELL_WINDOWACTIVATED => self.set_active(h),
            HSHELL_GETMINRECT => return self.min_rect(lp),
            _ => match code {
                HSHELL_RUDEAPPACTIVATED => self.set_active(h),
                HSHELL_FLASH => {
                    if !self.model.contains(lp.0) {
                        self.refresh(h);
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

    fn on_command(&mut self, cmd: bus::Command, arg: isize) {
        log::line(&format!("command {cmd:?} {arg}"));
        match cmd {
            bus::Command::TaskbarNth => {
                let n = arg.max(1) as usize;
                if n <= self.groups.len() {
                    self.activate_slot(n, false);
                }
            }
            bus::Command::TaskbarFocus => {
                self.focus = Some(if self.groups.is_empty() { 0 } else { 1 });
                self.effects.push(Effect::FocusBar);
                self.paint();
            }
            bus::Command::Shutdown => self.effects.push(Effect::Quit),
            _ => {}
        }
    }

    fn on_key(&mut self, vk: u16) -> bool {
        let Some(f) = self.focus else { return false };
        let n = self.groups.len() + 1;
        match vk {
            v if v == VK_LEFT.0 => self.focus = Some((f + n - 1) % n),
            v if v == VK_RIGHT.0 => self.focus = Some((f + 1) % n),
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
        return None;
    }
    let shellhook = BAR.with(|b| b.try_borrow().ok().and_then(|g| g.as_ref().map(|b| b.shellhook)));
    if shellhook.is_some_and(|m| m == msg) {
        return with_bar(|b| b.on_shellhook(wp.0, lp));
    }
    match msg {
        WM_MOUSEACTIVATE => Some(LRESULT(MA_NOACTIVATE as isize)),
        WM_APP_DIRTY => with_bar(|b| {
            b.dirty_posted = false;
            b.rebuild();
            b.paint();
            LRESULT(0)
        }),
        WM_APP_EVENT => with_bar(|b| {
            let h = HWND(wp.0 as _);
            match lp.0 as u32 {
                EVENT_SYSTEM_FOREGROUND | EVENT_MINIMIZEEND => b.set_active(h),
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
            let (slot, _) = b.slot_at(lp);
            b.set_hover(slot);
            LRESULT(0)
        }),
        WM_MOUSELEAVE => with_bar(|b| {
            b.set_hover(None);
            LRESULT(0)
        }),
        WM_LBUTTONDOWN => with_bar(|b| {
            let (slot, _) = b.slot_at(lp);
            b.pressed = slot;
            b.effects.push(Effect::Capture(true));
            b.animate();
            LRESULT(0)
        }),
        WM_LBUTTONUP | WM_MBUTTONUP => with_bar(|b| {
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
                    if b.focus.take().is_some() {
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
                TIMER_CLOCK => {
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
                    render::backdrop::apply(b.hwnd, render::backdrop::Backdrop::Acrylic { tint_abgr: b.theme.tint_abgr });
                    b.effects.push(Effect::Reposition);
                } else if wp.0 as u32 == SPI_SETWORKAREA.0 {
                    b.effects.push(Effect::WorkArea);
                }
            });
            Some(LRESULT(0))
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED => {
            with_bar(|b| b.effects.push(Effect::Reposition));
            Some(LRESULT(0))
        }
        WM_CLOSE => Some(LRESULT(0)),
        _ => None,
    }
}
