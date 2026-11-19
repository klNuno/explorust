//! Process-wide COM security, set the way explorer.exe sets it, before any other COM call.
//!
//! Without it COM's default access check refuses AppContainer callers, so the hosts twinui starts
//! for us inside an AppContainer (ShellExperienceHost: notification center, calendar) cannot call
//! back into the immersive shell in this process (E_ACCESSDENIED, event 5961). explorer.exe grants
//! COM_RIGHTS_EXECUTE | COM_RIGHTS_EXECUTE_LOCAL (0x3) to the principal self, AppContainers,
//! SYSTEM and Administrators.

use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::{ACL, MakeAbsoluteSD, PSECURITY_DESCRIPTOR, PSID};
use windows::Win32::System::Com::{
    COINIT_MULTITHREADED, CoInitializeEx, CoInitializeSecurity, EOAC_NONE, RPC_C_AUTHN_LEVEL_DEFAULT,
    RPC_C_IMP_LEVEL_IDENTIFY,
};
use windows::core::{BOOL, PCWSTR, w};

/// explorer.exe's string (26100 and 26300, read from its image).
const SDDL: PCWSTR = w!("O:BAG:BAD:(A;;0x3;;;PS)(A;;0x3;;;AC)(A;;0x3;;;SY)(A;;0x3;;;BA)");
const SDDL_REVISION_1: u32 = 1;

#[link(name = "advapi32")]
unsafe extern "system" {
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        string: PCWSTR,
        revision: u32,
        sd: *mut PSECURITY_DESCRIPTOR,
        size: *mut u32,
    ) -> BOOL;
}

/// Joins the MTA on the calling (main) thread and sets the process security. The MTA is kept for
/// the life of the process; the main thread never joins an STA (it pumps no messages).
pub fn init() -> Result<(), String> {
    // SAFETY: first COM call of the process, on the main thread.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok().map_err(|e| format!("CoInitializeEx: {e}"))?;
    let sd = absolute_sd()?;
    // SAFETY: `sd` is an absolute descriptor that lives for the whole process (leaked on purpose).
    unsafe {
        CoInitializeSecurity(
            Some(sd),
            -1,
            None,
            None,
            RPC_C_AUTHN_LEVEL_DEFAULT,
            RPC_C_IMP_LEVEL_IDENTIFY,
            None,
            EOAC_NONE,
            None,
        )
    }
    .map_err(|e| format!("CoInitializeSecurity: {e}"))
}

/// The SDDL as an absolute security descriptor, whose parts are leaked: COM keeps using it.
fn absolute_sd() -> Result<PSECURITY_DESCRIPTOR, String> {
    let mut rel = PSECURITY_DESCRIPTOR::default();
    // SAFETY: valid string, out pointers to locals.
    let ok = unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(SDDL, SDDL_REVISION_1, &mut rel, std::ptr::null_mut()) };
    if !ok.as_bool() {
        return Err(format!("SDDL: {}", windows::core::Error::from_thread()));
    }
    let (mut sd_len, mut dacl_len, mut sacl_len, mut owner_len, mut group_len) = (0u32, 0u32, 0u32, 0u32, 0u32);
    // SAFETY: the sizing call; it fails with ERROR_INSUFFICIENT_BUFFER and fills the sizes.
    let _ = unsafe {
        MakeAbsoluteSD(rel, None, &mut sd_len, None, &mut dacl_len, None, &mut sacl_len, None, &mut owner_len, None, &mut group_len)
    };
    // Buffers as u64 so the descriptor, ACL and SIDs are aligned.
    let buf = |len: u32| -> *mut std::ffi::c_void {
        let words = (len as usize).div_ceil(8).max(1);
        Box::leak(vec![0u64; words].into_boxed_slice()).as_mut_ptr().cast()
    };
    let (sd, dacl, sacl, owner, group) = (buf(sd_len), buf(dacl_len), buf(sacl_len), buf(owner_len), buf(group_len));
    // SAFETY: each buffer holds at least the size the sizing call asked for.
    let made = unsafe {
        MakeAbsoluteSD(
            rel,
            Some(PSECURITY_DESCRIPTOR(sd)),
            &mut sd_len,
            Some(dacl as *mut ACL),
            &mut dacl_len,
            Some(sacl as *mut ACL),
            &mut sacl_len,
            Some(PSID(owner)),
            &mut owner_len,
            Some(PSID(group)),
            &mut group_len,
        )
    };
    // SAFETY: `rel` came from LocalAlloc inside the conversion.
    unsafe { LocalFree(Some(HLOCAL(rel.0))) };
    made.map_err(|e| format!("MakeAbsoluteSD: {e}"))?;
    Ok(PSECURITY_DESCRIPTOR(sd))
}
