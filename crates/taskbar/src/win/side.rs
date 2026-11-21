//! The right side of the bar and the hosts behind `Shell_TrayWnd`: tray icons (promoted beside
//! the clock, the rest in the chevron flyout), their tooltips and input, AppBars, the quick
//! settings glyphs with their live state, the clock and show desktop. The bar owns one `Side` and
//! hands it its mouse events, WM_COPYDATA and status messages.

use super::appbars::{AppBarHost, to_rect};
use super::{context, log};
use super::popup::{self, Flyout, Popup};
use super::status::{Status, Watch};
use super::trayhost::{self, TrayHost};
use crate::appbar::Rect;
use crate::right::{Hit, Layout};
use crate::theme::Theme;
use crate::tray::Event;
use std::time::Instant;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_LWIN,
};
use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetCursorPos, GetWindowRect, GetWindowThreadProcessId, KillTimer, SetForegroundWindow, SetTimer, ShowWindow, SW_SHOW};

pub const TIMER_TIP: usize = 3;
const TIP_DELAY_MS: u32 = 500;

/// What the bar must do after the side handled something.
#[derive(Default)]
pub struct Asks {
    pub redraw: bool,
    pub work_area: bool,
    pub post: Option<bus::Command>,
    pub topmost: Option<bool>,
    pub menu: Option<(context::Kind, i32, i32)>,
}

pub struct Side {
    pub tray: TrayHost,
    pub appbars: AppBarHost,
    watch: Option<Watch>,
    pub status: Status,
    pub layout: Layout,
    pub hover: Option<Hit>,
    pub pressed: Option<Hit>,
    pub flyout: Option<Flyout>,
    flyout_closed: Option<Instant>,
    tip: Option<Popup>,
    /// The tray icon told NIN_POPUPOPEN, to tell it NIN_POPUPCLOSE.
    popup_open: Option<usize>,
    logo: Option<context::Logo>,
}

/// Where the bar is, for the side's coordinates.
pub struct Ctx<'a> {
    pub hwnd: HWND,
    pub monitor: RECT,
    pub scale: f32,
    pub theme: &'a Theme,
}

impl Ctx<'_> {
    pub fn bar_rect(&self) -> RECT {
        let mut r = RECT::default();
        // SAFETY: reads our own window's rect.
        let _ = unsafe { GetWindowRect(self.hwnd, &mut r) };
        r
    }

    /// Screen x of a DIP x on the bar.
    fn screen_x(&self, x: f32) -> i32 {
        self.bar_rect().left + (x * self.scale).round() as i32
    }
}

impl Side {
    pub fn new(hwnd: HWND, status_msg: u32) -> Side {
        let watch = Watch::new(hwnd, status_msg);
        let status = watch.read();
        log::line(&format!("status: {status:?}"));
        let mut tray = TrayHost::new();
        let logo = context::Logo::new(hwnd);
        if let Some(logo) = &logo {
            let _ = tray.tray.apply(&logo.registration(hwnd), false);
            log::line("explorust tray icon registered in the overflow");
        } else {
            log::line("explorust tray icon could not be loaded");
        }
        Side {
            tray,
            appbars: AppBarHost::new(),
            watch: Some(watch),
            status,
            layout: Layout::new(1280.0, 1261.0, 60.0, 0, false, 2),
            hover: None,
            pressed: None,
            flyout: None,
            flyout_closed: None,
            tip: None,
            popup_open: None,
            logo,
        }
    }

    pub fn refresh_status(&mut self) -> bool {
        let Some(w) = &self.watch else { return false };
        let s = w.read();
        let changed = s != self.status;
        if changed {
            log::line(&format!("status: {s:?}"));
            self.status = s;
        }
        changed
    }

    pub fn glyphs(&self) -> Vec<char> {
        self.status.glyphs()
    }

    /// Recomputes the layout for the clock's width and the promoted icons.
    pub fn relayout(&mut self, width: f32, clock_right: f32, clock_w: f32) {
        let icons = self.tray.tray.visible().len();
        let chevron = !self.tray.tray.overflow().is_empty();
        let l = Layout::new(width, clock_right, clock_w, icons, chevron, self.glyphs().len());
        if l != self.layout {
            log::line(&format!(
                "right side: chevron {:?} icons {:?} quick {:?} glyphs {:?} clock {:?}",
                l.chevron, l.icons, l.quick, l.glyphs, l.clock
            ));
            self.layout = l;
        }
    }

    /// Icon handles of the promoted icons, left to right.
    pub fn visible_hicons(&self) -> Vec<isize> {
        self.tray.tray.visible().iter().map(|&i| self.tray.tray.icons[i].hicon).collect()
    }

