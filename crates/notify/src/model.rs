//! Toast data, the banner policy (Do Not Disturb, per-app settings) and the card geometry, kept
//! free of Win32 so `cargo test -p notify` checks them on Linux.

/// A toast read from the notification platform.
#[derive(Clone, Debug, PartialEq)]
pub struct Toast {
    /// `UserNotification.Id`, unique while the notification lives in the center.
    pub id: u32,
    /// The sender's AppUserModelID (packaged `Family!App` or a Win32 id).
    pub aumid: String,
    /// The sender's display name, shown in the card's header.
    pub app: String,
    /// The ToastGeneric text elements: the title first, then the body lines.
    pub texts: Vec<String>,
}

impl Toast {
    pub fn title(&self) -> &str {
        self.texts.first().map(String::as_str).unwrap_or("")
    }

    /// Body lines under the title, at most `MAX_BODY_LINES`, empty ones dropped.
    pub fn body(&self) -> Vec<&str> {
        self.texts.iter().skip(1).map(String::as_str).filter(|s| !s.trim().is_empty()).take(MAX_BODY_LINES).collect()
    }
}

pub const MAX_BODY_LINES: usize = 3;

/// `SHQueryUserNotificationState` values (shellapi.h).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserState {
    NotPresent,
    Busy,
    FullScreenD3d,
    Presentation,
    Accepts,
    QuietTime,
    StoreApp,
    Unknown,
}

impl UserState {
    pub fn from_quns(v: i32) -> UserState {
        match v {
            1 => UserState::NotPresent,
            2 => UserState::Busy,
            3 => UserState::FullScreenD3d,
            4 => UserState::Presentation,
            5 => UserState::Accepts,
            6 => UserState::QuietTime,
            7 => UserState::StoreApp,
            _ => UserState::Unknown,
        }
    }
}

/// The user's notification settings that decide whether a toast pops up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Settings > System > Notifications, the main switch (`PushNotifications\ToastEnabled`).
    pub toasts_on: bool,
    /// Do Not Disturb (`Notifications\Settings\NOC_GLOBAL_SETTING_TOASTS_ENABLED` = 0).
    pub dnd: bool,
    /// The app's own switch (`Notifications\Settings\<aumid>\Enabled`).
    pub app_on: bool,
    /// The app's "Show notification banners" (`...\<aumid>\ShowBanner`).
    pub app_banner: bool,
    /// The app may break through Do Not Disturb (`...\<aumid>\AllowUrgentNotifications`).
    pub app_urgent: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { toasts_on: true, dnd: false, app_on: true, app_banner: true, app_urgent: false }
    }
}

/// What to do with a new toast.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Banner {
    /// Pop the card up with its sound.
    Show,
    /// Keep it in the notification center only (Do Not Disturb, full screen, banners off).
    Quiet,
}

/// Windows' rule: no banner when notifications or the app are off, the app's banners are off,
/// Do Not Disturb is on (unless the app is a priority one), or the user is away, presenting or
/// in a full-screen app.
pub fn banner(state: UserState, s: Settings) -> Banner {
    if !s.toasts_on || !s.app_on || !s.app_banner {
        return Banner::Quiet;
    }
    let busy = matches!(state, UserState::NotPresent | UserState::Busy | UserState::FullScreenD3d | UserState::Presentation);
    let quiet = s.dnd || state == UserState::QuietTime;
    if busy || (quiet && !s.app_urgent) { Banner::Quiet } else { Banner::Show }
}

/// Card geometry in DIPs, measured on Windows' toast in g1 (`captures/notify-1-windows-toast.png`):
/// 364 wide, 16 from the work area's right edge, 14 above its bottom.
pub mod geo {
    pub const WIDTH: f32 = 364.0;
    pub const RIGHT_GAP: f32 = 16.0;
    pub const BOTTOM_GAP: f32 = 14.0;
    /// Between two stacked cards.
    pub const STACK_GAP: f32 = 8.0;
    pub const PAD: f32 = 16.0;
    /// The header band: app icon and name, the close button.
    pub const HEADER: f32 = 46.0;
    pub const ICON: f32 = 16.0;
    pub const LINE: f32 = 20.0;
    /// Center of the close glyph from the card's top-right corner.
    pub const CLOSE_DX: f32 = 28.0;
    pub const CLOSE_CY: f32 = 23.0;
    pub const CLOSE_HIT: f32 = 32.0;
    pub const RADIUS: f32 = 8.0;
}

/// Card height in DIPs for a toast with a title and `body_lines` lines.
pub fn card_height(body_lines: usize) -> f32 {
    geo::HEADER + 4.0 + geo::LINE * (1 + body_lines) as f32 + geo::PAD
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Screen rects (physical pixels) of stacked cards, the newest (index 0) at the bottom right of
/// the work area, older ones above it. `work` = (left, top, right, bottom) in pixels.
pub fn stack(work: (i32, i32, i32, i32), heights_dip: &[f32], scale: f32) -> Vec<Rect> {
    let w = geo::WIDTH * scale;
    let x = work.2 as f32 - geo::RIGHT_GAP * scale - w;
    let mut bottom = work.3 as f32 - geo::BOTTOM_GAP * scale;
    heights_dip
        .iter()
        .map(|h| {
            let h = h * scale;
            let r = Rect { x: x.round(), y: (bottom - h).round(), w: w.round(), h: h.round() };
            bottom -= h + geo::STACK_GAP * scale;
            r
        })
        .collect()
}

/// Where a click on a card lands, from DIP coordinates inside the card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Close,
    Body,
}

