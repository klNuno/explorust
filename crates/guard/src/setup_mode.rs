//! Decides which confirmation to offer from the install and session probes.

#[derive(Debug, PartialEq, Eq)]
pub enum Mode {
    Install,
    UpdateOnSignIn,
    UpdateLive { pid: u32, preserves_files: bool },
}

pub fn choose(installed: bool, shell_pid: Option<u32>, reload: bool, guarded: bool, preserves_files: bool) -> Mode {
    match (installed, shell_pid, reload, guarded) {
        (true, Some(pid), true, true) => Mode::UpdateLive { pid, preserves_files },
        (true, _, _, _) => Mode::UpdateOnSignIn,
        (false, _, _, _) => Mode::Install,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_installed_guarded_explorust_with_a_reload_event_updates_live() {
        assert_eq!(choose(false, Some(42), true, true, true), Mode::Install);
        assert_eq!(choose(true, None, true, true, true), Mode::UpdateOnSignIn);
        assert_eq!(choose(true, Some(42), false, true, true), Mode::UpdateOnSignIn);
        assert_eq!(choose(true, Some(42), true, false, true), Mode::UpdateOnSignIn);
        for preserves_files in [false, true] {
            assert_eq!(choose(true, Some(42), true, true, preserves_files), Mode::UpdateLive { pid: 42, preserves_files });
        }
    }
}
