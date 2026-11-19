//! Back and forward history of one tab. The browser reports every completed navigation, ours
//! (back, forward, address, up) and its own (a double-click in the view); `navigated` tells a
//! back or forward move apart from a new place by the move we asked for last.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    Back,
    Forward,
}

#[derive(Default)]
pub struct History<T> {
    back: Vec<T>,
    current: Option<T>,
    forward: Vec<T>,
    pending: Option<(Move, T)>,
}

impl<T: Clone + PartialEq> History<T> {
    pub fn new() -> Self {
        Self { back: Vec::new(), current: None, forward: Vec::new(), pending: None }
    }

    pub fn current(&self) -> Option<&T> {
        self.current.as_ref()
    }

    pub fn can_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    /// The place a back (or forward) move goes to; the move is applied when `navigated` sees it.
    pub fn begin(&mut self, m: Move) -> Option<T> {
        let target = match m {
            Move::Back => self.back.last(),
            Move::Forward => self.forward.last(),
        }?
        .clone();
        self.pending = Some((m, target.clone()));
        Some(target)
    }

    /// A navigation completed on `place`.
    pub fn navigated(&mut self, place: T) {
        let pending = self.pending.take();
        if self.current.as_ref() == Some(&place) {
            return;
        }
        match pending {
            Some((Move::Back, t)) if t == place => {
                self.back.pop();
                if let Some(c) = self.current.take() {
                    self.forward.push(c);
                }
            }
            Some((Move::Forward, t)) if t == place => {
                self.forward.pop();
                if let Some(c) = self.current.take() {
                    self.back.push(c);
                }
            }
            _ => {
                if let Some(c) = self.current.take() {
                    self.back.push(c);
                }
                self.forward.clear();
            }
        }
        self.current = Some(place);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn back_and_forward() {
        let mut h = History::new();
        h.navigated("a");
        h.navigated("b");
        h.navigated("c");
        assert_eq!(h.begin(Move::Back), Some("b"));
        h.navigated("b");
        assert_eq!(h.current(), Some(&"b"));
        assert!(h.can_forward());
        assert_eq!(h.begin(Move::Forward), Some("c"));
        h.navigated("c");
        assert_eq!(h.current(), Some(&"c"));
        assert!(!h.can_forward());
    }

    #[test]
    fn a_new_place_clears_forward() {
        let mut h = History::new();
        h.navigated(1);
        h.navigated(2);
        h.begin(Move::Back);
        h.navigated(1);
        h.navigated(3);
        assert!(!h.can_forward());
        assert_eq!(h.begin(Move::Back), Some(1));
    }

    #[test]
    fn a_failed_move_does_not_stick() {
        let mut h = History::new();
        h.navigated(1);
        h.navigated(2);
        h.begin(Move::Back);
        // The browser went elsewhere instead (the move failed, a double-click won).
        h.navigated(5);
        assert_eq!(h.current(), Some(&5));
        assert_eq!(h.begin(Move::Back), Some(2));
    }

    #[test]
    fn same_place_is_not_a_step() {
        let mut h = History::new();
        h.navigated(1);
        h.navigated(1);
        assert!(!h.can_back());
        assert!(h.begin(Move::Forward).is_none());
    }
}