pub fn hit(x: f32, y: f32) -> Hit {
    let cx = geo::WIDTH - geo::CLOSE_DX;
    let half = geo::CLOSE_HIT / 2.0;
    if (x - cx).abs() <= half && (y - geo::CLOSE_CY).abs() <= half { Hit::Close } else { Hit::Body }
}

/// How many cards can stand on screen at once (Windows shows at most three).
pub const MAX_ON_SCREEN: usize = 3;

/// Who draws toast popups in this session. Windows' own (ShellExperienceHost) are the 1:1 target:
/// when they show, explorust's cards step aside; when they do not, the cards stand in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Popups {
    /// No toast checked yet.
    Unknown,
    /// Windows' popup showed for the last toast.
    Windows,
    /// Windows' popup did not show for the last toast.
    Ours,
}

impl Popups {
    /// Whether a new toast's card shows at once (else it waits for the check, hidden).
    pub fn card_first(self) -> bool {
        self != Popups::Windows
    }

    /// After the check of one toast: whether Windows' popup was on screen.
    pub fn after_check(self, windows_seen: bool) -> Popups {
        if windows_seen { Popups::Windows } else { Popups::Ours }
    }
}

/// Milliseconds after a toast arrives before looking for Windows' popup (it slides in within
/// about 300 ms of the platform's change event).
pub const POPUP_CHECK_MS: u32 = 700;

/// Whether a visible ShellExperienceHost window is Windows' toast popup rather than the
/// notification center or the calendar: a toast is about 364 DIPs wide and at most about 450 tall,
/// sitting at the bottom right of the work area; the center and the calendar are taller.
pub fn is_toast_popup(rect: (i32, i32, i32, i32), work: (i32, i32, i32, i32), scale: f32) -> bool {
    let (l, t, r, b) = rect;
    let (w, h) = ((r - l) as f32 / scale, (b - t) as f32 / scale);
    let near_right = (work.2 - r) as f32 / scale <= 64.0;
    let near_bottom = (work.3 - b) as f32 / scale <= 64.0;
    (250.0..=480.0).contains(&w) && (40.0..=450.0).contains(&h) && near_right && near_bottom
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popups() {
        assert!(Popups::Unknown.card_first());
        assert!(Popups::Ours.card_first());
        assert!(!Popups::Windows.card_first());
        assert_eq!(Popups::Unknown.after_check(true), Popups::Windows);
        assert_eq!(Popups::Windows.after_check(false), Popups::Ours);
    }

    #[test]
    fn toast_popup_shape() {
        let work = (0, 0, 1280, 752);
        assert!(is_toast_popup((900, 600, 1264, 738), work, 1.0));
        // The notification center: as tall as the work area.
        assert!(!is_toast_popup((900, 12, 1264, 738), work, 1.0));
        // Top left: not a toast.
        assert!(!is_toast_popup((0, 0, 364, 120), work, 1.0));
    }

    fn toast(texts: &[&str]) -> Toast {
        Toast { id: 1, aumid: "a".into(), app: "App".into(), texts: texts.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn title_and_body() {
        let t = toast(&["Title", "", "one", "two", "three", "four"]);
        assert_eq!(t.title(), "Title");
        assert_eq!(t.body(), vec!["one", "two", "three"]);
        assert_eq!(toast(&[]).title(), "");
    }

    #[test]
    fn banner_policy() {
        let s = Settings::default();
        assert_eq!(banner(UserState::Accepts, s), Banner::Show);
        assert_eq!(banner(UserState::StoreApp, s), Banner::Show);
        assert_eq!(banner(UserState::QuietTime, s), Banner::Quiet);
        assert_eq!(banner(UserState::Presentation, s), Banner::Quiet);
        assert_eq!(banner(UserState::Accepts, Settings { dnd: true, ..s }), Banner::Quiet);
        assert_eq!(banner(UserState::Accepts, Settings { dnd: true, app_urgent: true, ..s }), Banner::Show);
        assert_eq!(banner(UserState::Busy, Settings { app_urgent: true, ..s }), Banner::Quiet);
        assert_eq!(banner(UserState::Accepts, Settings { app_banner: false, ..s }), Banner::Quiet);
        assert_eq!(banner(UserState::Accepts, Settings { toasts_on: false, ..s }), Banner::Quiet);
        assert_eq!(UserState::from_quns(6), UserState::QuietTime);
        assert_eq!(UserState::from_quns(99), UserState::Unknown);
    }

    #[test]
    fn windows_card_height() {
        // Windows' two-line toast without buttons in g1: 106 px at 100%.
        assert_eq!(card_height(1), 106.0);
    }

    #[test]
    fn stack_from_bottom_right() {
        let r = stack((0, 0, 1280, 752), &[106.0, 126.0], 1.0);
        assert_eq!(r[0], Rect { x: 900.0, y: 632.0, w: 364.0, h: 106.0 });
        assert_eq!(r[1], Rect { x: 900.0, y: 498.0, w: 364.0, h: 126.0 });
        let r = stack((0, 0, 2560, 1504), &[106.0], 2.0);
        assert_eq!(r[0], Rect { x: 1800.0, y: 1264.0, w: 728.0, h: 212.0 });
    }

    #[test]
    fn close_hit() {
        assert_eq!(hit(336.0, 23.0), Hit::Close);
        assert_eq!(hit(350.0, 10.0), Hit::Close);
        assert_eq!(hit(100.0, 60.0), Hit::Body);
        assert_eq!(hit(336.0, 60.0), Hit::Body);
    }
}
