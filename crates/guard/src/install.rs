//! The uiAccess install. explorust-keys.exe, the shell's keyboard hook, built with
//! `uiAccess="true"` (EXPLORUST_UIACCESS=1, signed by `tools/authenticode-sign.py`) sees keys over
//! elevated windows, but Windows only starts it from a secure folder and signed by a certificate the
//! machine trusts; explorust.exe stays a plain build and starts it from there. `--install` run elevated copies
//! the shell to `%ProgramFiles%\explorust` and trusts the certificate below in the machine's Root and
//! TrustedPublisher stores; `--restore` run elevated undoes both.

use crate::log;
use std::path::{Path, PathBuf};
use windows::Win32::Security::Cryptography::{
    CERT_CONTEXT, CERT_FIND_EXISTING, CERT_OPEN_STORE_FLAGS, CERT_STORE_ADD_REPLACE_EXISTING, CERT_STORE_PROV_SYSTEM_W,
    CERT_SYSTEM_STORE_LOCAL_MACHINE, CertAddEncodedCertificateToStore, CertCloseStore, CertCreateCertificateContext,
    CertDeleteCertificateFromStore, CertFindCertificateInStore, CertOpenStore, CertFreeCertificateContext, HCERTSTORE,
    PKCS_7_ASN_ENCODING, X509_ASN_ENCODING,
};
use windows::Win32::Storage::FileSystem::{MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW};
use windows::Win32::UI::Shell::IsUserAnAdmin;
use windows::core::{HSTRING, PCWSTR};

/// The public half of the signing certificate (`tools/authenticode-sign.py keygen`).
const CERT: &[u8] = include_bytes!("../res/explorust-codesign.cer");
const STORES: [&str; 2] = ["Root", "TrustedPublisher"];
/// What runs from the install folder: the guard, the shell, the folder handler, the UWP probe
/// some host builds start beside explorust.exe, the toast listener's helper with its sparse
/// package (`package.rs`), the UI-access keyboard hook, the file index service and the default
/// handlers (image viewer, Task Manager for Win+X).
const FILES: [&str; 11] = [
    "explorust-guard.exe",
    "explorust.exe",
    "explorust-open.exe",
    "nickel-windows-app-probe.exe",
    "explorust-notify.exe",
    crate::package::MSIX,
    "explorust-keys.exe",
    "explorust_inject.dll",
    INDEX,
    crate::folders::VIEWER_EXE,
    "explorust-taskman.exe",
];
/// The MFT/USN index (`crates/index`), registered as the service `explorust-index`.
const INDEX: &str = "explorust-index.exe";

pub fn elevated() -> bool {
    // SAFETY: a plain token query.
    unsafe { IsUserAnAdmin() }.as_bool()
}

