//! How a user who never opens a terminal finds the way back: an entry in Settings > Apps
//! The Apps entry and Start shortcuts for Settings, uninstall and session fallback.
//! These machine-wide registrations are written elevated by `--install`.

use crate::{log, reg, ui::tr};
use std::path::{Path, PathBuf};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, IPersistFile};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::core::{HSTRING, Interface};

fn programs() -> PathBuf {
    let base = std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    base.join(r"Microsoft\Windows\Start Menu\Programs\explorust")
}

pub const SETTINGS_EXE: &str = "explorust-settings.exe";

/// Localized name, target arguments and description. An empty argument targets Settings.
fn shortcuts() -> [(&'static str, &'static str, &'static str); 4] {
    [
        (
            tr("Revenir à Windows (désinstaller explorust)", "Back to Windows (uninstall explorust)"),
            "--uninstall",
            tr(
                "Désinstalle explorust et remet tout de suite la barre des tâches et l'Explorateur de Windows.",
                "Uninstalls explorust and brings back the Windows taskbar and File Explorer right away.",
            ),
        ),
        (
            tr("Explorateur Windows pour cette session", "Windows Explorer for this session"),
            "--fallback",
            tr(
                "Remplace explorust par l'Explorateur Windows jusqu'à la prochaine ouverture de session.",
                "Replaces explorust with Windows Explorer until the next sign-in.",
            ),
        ),
        (
            tr("Passer à explorust", "Switch to explorust"),
            "--switch",
            tr("Passe sur explorust sans fermer la session.", "Switches to explorust without signing out."),
        ),
        (
            tr("Paramètres d'explorust", "explorust settings"),
            "",
            tr("Personnaliser l'apparence d'explorust.", "Customize the appearance of explorust."),
        ),
    ]
}

fn make_link(path: &Path, target: &Path, args: &str, description: &str, icon: &Path) -> Result<(), String> {
    // SAFETY: COM on this thread for the shell link object, released when it drops.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| format!("ShellLink: {e}"))?;
        link.SetPath(&HSTRING::from(target.as_os_str())).map_err(|e| format!("SetPath: {e}"))?;
        link.SetArguments(&HSTRING::from(args)).map_err(|e| format!("SetArguments: {e}"))?;
        link.SetDescription(&HSTRING::from(description)).map_err(|e| format!("SetDescription: {e}"))?;
        let _ = link.SetIconLocation(&HSTRING::from(icon.as_os_str()), 0);
        if let Some(dir) = target.parent() {
            let _ = link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()));
        }
        let file: IPersistFile = link.cast().map_err(|e| format!("IPersistFile: {e}"))?;
        file.Save(&HSTRING::from(path.as_os_str()), true).map_err(|e| format!("save {}: {e}", path.display()))
    }
}

/// Writes the Apps entry and the Start menu shortcuts for the installed guard. Err when the Apps
/// entry, the way back every user can find, could not be written; a missing shortcut only warns.
pub fn register(guard: &Path) -> Result<String, String> {
    let dir = guard.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let g = guard.display().to_string();
    let size_kb = std::fs::read_dir(&dir)
        .map(|r| r.flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum::<u64>() / 1024)
        .unwrap_or(0) as u32;
    let uninstall = format!("\"{g}\" --uninstall");
    let quiet = format!("\"{g}\" --uninstall --quiet");
    let location = dir.display().to_string();
    let settings = dir.join(SETTINGS_EXE);
    let icon = format!("\"{}\",0", settings.display());
    let entry = reg::set_uninstall_entry(
        &[
            ("DisplayName", "explorust"),
            ("DisplayVersion", env!("CARGO_PKG_VERSION")),
            ("Publisher", "explorust"),
            ("UninstallString", &uninstall),
            ("QuietUninstallString", &quiet),
            ("InstallLocation", &location),
            ("DisplayIcon", &icon),
        ],
        &[("NoModify", 1), ("NoRepair", 1), ("EstimatedSize", size_kb)],
    );
    let folder = programs();
    let mut made = 0;
    let mut errors = Vec::new();
    if let Err(e) = std::fs::create_dir_all(&folder) {
        errors.push(format!("create {}: {e}", folder.display()));
    }
    // A shortcut left by an install in the other language goes first.
    if let Ok(r) = std::fs::read_dir(&folder) {
        for e in r.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
    for (name, args, desc) in shortcuts() {
        let target = if args.is_empty() { &settings } else { guard };
        match make_link(&folder.join(format!("{name}.lnk")), target, args, desc, &settings) {
            Ok(()) => made += 1,
            Err(e) => errors.push(e),
        }
    }
    let r = format!(
        "apps entry {}, {made} Start menu shortcuts in {}{}",
        match &entry {
            Ok(()) => "written".to_string(),
            Err(e) => format!("failed ({e})"),
        },
        folder.display(),
        if errors.is_empty() { String::new() } else { format!(" ({})", errors.join("; ")) }
    );
    log::line(&format!("install: {r}"));
    entry.map(|()| r)
}

/// Removes the Apps entry and the Start menu folder; Err when the entry stays.
pub fn unregister() -> Result<String, String> {
    let entry = match reg::delete_uninstall_entry() {
        Ok(true) => "removed".to_string(),
        Ok(false) => "absent".to_string(),
        Err(e) => return Err(e),
    };
    let folder = programs();
    let links = match std::fs::remove_dir_all(&folder) {
        Ok(()) => "removed".to_string(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "absent".to_string(),
        Err(e) => format!("{e}"),
    };
    Ok(format!("apps entry {entry}, Start menu shortcuts {links}"))
}
