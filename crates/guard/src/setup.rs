//! `--setup`: what the installer (`explorust-setup.exe`, crates/setup) runs once it has unpacked
//! the files: one question, the install (`manage::install`: files, Apps entry and Start menu
//! shortcuts, then the Shell value), then restart the running shell or offer to sign out. Run elevated
//! (the installer asks Windows); not elevated it asks. Elevated with another account's password it
//! refuses: that account's settings would change instead of the signed-in user's.

use crate::ui::{self, Icon, tr};
use crate::launch_timing::{FALLBACK_EVENT, RELOAD_EVENT, reload_preserves_files_event};
use crate::manage::OTHER_ACCOUNT;
use crate::proc::Event;
use crate::setup_mode::{self, Mode};
use crate::{account, install, log, taskbar};
use std::path::Path;
use std::time::{Duration, Instant};
use windows::Win32::System::Shutdown::{EWX_LOGOFF, ExitWindowsEx, SHTDN_REASON_FLAG_PLANNED, SHTDN_REASON_MAJOR_APPLICATION};

const INSTALL: i32 = 1;
const SIGN_OUT: i32 = 10;
const LATER: i32 = 11;

pub fn run() -> u32 {
    if !install::elevated() {
        return crate::uninstall::elevated_again("--setup").unwrap_or(1);
    }
    if !account::is_session_user() {
        ui::tell(
            Icon::Warning,
            tr("Installez explorust depuis un compte administrateur", "Install explorust from an administrator account"),
            tr(
                "Le mot de passe saisi est celui d'un autre compte, alors explorust s'installerait pour ce compte-là et pas pour vous. Ouvrez une session avec un compte administrateur, puis relancez l'installation.",
                "The password you typed belongs to another account, so explorust would be set up for that account instead of you. Sign in with an administrator account, then run the installer again.",
            ),
            None,
        );
        return OTHER_ACCOUNT;
    }
    let exe = install::dir().join("explorust.exe");
    let mode = install_mode(&exe);
    let go = match mode {
        Mode::UpdateLive { preserves_files: true, .. } => tr(
            "Mettre à jour\nRedémarre explorust en quelques secondes, sans fermer la session.",
            "Update\nRestarts explorust in a few seconds without signing out.",
        ),
        Mode::UpdateLive { preserves_files: false, .. } => tr(
            "Mettre à jour\nRedémarre explorust sans fermer la session. Les fenêtres de Fichiers se ferment et les copies, déplacements et suppressions en cours s'arrêtent.",
            "Update\nRestarts explorust without signing out. File Explorer windows close and ongoing copies, moves and deletions stop.",
        ),
        Mode::UpdateOnSignIn => tr("Mettre à jour\nPrend effet à la prochaine ouverture de session.", "Update\nTakes effect at the next sign-in."),
        Mode::Install => tr("Installer\nPrend effet à la prochaine ouverture de session.", "Install\nTakes effect at the next sign-in."),
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
        crate::manage::install,
    );
    if code != 0 {
        ui::tell(
            Icon::Warning,
            tr("L'installation a échoué", "The install failed"),
            if code == crate::manage::BUSY {
                tr(
                    "Une autre installation ou désinstallation d'explorust est en cours. Attendez qu'elle finisse, puis relancez.",
                    "Another explorust install or uninstall is running. Wait for it to finish, then run this again.",
                )
            } else {
                tr("Rien n'a changé pour Windows. Le détail est dans le journal.", "Nothing changed for Windows. The details are in the log.")
            },
            Some(&log::dir().join("guard.log").display().to_string()),
        );
        return code;
    }
    if let Mode::UpdateLive { pid, .. } = mode {
        return finish_live_update(&exe, pid);
    }
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

fn install_mode(exe: &Path) -> Mode {
    let installed = exe.is_file();
    let shell = installed.then(|| taskbar::explorust(exe)).flatten();
    // The session-scoped fallback event is held by the supervising guard, including an adopted one.
    let reload = shell.is_some() && Event::open_for_signal(RELOAD_EVENT).is_ok();
    let guarded = reload && Event::open_for_signal(FALLBACK_EVENT).is_ok();
    let preserves_files = shell.is_some_and(|pid| Event::open_for_signal(&reload_preserves_files_event(pid)).is_ok());
    setup_mode::choose(installed, shell, reload, guarded, preserves_files)
}

enum Restart {
    Updated,
    Windows,
    Unconfirmed,
}

fn finish_live_update(exe: &Path, pid: u32) -> u32 {
    let result = ui::working(
        tr("Redémarrage d'explorust", "Restarting explorust"),
        tr(
            "Les opérations de fichiers en cours peuvent prendre du temps.",
            "File operations in progress can take time.",
        ),
        || reload(exe, pid),
    );
    let details = log::dir().join("guard.log").display().to_string();
    match result {
        Restart::Updated => ui::tell(
            Icon::Info,
            tr("explorust est à jour", "explorust is up to date"),
            tr("La nouvelle version est déjà en cours d'exécution.", "The new version is already running."),
            None,
        ),
        Restart::Windows => ui::tell(
            Icon::Warning,
            tr("Windows a repris la main", "Windows took over"),
            tr("La nouvelle version a été installée. Le journal indique pourquoi explorust s'est arrêté.", "The new version was installed. See the log for why explorust stopped."),
            Some(&details),
        ),
        Restart::Unconfirmed => ui::tell(
            Icon::Warning,
            tr("Redémarrage d'explorust non confirmé", "Could not confirm explorust restarted"),
            tr("La nouvelle version a été installée. Consultez le journal pour en savoir plus.", "The new version was installed. See the log for details."),
            Some(&details),
        ),
    }
    0
}

fn reload(exe: &Path, original: u32) -> Restart {
    // A running image may have been renamed during the swap. Its tray still identifies it.
    let shell = taskbar::owns_tray(original).then_some(original).or_else(|| taskbar::explorust(exe));
    if let Some(pid) = shell {
        // The guard keeps running its previous image until the next sign-in. It starts the new
        // explorust.exe in the same session once the old shell exits with the reload code.
        if let Err(error) = Event::set_existing(RELOAD_EVENT) {
            log::line(&format!("setup: could not request reload of pid {pid}: {error}"));
        } else {
            log::line(&format!("setup: reload requested for pid {pid}"));
            return wait_for_restart(exe, pid);
        }
    } else {
        log::line("setup: explorust's taskbar disappeared during installation");
    }
    if taskbar::explorer().is_some() { Restart::Windows } else { Restart::Unconfirmed }
}

fn wait_for_restart(exe: &Path, previous: u32) -> Restart {
    let mut last_old_tray = Instant::now();
    loop {
        if taskbar::owns_tray(previous) {
            // While Files drains, the old shell still serves its taskbar.
            last_old_tray = Instant::now();
        } else if let Some(pid) = taskbar::explorust(exe)
            && pid != previous && taskbar::probe(pid) == crate::policy::Probe::Answered
        {
            log::line(&format!("setup: new explorust pid {pid} owns an answering taskbar (was {previous})"));
            return Restart::Updated;
        }
        if let Some(pid) = taskbar::explorer() {
            log::line(&format!("setup: Windows took over (explorer pid {pid})"));
            return Restart::Windows;
        }
        if last_old_tray.elapsed() >= Duration::from_secs(60) {
            log::line("setup: no answering replacement taskbar within 60 s");
            return Restart::Unconfirmed;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
