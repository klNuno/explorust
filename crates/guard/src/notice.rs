//! The one message after a fallback the user did not ask for. It waits a few seconds so
//! explorer's taskbar is up behind it, then offers to try again at the next sign-in (nothing to do)
//! or to go back to Windows for good (`--uninstall`, which asks Windows for the right to remove
//! the files). Shown once: the session stays on explorer after it.

use crate::log;
use crate::ui::{self, Icon, tr};
use std::time::Duration;

pub enum Notice {
    /// A problem in this session; the reason, in English, goes behind "Details".
    Problem(String),
    /// explorust has been turned off (the Shell value is gone): problems at `policy::MAX_STREAK`
    /// logons in a row, or its files are missing. The reason goes behind "Details".
    TurnedOff(String),
    /// explorer's taskbar did not come up either: how to start it by hand.
    NoShell,
}

const RETRY: i32 = 100;
const UNINSTALL: i32 = 101;

/// Shown on its own thread a moment after explorer's taskbar answers, so the dialog sits over a
/// painted desktop.
pub fn show_later(n: Notice) {
    let _ = std::thread::Builder::new().name("notice".into()).spawn(move || {
        std::thread::sleep(Duration::from_secs(1));
        show(n);
    });
}

fn log_path() -> String {
    log::dir().join("guard.log").display().to_string()
}

fn show(n: Notice) {
    match n {
        Notice::Problem(why) => {
            let details = format!("{why}\n\n{}: {}", tr("Journal", "Log"), log_path());
            let retry = tr(
                "Réessayer à la prochaine ouverture de session\nexplorust revient la prochaine fois que vous vous connectez.",
                "Try again at the next sign-in\nexplorust comes back the next time you sign in.",
            );
            let uninstall = tr(
                "Revenir à Windows pour de bon\nDésinstalle explorust. Windows demande votre accord.",
                "Go back to Windows for good\nUninstalls explorust. Windows asks for your permission.",
            );
            let pick = ui::choose(
                Icon::Warning,
                tr("explorust a eu un problème, Windows a repris la main", "explorust ran into a problem, Windows took over"),
                tr(
                    "La barre des tâches et l'Explorateur de Windows sont revenus. Vos fenêtres ouvertes n'ont pas bougé.",
                    "The Windows taskbar and File Explorer are back. Your open windows were not touched.",
                ),
                &[(RETRY, retry), (UNINSTALL, uninstall)],
                false,
                Some(&details),
            );
            log::line(&format!("notice: problem shown, user picked {}", if pick == UNINSTALL { "uninstall" } else { "retry" }));
            if pick == UNINSTALL {
                crate::uninstall::start_detached();
            }
        }
        Notice::TurnedOff(why) => {
            let details = format!("{why}\n\n{}: {}", tr("Journal", "Log"), log_path());
            ui::tell(
                Icon::Warning,
                tr("explorust est désactivé", "explorust is turned off"),
                tr(
                    "Il ne pouvait plus démarrer correctement, alors Windows est revenu pour de bon. Pour le réessayer, relancez l'installation. Pour le supprimer : Paramètres > Applications > explorust > Désinstaller.",
                    "It could no longer start properly, so Windows is back for good. To try it again, run the installer again. To remove it: Settings > Apps > explorust > Uninstall.",
                ),
                Some(&details),
            );
            log::line("notice: turned off shown");
        }
        Notice::NoShell => {
            ui::tell(
                Icon::Warning,
                tr("La barre des tâches de Windows ne démarre pas", "The Windows taskbar does not start"),
                tr(
                    "Appuyez sur Ctrl+Maj+Échap, puis Exécuter une nouvelle tâche, tapez explorer.exe et validez. Si rien ne change, fermez la session et rouvrez-la.",
                    "Press Ctrl+Shift+Esc, then Run new task, type explorer.exe and press Enter. If nothing changes, sign out and sign in again.",
                ),
                Some(&log_path()),
            );
            log::line("notice: no shell shown");
        }
    }
}