    pub fn all_hicons(&self) -> Vec<isize> {
        self.tray.tray.icons.iter().map(|i| i.hicon).collect()
    }

    /// WM_COPYDATA on `Shell_TrayWnd`: (result, asks).
    pub fn on_copydata(&mut self, cds: &COPYDATASTRUCT, cx: &Ctx) -> (isize, Asks) {
        let data = if cds.lpData.is_null() || cds.cbData == 0 {
            &[][..]
        } else {
            // SAFETY: the sender's buffer, valid for this message.
            unsafe { std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize) }
        };
        let mut asks = Asks::default();
        match cds.dwData {
            1 => {
                let (ok, redraw) = self.tray.on_copydata(data);
                asks.redraw = redraw;
                if redraw {
                    self.refresh_flyout(cx);
                }
                (ok as isize, asks)
            }
            0 => {
                let monitor = to_rect(cx.monitor);
                let out = self.appbars.on_copydata(data, monitor, to_rect(cx.bar_rect()));
                asks.work_area = out.work_area;
                (out.result, asks)
            }
            n => {
                log::line(&format!("WM_COPYDATA dwData {n}, {} bytes: ignored", data.len()));
                (0, asks)
            }
        }
    }

    /// The work area the taskbar and the AppBars leave.
    pub fn work_area(&self, monitor: RECT, bar: RECT) -> Rect {
        self.appbars.bars.work_area(to_rect(monitor), to_rect(bar))
    }

    /// Drops dead icons and bars.
    pub fn prune(&mut self) -> Asks {
        let mut a = Asks::default();
        a.redraw = self.tray.prune();
        a.work_area = self.appbars.prune();
        a
    }

    fn icon_at(&self, hit: Option<Hit>) -> Option<usize> {
        match hit {
            Some(Hit::Icon(i)) => self.tray.tray.visible().get(i).copied(),
            _ => None,
        }
    }

    fn is_logo(&self, index: usize) -> bool {
        self.logo.as_ref().is_some_and(|logo| logo.matches(self.tray.tray.icons[index].hicon))
    }

    fn cursor() -> (i32, i32) {
        let mut p = windows::Win32::Foundation::POINT::default();
        // SAFETY: reads the cursor position.
        let _ = unsafe { GetCursorPos(&mut p) };
        (p.x, p.y)
    }

    /// Pointer over the bar at DIP (x, y); None when it left the right side.
    pub fn on_move(&mut self, hit: Option<Hit>, cx: &Ctx) -> bool {
        if let Some(i) = self.icon_at(hit) {
            let (x, y) = Self::cursor();
            trayhost::forward(&self.tray.tray.icons[i], Event::Move, x, y);
        }
        if hit == self.hover {
            return false;
        }
        self.leave_tip();
        self.hover = hit;
        if self.icon_at(hit).is_some() {
            // SAFETY: a timer on our own window.
            unsafe { SetTimer(Some(cx.hwnd), TIMER_TIP, TIP_DELAY_MS, None) };
        }
        true
    }

    fn leave_tip(&mut self) {
        self.tip = None;
        if let Some(i) = self.popup_open.take() {
            if let Some(icon) = self.tray.tray.icons.get(i) {
                let (x, y) = Self::cursor();
                trayhost::forward(icon, Event::Hover(false), x, y);
            }
        }
    }

    /// The hover delay ran out: show the icon's tip, or tell a version 4 icon to show its own.
    pub fn on_tip_timer(&mut self, cx: &Ctx) {
        // SAFETY: our own timer.
        let _ = unsafe { KillTimer(Some(cx.hwnd), TIMER_TIP) };
        let Some(i) = self.icon_at(self.hover) else { return };
        let icon = self.tray.tray.icons[i].clone();
        let Some(Hit::Icon(v)) = self.hover else { return };
        let (a, b) = self.layout.icons[v];
        let anchor = cx.screen_x((a + b) / 2.0);
        if icon.version >= 4 && !icon.show_tip {
            let (x, y) = Self::cursor();
            trayhost::forward(&icon, Event::Hover(true), x, y);
            self.popup_open = Some(i);
        } else if !icon.tip.is_empty() {
            self.tip = popup::tooltip(&icon.tip, anchor, cx.bar_rect().top, cx.scale, cx.theme);
            log::line(&format!("tooltip [{}]: {}", icon.tip, self.tip.is_some()));
        }
    }

    pub fn on_leave(&mut self) -> bool {
        self.leave_tip();
        let changed = self.hover.is_some();
        self.hover = None;
        changed
    }

    /// A button went down on the right side.
    pub fn on_down(&mut self, hit: Option<Hit>, right_button: bool) -> bool {
        self.leave_tip();
        self.pressed = hit;
        if let Some(i) = self.icon_at(hit) {
            let (x, y) = Self::cursor();
            trayhost::forward(&self.tray.tray.icons[i], if right_button { Event::RightDown } else { Event::LeftDown }, x, y);
        }
        hit.is_some()
    }

    /// A button came up on the right side: the click's action.
    pub fn on_up(&mut self, hit: Option<Hit>, right_button: bool, cx: &Ctx) -> Asks {
        let mut a = Asks { redraw: true, ..Default::default() };
        let pressed = self.pressed.take();
        if hit.is_none() || hit != pressed {
            return a;
        }
        if let Some(i) = self.icon_at(hit) {
            let (x, y) = Self::cursor();
            if self.is_logo(i) {
                a.menu = Some((context::Kind::Explorust, x, cx.bar_rect().top));
            } else {
                trayhost::forward(&self.tray.tray.icons[i], if right_button { Event::RightUp } else { Event::LeftUp }, x, y);
            }
            return a;
        }
        if right_button {
            let kind = match hit {
                Some(Hit::Clock) => context::Kind::Clock,
                Some(Hit::QuickSettings) => context::Kind::QuickSettings,
                Some(Hit::ShowDesktop) => context::Kind::ShowDesktop,
                _ => context::Kind::Taskbar,
            };
            a.menu = Some((kind, Self::cursor().0, cx.bar_rect().top));
            return a;
        }
        match hit {
            Some(Hit::Chevron) => self.toggle_flyout(cx),
            Some(Hit::QuickSettings) => quick_settings(),
            Some(Hit::Clock) => a.post = Some(bus::Command::NotificationCenter),
            Some(Hit::ShowDesktop) => a.post = Some(bus::Command::ShowDesktop),
            _ => {}
        }
        a
    }

    fn toggle_flyout(&mut self, cx: &Ctx) {
        if self.flyout.take().is_some() {
            log::line("flyout closed by the chevron");
            return;
        }
        // A click on the chevron first deactivates the open flyout, which closes it.
        if self.flyout_closed.is_some_and(|t| t.elapsed().as_millis() < 300) {
            return;
        }
        let icons = self.tray.tray.overflow();
        let Some((a, b)) = self.layout.chevron else { return };
        let anchor = cx.screen_x((a + b) / 2.0);
        self.flyout = Flyout::open(icons.clone(), anchor, cx.bar_rect().top, cx.scale, cx.theme);
        let Some(f) = self.flyout.as_mut() else {
            log::line("flyout: not created");
            return;
        };
        let hicons: Vec<isize> = icons.iter().map(|&i| self.tray.tray.icons[i].hicon).collect();
        let px = f.icon_px();
        for h in &hicons {
            if !f.has_bitmap(*h) {
                f.set_bitmap(*h, trayhost::icon_image(*h, px));
            }
        }
        f.draw(cx.theme, &hicons);
        // SAFETY: shows and activates our flyout; the click on the bar gave us the foreground right.
        let fg = unsafe {
            let _ = ShowWindow(f.popup.hwnd, SW_SHOW);
            SetForegroundWindow(f.popup.hwnd).as_bool()
        };
        let mut r = RECT::default();
        // SAFETY: reads our own window's rect.
        let _ = unsafe { GetWindowRect(f.popup.hwnd, &mut r) };
        log::line(&format!("flyout open: {} icons at {},{} {}x{}, foreground {fg}", icons.len(), r.left, r.top, r.right - r.left, r.bottom - r.top));
    }

    /// Redraws an open flyout after the icons changed.
    fn refresh_flyout(&mut self, cx: &Ctx) {
        let Some(f) = self.flyout.as_mut() else { return };
        let icons = self.tray.tray.overflow();
        if icons.len() != f.icons.len() {
            self.flyout = None;
            return;
        }
        f.icons = icons.clone();
        let hicons: Vec<isize> = icons.iter().map(|&i| self.tray.tray.icons[i].hicon).collect();
        let px = f.icon_px();
        for h in &hicons {
            if !f.has_bitmap(*h) {
                f.set_bitmap(*h, trayhost::icon_image(*h, px));
            }
        }
        f.draw(cx.theme, &hicons);
    }

    pub fn is_flyout(&self, h: HWND) -> bool {
        self.flyout.as_ref().is_some_and(|f| f.popup.hwnd == h)
    }

    pub fn close_flyout(&mut self) {
        if self.flyout.take().is_some() {
            self.flyout_closed = Some(Instant::now());
            log::line("flyout closed");
        }
    }

    /// Mouse input in the flyout (client px); returns the anchor of the explorust menu.
    pub fn flyout_mouse(&mut self, msg: u32, x: i32, y: i32, theme: &Theme) -> Option<(i32, i32)> {
        use windows::Win32::UI::WindowsAndMessaging::{WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_RBUTTONDOWN, WM_RBUTTONUP};
        let f = self.flyout.as_mut()?;
        let cell = f.hit(x, y);
        let Some(i) = cell.and_then(|c| f.icons.get(c).copied()) else {
            if f.hover.take().is_some() {
                let h: Vec<isize> = f.icons.iter().map(|&i| self.tray.tray.icons[i].hicon).collect();
                f.draw(theme, &h);
            }
            return None;
        };
        let (sx, sy) = f.cell_center(cell.unwrap_or(0));
        let icon = self.tray.tray.icons[i].clone();
        let ev = match msg {
            WM_MOUSEMOVE => {
                if f.hover != cell {
                    f.hover = cell;
                    let h: Vec<isize> = f.icons.iter().map(|&i| self.tray.tray.icons[i].hicon).collect();
                    f.draw(theme, &h);
                }
                Event::Move
            }
            WM_LBUTTONDOWN => Event::LeftDown,
            WM_LBUTTONUP => Event::LeftUp,
            WM_RBUTTONDOWN => Event::RightDown,
            WM_RBUTTONUP => Event::RightUp,
            _ => return None,
        };
        if self.is_logo(i) && matches!(ev, Event::LeftUp | Event::RightUp) {
            self.close_flyout();
            return Some((sx, sy));
        }
        trayhost::forward(&icon, ev, sx, sy);
        if matches!(ev, Event::LeftUp | Event::RightUp) {
            log::line(&format!("flyout click {ev:?} on {:#x}/{}", icon.hwnd, icon.uid));
            if ev == Event::LeftUp {
                self.close_flyout();
            }
        }
        None
    }

    /// The foreground changed: tell the AppBars whether a window now covers the monitor.
    pub fn on_foreground(&mut self, fg: HWND, cx: &Ctx) -> Asks {
        let mut a = Asks::default();
        let full = is_fullscreen(fg, cx.monitor, cx.hwnd);
        if self.appbars.set_fullscreen(full) {
            a.topmost = Some(!full);
        }
        a
    }
}