pub fn dir() -> PathBuf {
    let base = std::env::var_os("ProgramW6432")
        .or_else(|| std::env::var_os("ProgramFiles"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
    base.join("explorust")
}

/// Copies the shell's files from `from` into the install folder; returns the installed guard.
pub fn copy_files(from: &Path) -> Result<PathBuf, String> {
    let to = dir();
    std::fs::create_dir_all(&to).map_err(|e| format!("create {}: {e}", to.display()))?;
    if same_dir(from, &to) {
        return Ok(to.join(FILES[0]));
    }
    for name in FILES {
        let src = from.join(name);
        if !src.exists() {
            if name == "explorust.exe" || name == "explorust-guard.exe" {
                return Err(format!("{} missing", src.display()));
            }
            continue;
        }
        let dst = to.join(name);
        set_aside(&dst);
        std::fs::copy(&src, &dst).map_err(|e| format!("copy {} to {}: {e}", src.display(), dst.display()))?;
        log::line(&format!("install: copied {name} to {}", to.display()));
    }
    Ok(to.join(FILES[0]))
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// A running exe cannot be overwritten but can be renamed: the old one moves to `<name>.old`,
/// deleted now when nothing runs it, else at the next boot.
fn set_aside(path: &Path) {
    if !path.exists() {
        return;
    }
    let old = path.with_extension("old");
    if old.exists() && std::fs::remove_file(&old).is_err() {
        delete_at_boot(&old);
        let unique = path.with_extension(format!("old{}", std::process::id()));
        if std::fs::rename(path, &unique).is_ok() {
            delete_at_boot(&unique);
        }
        return;
    }
    if std::fs::rename(path, &old).is_ok() && std::fs::remove_file(&old).is_err() {
        delete_at_boot(&old);
    }
}

fn delete_at_boot(path: &Path) {
    let p = HSTRING::from(path.as_os_str());
    // SAFETY: a NULL target with this flag deletes the file at the next boot (admin only).
    let r = unsafe { MoveFileExW(&p, PCWSTR::null(), MOVEFILE_DELAY_UNTIL_REBOOT) };
    log::line(&format!("{} in use: deleted at next boot ({r:?})", path.display()));
}

/// Removes the install folder. A file still running is renamed to a unique name first and that
/// name is deleted at the next boot, so a later `--install` into the same folder is not undone.
pub fn remove_files() -> String {
    let to = dir();
    let Ok(entries) = std::fs::read_dir(&to) else { return format!("{} absent", to.display()) };
    let (mut gone, mut later) = (0, 0);
    for (i, e) in entries.flatten().enumerate() {
        let path = e.path();
        if std::fs::remove_file(&path).is_ok() {
            gone += 1;
            continue;
        }
        let unique = path.with_extension(format!("del{}-{i}", std::process::id()));
        let doomed = if std::fs::rename(&path, &unique).is_ok() { unique } else { path };
        delete_at_boot(&doomed);
        later += 1;
    }
    if later == 0 {
        let _ = std::fs::remove_dir(&to);
    } else {
        delete_at_boot(&to);
    }
    format!("{} removed: {gone} files now, {later} at next boot", to.display())
}

fn open_store(name: &str) -> Result<HCERTSTORE, String> {
    let wide = HSTRING::from(name);
    // SAFETY: the store name outlives the call.
    unsafe {
        CertOpenStore(
            CERT_STORE_PROV_SYSTEM_W,
            X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
            None,
            CERT_OPEN_STORE_FLAGS(CERT_SYSTEM_STORE_LOCAL_MACHINE),
            Some(wide.as_ptr().cast()),
        )
    }
    .map_err(|e| format!("open LocalMachine\\{name}: {e}"))
}

/// Adds (`true`) or removes (`false`) the signing certificate in LocalMachine Root and
/// TrustedPublisher.
pub fn trust(add: bool) -> Result<String, String> {
    let mut done = Vec::new();
    for name in STORES {
        let store = open_store(name)?;
        // SAFETY: the store is open; contexts found here are freed or consumed by the delete.
        let r = unsafe {
            if add {
                CertAddEncodedCertificateToStore(Some(store), X509_ASN_ENCODING, CERT, CERT_STORE_ADD_REPLACE_EXISTING, None)
                    .map(|()| format!("{name}: added"))
                    .map_err(|e| format!("{name}: {e}"))
            } else {
                remove_from(store, name)
            }
        };
        // SAFETY: closes the store opened above.
        let _ = unsafe { CertCloseStore(Some(store), 0) };
        done.push(r?);
    }
    Ok(done.join(", "))
}

unsafe fn remove_from(store: HCERTSTORE, name: &str) -> Result<String, String> {
    // SAFETY: the caller keeps the store open; every context is freed or deleted.
    unsafe {
        let ours = CertCreateCertificateContext(X509_ASN_ENCODING, CERT);
        if ours.is_null() {
            return Err("the embedded certificate does not parse".into());
        }
        let mut removed = 0;
        loop {
            let found = CertFindCertificateInStore(
                store,
                X509_ASN_ENCODING,
                0,
                CERT_FIND_EXISTING,
                Some(ours as *const CERT_CONTEXT as *const _),
                None,
            );
            if found.is_null() {
                break;
            }
            CertDeleteCertificateFromStore(found).map_err(|e| format!("{name}: {e}"))?;
            removed += 1;
        }
        let _ = CertFreeCertificateContext(Some(ours));
        Ok(format!("{name}: {removed} removed"))
    }
}

/// True when LocalMachine Root holds the signing certificate.
pub fn trusted() -> bool {
    let Ok(store) = open_store("Root") else { return false };
    // SAFETY: the store is open; the contexts are freed.
    unsafe {
        let ours = CertCreateCertificateContext(X509_ASN_ENCODING, CERT);
        let found = if ours.is_null() {
            std::ptr::null_mut()
        } else {
            CertFindCertificateInStore(store, X509_ASN_ENCODING, 0, CERT_FIND_EXISTING, Some(ours as *const _), None)
        };
        let yes = !found.is_null();
        if yes {
            let _ = CertFreeCertificateContext(Some(found));
        }
        if !ours.is_null() {
            let _ = CertFreeCertificateContext(Some(ours));
        }
        let _ = CertCloseStore(Some(store), 0);
        yes
    }
}

/// Registers (`--install`) or removes (`--uninstall`) the index service through the index exe of
/// the install folder, which knows its own service; elevated only. Returns its output.
pub fn index_service(install: bool) -> String {
    let exe = dir().join(INDEX);
    if !exe.exists() {
        return format!("{} absent, no service", exe.display());
    }
    let arg = if install { "--install" } else { "--uninstall" };
    use std::os::windows::process::CommandExt;
    // CREATE_NO_WINDOW: the index exe is a console program.
    match std::process::Command::new(&exe).arg(arg).creation_flags(0x0800_0000).output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(e) => format!("{} {arg}: {e}", exe.display()),
    }
}
