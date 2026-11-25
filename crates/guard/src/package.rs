//! Per-user registration and notification access for the signed sparse shell package.

use crate::log;
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS};
use windows::Win32::Storage::Packaging::Appx::GetPackagesByPackageFamily;
use windows::core::{HSTRING, PWSTR};

use crate::msix_identity::FAMILY;

pub const MSIX: &str = "explorust-shell.msix";
pub const LEGACY_MSIX: &str = "explorust-notify.msix";
const NAME: &str = "explorust.shell";
const LEGACY_NAME: &str = "explorust.notify";
const LEGACY_FAMILY: &str = "explorust.notify_m23m5a37cevzy";
const CONSENT: &str = r"HKCU:\Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\userNotificationListener";
const VERSION: &str = env!("EXPLORUST_MSIX_VERSION");

/// PowerShell single-quoted literal.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// The longest an Appx step may take; past it PowerShell is stopped and the step fails.
const LIMIT: Duration = Duration::from_secs(180);
const LOGON_LIMIT: Duration = Duration::from_secs(45);

fn powershell(script: &str, limit: Duration) -> Result<String, String> {
    let (ok, text, err) = crate::proc::run_bounded(
        Command::new(crate::install::system32().join(r"WindowsPowerShell\v1.0\powershell.exe")).args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &format!("$ProgressPreference='SilentlyContinue'; {script}"),
        ]),
        limit,
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

fn version(text: &str) -> Option<[u16; 4]> {
    let mut parts = text.split('.');
    let mut version = [0; 4];
    for component in &mut version {
        *component = parts.next()?.parse().ok()?;
    }
    parts.next().is_none().then_some(version)
}

/// Check the current user's registration without starting PowerShell on the normal logon path.
fn current_user_has_version() -> bool {
    let expected = version(VERSION).expect("shell package identity version");
    let family = HSTRING::from(FAMILY);
    let (mut count, mut size) = (0, 0);
    // SAFETY: the first call obtains the required count and buffer size without writing any names.
    let status = unsafe { GetPackagesByPackageFamily(&family, &mut count, None, &mut size, None) };
    if status != ERROR_INSUFFICIENT_BUFFER {
        return false;
    }
    let mut names = vec![PWSTR::null(); count as usize];
    let mut buffer = vec![0u16; size as usize];
    // SAFETY: both allocations match the lengths reported for this package family.
    let status = unsafe {
        GetPackagesByPackageFamily(&family, &mut count, Some(names.as_mut_ptr()), &mut size, Some(PWSTR(buffer.as_mut_ptr())))
    };
    status == ERROR_SUCCESS && names.iter().take(count as usize).any(|name| {
        // SAFETY: on success each returned name is NUL-terminated and points into buffer.
        unsafe { name.to_string() }.ok()
            .and_then(|name| name.split('_').nth(1).and_then(version))
            .is_some_and(|installed| installed >= expected)
    })
}

/// A shared update swaps the files for all accounts but only registers its caller.
pub fn repair_at_logon(dir: &Path) -> Result<(), String> {
    if current_user_has_version() {
        return Ok(());
    }
    let _user = crate::install::user_setup_lock()?;
    let _machine = crate::install::lock()?;
    log::line("package: registering the installed shell for this user at logon");
    register(dir, "", LOGON_LIMIT).map(|_| ())
}

/// Registers the package from the protected install directory before enabling the Winlogon shell.
/// A healthy registration at that same fixed location survives updates and keeps its consent.
pub fn add(dir: &Path) -> Result<bool, String> {
    register(dir, "Allow", LIMIT)
}

fn register(dir: &Path, default_consent: &str, limit: Duration) -> Result<bool, String> {
    let current = quote(&format!(r"{CONSENT}\{FAMILY}"));
    let legacy = quote(&format!(r"{CONSENT}\{LEGACY_FAMILY}"));
    let script = format!(
        "$ErrorActionPreference='Stop'; $newConsent={current}; \
         $requiredVersion = [version]'{VERSION}'; \
         $previousChoice = Get-ItemPropertyValue $newConsent Value -ErrorAction SilentlyContinue; \
         $choice = $previousChoice; \
         if (-not $choice) {{ $choice = Get-ItemPropertyValue {legacy} Value -ErrorAction SilentlyContinue }}; \
         $old = Get-AppxPackage -Name {NAME}; \
         if (-not $choice -and -not $old) {{ $choice = '{default_consent}' }}; \
         try {{ \
           if (-not $old -or $old.Status -ne 'Ok' -or [version]$old.Version -lt $requiredVersion) {{ \
             if ($old -and $old.Status -ne 'Ok') {{ $old | Remove-AppxPackage }}; \
             Add-AppxPackage -Path {} -ExternalLocation {}; \
           }}; \
           $p = Get-AppxPackage -Name {NAME}; \
           if ($p.PackageFamilyName -ne '{FAMILY}' -or $p.Status -ne 'Ok' -or [version]$p.Version -lt $requiredVersion) {{ throw 'Shell package is not ready' }}; \
           if ($choice -and (-not $old -or ((Get-ItemPropertyValue $newConsent Value -EA SilentlyContinue) -ne $choice))) {{ \
             if (-not (Test-Path $newConsent)) {{ New-Item $newConsent -Force | Out-Null }}; \
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
    let status = powershell(&script, limit)?;
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
    powershell(&script, LIMIT).map(|_| ())
}

fn remove_family(name: &str, family: &str) -> Result<(), String> {
    let key = quote(&format!(r"{CONSENT}\{family}"));
    let script = format!(
        "$ErrorActionPreference='Stop'; Get-AppxPackage -Name {name} | Remove-AppxPackage; \
         if (Test-Path {key}) {{ Remove-Item {key} -Force }}",
    );
    powershell(&script, LIMIT).map(|_| ())
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
