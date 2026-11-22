//! Owned Rust jobs for an STA with a Windows message pump.

use std::collections::VecDeque;
use std::sync::Mutex;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, WM_APP};

const WAKE: u32 = WM_APP + 0x41;

struct State<T> {
    thread: Option<u32>,
    jobs: VecDeque<T>,
    closed: bool,
}

pub struct Inbox<T>(Mutex<State<T>>);

impl<T> Default for Inbox<T> {
    fn default() -> Self {
        Self(Mutex::new(State { thread: None, jobs: VecDeque::new(), closed: false }))
    }
}

impl<T> Inbox<T> {
    pub fn send(&self, job: T) -> bool {
        self.send_with(|| job)
    }

    pub fn send_with(&self, job: impl FnOnce() -> T) -> bool {
        let mut state = self.0.lock().unwrap();
        if state.closed {
            return false;
        }
        state.jobs.push_back(job());
        state.wake();
        true
    }

    pub fn stop(&self, last: T) {
        self.close(last, |_| false);
    }

    pub fn close(&self, last: T, keep: impl FnMut(&T) -> bool) {
        let mut state = self.0.lock().unwrap();
        if !state.closed {
            state.closed = true;
            state.jobs.retain(keep);
            state.jobs.push_back(last);
            state.wake();
        }
    }

    pub fn attach(&self) -> Receiver<'_, T> {
        unsafe {
            // Ensure the native message queue exists before publishing its address.
            let _ = PeekMessageW(&mut MSG::default(), None, 0, 0, PM_NOREMOVE);
            self.0.lock().unwrap().thread = Some(GetCurrentThreadId());
        }
        Receiver(self)
    }

    pub fn closed(&self) -> bool {
        self.0.lock().unwrap().closed
    }
}

impl<T> State<T> {
    fn wake(&self) {
        if let Some(thread) = self.thread {
            // Posting while holding the lock prevents a queue from reusing its thread ID.
            let _ = unsafe { PostThreadMessageW(thread, WAKE, WPARAM(0), LPARAM(0)) };
        }
    }
}

pub struct Receiver<'a, T>(&'a Inbox<T>);

impl<T> Receiver<'_, T> {
    pub fn next(&self) -> Option<T> {
        self.0.0.lock().unwrap().jobs.pop_front()
    }
}

impl<T> Drop for Receiver<'_, T> {
    fn drop(&mut self) {
        let mut state = self.0.0.lock().unwrap();
        state.closed = true;
        state.jobs.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_seals_the_queue_and_keeps_accepted_mutations_in_order() {
        let inbox = Inbox::default();
        assert!(inbox.send(Some("paste")));
        assert!(inbox.send(Some("address")));
        assert!(inbox.send(Some("new folder")));
        inbox.close(None, |job| *job != Some("address"));
        assert!(!inbox.send(Some("late request")));
        assert_eq!(inbox.0.lock().unwrap().jobs, [Some("paste"), Some("new folder"), None]);
    }
}
