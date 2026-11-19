//! The notify helper's sparse package (`crates/notify/package`): `explorust-notify.exe` gets the
//! package identity `UserNotificationListener.NotificationChanged` asks for only when this
//! unsigned package is registered for the user with the shell's folder as its external location.
//! `--install` registers it from `explorust-notify.msix` beside the exes and allows its
//! notification access; `--restore` removes both. PowerShell's Appx module does the work (no
//! WinRT in the guard), with no window.

use crate::log;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

pub const MSIX: &str = "explorust-notify.msix";
const NAME: &str = "explorust.notify";
/// Family name: the package name and the hash of its unsigned OID publisher.
const FAMILY: &str = "explorust.notify_m23m5a37cevzy";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CONSENT: &str = r"HKCU:\Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\userNotificationListener";

/// PowerShell single-quoted literal.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn powershell(script: &str) -> Result<String, String> {
    let out = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("powershell: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if out.status.success() && err.is_empty() {
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
         if ($p) {{ $p | Remove-AppxPackage; 'removed' }} else {{ 'absent' }}; \
         Remove-Item {} -Force -EA SilentlyContinue",
        quote(&format!(r"{CONSENT}\{FAMILY}")),
    );
    let r = powershell(&script);
    log::line(&format!("package: remove: {r:?}"));
    r
}
