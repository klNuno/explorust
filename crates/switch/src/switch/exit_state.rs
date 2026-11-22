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

pub fn resolve_repost(
    result: Result<bool, String>,
    original_owns_taskbar: impl FnOnce() -> bool,
) -> Result<(), String> {
    match result {
        Ok(true) => Ok(()),
        _ if !original_owns_taskbar() => Ok(()),
        Ok(false) => Err("cannot confirm Exit Explorer while a file operation is running".into()),
        Err(error) => Err(error),
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
    fn a_tray_vanishing_during_the_repost_continues_recovery() {
        let failed_post = || Err("Explorer's taskbar changed while preparing to switch".into());
        assert_eq!(resolve_repost(failed_post(), || false), Ok(()));
        assert_eq!(resolve_repost(failed_post(), || true), failed_post().map(|_| ()));
        assert!(resolve_repost(Ok(false), || true).is_err());
    }
}
