//! Who opens folders (double-click, Win+R, an app's "show in folder"): explorust's File Explorer
//! while explorust runs, Windows' own the rest of the time. `explorust-open.exe`, beside the
//! guard, writes or removes the per-user folder verbs; the guard asks it once explorust is ready
//! and again when it falls back to explorer or `--restore` runs.

use crate::log;
use crate::proc::{Child, wait_any};

const OPEN_EXE: &str = "explorust-open.exe";

/// `register` true: folders open in explorust; false: Windows' handler answers again. Waits up to
/// 3 s for the helper so a fallback's explorer starts with the verbs already gone.
pub fn handler(register: bool) {
    let Some(exe) = std::env::current_exe().ok().and_then(|p| Some(p.parent()?.join(OPEN_EXE))) else { return };
    if !exe.exists() {
        return;
    }
    let arg = if register { "--register-folder-handler" } else { "--unregister-folder-handler" };
    match Child::spawn(&exe, arg) {
        Ok(child) => {
            let done = wait_any(&[child.handle], Some(3000)).is_some();
            log::line(&format!("folder handler {arg}: {}", if done { format!("exit {}", child.exit_code()) } else { "still running".into() }));
        }
        Err(e) => log::line(&format!("folder handler {arg}: {e}")),
    }
}
