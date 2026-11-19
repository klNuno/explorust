//! Selection rules of Alt+Tab, pure: the first press selects the window behind the foreground one
//! (Shift: the last one), each press moves on and wraps, a closed window keeps the selection on its
//! neighbour; and the timings of the UI.

/// Alt released sooner than this after the first Tab: switch without showing the switcher.
pub const SHOW_DELAY_MS: u32 = 100;
/// The selection ring glides to its new tile in this time.
pub const RING_MS: f32 = 150.0;

/// The selection when the switcher opens on `n` windows (the foreground one is index 0).
pub fn first(n: usize, back: bool) -> usize {
    match n {
        0 | 1 => 0,
        _ if back => n - 1,
        _ => 1,
    }
}

/// The next selection after a Tab (or Shift+Tab when `back`).
pub fn step(sel: usize, n: usize, back: bool) -> usize {
    if n == 0 {
        return 0;
    }
    if back { (sel + n - 1) % n } else { (sel + 1) % n }
}

/// The selection after the window at `removed` left a list that now holds `n` windows.
pub fn after_remove(sel: usize, removed: usize, n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    let s = if removed < sel { sel - 1 } else { sel };
    s.min(n - 1)
}

/// The desktop Ctrl+Win+Left (`dir` < 0) or Right goes to from `cur` of `n`; Windows does not wrap.
pub fn adjacent(cur: usize, n: usize, dir: isize) -> Option<usize> {
    let t = cur as isize + dir.signum();
    (dir != 0 && t >= 0 && (t as usize) < n).then_some(t as usize)
}

/// Where Ctrl+Win+F4 on desktop `cur` of `n` sends its windows and the user: the desktop to the
/// left, or the right one when it is the first; None for the last desktop left.
pub fn close_fallback(cur: usize, n: usize) -> Option<usize> {
    match (cur, n) {
        (_, 0 | 1) => None,
        (0, _) => Some(1),
        _ => Some(cur - 1),
    }
}

/// Ease-out cubic for `t` in 0..1, clamped.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_selection() {
        assert_eq!(first(0, false), 0);
        assert_eq!(first(1, false), 0);
        assert_eq!(first(3, false), 1);
        assert_eq!(first(3, true), 2);
    }

    #[test]
    fn steps_wrap() {
        assert_eq!(step(1, 3, false), 2);
        assert_eq!(step(2, 3, false), 0);
        assert_eq!(step(0, 3, true), 2);
        assert_eq!(step(0, 0, false), 0);
    }

    #[test]
    fn removal_keeps_neighbour() {
        assert_eq!(after_remove(2, 0, 3), 1);
        assert_eq!(after_remove(2, 2, 2), 1);
        assert_eq!(after_remove(1, 2, 2), 1);
        assert_eq!(after_remove(0, 0, 0), 0);
    }

    #[test]
    fn desktops_do_not_wrap() {
        assert_eq!(adjacent(0, 3, -1), None);
        assert_eq!(adjacent(0, 3, 1), Some(1));
        assert_eq!(adjacent(2, 3, 1), None);
        assert_eq!(adjacent(1, 3, 0), None);
        assert_eq!(close_fallback(0, 1), None);
        assert_eq!(close_fallback(0, 3), Some(1));
        assert_eq!(close_fallback(2, 3), Some(1));
    }

    #[test]
    fn easing_ends() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert_eq!(ease_out(2.0), 1.0);
        assert!(ease_out(0.5) > 0.5);
    }
}
