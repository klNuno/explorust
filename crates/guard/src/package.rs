//! The notify helper's sparse package (`crates/notify/package`): `explorust-notify.exe` gets the
//! package identity `UserNotificationListener.NotificationChanged` asks for only when this
//! unsigned package is registered for the user with the shell's folder as its external location.
//! `--install` registers it from `explorust-notify.msix` beside the exes and allows its
//! notification access; `--restore` removes both. PowerShell's Appx module does the work (no
//! WinRT in the guard), with no window.

use crate::log;
use std::path::Path;
use std::process::Command;

pub const MSIX: &str = "explorust-notify.msix";
const NAME: &str = "explorust.notify";
/// Family name: the package name and the hash of its unsigned OID publisher.
const FAMILY: &str = "explorust.notify_m23m5a37cevzy";
const CONSENT: &str = r"HKCU:\Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\userNotificationListener";

/// PowerShell single-quoted literal.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// The longest an Appx step may take; past it PowerShell is stopped and the step fails.
const LIMIT: std::time::Duration = std::time::Duration::from_secs(180);

fn powershell(script: &str) -> Result<String, String> {
    let (ok, text, err) = crate::proc::run_bounded(
        Command::new(crate::install::system32().join(r"WindowsPowerShell\v1.0\powershell.exe")).args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &format!("$ProgressPreference='SilentlyContinue'; {script}"),
        ]),
        LIMIT,
    )?;
    // Every script sets ErrorActionPreference Stop, so a failure ends with a non-zero code;
    // stderr alone can be a progress record PowerShell serializes when its output is redirected.
    if ok {
        if !err.is_empty() {
            log::line(&format!("package: stderr: {}", err.chars().take(300).collect::<String>()));
        }
        Ok(text)
    } else {
        Err(format!("{text} {err}").trim().chars().take(600).collect())
    }
}

/// Registers the package with `dir` (where explorust-notify.exe lives) as its external location,
/// replacing an earlier registration (an older folder, such as a dev copy), and allows it to read
/// notifications. `from` holds the .msix.
pub fn add(from: &Path, dir: &Path) -> Result<String, String> {
    let msix = from.join(MSIX);
    if !msix.exists() {
        return Err(format!("{} missing", msix.display()));
    }
    if !dir.join("explorust-notify.exe").exists() {
        return Err(format!("explorust-notify.exe missing in {}", dir.display()));
    }
    let script = format!(
        "$ErrorActionPreference='Stop'; Get-AppxPackage {NAME} | Remove-AppxPackage; \
         Add-AppxPackage -Path {} -ExternalLocation {} -AllowUnsigned; \
         New-Item {} -Force | Out-Null; Set-ItemProperty {} Value Allow; \
         $p = Get-AppxPackage {NAME}; \"$($p.PackageFamilyName) $($p.Status)\"",
        quote(&msix.to_string_lossy()),
        quote(&dir.to_string_lossy()),
        quote(&format!(r"{CONSENT}\{FAMILY}")),
        quote(&format!(r"{CONSENT}\{FAMILY}")),
    );
    let t0 = std::time::Instant::now();
    let r = powershell(&script);
    log::line(&format!("package: add from {} for {} in {} ms: {r:?}", msix.display(), dir.display(), t0.elapsed().as_millis()));
    r
}

/// Removes the package and its notification access; Ok with what was done.
pub fn remove() -> Result<String, String> {
    let script = format!(
        "$ErrorActionPreference='Stop'; $p = Get-AppxPackage {NAME}; \
         $c = {}; $k = 'consent removed'; \
         if (Test-Path $c) {{ try {{ Remove-Item $c -Force }} catch {{ $k = \"consent kept ($($_.Exception.Message))\" }} }}; \
         if ($p) {{ $p | Remove-AppxPackage; \"removed, $k\" }} else {{ \"absent, $k\" }}",
        quote(&format!(r"{CONSENT}\{FAMILY}")),
    );
    let r = powershell(&script);
    log::line(&format!("package: remove: {r:?}"));
    r
}
