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

/// True once a problem was reported (not yet taken).
#[cfg(target_os = "windows")]
pub fn pending() -> bool {
    FIRST.lock().unwrap_or_else(|e| e.into_inner()).is_some()
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
/// one that has not answered (or has had no window) for `HUNG_FOR` of wall time, over at least
/// `MIN_MISSES` probes in a row, is a problem.
#[cfg(target_os = "windows")]
pub const PROBE: Duration = Duration::from_secs(5);
pub const HUNG_FOR: Duration = Duration::from_secs(20);
const MIN_MISSES: u32 = 4;

/// Per part: when its run of unanswered probes began and how long it is.
pub struct Hung {
    since: Vec<Option<(Duration, u32)>>,
}

impl Hung {
    pub fn new(parts: usize) -> Hung {
        Hung { since: vec![None; parts] }
    }

    /// `answered`: whether part `i` answered at `now` (time since the watchdog started). Returns
    /// true when it is hung.
    pub fn probe(&mut self, i: usize, answered: bool, now: Duration) -> bool {
        if answered {
            self.since[i] = None;
            return false;
        }
        let (start, misses) = self.since[i].get_or_insert((now, 0));
        *misses += 1;
        now.saturating_sub(*start) >= HUNG_FOR && *misses >= MIN_MISSES
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn silent_for_20_s_over_4_probes_is_hung() {
        let mut h = Hung::new(2);
        assert!(!h.probe(0, false, at(0)));
        assert!(!h.probe(0, false, at(7)));
        assert!(!h.probe(1, false, at(7)));
        assert!(!h.probe(0, false, at(14)));
        assert!(h.probe(0, false, at(21)));
    }

    #[test]
    fn an_answer_resets() {
        let mut h = Hung::new(1);
        for s in [0, 7, 14] {
            h.probe(0, false, at(s));
        }
        assert!(!h.probe(0, true, at(21)));
        assert!(!h.probe(0, false, at(28)));
        assert!(!h.probe(0, false, at(35)));
    }

    #[test]
    fn few_slow_probes_are_not_enough() {
        // Two misses 30 s apart (a long sleep of the watchdog itself) are not four in a row.
        let mut h = Hung::new(1);
        assert!(!h.probe(0, false, at(0)));
        assert!(!h.probe(0, false, at(30)));
    }
}
