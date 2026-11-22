use std::{
    collections::HashSet,
    hash::Hash,
    sync::{Arc, Mutex},
};

pub struct Cut<T>(Mutex<State<T>>);

struct State<T> {
    status: Status,
    moved: HashSet<T>,
}

#[derive(Default)]
enum Status {
    #[default]
    Ready,
    Reserved,
    Consumed,
}

impl<T> Default for Cut<T> {
    fn default() -> Self {
        Self(Mutex::new(State {
            status: Status::Ready,
            moved: HashSet::new(),
        }))
    }
}

impl<T: Eq + Hash> Cut<T> {
    #[cfg(target_os = "windows")]
    pub fn consumed(&self) -> bool {
        matches!(self.0.lock().unwrap().status, Status::Consumed)
    }

    pub fn consume(&self) -> bool {
        let mut state = self.0.lock().unwrap();
        let changed = !matches!(state.status, Status::Consumed);
        state.status = Status::Consumed;
        state.moved.clear();
        changed
    }

    pub fn reserve(self: &Arc<Self>, items: &mut Vec<T>) -> Option<Reservation<T>> {
        let mut state = self.0.lock().unwrap();
        if !matches!(state.status, Status::Ready) {
            return None;
        }
        items.retain(|item| !state.moved.contains(item));
        if items.is_empty() {
            return None;
        }
        state.status = Status::Reserved;
        Some(Reservation(self.clone()))
    }
}

pub struct Reservation<T>(Arc<Cut<T>>);

impl<T: Eq + Hash> Reservation<T> {
    #[cfg(target_os = "windows")]
    pub fn cut(&self) -> &Arc<Cut<T>> {
        &self.0
    }

    pub fn finish(self, moved: Vec<T>, complete: bool) {
        if complete {
            self.0.consume();
            return;
        }
        let mut state = self.0.0.lock().unwrap();
        if matches!(state.status, Status::Reserved) {
            state.moved.extend(moved);
        }
    }
}

impl<T> Drop for Reservation<T> {
    fn drop(&mut self) {
        let mut state = self.0.0.lock().unwrap();
        if matches!(state.status, Status::Reserved) {
            state.status = Status::Ready;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abort_releases_the_cut_and_success_consumes_it() {
        let cut = Arc::new(Cut::default());
        let mut files = vec!["a", "b"];
        let pending = cut.reserve(&mut files).unwrap();
        assert!(cut.reserve(&mut files).is_none());
        drop(pending);
        cut.reserve(&mut files).unwrap().finish(vec!["a"], false);
        let retry = cut.reserve(&mut files).unwrap();
        assert_eq!(files, ["b"]);
        retry.finish(vec!["b"], true);
        assert!(cut.reserve(&mut vec!["a", "b"]).is_none());
    }

    #[test]
    fn native_completion_survives_cancelling_a_reserved_paste() {
        let cut = Arc::new(Cut::default());
        let pending = cut.reserve(&mut vec!["a", "b"]).unwrap();
        assert!(cut.consume());
        assert!(!cut.consume());
        pending.finish(vec!["a"], false);
        assert!(cut.reserve(&mut vec!["a", "b"]).is_none());
    }
}
