//! Per-user registration and notification access for the signed sparse shell package.

use crate::log;
use std::path::Path;
use std::process::Command;

use crate::msix_identity::FAMILY;

pub const MSIX: &str = "explorust-shell.msix";
pub const LEGACY_MSIX: &str = "explorust-notify.msix";
const NAME: &str = "explorust.shell";
const LEGACY_NAME: &str = "explorust.notify";
const LEGACY_FAMILY: &str = "explorust.notify_m23m5a37cevzy";
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

/// Registers the package from the protected install directory before enabling the Winlogon shell.
/// A healthy registration at that same fixed location survives updates and keeps its consent.
pub fn add(dir: &Path) -> Result<bool, String> {
    let current = quote(&format!(r"{CONSENT}\{FAMILY}"));
    let legacy = quote(&format!(r"{CONSENT}\{LEGACY_FAMILY}"));
    let script = format!(
        "$ErrorActionPreference='Stop'; $newConsent={current}; \
         $previousChoice = Get-ItemPropertyValue $newConsent Value -ErrorAction SilentlyContinue; \
         $choice = $previousChoice; \
         if (-not $choice) {{ $choice = Get-ItemPropertyValue {legacy} Value -ErrorAction SilentlyContinue }}; \
         $old = Get-AppxPackage -Name {NAME}; \
         try {{ \
           if (-not $old -or $old.Status -ne 'Ok') {{ \
             if ($old) {{ $old | Remove-AppxPackage }}; \
             Add-AppxPackage -Path {} -ExternalLocation {}; \
           }}; \
           $p = Get-AppxPackage -Name {NAME}; \
           if ($p.PackageFamilyName -ne '{FAMILY}' -or $p.Status -ne 'Ok') {{ throw 'Shell package is not ready' }}; \
           if (-not $old -or ($choice -and ((Get-ItemPropertyValue $newConsent Value -EA SilentlyContinue) -ne $choice))) {{ \
             if (-not (Test-Path $newConsent)) {{ New-Item $newConsent -Force | Out-Null }}; \
             if (-not $choice) {{ $choice = 'Allow' }}; \
             Set-ItemProperty $newConsent Value $choice; \
           }}; \
           Write-Output ($p.PackageFamilyName + ' ' + $p.Status); \
           if (-not $old) {{ Write-Output 'created' }}; \
         }} catch {{ \
           if (-not $old) {{ \
             Get-AppxPackage -Name {NAME} | Remove-AppxPackage -ErrorAction SilentlyContinue; \
             if (-not $previousChoice) {{ Remove-Item $newConsent -Force -ErrorAction SilentlyContinue }}; \
           }}; \
           throw \
         }}",
        quote(&dir.join(MSIX).to_string_lossy()),
        quote(&dir.to_string_lossy()),
    );
    let status = powershell(&script)?;
    log::line(&format!("package: {status}"));
    Ok(status.lines().any(|line| line.trim() == "created"))
}

/// Plain development builds can still use the standalone notification helper package.
pub fn add_legacy(from: &Path, dir: &Path) -> Result<(), String> {
    let path = from.join(LEGACY_MSIX);
    if !path.is_file() {
        return Ok(());
    }
    let key = quote(&format!(r"{CONSENT}\{LEGACY_FAMILY}"));
    let script = format!(
        "$ErrorActionPreference='Stop'; Get-AppxPackage -Name {LEGACY_NAME} | Remove-AppxPackage; \
         Add-AppxPackage -Path {} -ExternalLocation {} -AllowUnsigned; \
         if (-not (Test-Path {key})) {{ New-Item {key} -Force | Out-Null; Set-ItemProperty {key} Value Allow }}",
        quote(&path.to_string_lossy()),
        quote(&dir.to_string_lossy()),
    );
    powershell(&script).map(|_| ())
}

fn remove_family(name: &str, family: &str) -> Result<(), String> {
    let key = quote(&format!(r"{CONSENT}\{family}"));
    let script = format!(
        "$ErrorActionPreference='Stop'; Get-AppxPackage -Name {name} | Remove-AppxPackage; \
         if (Test-Path {key}) {{ Remove-Item {key} -Force }}",
    );
    powershell(&script).map(|_| ())
}

pub fn remove_legacy() -> Result<(), String> {
    remove_family(LEGACY_NAME, LEGACY_FAMILY)
}

pub fn remove_current() -> Result<(), String> {
    remove_family(NAME, FAMILY)
}

pub fn remove() -> Result<(), String> {
    let current = remove_current();
    let legacy = remove_legacy();
    current.and(legacy)
}
