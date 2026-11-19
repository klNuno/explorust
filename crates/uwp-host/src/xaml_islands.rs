//! explorust: whether the host process may run XAML islands.
//!
//! twinui components hosted here create XAML islands on demand. The first one seen is the
//! hardware confirmator (`Windows.Internal.HardwareConfirmator.dll`), which hides itself on a
//! virtual desktop switch through `WindowsXamlManager::InitializeForCurrentThread`. Windows.UI.Xaml
//! refuses islands with E_UNEXPECTED (`CQuirksMode2::QuirkBlockXamlIslands`) unless the process is
//! inbox or its manifest declares a `maxversiontested` of 10.0.18226 or later, and the
//! confirmator turns that error into a fail-fast (combase, 0xC000027B, 8000ffff): the shell dies.
//! The confirmator host is registered by twinui.dll's `CAudioFlyoutController::RegisterConfirmatorHost`
//! (the volume flyout), so skipping a component is no fix. The fix belongs in the exe's manifest;
//! this module logs the state at host start and answers `islands_allowed()`.

/// XAML islands need a tested version of at least 10.0.18226 (Windows 10 1903 previews).
pub(crate) const ISLANDS_MIN: u64 = (10 << 48) | (18226 << 16);

/// Highest `maxversiontested` of the process default activation context, packed as
/// major.minor.build.revision in 16-bit fields. None when the manifest has none.
#[cfg(target_os = "windows")]
pub(crate) fn max_version_tested() -> Result<Option<u64>, String> {
    use windows::Win32::{
        Foundation::HANDLE, System::ApplicationInstallationAndServicing::QueryActCtxW,
    };
    const COMPATIBILITY_INFORMATION: u32 = 6;
    const ELEMENT_MAXVERSIONTESTED: u32 = 3;
    // ElementCount (u32, padded to 8) then elements of { GUID, u32 type, u64 version } = 32 bytes.
    let mut buffer = [0u64; 1 + 4 * 32];
    let mut written = 0usize;
    // SAFETY: a null handle names the process default activation context; the buffer is writable
    // and its size is passed.
    unsafe {
        QueryActCtxW(
            0,
            HANDLE::default(),
            None,
            COMPATIBILITY_INFORMATION,
            Some(buffer.as_mut_ptr().cast()),
            std::mem::size_of_val(&buffer),
            Some(&mut written),
        )
    }
    .map_err(|error| format!("QueryActCtxW: {error}"))?;
    let count = (buffer[0] as u32 as usize).min(32);
    Ok((0..count)
        .filter(|i| buffer[1 + i * 4 + 2] as u32 == ELEMENT_MAXVERSIONTESTED)
        .map(|i| buffer[1 + i * 4 + 3])
        .max())
}

/// Whether this process may run XAML islands, so whether a virtual desktop switch is safe for
/// the immersive shell hosted here (false: twinui's audio flyout hides its hardware confirmator
/// through an island on the switch and fails fast). Callers such as the switcher gate desktop
/// writes on it.
#[cfg(target_os = "windows")]
pub fn islands_allowed() -> bool {
    matches!(max_version_tested(), Ok(Some(v)) if v >= ISLANDS_MIN)
}

pub(crate) fn format_version(v: u64) -> String {
    format!("{}.{}.{}.{}", v >> 48, (v >> 32) & 0xffff, (v >> 16) & 0xffff, v & 0xffff)
}

/// One log line: the declared version and whether islands are allowed by it.
#[cfg(target_os = "windows")]
pub(crate) fn log_state() {
    match max_version_tested() {
        Ok(Some(v)) if v >= ISLANDS_MIN => {
            hlog!("phase=xaml-islands maxversiontested={} result=allowed", format_version(v))
        }
        Ok(v) => hlog!(
            "phase=xaml-islands maxversiontested={} result=blocked: a twinui XAML component (the hardware confirmator on a virtual desktop switch) will fail fast; add <maxversiontested Id=\"10.0.26100.0\"/> to the exe manifest",
            v.map(format_version).unwrap_or_else(|| "none".into())
        ),
        Err(error) => hlog!("phase=xaml-islands error={error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_versions_like_the_activation_context() {
        let v = (10u64 << 48) | (26100 << 16);
        assert_eq!(format_version(v), "10.0.26100.0");
        assert!(v >= ISLANDS_MIN);
        assert!(((10u64 << 48) | (17763 << 16)) < ISLANDS_MIN);
    }
}
