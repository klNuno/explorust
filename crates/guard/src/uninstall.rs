//! `--uninstall`: the way back for anyone, from Settings > Apps, the Start menu shortcut or the
//! problem notice. In the user's own process it first turns explorust off for this user (the Shell
//! value) and brings explorer back, and only then asks Windows for the right (UAC) to remove the
//! shared files (`--uninstall --machine`, `manage::restore_machine`). Refused, explorust stays off
//! and the files stay until an uninstall with permission. `--yes` skips the question, `--quiet`
//! shows nothing (Apps' QuietUninstallString).

use crate::manage::{self, BUSY, FAILED, KEPT, OTHER_ACCOUNT, RestoreMode, SessionDesktop};
use crate::ui::{self, Icon, tr};
use crate::{account, install, log};
use std::os::windows::process::CommandExt;
use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED};
use windows::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
use windows::Win32::UI::Shell::{SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::core::{HSTRING, PCWSTR, w};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// From the notice: the user already chose, so no question, only the result.
pub fn start_detached() {
    let Ok(exe) = std::env::current_exe() else { return };
    let guard = exe.with_file_name(install::GUARD_EXE);
    match std::process::Command::new(guard).args(["--uninstall", "--yes"]).creation_flags(CREATE_NO_WINDOW).spawn() {
        Ok(c) => log::line(&format!("uninstall started, pid {}", c.id())),
        Err(e) => log::line(&format!("uninstall did not start: {e}")),
    }
}

/// The initiating window can close while cancellation and desktop recovery finish.
pub fn finish_detached(quiet: bool) -> Result<(), String> {
    let me = std::env::current_exe().map_err(|e| format!("locate the uninstaller: {e}"))?;
    let mut child = std::process::Command::new(me);
    child.args(["--uninstall", "--yes", "--finish-session-restore"]).creation_flags(CREATE_NO_WINDOW);
    if quiet {
        child.arg("--quiet");
    }
    child.spawn().map(|_| ()).map_err(|e| format!("start the deferred uninstaller: {e}"))
}

/// Runs this exe again elevated with `args`. Ok(exit code), Err(true) when the user said no.
pub fn elevated_again(args: &str) -> Result<u32, bool> {
    let Ok(me) = std::env::current_exe() else { return Err(false) };
    let file = HSTRING::from(me.as_os_str());
    let params = HSTRING::from(args);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: 1,
        ..Default::default()
    };
    // SAFETY: every string outlives the call; the process handle is waited on and closed.
    unsafe {
        if let Err(e) = ShellExecuteExW(&mut info) {
            log::line(&format!("elevate {args}: {e}"));
            return Err(e.code() == ERROR_CANCELLED.to_hresult());
        }
        let mut code = 1u32;
        if !info.hProcess.is_invalid() {
            WaitForSingleObject(info.hProcess, INFINITE);
            let _ = GetExitCodeProcess(info.hProcess, &mut code);
            let _ = CloseHandle(info.hProcess);
        }
        Ok(code)
    }
}

/// The elevated half: no dialog, the exit code tells the user's process what happened.
pub fn machine(session_sid: Option<&str>) -> u32 {
    if !install::elevated() {
        log::line("uninstall --machine: not elevated");
        return FAILED;
    }
    match manage::restore_machine(session_sid) {
        Ok(true) => 0,
        Ok(false) => KEPT,
        Err(e) if e.contains("another explorust install") => {
            log::line(&format!("uninstall: {e}"));
            BUSY
        }
        Err(e) => {
            log::line(&format!("uninstall failed: {e}"));
            FAILED
        }
    }
}

/// What the machine half gave.
enum Machine {
    Code(u32),
    Refused,
}

