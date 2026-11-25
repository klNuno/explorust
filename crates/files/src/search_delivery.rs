//! A single active search per frame, with one response in flight to its UI thread.

use std::sync::{Condvar, Mutex};

pub(crate) struct Delivery<T> {
    state: Mutex<State<T>>,
    changed: Condvar,
}

struct State<T> {
    ticket: u64,
    response: Option<T>,
}

impl<T> Default for Delivery<T> {
    fn default() -> Self {
        Self {
            state: Mutex::new(State {
                ticket: 0,
                response: None,
            }),
            changed: Condvar::new(),
        }
    }
}

impl<T> Delivery<T> {
    pub fn cancel(&self) -> u64 {
        let mut state = self.state.lock().unwrap();
        state.ticket = state.ticket.wrapping_add(1);
        state.response = None;
        self.changed.notify_all();
        state.ticket
    }

    pub fn active(&self, ticket: u64) -> bool {
        self.state.lock().unwrap().ticket == ticket
    }

    pub fn post(&self, ticket: u64, value: T, notify: impl FnOnce() -> bool) -> bool {
        let state = self.state.lock().unwrap();
        let mut state = self
            .changed
            .wait_while(state, |state| {
                state.ticket == ticket && state.response.is_some()
            })
            .unwrap();
        if state.ticket != ticket {
            return false;
        }
        // Keep notification and delivery in the same critical section as invalidation.
        if notify() {
            state.response = Some(value);
            true
        } else {
            false
        }
    }

    pub fn take(&self) -> Option<T> {
        let result = self.state.lock().unwrap().response.take();
        self.changed.notify_all();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::Delivery;
    use std::sync::mpsc;
    use std::thread;

    #[test]
    fn leaving_releases_a_blocked_provider_and_drops_its_pages() {
        let delivery = Delivery::default();
        let ticket = delivery.cancel();
        assert!(delivery.post(ticket, "visible", || true));
        let (started, wait) = mpsc::channel();
        thread::scope(|threads| {
            let task = threads.spawn(|| {
                started.send(()).unwrap();
                delivery.post(ticket, "stale", || panic!("stale page posted"))
            });
            wait.recv().unwrap();
            let next = delivery.cancel();
            assert!(!task.join().unwrap());
            assert!(!delivery.active(ticket));
            assert_eq!(delivery.take(), None);
            assert!(delivery.post(next, "fresh", || true));
            assert_eq!(delivery.take(), Some("fresh"));
        });
    }
}
