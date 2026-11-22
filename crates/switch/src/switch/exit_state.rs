#[derive(Debug, PartialEq, Eq)]
pub enum ExitState {
    Unsent,
    Uncertain,
    Posted,
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
}
