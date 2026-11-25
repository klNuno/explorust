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
use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Storage::FileSystem::{MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::Win32::UI::Shell::IsUserAnAdmin;
use windows::core::{HSTRING, PCWSTR};

/// The public half of the signing certificate (`tools/authenticode-sign.py keygen`).
const CERT: &[u8] = include_bytes!("../res/explorust-codesign.cer");
const STORES: [&str; 2] = ["Root", "TrustedPublisher"];
/// What runs from the install folder: the guard, the shell, the folder handler, the UWP probe
/// some host builds start beside explorust.exe, the toast listener's helper with its sparse
/// package (`package.rs`), the UI-access keyboard hook, the file index service and the default
/// handlers (image viewer, Task Manager for Win+X).
pub const GUARD_EXE: &str = "explorust-guard.exe";
pub const SUPERVISOR_EXE: &str = "explorust-supervisor.exe";
const FILES: [&str; 15] = [
    GUARD_EXE,
    SUPERVISOR_EXE,
    crate::switch_events::SWITCH_EXE,
    "explorust.exe",
    "explorust-open.exe",
    "nickel-windows-app-probe.exe",
    "explorust-notify.exe",
    crate::package::MSIX,
    crate::package::LEGACY_MSIX,
    "explorust-keys.exe",
    "explorust_inject.dll",
    INDEX,
    crate::folders::VIEWER_EXE,
    "explorust-taskman.exe",
    crate::apps::SETTINGS_EXE,
];

/// The MFT/USN index (`crates/index`), registered as the service `explorust-index`.
const INDEX: &str = "explorust-index.exe";
const SERVICE: &str = "explorust-index";

pub fn elevated() -> bool {
    // SAFETY: a plain token query.
    unsafe { IsUserAnAdmin() }.as_bool()
}

/// A Windows folder from the shell's known folders, never from environment variables: an elevated
/// install must not take a path the user's own programs can redirect.
pub fn known(id: &windows::core::GUID) -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    // SAFETY: the returned string is copied, then freed with CoTaskMemFree.
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let out = p.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(p.0 as *const _));
        out
    }
}

pub fn dir() -> PathBuf {
    use windows::Win32::UI::Shell::FOLDERID_ProgramFiles;
    known(&FOLDERID_ProgramFiles).unwrap_or_else(|| PathBuf::from(r"C:\Program Files")).join("explorust")
}

/// `%SystemRoot%\System32`, from the known folders.
pub fn system32() -> PathBuf {
    use windows::Win32::UI::Shell::FOLDERID_System;
    known(&FOLDERID_System).unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32"))
}

/// Copies the shell's files from `from` into the install folder; returns the installed guard.
/// Every file is first copied into a staging folder beside the live one and checked; only then are
/// the live files swapped one by one (a rename on the same volume), and a failed swap puts every
/// earlier file back, so a full disk or a broken payload leaves the installed version working.
pub fn copy_files(from: &Path, activate: impl FnOnce(&Path) -> Result<(), String>) -> Result<PathBuf, String> {
    let to = dir();
    let guard = to.join(GUARD_EXE);
    std::fs::create_dir_all(&to).map_err(|e| format!("create {}: {e}", to.display()))?;
    if same_dir(from, &to) {
        activate(&guard)?;
        return Ok(guard);
    }
    clean_leftovers(&to);
    let stage = to.join(format!(".new-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir(&stage).map_err(|e| format!("create {}: {e}", stage.display()))?;
    let staged = stage_files(from, &stage);
    let names = match staged {
        Ok(n) => n,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&stage);
            return Err(e);
        }
    };
    let mut swapped: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    for name in &names {
        let dst = to.join(name);
        let result = set_aside(&dst).and_then(|aside| match std::fs::rename(stage.join(name), &dst) {
            Ok(()) => Ok(aside),
            Err(e) => {
                if let Some(a) = &aside {
                    let _ = std::fs::rename(a, &dst);
                }
                Err(format!("swap in {name}: {e}"))
            }
        });
        match result {
            Ok(aside) => swapped.push((dst, aside)),
            Err(e) => {
                rollback_files(&swapped);
                let _ = std::fs::remove_dir_all(&stage);
                return Err(format!("{e}; the previous files are back"));
            }
        }
    }
    if let Err(error) = activate(&guard) {
        rollback_files(&swapped);
        let _ = std::fs::remove_dir_all(&stage);
        return Err(error);
    }
    for (_, aside) in swapped {
        if let Some(a) = aside {
            discard(&a);
        }
    }
    let _ = std::fs::remove_dir_all(&stage);
    log::line(&format!("install: {} files swapped in {}", names.len(), to.display()));
    Ok(guard)
}

