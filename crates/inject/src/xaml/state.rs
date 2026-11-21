//! Ownership and site ordering, independent of the diagnostics ABI.

pub struct StyleOwner<T> {
    pub process: Option<T>,
    rolling_back: bool,
}

impl<T> Default for StyleOwner<T> {
    fn default() -> Self {
        Self { process: None, rolling_back: false }
    }
}

impl<T> StyleOwner<T> {
    pub fn refresh(
        &mut self,
        alive: impl FnOnce(&T) -> bool,
        discover: impl FnOnce() -> Option<T>,
        rolled_back: bool,
    ) -> bool {
        if let Some(process) = &self.process {
            if alive(process) {
                return false;
            }
            self.process = None;
            self.rolling_back = true;
            return true;
        }
        if self.rolling_back && !rolled_back {
            return false;
        }
        self.rolling_back = false;
        self.process = discover();
        self.process.is_some()
    }
}

#[derive(Clone)]
pub struct Background<T> {
    pub element: T,
    pub index: u32,
    pub brush: T,
}

pub struct Backgrounds<T>(pub std::collections::HashMap<usize, Background<T>>);

impl<T> Backgrounds<T> {
    pub fn prune(&mut self, alive: impl Fn(&T) -> bool) {
        self.0.retain(|_, owned| alive(&owned.element));
    }
}

#[derive(Default)]
pub struct BackgroundOwnership {
    ours: bool,
    protected: bool,
}

impl BackgroundOwnership {
    pub fn observe(&mut self, local: bool, binding: bool, same_brush: bool) {
        let ours = local && same_brush;
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
    use std::rc::{Rc, Weak};

    #[test]
    fn a_replacement_owner_can_only_reapply_after_the_old_style_rolls_back() {
        let mut owner = StyleOwner::default();
        assert!(owner.refresh(|_| true, || Some(1), false));
        assert!(owner.refresh(|_| false, || Some(2), false));
        assert_eq!(owner.process, None);
        assert!(!owner.refresh(|_| true, || Some(2), false));
        assert_eq!(owner.process, None);
        assert!(owner.refresh(|_| true, || Some(2), true));
        assert_eq!(owner.process, Some(2));
    }

    #[test]
    fn a_recycled_element_keeps_its_brush_owned_until_rollback() {
        let element = Rc::new(());
        let brush = Rc::new(());
        let mut backgrounds = Backgrounds(std::collections::HashMap::new());
        backgrounds.0.insert(7, Background {
            element: Rc::downgrade(&element), index: 5, brush: Rc::downgrade(&brush),
        });
        backgrounds.prune(|reference| Weak::upgrade(reference).is_some());
        // New handles after reattachment still resolve to the original element and brush.
        let owned = &backgrounds.0[&7];
        assert_eq!(owned.index, 5);
        let mut chain = BackgroundOwnership::default();
        chain.observe(true, false, Rc::ptr_eq(&brush, &owned.brush.upgrade().unwrap()));
        assert!(!chain.can_set());
        assert!(chain.can_clear());
        backgrounds.0.remove(&7);
        assert!(backgrounds.0.is_empty());
    }

    #[test]
    fn overridden_local_value_and_binding_block_styling() {
        let mut chain = BackgroundOwnership::default();
        chain.observe(false, false, false);
        chain.observe(true, false, false);
        assert!(!chain.can_set());

        let mut chain = BackgroundOwnership::default();
        chain.observe(false, true, false);
        assert!(!chain.can_set());
    }

    #[test]
    fn clear_requires_the_same_local_brush_and_no_binding() {
        let mut chain = BackgroundOwnership::default();
        chain.observe(false, false, true);
        assert!(!chain.can_clear());
        chain.observe(true, false, true);
        assert!(chain.can_clear());
        chain.observe(true, false, false);
        assert!(!chain.can_clear());

        let mut chain = BackgroundOwnership::default();
        chain.observe(true, true, true);
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
