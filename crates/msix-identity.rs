//! The identity shared by the signed session executables and notification listener.

pub const FAMILY: &str = "explorust.shell_39dr9n8mypr18";
pub const NOTIFY_AUMID: &str = "explorust.shell_39dr9n8mypr18!notify";

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentPackageFamilyName(length: *mut u32, family: *mut u16) -> i32;
}

pub fn require() -> Result<(), String> {
    // PACKAGE_FAMILY_NAME_MAX_LENGTH plus its terminating NUL.
    let mut family = [0u16; 65];
    let mut length = family.len() as u32;
    // SAFETY: length describes the writable buffer. Windows returns its actual length on success.
    let status = unsafe { GetCurrentPackageFamilyName(&mut length, family.as_mut_ptr()) };
    if status != 0 {
        return Err(format!("package identity unavailable (Win32 {status})"));
    }
    let actual = String::from_utf16_lossy(&family[..length.saturating_sub(1) as usize]);
    if actual != FAMILY {
        return Err(format!("unexpected package identity {actual}"));
    }
    Ok(())
}