fn rollback_files(swapped: &[(PathBuf, Option<PathBuf>)]) {
    for (live, previous) in swapped.iter().rev() {
        if std::fs::remove_file(live).is_err() && live.exists() {
            let abandoned = live.with_extension(format!("del{}-rollback", std::process::id()));
            if let Err(error) = std::fs::rename(live, &abandoned) {
                log::line(&format!("install: rollback could not move {}: {error}", live.display()));
                continue;
            }
            discard(&abandoned);
        }
        if let Some(previous) = previous
            && let Err(error) = std::fs::rename(previous, live)
        {
            log::line(&format!("install: rollback could not restore {}: {error}", live.display()));
        }
    }
}

/// Copies the payload into `stage` and checks each copy's size; returns the names copied.
fn stage_files(from: &Path, stage: &Path) -> Result<Vec<&'static str>, String> {
    let mut names = Vec::new();
    for name in FILES {
        let src = from.join(name);
        if !src.exists() {
            if matches!(name, "explorust.exe" | GUARD_EXE)
                || (option_env!("EXPLORUST_PACKAGE_BUILD").is_some()
                    && matches!(name, SUPERVISOR_EXE | "explorust-keys.exe" | "explorust-notify.exe" | crate::package::MSIX))
            {
                return Err(format!("{} missing", src.display()));
            }
            continue;
        }
        let dst = stage.join(name);
        let n = std::fs::copy(&src, &dst).map_err(|e| format!("copy {} to {}: {e}", src.display(), dst.display()))?;
        let want = std::fs::metadata(&src).map(|m| m.len()).unwrap_or(u64::MAX);
        if n != want {
            return Err(format!("copy of {name}: {n} bytes of {want}"));
        }
        names.push(name);
    }
    Ok(names)
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// A running exe cannot be overwritten but can be renamed: the live file moves to a unique
/// `<name>.old<pid>` kept until the new one is in place.
fn set_aside(path: &Path) -> Result<Option<PathBuf>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let aside = path.with_file_name(format!("{name}.old{}", std::process::id()));
    let _ = std::fs::remove_file(&aside);
    std::fs::rename(path, &aside).map_err(|e| format!("set aside {name}: {e}"))?;
    Ok(Some(aside))
}

/// Deletes an old copy now, or at the next boot when something still runs it.
fn discard(path: &Path) {
    if std::fs::remove_file(path).is_err() {
        delete_at_boot(path);
    }
}

