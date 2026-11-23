//! Turning explorust on and off. Two halves: the machine (Program Files copy, certificate, index
//! service, Apps entry and Start menu shortcuts; elevated, shared by every account) and the user
//! (the Winlogon `Shell` value, the notification package, the folder handlers; always the
//! signed-in user's own process). The order keeps a way back at every step: on install, the Apps
//! entry exists before the Shell value; on removal, the Shell value is gone before any file.

use crate::policy::{self, DESKTOP_RECOVERY_LIMIT, RestoreLocks};
use crate::switch_events::{CANCEL_FOR_RESTORE, EXITING_EXPLORER};
use crate::{account, apps, desktop_recovery, folders, install, log, package, proc, reg, taskbar, uninstall};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Exit codes shared by `--install`, `--setup` and `--uninstall`.
pub const FAILED: u32 = 1;
/// Elevated with another account's password: that account's settings are not this user's.
pub const OTHER_ACCOUNT: u32 = 3;
/// Another install or uninstall holds the lock.
pub const BUSY: u32 = 4;
/// The machine part stays: another account still uses explorust.
pub const KEPT: u32 = 7;

fn say(msg: &str) {
    log::line(msg);
    println!("{msg}");
}

/// `--install`: elevated, the machine part then this user's; not elevated (a dev run), only the
/// Shell value pointing to this exe.
pub fn install() -> u32 {
    let _user = match install::user_setup_lock() {
        Ok(lock) => lock,
        Err(e) => {
            say(&format!("install: {e}"));
            return BUSY;
        }
    };
    let _switch = match install::switch_lock_for(0) {
        Ok(lock) => lock,
        Err(e) => {
            say(&format!("install: {e}"));
            return BUSY;
        }
    };
    let me = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            say(&format!("install: {e}"));
            return FAILED;
        }
    };
    let from = me.parent().map(Path::to_path_buf).unwrap_or_default();
    let elevated = install::elevated();
    if elevated && !account::is_session_user() {
        say("install: elevated as another account; sign in with an administrator account and install from there");
        return OTHER_ACCOUNT;
    }
    let lock = if elevated {
        match install::lock() {
            Ok(lock) => Some(lock),
            Err(e) => {
                say(&format!("install: {e}"));
                return BUSY;
            }
        }
    } else {
        None
    };
    // Anything already there (even a damaged install other accounts may use) is not undone on a
    // failure: only a machine with no trace of explorust is.
    let fresh = !install::dir().exists() && !reg::has_uninstall_entry() && reg::machine_users().is_empty();
    let guard = if elevated {
        match machine_install(&from) {
            Ok(g) => g,
            Err(e) => {
                say(&format!("install failed: {e}"));
                if fresh {
                    undo_machine();
                }
                return FAILED;
            }
        }
    } else {
        say("install: not elevated, so no Program Files copy: a uiAccess build will not start from here");
        me.clone()
    };
    let dir = guard.parent().map(Path::to_path_buf).unwrap_or_default();
    // The toast listener's identity; explorust runs without it (it polls), so a failure only warns.
    match package::add(&from, &dir) {
        Ok(p) => say(&format!("install: notification helper package {p}, external location {}", dir.display())),
        Err(e) => say(&format!("install: notification helper package not installed ({e}); toasts are polled")),
    }
    folders::viewer(&dir, true);
    // The users list keeps the shared files while any account still starts the guard: without this
    // account in it, an uninstall from another account could remove them under it.
    let owner = if elevated {
        match account::own_sid() {
            Some(sid) => reg::add_machine_user(&sid).map_err(|e| format!("users list: {e}")),
            None => Err("users list: this account's SID is unknown".to_string()),
        }
    } else {
        Ok(())
    };
    if let Err(e) = owner.and_then(|()| reg::set_shell(&format!("\"{}\"", guard.display()))) {
        say(&format!("install failed: {e}"));
        // This user's registrations point into the files: they go first.
        folders::viewer(&dir, false);
        remove_package();
        if elevated && fresh {
            undo_machine();
        }
        return FAILED;
    }
    // A fresh try: the streak of problem logons starts again.
    reg::set_problem_streak(0);
    reg::set_logon_had_problem(false);
    say(&format!("installed: next logon starts {}", guard.display()));
    drop(lock);
    if let Err(e) = crate::prefetch::start(&dir.join("explorust.exe"), elevated) {
        log::line(&format!("install: could not start optional symbol prefetch ({e})"));
    }
    0
}

