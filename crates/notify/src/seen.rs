//! Which toasts already got a card. The center's ids are unique for the life of a notification and
//! grow, but the platform hands the same ones out again after its service restarts (WpnUserService
//! died on every NotificationChanged subscription in g1, then re-listed ids 33 and 47 as if new,
//! step notify-6). A card is drawn once per id: an id seen once, at start or later, never again.

use std::collections::HashSet;

/// Ids kept before the oldest are forgotten; the center holds far fewer.
const KEEP: usize = 4096;

#[derive(Default)]
pub struct Seen {
    ever: HashSet<u32>,
    /// The highest id seen at start: anything at or under it was in the center before us.
    floor: u32,
    /// The ids currently in the center, for removals.
    now: HashSet<u32>,
}

/// One diff of the center against what was seen.
#[derive(Debug, Default, PartialEq)]
pub struct Diff {
    /// Ids that deserve a card.
    pub added: Vec<u32>,
    /// Ids that left the center.
    pub removed: Vec<u32>,
    /// Ids back in the center after a gap (a service restart): no card.
    pub returned: Vec<u32>,
}

impl Seen {
    /// The center as found at start: none of these gets a card.
    pub fn start(ids: impl IntoIterator<Item = u32>) -> Seen {
        let now: HashSet<u32> = ids.into_iter().collect();
        Seen { floor: now.iter().copied().max().unwrap_or(0), ever: now.clone(), now }
    }

    pub fn len(&self) -> usize {
        self.now.len()
    }

    /// Records the center's ids now; returns what changed.
    pub fn update(&mut self, ids: impl IntoIterator<Item = u32>) -> Diff {
        let next: HashSet<u32> = ids.into_iter().collect();
        let mut d = Diff::default();
        for &id in &next {
            if self.now.contains(&id) {
                continue;
            }
            if id <= self.floor || !self.ever.insert(id) {
                d.returned.push(id);
            } else {
                d.added.push(id);
            }
        }
        d.removed = self.now.difference(&next).copied().collect();
        d.added.sort_unstable();
        d.removed.sort_unstable();
        d.returned.sort_unstable();
        if self.ever.len() > KEEP {
            let mut v: Vec<u32> = self.ever.iter().copied().collect();
            v.sort_unstable();
            let cut = v[v.len() - KEEP / 2];
            self.floor = self.floor.max(cut.saturating_sub(1));
            self.ever.retain(|&id| id >= cut);
        }
        self.now = next;
        d
    }

    /// Whether the change event's id deserves a card (the event path, no full diff).
    pub fn fresh(&mut self, id: u32) -> bool {
        self.now.insert(id);
        id > self.floor && self.ever.insert(id)
    }

    pub fn gone(&mut self, id: u32) {
        self.now.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_does_not_reshow() {
        let mut s = Seen::start([33, 47]);
        // The service died: the center reads empty, then lists both again.
        assert_eq!(s.update([]), Diff { removed: vec![33, 47], ..Diff::default() });
        assert_eq!(s.update([33, 47]), Diff { returned: vec![33, 47], ..Diff::default() });
        assert_eq!(s.update([33, 47, 48]), Diff { added: vec![48], ..Diff::default() });
        s.update([33, 47]);
        assert_eq!(s.update([33, 47, 48]), Diff { returned: vec![48], ..Diff::default() });
    }

    #[test]
    fn event_path() {
        let mut s = Seen::start([5]);
        assert!(!s.fresh(5));
        assert!(s.fresh(6));
        assert!(!s.fresh(6));
        s.gone(6);
        assert!(!s.fresh(6));
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn bounded() {
        let mut s = Seen::start([]);
        for id in 1..=(KEEP as u32 + 1) {
            s.update([id]);
        }
        assert!(s.ever.len() <= KEEP);
        assert_eq!(s.update([3]), Diff { removed: vec![KEEP as u32 + 1], returned: vec![3], ..Diff::default() });
    }
}