/// Old copies and staging folders an earlier install could not delete at once.
fn clean_leftovers(to: &Path) {
    let Ok(entries) = std::fs::read_dir(to) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let path = e.path();
        if name.starts_with(".new-") {
            let _ = std::fs::remove_dir_all(&path);
        } else if !FILES.contains(&name.as_str()) && (name.contains(".old") || name.contains(".del")) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// A named mutex owned by this thread, released on drop.
pub struct Lock(HANDLE);

impl Drop for Lock {
    fn drop(&mut self) {
        // SAFETY: the mutex this process owns.
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}

pub fn lock() -> Result<Lock, String> {
    named_lock(r"Global\explorust-setup", 0, "another explorust install or uninstall is running")
}

pub fn user_setup_lock() -> Result<Lock, String> {
    named_lock(r"Local\explorust-user-setup", 0, "another explorust install or uninstall is running in this session")
}

pub fn switch_lock_for(timeout_ms: u32) -> Result<Lock, String> {
    named_lock(crate::switch_events::SWITCH_MUTEX, timeout_ms, "another switch is running in this session")
}

pub fn recovery_lock_for(timeout_ms: u32) -> Result<Lock, String> {
    try_recovery_lock_for(timeout_ms)?
        .ok_or_else(|| "the independent desktop recovery is still running".into())
}

pub fn try_recovery_lock_for(timeout_ms: u32) -> Result<Option<Lock>, String> {
    try_named_lock(crate::switch_events::RECOVERY_MUTEX, timeout_ms)
}

pub fn try_folder_close_lock(owner: u32, hwnd: isize) -> Result<Option<Lock>, String> {
    try_named_lock(&format!(r"Local\explorust-switch-folder-close-{owner}-{hwnd}"), 0)
}

fn named_lock(name: &str, timeout_ms: u32, busy: &str) -> Result<Lock, String> {
    try_named_lock(name, timeout_ms)?.ok_or_else(|| busy.into())
}

fn try_named_lock(name: &str, timeout_ms: u32) -> Result<Option<Lock>, String> {
    // SAFETY: a named mutex; the handle moves into the Lock.
    unsafe {
        let h = CreateMutexW(None, false, &HSTRING::from(name)).map_err(|e| format!("{name}: {e}"))?;
        match WaitForSingleObject(h, timeout_ms) {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Some(Lock(h))),
            result => {
                let error = (result != WAIT_TIMEOUT).then(|| GetLastError());
                let _ = CloseHandle(h);
                match error {
                    Some(error) => Err(format!("wait for {name}: {error:?}")),
                    None => Ok(None),
                }
            }
        }
    }
}

fn delete_at_boot(path: &Path) {
    let p = HSTRING::from(path.as_os_str());
    // SAFETY: a NULL target with this flag deletes the file at the next boot (admin only).
    let r = unsafe { MoveFileExW(&p, PCWSTR::null(), MOVEFILE_DELAY_UNTIL_REBOOT) };
    log::line(&format!("{} in use: deleted at next boot ({r:?})", path.display()));
}

/// Removes the install folder. A file still running is renamed to a unique name and that name is
/// deleted at the next boot; the live name itself is never scheduled, or a reinstall before the
/// reboot would lose its new file. Err lists what could be neither removed nor renamed.
pub fn remove_files() -> Result<String, String> {
    let to = dir();
    let entries = match std::fs::read_dir(&to) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(format!("{} absent", to.display())),
        Err(e) => return Err(format!("{}: {e}", to.display())),
    };
    let (mut gone, mut later, mut stuck) = (0, 0, Vec::new());
    for (i, e) in entries.flatten().enumerate() {
        let path = e.path();
        if path.is_dir() {
            if std::fs::remove_dir_all(&path).is_err() {
                stuck.push(path.display().to_string());
            }
            continue;
        }
        if std::fs::remove_file(&path).is_ok() {
            gone += 1;
            continue;
        }
        let unique = path.with_extension(format!("del{}-{i}", std::process::id()));
        if std::fs::rename(&path, &unique).is_ok() {
            delete_at_boot(&unique);
            later += 1;
        } else {
            stuck.push(path.display().to_string());
        }
    }
    if !stuck.is_empty() {
        return Err(format!("still in use: {}", stuck.join(", ")));
    }
    if later == 0 {
        let _ = std::fs::remove_dir(&to);
    } else {
        // Deleted at boot only if empty by then: a reinstall in between keeps it.
        delete_at_boot(&to);
    }
    Ok(format!("{} removed: {gone} files now, {later} at next boot", to.display()))
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
/// the install folder, which knows its own service; elevated only. Ok with its output, Err when it
/// failed or took over a minute.
pub fn index_service(install: bool) -> Result<String, String> {
    let exe = dir().join(INDEX);
    if !exe.exists() {
        if install {
            return Ok(format!("{} absent, no service", exe.display()));
        }
        // A service entry can outlive its exe (a partial install): remove it by name.
        let sc = system32().join("sc.exe");
        let _ = crate::proc::run_bounded(std::process::Command::new(&sc).args(["stop", SERVICE]), std::time::Duration::from_secs(30));
        return match crate::proc::run_bounded(std::process::Command::new(&sc).args(["delete", SERVICE]), std::time::Duration::from_secs(30)) {
            Ok((true, out, _)) => Ok(out),
            Ok((false, out, _)) if out.contains("1060") || out.contains("1072") => Ok(format!("service {SERVICE} absent")),
            Ok((false, out, err)) => Err(format!("sc delete {SERVICE}: {out} {err}")),
            Err(e) => Err(e),
        };
    }
    let arg = if install { "--install" } else { "--uninstall" };
    match crate::proc::run_bounded(std::process::Command::new(&exe).arg(arg), std::time::Duration::from_secs(60)) {
        Ok((true, out, _)) => Ok(out),
        Ok((false, out, err)) => Err(format!("{} {arg}: {out} {err}", exe.display()).trim().to_string()),
        Err(e) => Err(e),
    }
}