/// Copies the files (all or nothing), trusts the certificate, installs the index service and writes
/// the Apps entry. Err stops the install before the Shell value is touched.
fn machine_install(from: &Path) -> Result<PathBuf, String> {
    let guard = install::copy_files(from)?;
    let trust = install::trust(true)?;
    say(&format!("install: shell copied to {}, certificate trusted ({trust})", install::dir().display()));
    match install::index_service(true) {
        Ok(s) => say(&format!("install: {s}")),
        // Search falls back to Windows Search: not a reason to refuse the shell.
        Err(e) => say(&format!("install: index service not installed ({e})")),
    }
    apps::register(&guard).map_err(|e| format!("Apps entry: {e}"))?;
    Ok(guard)
}

/// A fresh install that failed halfway leaves nothing behind.
fn undo_machine() {
    let svc = install::index_service(false);
    let cert = install::trust(false);
    let apps = apps::unregister();
    let files = install::remove_files();
    reg::delete_machine_state();
    say(&format!("install undone: service {svc:?}, certificate {cert:?}, {apps:?}, files {files:?}"));
}

/// This user's half, in this user's own process: Ok once the Shell value is gone (or was never
/// there); the rest only warns. The notification package goes later (`remove_package`), once
/// explorer is back: its removal can take half a minute.
fn restore_user() -> Result<String, String> {
    let shell = reg::delete_shell().map_err(|e| format!("the Shell value stays: {e}"))?;
    folders::handler(false);
    folders::viewer(&install::dir(), false);
    reg::set_problem_streak(0);
    reg::set_logon_had_problem(false);
    let r = format!("Shell value {}", if shell { "deleted" } else { "absent" });
    say(&format!("restore: {r}"));
    Ok(r)
}

pub enum RestoreMode {
    Repair,
    Uninstall { quiet: bool },
    FinishUninstall,
}

pub enum SessionDesktop {
    Ready(String),
    UninstallPending,
}

