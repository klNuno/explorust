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
    pub fn finish(self, moved: Vec<T>, complete: bool) {
        let mut state = self.0.0.lock().unwrap();
        if complete {
            state.status = Status::Consumed;
            state.moved.clear();
        } else {
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
}
