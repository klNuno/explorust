//! `--setup`: what the installer (`explorust-setup.exe`, crates/setup) runs once it has unpacked
//! the files: one question, the install, the Apps entry and Start menu shortcuts, then an offer to
//! sign out so explorust starts. Run elevated (the installer asks Windows); not elevated it asks.

use crate::ui::{self, Icon, tr};
use crate::{apps, install, log, reg};
use windows::Win32::System::Shutdown::{EWX_LOGOFF, ExitWindowsEx, SHTDN_REASON_FLAG_PLANNED, SHTDN_REASON_MAJOR_APPLICATION};

const INSTALL: i32 = 1;
const SIGN_OUT: i32 = 10;
const LATER: i32 = 11;

pub fn run() -> u32 {
    if !install::elevated() {
        return crate::uninstall::elevated_again("--setup").unwrap_or(1);
    }
    let again = install::dir().join("explorust.exe").exists();
    let go = if again {
        tr("Mettre à jour\nRemplace la version installée.", "Update\nReplaces the installed version.")
    } else {
        tr("Installer\nPrend effet à la prochaine ouverture de session.", "Install\nTakes effect at the next sign-in.")
    };
    let pick = ui::choose(
        Icon::Shield,
        tr("Installer explorust ?", "Install explorust?"),
        tr(
            "explorust remplace la barre des tâches, le menu Démarrer et l'Explorateur de Windows.\n\nAu moindre problème, Windows reprend la main tout seul. Pour revenir à Windows à tout moment : Ctrl+Alt+Maj+F12, ou menu Démarrer > explorust > Revenir à Windows.",
            "explorust replaces the Windows taskbar, Start menu and File Explorer.\n\nAt the first problem, Windows takes over by itself. To go back to Windows at any time: Ctrl+Alt+Shift+F12, or Start > explorust > Back to Windows.",
        ),
        &[(INSTALL, go)],
        true,
        None,
    );
    if pick != INSTALL {
        log::line("setup: cancelled by the user");
        return 1;
    }
    let code = ui::working(
        tr("Installation d'explorust", "Installing explorust"),
        tr("Cela peut prendre une ou deux minutes.", "This can take a minute or two."),
        crate::install,
    );
    if code != 0 {
        ui::tell(
            Icon::Warning,
            tr("L'installation a échoué", "The install failed"),
            tr("Rien n'a changé pour Windows. Le détail est dans le journal.", "Nothing changed for Windows. The details are in the log."),
            Some(&log::dir().join("guard.log").display().to_string()),
        );
        return code;
    }
    let r = apps::register(&install::dir().join("explorust-guard.exe"));
    log::line(&format!("setup: {r}"));
    // A reinstall is a fresh try: the streak of problem logons starts again.
    reg::set_problem_streak(0);
    let pick = ui::choose(
        Icon::Info,
        tr("explorust est installé", "explorust is installed"),
        tr(
            "Il démarre à la prochaine ouverture de session.",
            "It starts at the next sign-in.",
        ),
        &[
            (
                SIGN_OUT,
                tr("Fermer la session maintenant\nEnregistrez votre travail avant.", "Sign out now\nSave your work first."),
            ),
            (LATER, tr("Plus tard", "Later")),
        ],
        false,
        None,
    );
    if pick == SIGN_OUT {
        log::line("setup: signing out");
        // SAFETY: ends this user's session, as the user asked.
        if let Err(e) = unsafe { ExitWindowsEx(EWX_LOGOFF, SHTDN_REASON_MAJOR_APPLICATION | SHTDN_REASON_FLAG_PLANNED) } {
            log::line(&format!("sign out: {e}"));
        }
    }
    0
}