/// Serializes the user cleanup and return to Explorer with a live handoff.
pub fn restore_session(mode: RestoreMode) -> (Result<String, String>, Result<SessionDesktop, String>) {
    let started = Instant::now();
    let lock = || {
        let user = loop {
            match install::user_setup_lock() {
                Ok(lock) => break lock,
                Err(_) if matches!(mode, RestoreMode::FinishUninstall)
                    && started.elapsed() < Duration::from_secs(5) =>
                {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(error) => return Err(error),
            }
        };
        let cancel = proc::Event::named(CANCEL_FOR_RESTORE)?;
        if matches!(mode, RestoreMode::FinishUninstall) && reg::shell().is_some() {
            return Err("the user installed a shell while uninstall was pending".into());
        }
        cancel.signal()?;
        say("restore: requested Windows desktop");
        Ok((user, cancel))
    };
    let _locks = match lock() {
        Ok(lock) => lock,
        Err(e) => return (Err(e), Err("not tried".into())),
    };
    let user = restore_user();
    if user.is_err() {
        return (user, Err("not tried".into()));
    }
    let mut deadline = Instant::now() + DESKTOP_RECOVERY_LIMIT;
    let back = loop {
        if matches!(mode, RestoreMode::FinishUninstall) && taskbar::explorer_file_operations() {
            deadline = Instant::now() + DESKTOP_RECOVERY_LIMIT;
            std::thread::sleep(Duration::from_millis(250));
            continue;
        }
        let locks = policy::try_restore_locks(install::switch_lock_for, install::recovery_lock_for);
        let pending = match locks {
            RestoreLocks::Acquired(switch, recovery) if !proc::Event::is_signalled(EXITING_EXPLORER) => {
                let result = bring_back_explorer();
                drop(recovery);
                drop(switch);
                if result.is_ok() && !matches!(mode, RestoreMode::Repair) {
                    remove_package();
                }
                break result.map(SessionDesktop::Ready);
            }
            RestoreLocks::Acquired(..) => "Explorer's exit is pending".into(),
            RestoreLocks::Pending(error) => error,
        };
        match mode {
            RestoreMode::Uninstall { quiet } => {
                say(&format!("restore: finishing uninstall after the switch cancels ({pending})"));
                break uninstall::finish_detached(quiet).map(|()| SessionDesktop::UninstallPending);
            }
            RestoreMode::FinishUninstall if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            _ => break Err(format!("desktop recovery is already in progress: {pending}")),
        }
    };
    (user, back)
}

/// This user's notification helper package; a failure only warns (it does nothing without the
/// files).
pub fn remove_package() {
    match package::remove() {
        Ok(r) => say(&format!("restore: notification package {r}")),
        Err(e) => say(&format!("restore: notification package not removed ({e})")),
    }
}

/// Brings explorer's taskbar back now and says so only once it answers. A running guard does it
/// (it ends explorust and keeps explorer up); with no guard and no taskbar, explorer starts here.
pub fn bring_back_explorer() -> Result<String, String> {
    let result = desktop_recovery::restore_desktop(&install::dir().join("explorust.exe"), None)
        .map(|desktop| desktop.description);
    match &result {
        Ok(status) | Err(status) => say(&format!("restore: {status}")),
    }
    result
}

/// The machine half, elevated. Kept (Ok(false)) while another account's Shell value still points to
/// the guard, or that account is signed out and cannot be checked: removing the files would leave
/// it with no shell. Removed (Ok(true)) in this order: service, certificate, files, then the Apps
/// entry, so a failed step keeps the entry for a retry. `session_sid`: the user who asked.
pub fn restore_machine(session_sid: Option<&str>) -> Result<bool, String> {
    let _lock = install::lock()?;
    let mut sids = reg::machine_users();
    for extra in session_sid.map(str::to_string).into_iter().chain(account::is_session_user().then(account::own_sid).flatten()) {
        if !sids.contains(&extra) {
            sids.push(extra);
        }
    }
    let mut users = Vec::new();
    for sid in sids {
        let state = if !reg::profile_exists(&sid) {
            None
        } else {
            match reg::user_shell(&sid) {
                Some(Some(v)) if v.to_ascii_lowercase().contains("explorust-guard") => Some("still on"),
                Some(_) => None,
                None => Some("signed out, cannot check"),
            }
        };
        match state {
            Some(why) => users.push(format!("{sid} ({why})")),
            None => {
                let _ = reg::remove_machine_user(&sid);
            }
        }
    }
    if !users.is_empty() {
        say(&format!("restore: the shared install stays for {}", users.join(", ")));
        return Ok(false);
    }
    // The guard that ran this session lets go of its files once explorer is back.
    std::thread::sleep(Duration::from_secs(2));
    let svc = install::index_service(false).map_err(|e| format!("index service: {e}"))?;
    say(&format!("restore: {svc}"));
    let cert = install::trust(false).map_err(|e| format!("certificate: {e}"))?;
    say(&format!("restore: certificate {cert}"));
    let files = install::remove_files().map_err(|e| format!("files: {e}"))?;
    say(&format!("restore: {files}"));
    let apps = apps::unregister().map_err(|e| format!("Apps entry: {e}"))?;
    say(&format!("restore: {apps}"));
    reg::delete_machine_state();
    Ok(true)
}

/// `--restore` for tools and repairs: this user (when this process is theirs), explorer back, then
/// the machine when elevated. 0 only when every step worked.
pub fn restore() -> u32 {
    let mine = !install::elevated() || account::is_session_user();
    let (user, back) = if mine {
        let (user, desktop) = restore_session(RestoreMode::Repair);
        let back = desktop.and_then(|status| match status {
            SessionDesktop::Ready(description) => Ok(description),
            SessionDesktop::UninstallPending => Err("desktop recovery is pending".into()),
        });
        (user, back)
    } else {
        let back = install::recovery_lock_for(0).and_then(|_lock| bring_back_explorer());
        (Ok(String::new()), back)
    };
    // Only once the shell is off for good and explorer answers: the package is explorust's.
    if mine && user.is_ok() && back.is_ok() {
        remove_package();
    }
    let machine = match (&user, &back, install::elevated()) {
        (Ok(_), Ok(_), true) => restore_machine(None).map(|_| ()),
        (Err(_), _, true) => Err("files kept: the Shell value is still there".to_string()),
        (_, Err(_), true) => Err("files kept: explorer is not back yet; sign out and in, then run it again".to_string()),
        (_, _, false) => {
            if install::dir().exists() || install::trusted() {
                say("restore: run it elevated to remove the Program Files copy and the certificate");
            }
            Ok(())
        }
    };
    let mut code = 0;
    for (what, r) in [("user", user.map(|_| ())), ("explorer", back.clone().map(|_| ())), ("machine", machine)] {
        if let Err(e) = r {
            say(&format!("restore failed ({what}): {e}"));
            code = FAILED;
        }
    }
    code
}
