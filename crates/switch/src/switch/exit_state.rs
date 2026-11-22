#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitState {
    Unsent,
    Uncertain,
    Posted,
}

impl ExitState {
    pub fn may_have_been_posted(self) -> bool {
        self != Self::Unsent
    }
}

pub fn exit_state(intent: bool, posted: bool, cancelled: bool) -> ExitState {
    if posted {
        ExitState::Posted
    } else if intent && !cancelled {
        ExitState::Uncertain
    } else {
        ExitState::Unsent
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Recovery {
    WaitForOriginal,
    Restore,
    Ready,
}

pub fn recovery(exit: ExitState, original_exited: bool, desktop_answers: bool) -> Recovery {
    if exit.may_have_been_posted() && !original_exited {
        Recovery::WaitForOriginal
    } else if desktop_answers {
        Recovery::Ready
    } else {
        Recovery::Restore
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_watcher_lost_during_folder_closure_can_keep_the_original_desktop() {
        assert_eq!(exit_state(false, false, false), ExitState::Unsent);
    }

    #[test]
    fn interruption_between_post_and_confirmation_requires_resolution() {
        assert_eq!(exit_state(true, false, false), ExitState::Uncertain);
        assert_eq!(exit_state(true, false, true), ExitState::Unsent);
        assert_eq!(exit_state(true, true, false), ExitState::Posted);
    }

    #[test]
    fn watches_an_exit_through_a_dead_worker_and_a_new_copy() {
        for confirmed_before_worker_died in [false, true] {
            let exit = exit_state(true, confirmed_before_worker_died, false);
            for exit_at in [1, 60, 600] {
                for second in 0..=exit_at + 3 {
                    let original_exited = second >= exit_at;
                    // The original can answer during a new copy. A replacement answers
                    // two seconds after the original exits.
                    let copy_running = (1..exit_at).contains(&second);
                    let taskbar_answers = copy_running || second >= exit_at + 2;
                    let expected = if second < exit_at {
                        Recovery::WaitForOriginal
                    } else if second < exit_at + 2 {
                        Recovery::Restore
                    } else {
                        Recovery::Ready
                    };
                    assert_eq!(
                        recovery(exit, original_exited, taskbar_answers),
                        expected,
                        "at {second}s of {exit_at}s, {exit:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_confirmed_unsent_request_can_keep_the_original_desktop() {
        let cancelled = exit_state(true, false, true);
        assert_eq!(recovery(cancelled, false, true), Recovery::Ready);
        assert_eq!(recovery(cancelled, false, false), Recovery::Restore);
    }
}