/// Shell hosts whose full-monitor surfaces (search, Start, the touch keyboard) are part of the
/// shell, not a fullscreen app, as explorer treats them.
const SHELL_HOSTS: [&str; 6] = [
    "searchhost.exe",
    "startmenuexperiencehost.exe",
    "shellexperiencehost.exe",
    "shellhost.exe",
    "textinputhost.exe",
    "lockapp.exe",
];

fn is_fullscreen(fg: HWND, monitor: RECT, bar: HWND) -> bool {
    if fg.is_invalid() || fg == bar {
        return false;
    }
    let mut class = [0u16; 64];
    // SAFETY: reads a class name and a rect of the foreground window.
    let n = unsafe { GetClassNameW(fg, &mut class) };
    let class = String::from_utf16_lossy(&class[..n.max(0) as usize]);
    if matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "explorust.taskbar.popup") {
        return false;
    }
    let mut r = RECT::default();
    let _ = unsafe { GetWindowRect(fg, &mut r) };
    if !to_rect(r).covers(&to_rect(monitor)) {
        return false;
    }
    let mut pid = 0u32;
    // SAFETY: reads the owner process id of a window.
    unsafe { GetWindowThreadProcessId(fg, Some(&mut pid)) };
    // Start, Task View, the desktop and Files are explorust's own windows: never an app.
    if pid == std::process::id() {
        return false;
    }
    let exe = shelldata::items::process_path(pid).unwrap_or_default();
    let file = exe.rsplit(['\\', '/']).next().unwrap_or(&exe).to_lowercase();
    if SHELL_HOSTS.contains(&file.as_str()) {
        return false;
    }
    log::line(&format!("fullscreen window {:#x} {class} {file}", fg.0 as usize));
    true
}

/// Opens Windows' quick settings: the notify part when it answers the bus, else Win+A, which
/// Windows' own host answers without explorer (`.agents/research/twinui.md`).
fn quick_settings() {
    if bus::post(bus::Command::QuickSettings, 0) {
        log::line("quick settings: bus QuickSettings posted");
        return;
    }
    let key = |vk: VIRTUAL_KEY, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, ..Default::default() } },
    };
    let a = VIRTUAL_KEY(0x41);
    let inputs = [key(VK_LWIN, false), key(a, false), key(a, true), key(VK_LWIN, true)];
    // SAFETY: four keyboard events.
    let n = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
    log::line(&format!("quick settings: Win+A sent ({n} events)"));
}