pub fn run(yes: bool, quiet: bool) -> u32 {
    if install::elevated() && !account::is_session_user() {
        // This process's HKCU is another account's: the user's Shell value is out of reach here.
        log::line("uninstall: elevated as another account");
        if !quiet {
            ui::tell(
                Icon::Warning,
                tr("Lancez la désinstallation depuis votre session", "Run the uninstall from your own account"),
                tr(
                    "Cette fenêtre tourne avec un autre compte. Ouvrez Paramètres > Applications > explorust > Désinstaller depuis votre propre session.",
                    "This window runs as another account. Open Settings > Apps > explorust > Uninstall from your own account.",
                ),
                None,
            );
        }
        return OTHER_ACCOUNT;
    }
    if !yes && !quiet && !confirm() {
        log::line("uninstall: cancelled by the user");
        return 1;
    }
    let finishing = std::env::args().any(|arg| arg == "--finish-session-restore");
    let steps = || {
        let mode = if finishing { RestoreMode::FinishUninstall } else { RestoreMode::Uninstall { quiet } };
        let (user, desktop) = manage::restore_session(mode);
        let back = match desktop {
            Ok(SessionDesktop::Ready(description)) => Ok(description),
            Ok(SessionDesktop::UninstallPending) => return None,
            Err(error) => Err(error),
        };
        if user.is_err() {
            return Some((user, back, Machine::Code(FAILED)));
        }
        // The files stay until explorer is confirmed back: the guard of this session still runs
        // them, and a retry needs the Apps entry.
        let machine = if back.is_err() {
            Machine::Code(FAILED)
        } else if install::elevated() {
            Machine::Code(machine(account::own_sid().as_deref()))
        } else {
            let mut args = String::from("--uninstall --machine");
            if let Some(sid) = account::own_sid() {
                args.push_str(&format!(" --for {sid}"));
            }
            match elevated_again(&args) {
                Ok(code) => Machine::Code(code),
                Err(refused) => {
                    log::line(&format!("uninstall: no permission for the files (refused: {refused})"));
                    Machine::Refused
                }
            }
        };
        Some((user, back, machine))
    };
    let Some((user, back, machine)) = (if quiet || finishing {
        steps()
    } else {
        ui::working(
            tr("Retour à Windows", "Going back to Windows"),
            tr("explorust se désinstalle, cela peut prendre une minute.", "explorust is being uninstalled, this can take a minute."),
            steps,
        )
    }) else {
        return 0;
    };
    let log_path = log::dir().join("guard.log").display().to_string();
    if let Err(e) = &user {
        log::line(&format!("uninstall failed: {e}"));
        if !quiet {
            ui::tell(
                Icon::Warning,
                tr("explorust n'a pas pu être désactivé", "explorust could not be turned off"),
                tr(
                    "Rien n'a été supprimé. Réessayez, ou appuyez sur Ctrl+Alt+Maj+F12 pour revenir à Windows dans cette session.",
                    "Nothing was removed. Try again, or press Ctrl+Alt+Shift+F12 to go back to Windows for this session.",
                ),
                Some(&format!("{e}\n\n{log_path}")),
            );
        }
        return FAILED;
    }
    let back_note = match &back {
        Ok(_) => None,
        Err(e) => Some(e.clone()),
    };
    log::line(&format!("uninstall: explorer {back:?}"));
    let code = match machine {
        Machine::Code(0) | Machine::Code(KEPT) => 0,
        Machine::Code(c) => c,
        Machine::Refused => FAILED,
    };
    if quiet {
        return if back_note.is_some() { FAILED } else { code };
    }
    // A missing taskbar first: the user needs to know how to get it back.
    let (icon, main, content): (Icon, &str, &str) = match (&machine, &back_note) {
        (_, Some(_)) => (
            Icon::Warning,
            tr("explorust est désactivé, mais la barre des tâches n'est pas revenue", "explorust is turned off, but the taskbar did not come back"),
            tr(
                "Fermez la session et rouvrez-la : Windows démarrera normalement. Relancez ensuite la désinstallation pour supprimer ses fichiers.",
                "Sign out and sign in again: Windows will start normally. Then run the uninstall again to remove its files.",
            ),
        ),
        (Machine::Code(0), None) => (
            Icon::Info,
            tr("C'est fait, Windows est revenu", "Done, Windows is back"),
            tr(
                "explorust est désinstallé. La barre des tâches et l'Explorateur de Windows sont de retour. Les derniers fichiers seront effacés au prochain redémarrage.",
                "explorust is uninstalled. The Windows taskbar and File Explorer are back. The last files are deleted at the next restart.",
            ),
        ),
        (Machine::Code(KEPT), None) => (
            Icon::Info,
            tr("C'est fait, Windows est revenu", "Done, Windows is back"),
            tr(
                "explorust est désactivé pour vous. Un autre compte de cet ordinateur l'utilise encore, alors ses fichiers restent pour lui.",
                "explorust is turned off for you. Another account on this computer still uses it, so its files stay for that account.",
            ),
        ),
        (Machine::Refused, None) => (
            Icon::Info,
            tr("explorust est désactivé", "explorust is turned off"),
            tr(
                "Windows est revenu et explorust ne se lancera plus. Ses fichiers restent sur l'ordinateur : pour les supprimer, Paramètres > Applications > explorust > Désinstaller, et acceptez la demande de Windows.",
                "Windows is back and explorust will not run again. Its files stay on the computer: to remove them, Settings > Apps > explorust > Uninstall, and accept Windows' request.",
            ),
        ),
        (Machine::Code(BUSY), None) => (
            Icon::Warning,
            tr("explorust est désactivé, ses fichiers restent", "explorust is turned off, its files stay"),
            tr(
                "Une autre installation ou désinstallation était en cours. Relancez la désinstallation une fois qu'elle est finie.",
                "Another install or uninstall was running. Run the uninstall again once it is done.",
            ),
        ),
        (Machine::Code(_), None) => (
            Icon::Warning,
            tr("Désinstallation incomplète", "Uninstall incomplete"),
            tr(
                "Windows est revenu et explorust ne se lancera plus, mais la suppression de ses fichiers a échoué. Relancez la désinstallation, le détail est dans le journal.",
                "Windows is back and explorust will not run again, but removing its files failed. Run the uninstall again; the details are in the log.",
            ),
        ),
    };
    let details = match &back_note {
        Some(e) => format!("{e}\n\n{log_path}"),
        None => log_path,
    };
    ui::tell(icon, main, content, Some(&details));
    if back_note.is_some() { FAILED } else { code }
}

fn confirm() -> bool {
    let go = tr(
        "Revenir à Windows\nDésinstalle explorust et remet tout de suite la barre des tâches et l'Explorateur de Windows.",
        "Go back to Windows\nUninstalls explorust and brings back the Windows taskbar and File Explorer right away.",
    );
    ui::choose(
        Icon::Shield,
        tr("Revenir à Windows ?", "Go back to Windows?"),
        tr("Vos fenêtres ouvertes restent ouvertes.", "Your open windows stay open."),
        &[(1, go)],
        true,
        None,
    ) == 1
}
