//! The bar's settings as Windows keeps them (read only; explorust never writes them):
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced` (TaskbarAl, TaskbarGlomLevel,
//! MMTaskbarEnabled, MMTaskbarMode, MMTaskbarGlomLevel, TaskbarSmallIcons, IconSizePreference) and
//! `...\Explorer\StuckRects3` `Settings` (auto-hide at byte 8, the edge at byte 12).

/// How buttons of one app combine (TaskbarGlomLevel, MMTaskbarGlomLevel).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Combine {
    /// 0: always combine, hide labels (Windows 11's default).
    #[default]
    Always,
    /// 1: combine when the bar is full.
    WhenFull,
    /// 2: never combine.
    Never,
}

impl Combine {
    pub fn from_dword(v: Option<u32>) -> Combine {
        match v {
            Some(1) => Combine::WhenFull,
            Some(2) => Combine::Never,
            _ => Combine::Always,
        }
    }
}

/// Which bars show a window's button when the bar is on all displays (MMTaskbarMode).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MultiMode {
    /// 0: every bar shows every button.
    #[default]
    All,
    /// 1: the main bar shows all, the others their monitor's windows.
    MainAndWhereOpen,
    /// 2: each bar shows its monitor's windows.
    WhereOpen,
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Settings {
    /// TaskbarAl = 0: buttons from the left edge (Windows 11 centers them by default).
    pub align_left: bool,
    /// StuckRects3 byte 8, bit 0.
    pub autohide: bool,
    /// Smaller buttons: TaskbarSmallIcons = 1 or IconSizePreference = 1 (always).
    pub small: bool,
    pub combine: Combine,
    /// MMTaskbarEnabled = 1: a bar on every display.
    pub all_displays: bool,
    pub multi_mode: MultiMode,
    pub combine_other: Combine,
}

/// The raw values, None where Windows has no value (its default applies).
#[derive(Clone, Debug, Default)]
pub struct Raw {
    pub taskbar_al: Option<u32>,
    pub glom: Option<u32>,
    pub mm_enabled: Option<u32>,
    pub mm_mode: Option<u32>,
    pub mm_glom: Option<u32>,
    pub small_icons: Option<u32>,
    pub icon_size: Option<u32>,
    pub stuck_rects: Option<Vec<u8>>,
}

impl Settings {
    pub fn from_raw(r: &Raw) -> Settings {
        Settings {
            align_left: r.taskbar_al == Some(0),
            autohide: r.stuck_rects.as_deref().is_some_and(autohide_bit),
            small: r.small_icons == Some(1) || r.icon_size == Some(1),
            combine: Combine::from_dword(r.glom),
            all_displays: r.mm_enabled == Some(1),
            multi_mode: match r.mm_mode {
                Some(1) => MultiMode::MainAndWhereOpen,
                Some(2) => MultiMode::WhereOpen,
                _ => MultiMode::All,
            },
            combine_other: Combine::from_dword(r.mm_glom.or(r.glom)),
        }
    }
}

/// StuckRects3 `Settings`: byte 8 holds the auto-hide bit (3 with auto-hide, 2 without).
pub fn autohide_bit(b: &[u8]) -> bool {
    b.get(8).is_some_and(|v| v & 1 != 0)
}

/// Auto-hide: how many pixels of the hidden bar stay on screen, and the slide time.
pub const HIDDEN_PEEK_PX: i32 = 2;
/// The show slide of the owner's Windhawk mod `taskbar-autohide-instant-show`.
pub const SHOW_MS: f32 = 200.0;
pub const HIDE_MS: f32 = 200.0;
/// How long the pointer must be away before the bar hides again.
pub const HIDE_DELAY_MS: u32 = 400;

/// Ease-out of the slide, `t` in 0..=1.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t)
}

/// The bar's top edge for a slide position `shown` (0 hidden, 1 shown) on a monitor whose bottom is `bottom`.
pub fn slide_top(bottom: i32, bar_h: i32, shown: f32) -> i32 {
    let hidden = bottom - HIDDEN_PEEK_PX;
    let visible = bottom - bar_h;
    hidden + ((visible - hidden) as f32 * shown.clamp(0.0, 1.0)).round() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stuck_rects() {
        let mut b = vec![0u8; 48];
        b[8] = 3;
        assert!(autohide_bit(&b));
        b[8] = 2;
        assert!(!autohide_bit(&b));
        assert!(!autohide_bit(&[0, 1]));
    }

    #[test]
    fn from_raw() {
        let s = Settings::from_raw(&Raw::default());
        assert!(!s.align_left && !s.autohide && !s.small && !s.all_displays);
        assert_eq!(s.combine, Combine::Always);
        let r = Raw { taskbar_al: Some(0), glom: Some(2), mm_enabled: Some(1), mm_mode: Some(2), stuck_rects: Some(vec![0x30, 0, 0, 0, 0xfe, 0xff, 0xff, 0xff, 3]), ..Default::default() };
        let s = Settings::from_raw(&r);
        assert!(s.align_left && s.autohide && s.all_displays);
        assert_eq!(s.combine, Combine::Never);
        assert_eq!(s.combine_other, Combine::Never);
        assert_eq!(s.multi_mode, MultiMode::WhereOpen);
    }

    #[test]
    fn slide() {
        assert_eq!(slide_top(800, 48, 0.0), 798);
        assert_eq!(slide_top(800, 48, 1.0), 752);
        assert!(ease_out(0.5) > 0.5);
        assert_eq!(ease_out(2.0), 1.0);
    }
}
