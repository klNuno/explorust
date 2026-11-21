//! Ownership and site ordering, independent of the diagnostics ABI.

#[derive(Default)]
pub struct BackgroundOwnership {
    ours: bool,
    protected: bool,
}

impl BackgroundOwnership {
    pub fn observe(&mut self, local: bool, binding: bool, value: Option<u64>, owned: Option<u64>) {
        let ours = local && value.is_some() && value == owned;
        self.ours |= ours;
        self.protected |= binding || (local && !ours);
    }

    pub fn can_set(&self) -> bool {
        !self.protected && !self.ours
    }

    pub fn can_clear(&self) -> bool {
        !self.protected && self.ours
    }
}

pub enum SiteChange<T> {
    Attach(u64, T),
    Detach(u64),
}

pub struct SiteQueue<T> {
    pending: Option<(u64, T)>,
    pub retiring: bool,
}

impl<T> Default for SiteQueue<T> {
    fn default() -> Self {
        Self {
            pending: None,
            retiring: false,
        }
    }
}

impl<T> SiteQueue<T> {
    pub fn update(&mut self, change: SiteChange<T>, active: Option<u64>) {
        match change {
            SiteChange::Attach(id, site) => {
                self.pending = Some((id, site));
                self.retiring |= active.is_some();
            }
            SiteChange::Detach(id) => {
                if self.pending.as_ref().is_some_and(|(pending, _)| *pending == id) {
                    self.pending = None;
                }
                self.retiring |= active == Some(id);
            }
        }
    }

    pub fn take_ready(&mut self) -> Option<(u64, T)> {
        if self.retiring { None } else { self.pending.take() }
    }
}

/// State shared by a queued dispatcher callback and the worker waiting for its result.
pub enum DispatchProgress {
    Queued,
    Running,
    Done(bool),
    Cancelled,
}

impl DispatchProgress {
    pub fn begin(&mut self) -> bool {
        if !matches!(self, Self::Queued) {
            return false;
        }
        *self = Self::Running;
        true
    }

    pub fn result(&mut self, expired: bool) -> Option<bool> {
        match *self {
            Self::Done(ok) => Some(ok),
            Self::Queued if expired => {
                *self = Self::Cancelled;
                Some(false)
            }
            Self::Cancelled => Some(false),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overridden_local_value_and_binding_block_styling() {
        let mut chain = BackgroundOwnership::default();
        chain.observe(false, false, None, None);
        chain.observe(true, false, None, None);
        assert!(!chain.can_set());

        let mut chain = BackgroundOwnership::default();
        chain.observe(false, true, Some(42), None);
        assert!(!chain.can_set());
    }

    #[test]
    fn clear_requires_the_same_local_brush_and_no_binding() {
        let mut chain = BackgroundOwnership::default();
        chain.observe(false, false, Some(42), Some(42));
        assert!(!chain.can_clear());
        chain.observe(true, false, Some(42), Some(42));
        assert!(chain.can_clear());
        chain.observe(true, false, Some(47), Some(42));
        assert!(!chain.can_clear());

        let mut chain = BackgroundOwnership::default();
        chain.observe(true, true, Some(42), Some(42));
        assert!(!chain.can_clear());
    }

    #[test]
    fn detach_invalidates_a_pending_attach_before_rollback_finishes() {
        let mut queue = SiteQueue::default();
        queue.update(SiteChange::Attach(2, "new site"), Some(1));
        assert!(queue.retiring);
        assert_eq!(queue.take_ready(), None);
        queue.update(SiteChange::Detach(2), Some(1));
        assert!(queue.retiring);
        queue.retiring = false;
        assert_eq!(queue.take_ready(), None);
    }

    #[test]
    fn old_detach_leaves_a_newer_attach_queued_until_rollback_finishes() {
        let mut queue = SiteQueue::default();
        queue.update(SiteChange::Attach(2, "new site"), Some(1));
        queue.update(SiteChange::Detach(1), Some(1));
        assert_eq!(queue.take_ready(), None);
        queue.retiring = false;
        assert_eq!(queue.take_ready(), Some((2, "new site")));
    }

    #[test]
    fn a_timed_out_queued_rollback_cannot_run_later() {
        let mut phase = DispatchProgress::Queued;
        assert_eq!(phase.result(false), None);
        assert_eq!(phase.result(true), Some(false));
        assert!(!phase.begin());
    }

    #[test]
    fn a_running_rollback_must_finish_even_after_its_deadline() {
        let mut phase = DispatchProgress::Queued;
        assert!(phase.begin());
        assert_eq!(phase.result(true), None);
        phase = DispatchProgress::Done(true);
        assert_eq!(phase.result(true), Some(true));
    }
}
