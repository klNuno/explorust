//! Settings' look from inside its XAML: a TAP (the XAML diagnostics extension point that Visual
//! Studio's Live Visual Tree uses) in `explorust_inject.dll`, loaded into SystemSettings.exe when
//! explorust calls `InitializeXamlDiagnosticsEx` on it ([`attach`]), only when
//! `settings-style.txt` exists. The TAP watches Settings' visual tree and applies the file's rules:
//! element backgrounds and the window's system backdrop ([`rules`]). Kill switch: a line
//! `settingsxaml` (or `inject`) in `parts-off.txt`.

pub mod rules;

#[cfg(target_os = "windows")]
mod abi;
#[cfg(target_os = "windows")]
mod backdrop;
#[cfg(target_os = "windows")]
pub mod attach;
#[cfg(target_os = "windows")]
pub mod report;
#[cfg(target_os = "windows")]
mod tap;
#[cfg(target_os = "windows")]
mod watch;

/// The TAP's class id (ours, random): passed to `InitializeXamlDiagnosticsEx`, answered by
/// `DllGetClassObject`.
#[cfg(target_os = "windows")]
pub const CLSID_TAP: windows_core::GUID = windows_core::GUID::from_u128(0xa915146c_ed50_48c0_ae58_4208386987fd);

/// The kill switch line of `parts-off.txt` for this part alone.
pub fn switched_off(parts_off: &str) -> bool {
    parts_off.lines().any(|l| l.trim().eq_ignore_ascii_case("settingsxaml"))
}
