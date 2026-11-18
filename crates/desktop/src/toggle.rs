//! Win+D and Win+M: the first press minimizes the open windows, the next one restores them in
//! their Z-order, as long as the desktop is still shown. Pure logic over window ids; the Windows
//! side lists the windows and applies the action.

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Minimize these windows (top of the Z-order first).
    Minimize(Vec<isize>),
    /// Restore these windows, bottom first, so the last one ends on top and gets the focus.
    Restore(Vec<isize>),
    Nothing,
}

#[derive(Default)]
pub struct Toggle {
    /// What the last press hid, top of the Z-order first.
    hidden: Vec<isize>,
}

impl Toggle {
    /// `shown`: the windows a press would hide, top first. `minimized`: whether a window id is
    /// still minimized (or still exists at all).
    pub fn press(&mut self, shown: &[isize], minimized: impl Fn(isize) -> bool) -> Action {
        if shown.is_empty() && !self.hidden.is_empty() {
            let back: Vec<isize> = self.hidden.drain(..).rev().filter(|&h| minimized(h)).collect();
            return if back.is_empty() { Action::Nothing } else { Action::Restore(back) };
        }
        self.hidden = shown.to_vec();
        if shown.is_empty() { Action::Nothing } else { Action::Minimize(shown.to_vec()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn press_twice_restores_bottom_first() {
        let mut t = Toggle::default();
        assert_eq!(t.press(&[3, 2, 1], |_| true), Action::Minimize(vec![3, 2, 1]));
        assert_eq!(t.press(&[], |_| true), Action::Restore(vec![1, 2, 3]));
        // A third press with nothing shown and nothing hidden does nothing.
        assert_eq!(t.press(&[], |_| true), Action::Nothing);
    }

    #[test]
    fn closed_or_restored_windows_are_skipped() {
        let mut t = Toggle::default();
        t.press(&[3, 2, 1], |_| true);
        assert_eq!(t.press(&[], |h| h != 2), Action::Restore(vec![1, 3]));
    }

    #[test]
    fn a_window_shown_in_between_starts_over() {
        let mut t = Toggle::default();
        t.press(&[3, 2, 1], |_| true);
        // The user opened window 9: the next press hides it, not restores the old set.
        assert_eq!(t.press(&[9], |_| true), Action::Minimize(vec![9]));
        assert_eq!(t.press(&[], |_| true), Action::Restore(vec![9]));
    }
}
