//! A problem explorust finds in itself: a part that crashed twice in five minutes, a part whose
//! window stops answering, the keyboard helper dying again and again. The main loop sees it, closes
//! the parts and exits with `run::PROBLEM`; the guard then starts explorer.exe and tells the user,
//! with the reason written here to `%LOCALAPPDATA%\explorust\problem.txt`.

#[cfg(target_os = "windows")]
use crate::log;
#[cfg(target_os = "windows")]
use std::sync::Mutex;
use std::time::Duration;

#[cfg(target_os = "windows")]
static FIRST: Mutex<Option<String>> = Mutex::new(None);

/// Records a problem; only the first one counts.
#[cfg(target_os = "windows")]
pub fn report(why: String) {
    let mut first = FIRST.lock().unwrap_or_else(|e| e.into_inner());
    if first.is_none() {
        log::line(&format!("problem: {why}"));
        *first = Some(why);
    }
}

/// The problem to stop on, written for the guard.
#[cfg(target_os = "windows")]
pub fn take() -> Option<String> {
    let why = FIRST.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
    let path = log::dir().join("problem.txt");
    if let Err(e) = std::fs::write(&path, &why) {
        log::line(&format!("problem.txt: {e}"));
    }
    Some(why)
}

/// The part watchdog: every `PROBE`, each part's command window must answer a WM_NULL within 2 s;
/// one that has not answered for `HUNG_FOR` is a problem. A part not running (off, or between two
/// starts) is skipped.
pub const PROBE: Duration = Duration::from_secs(5);
pub const HUNG_FOR: Duration = Duration::from_secs(20);

/// Counts unanswered probes in a row per part.
pub struct Hung {
    misses: Vec<u32>,
}

impl Hung {
    pub fn new(parts: usize) -> Hung {
        Hung { misses: vec![0; parts] }
    }

    /// `answered`: None when the part has no window. Returns true when part `i` is hung.
    pub fn probe(&mut self, i: usize, answered: Option<bool>) -> bool {
        match answered {
            Some(false) => self.misses[i] += 1,
            _ => self.misses[i] = 0,
        }
        self.misses[i] >= (HUNG_FOR.as_secs() / PROBE.as_secs()) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_misses_in_a_row_is_hung() {
        let mut h = Hung::new(2);
        for _ in 0..3 {
            assert!(!h.probe(0, Some(false)));
        }
        assert!(!h.probe(1, Some(false)));
        assert!(h.probe(0, Some(false)));
    }

    #[test]
    fn an_answer_or_no_window_resets() {
        let mut h = Hung::new(1);
        for _ in 0..3 {
            h.probe(0, Some(false));
        }
        assert!(!h.probe(0, None));
        for _ in 0..3 {
            assert!(!h.probe(0, Some(false)));
        }
        assert!(!h.probe(0, Some(true)));
    }
}
