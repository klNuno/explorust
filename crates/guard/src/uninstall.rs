//! `--uninstall`: the way back for anyone, from Settings > Apps, the Start menu shortcut or the
//! problem notice. Not elevated, it asks Windows for the right (UAC) and runs itself again; refused,
//! it still turns explorust off for this user (the Shell value) and brings explorer back now, and
//! says the files stay until an uninstall with permission. `--yes` skips the question, `--quiet`
//! shows nothing (Apps' QuietUninstallString).

use crate::ui::{self, Icon, tr};
use crate::{apps, install, log, reg, supervise};
use std::os::windows::process::CommandExt;
use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED};
use windows::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
use windows::Win32::UI::Shell::{SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::core::{HSTRING, PCWSTR, w};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// From the notice: the user already chose, so no question, only the result.
pub fn start_detached() {
    let Ok(me) = std::env::current_exe() else { return };
    match std::process::Command::new(&me).args(["--uninstall", "--yes"]).creation_flags(CREATE_NO_WINDOW).spawn() {
        Ok(c) => log::line(&format!("uninstall started, pid {}", c.id())),
        Err(e) => log::line(&format!("uninstall did not start: {e}")),
    }
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

pub fn run(yes: bool, quiet: bool) -> u32 {
    if !install::elevated() {
        // The question is asked here, before Windows' own, and not again by the elevated run.
        let mut args = String::from("--uninstall");
        if !yes && !quiet && !confirm() {
            log::line("uninstall: cancelled by the user");
            return 1;
        }
        args.push_str(if quiet { " --quiet" } else { " --yes" });
        return match elevated_again(&args) {
            Ok(code) => code,
            Err(refused) => {
                // No permission: this user's Shell value goes and explorer comes back now; the
                // files and the machine-wide parts stay until an uninstall with permission.
                let gone = reg::delete_shell();
                let _ = crate::proc::Event::set_existing(supervise::FALLBACK_EVENT);
                reg::set_problem_streak(0);
                log::line(&format!("uninstall: not elevated (refused: {refused}), Shell value deleted: {gone:?}"));
                if !quiet {
                    ui::tell(
                        Icon::Info,
                        tr("explorust est désactivé", "explorust is turned off"),
                        tr(
                            "Windows est revenu et explorust ne se lancera plus. Ses fichiers restent sur l'ordinateur : pour les supprimer, Paramètres > Applications > explorust > Désinstaller, et acceptez la demande de Windows.",
                            "Windows is back and explorust will not run again. Its files stay on the computer: to remove them, Settings > Apps > explorust > Uninstall, and accept Windows' request.",
                        ),
                        None,
                    );
                }
                0
            }
        };
    }
    if !yes && !quiet && !confirm() {
        log::line("uninstall: cancelled by the user");
        return 1;
    }
    let code = if quiet {
        crate::restore()
    } else {
        ui::working(
            tr("Retour à Windows", "Going back to Windows"),
            tr("explorust se désinstalle, cela peut prendre une minute.", "explorust is being uninstalled, this can take a minute."),
            crate::restore,
        )
    };
    let apps = apps::unregister();
    log::line(&format!("uninstall: {apps}"));
    reg::set_problem_streak(0);
    if !quiet {
        if code == 0 {
            ui::tell(
                Icon::Info,
                tr("C'est fait, Windows est revenu", "Done, Windows is back"),
                tr(
                    "explorust est désinstallé. La barre des tâches et l'Explorateur de Windows sont de retour. Les derniers fichiers seront effacés au prochain redémarrage.",
                    "explorust is uninstalled. The Windows taskbar and File Explorer are back. The last files are deleted at the next restart.",
                ),
                None,
            );
        } else {
            ui::tell(
                Icon::Warning,
                tr("Désinstallation incomplète", "Uninstall incomplete"),
                tr(
                    "Windows est revenu, mais une étape a échoué. Le détail est dans le journal.",
                    "Windows is back, but one step failed. The details are in the log.",
                ),
                Some(&log::dir().join("guard.log").display().to_string()),
            );
        }
    }
    code
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
