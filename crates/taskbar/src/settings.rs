//! The bar's settings as Windows keeps them (read only; explorust never writes them):
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced` (TaskbarAl, TaskbarGlomLevel,
//! MMTaskbarEnabled, MMTaskbarMode, MMTaskbarGlomLevel, TaskbarSmallIcons, IconSizePreference) and
//! `...\Explorer\StuckRects3` `Settings` (auto-hide at byte 8, the edge at byte 12).

use crate::appbar::Rect;

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

    /// Space this taskbar removes from the monitor's work area.
    pub fn reserved_height(self, bar_height: i32) -> i32 {
        if self.autohide { 0 } else { bar_height }
    }
}

/// StuckRects3 `Settings`: byte 8 holds the auto-hide bit (3 with auto-hide, 2 without).
pub fn autohide_bit(b: &[u8]) -> bool {
    b.get(8).is_some_and(|v| v & 1 != 0)
}

/// Pointer distance from the bottom that wakes a hidden taskbar.
const REVEAL_EDGE_PX: i32 = 2;
/// The show slide of the owner's Windhawk mod `taskbar-autohide-instant-show`.
pub const SHOW_MS: f32 = 200.0;
pub const HIDE_MS: f32 = 200.0;
/// How long the pointer must be away before the bar hides again.
pub const HIDE_DELAY_MS: u32 = 400;

/// Only the bottom edge of this bar's monitor wakes it.
pub fn at_reveal_edge(monitor: Rect, x: i32, y: i32) -> bool {
    let edge = Rect { top: monitor.bottom - REVEAL_EDGE_PX, ..monitor };
    contains(edge, x, y)
}

pub fn should_hide(bar: Rect, x: i32, y: i32, held_open: bool) -> bool {
    !held_open && !contains(bar, x, y)
}

fn contains(rect: Rect, x: i32, y: i32) -> bool {
    (rect.left..rect.right).contains(&x) && (rect.top..rect.bottom).contains(&y)
}

/// A maximized framed window's invisible resize borders can cover the whole monitor.
pub fn fullscreen_cover(window: Rect, monitor: Rect, maximized_framed: bool) -> bool {
    !maximized_framed && window.covers(&monitor)
}

/// Ease-out of the slide, `t` in 0..=1.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t)
}

/// The bar's top edge for a slide position `shown` (0 hidden, 1 shown) on a monitor whose bottom is `bottom`.
pub fn slide_top(bottom: i32, bar_h: i32, shown: f32) -> i32 {
    bottom - (bar_h as f32 * shown.clamp(0.0, 1.0)).round() as i32
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
        assert_eq!(slide_top(800, 48, 0.0), 800);
        assert_eq!(slide_top(800, 48, 1.0), 752);
        assert!(ease_out(0.5) > 0.5);
        assert_eq!(ease_out(2.0), 1.0);
    }

    #[test]
    fn auto_hide_reserves_no_work_area() {
        let regular = Settings::default();
        let auto_hide = Settings { autohide: true, ..regular };
        assert_eq!(regular.reserved_height(48), 48);
        assert_eq!(auto_hide.reserved_height(48), 0);
    }

    #[test]
    fn reveal_uses_the_bottom_edge_of_its_own_monitor() {
        let left = Rect::new(-1920, -40, 0, 1040);
        let primary = Rect::new(0, 0, 1280, 800);
        assert!(at_reveal_edge(left, -1, 1039));
        assert!(at_reveal_edge(left, -1920, 1038));
        assert!(!at_reveal_edge(left, -1, 1040));
        assert!(!at_reveal_edge(left, 0, 1039));
        assert!(!at_reveal_edge(left, -1, 1037));
        assert!(at_reveal_edge(primary, 0, 799));
        assert!(!at_reveal_edge(left, 0, 799));
    }

    #[test]
    fn hide_when_the_pointer_leaves_and_nothing_holds_it_open() {
        let bar = Rect::new(0, 752, 1280, 800);
        assert!(!should_hide(bar, 640, 799, false));
        assert!(should_hide(bar, 640, 751, false));
        assert!(should_hide(bar, 1280, 799, false));
        assert!(!should_hide(bar, 640, 751, true));
    }

    #[test]
    fn maximize_borders_do_not_make_a_window_fullscreen() {
        let monitor = Rect::new(0, 0, 1280, 800);
        assert!(!fullscreen_cover(Rect::new(-8, -8, 1288, 808), monitor, true));
        assert!(!fullscreen_cover(monitor, monitor, true));
        assert!(fullscreen_cover(monitor, monitor, false));
        assert!(!fullscreen_cover(Rect::new(0, 0, 1280, 798), monitor, false));
    }
}
