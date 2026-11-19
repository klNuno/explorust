//! Bar geometry in DIPs (96 DPI units), measured on Windows 11's bar at 100%, and its scale.

pub const BAR_H: f32 = 48.0;
pub const SLOT: f32 = 44.0;
pub const ICON: f32 = 24.0;
pub const ICON_TOP: f32 = 12.0;
/// Button background: x offset from the slot center, top, width, height.
pub const BUTTON_BG: (f32, f32, f32, f32) = (-20.0, 4.0, 40.0, 40.0);
pub const BUTTON_RADIUS: f32 = 4.0;
pub const INDICATOR_Y: f32 = 40.0;
pub const INDICATOR_H: f32 = 3.0;
pub const RUNNING_W: f32 = 6.0;
pub const ACTIVE_W: f32 = 16.0;
/// Indicator of a button in the attention state (measured on the owner's bar).
pub const ATTENTION_W: f32 = 20.0;
/// Taskbar progress bar width (ITaskbarList3), drawn where the indicator is; not measured.
pub const PROGRESS_W: f32 = 28.0;
/// How far an overlay badge sticks out past the icon's bottom-right corner; not measured.
pub const OVERLAY_OUT: f32 = 4.0;
/// Start logo: four squares and a 1 px gap.
pub const START_SQ: f32 = 11.0;
pub const START_TOP: f32 = 13.0;
/// Clock: right edge of the text from the bar's right end, baselines from the top.
pub const CLOCK_RIGHT: f32 = 19.0;
pub const LINE1_BASELINE: f32 = 20.0;
pub const LINE2_BASELINE: f32 = 36.0;
pub const TEXT_PX: f32 = 12.0;
/// Width of the clock's click area from the bar's right end.
pub const CLOCK_HIT_W: f32 = 100.0;
/// Quick settings glyphs (16 px Segoe Fluent Icons) and the chevron: their baselines.
pub const GLYPH_BASELINE: f32 = 32.0;
pub const CHEVRON_PX: f32 = 12.0;
pub const CHEVRON_BASELINE: f32 = 30.0;
/// Pointer-over fade, XAML ControlFasterAnimationDuration (83 ms) in, 167 ms out.
pub const HOVER_IN_MS: f32 = 83.0;
pub const HOVER_OUT_MS: f32 = 167.0;
/// Pressed icon scale.
pub const PRESS_SCALE: f32 = 0.86;

/// Bar height in physical pixels at `dpi`.
pub fn bar_height_px(dpi: u32) -> i32 {
    ((BAR_H * dpi as f32) / 96.0).round() as i32
}

/// The button row of a bar `width` DIPs wide with `buttons` slots (Start is slot 0).
#[derive(Clone, Copy, Debug)]
pub struct Row {
    pub width: f32,
    pub buttons: usize,
    /// Buttons from the left edge (TaskbarAl = 0) instead of centered.
    pub left: bool,
}

/// Left of the first slot when the buttons are left-aligned (not measured on a real bar).
pub const LEFT_MARGIN: f32 = 2.0;

impl Row {
    pub fn x0(&self) -> f32 {
        if self.left {
            return LEFT_MARGIN;
        }
        (self.width / 2.0 - self.buttons as f32 * SLOT / 2.0).round()
    }

    pub fn cx(&self, i: usize) -> f32 {
        self.x0() + SLOT / 2.0 + SLOT * i as f32
    }

    /// The slot under `x`, None outside the row.
    pub fn at(&self, x: f32) -> Option<usize> {
        let rel = x - self.x0();
        (rel >= 0.0 && rel < self.buttons as f32 * SLOT).then(|| (rel / SLOT) as usize)
    }

    /// The slot's rect (left, top, right, bottom), the area Windows animates minimized windows to.
    pub fn slot_rect(&self, i: usize) -> (f32, f32, f32, f32) {
        let l = self.x0() + SLOT * i as f32;
        (l, 0.0, l + SLOT, BAR_H)
    }

    pub fn in_clock(&self, x: f32) -> bool {
        x >= self.width - CLOCK_HIT_W && x < self.width
    }
}

/// Hover or press fade: moves `v` toward `target` for `dt_ms`; true while still moving.
pub fn fade(v: &mut f32, target: f32, dt_ms: f32) -> bool {
    let dur = if target > *v { HOVER_IN_MS } else { HOVER_OUT_MS };
    let step = dt_ms / dur;
    if target > *v {
        *v = (*v + step).min(target)
    } else {
        *v = (*v - step).max(target)
    }
    (*v - target).abs() > f32::EPSILON
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_is_centered_and_hit_tested() {
        let r = Row { width: 1280.0, buttons: 4, left: false };
        assert_eq!(r.x0(), 552.0);
        assert_eq!(r.cx(0), 574.0);
        assert_eq!(r.at(551.0), None);
        assert_eq!(r.at(552.0), Some(0));
        assert_eq!(r.at(552.0 + 44.0 * 3.5), Some(3));
        assert_eq!(r.at(552.0 + 44.0 * 4.0), None);
        assert_eq!(r.slot_rect(1), (596.0, 0.0, 640.0, 48.0));
        assert!(r.in_clock(1279.0) && !r.in_clock(1100.0));
    }

    #[test]
    fn row_left_aligned() {
        let r = Row { width: 1280.0, buttons: 4, left: true };
        assert_eq!(r.x0(), LEFT_MARGIN);
        assert_eq!(r.at(LEFT_MARGIN + 1.0), Some(0));
    }

    #[test]
    fn height_scales_with_dpi() {
        assert_eq!(bar_height_px(96), 48);
        assert_eq!(bar_height_px(120), 60);
        assert_eq!(bar_height_px(144), 72);
    }

    #[test]
    fn fade_reaches_target() {
        let mut v = 0.0;
        assert!(fade(&mut v, 1.0, 40.0));
        assert!(!fade(&mut v, 1.0, 100.0));
        assert_eq!(v, 1.0);
        assert!(!fade(&mut v, 0.0, 200.0));
        assert_eq!(v, 0.0);
    }
}
