//! Windows Notification Facility (WNF) state names, through ntdll: the platform's change signal
//! for new toasts that an unpackaged process can wait on (`UserNotificationListener`'s own event
//! needs package identity). `RtlSubscribeWnfStateChangeNotification` calls back on ntdll's WNF
//! thread when the state changes, so waiting costs nothing.
//!
//! `scan` is a dev probe (`notify-dev --wnf-scan <file>`): it reads the change stamp of every
//! well-known state name (`HKLM\SYSTEM\CurrentControlSet\Control\Notifications`) three times and
//! writes the ones that moved, to find which names change when a toast is published.

use std::ffi::c_void;
use std::io::Write;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Registry::{HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RegCloseKey, RegEnumValueW, RegOpenKeyExW};
use windows_core::{s, w};

/// `WNF_SHEL_TOAST_PUBLISHED`, user scope (name from the public WNF name lists).
pub const SHEL_TOAST_PUBLISHED: u64 = 0x0d83063ea3bd0035;
/// `WNF_SHEL_NOTIFICATIONS`.
pub const SHEL_NOTIFICATIONS: u64 = 0x0d83063ea3bc1035;

type Query = unsafe extern "system" fn(*const u64, *const c_void, *const c_void, *mut u32, *mut c_void, *mut u32) -> i32;
pub type Callback = unsafe extern "system" fn(u64, u32, *const c_void, *mut c_void, *const c_void, u32) -> i32;
type Subscribe = unsafe extern "system" fn(*mut *mut c_void, u64, u32, Callback, *mut c_void, *const c_void, u32, u32) -> i32;

fn proc(name: windows_core::PCSTR) -> Option<unsafe extern "system" fn() -> isize> {
    // SAFETY: ntdll is mapped in every process; the name is a static C string.
    unsafe { GetProcAddress(GetModuleHandleW(w!("ntdll.dll")).ok()?, name) }
}

/// The change stamp of a state name and its data (up to 4 KB), or the NTSTATUS on failure.
pub fn query(name: u64) -> Result<(u32, Vec<u8>), i32> {
    let f = proc(s!("NtQueryWnfStateData")).ok_or(-1)?;
    // SAFETY: NtQueryWnfStateData has this signature since Windows 8.
    let f: Query = unsafe { std::mem::transmute(f) };
    let mut stamp = 0u32;
    let mut buf = vec![0u8; 4096];
    let mut size = buf.len() as u32;
    // SAFETY: all pointers are to locals of the given sizes.
    let st = unsafe { f(&name, std::ptr::null(), std::ptr::null(), &mut stamp, buf.as_mut_ptr() as _, &mut size) };
    if st < 0 {
        return Err(st);
    }
    buf.truncate(size as usize);
    Ok((stamp, buf))
}

/// Calls `cb(context)` on ntdll's WNF thread on every change of `name` after its current stamp.
/// The subscription lives as long as the process.
pub fn subscribe(name: u64, cb: Callback, context: *mut c_void) -> Result<(), i32> {
    let stamp = query(name).map(|(s, _)| s).unwrap_or(0);
    let f = proc(s!("RtlSubscribeWnfStateChangeNotification")).ok_or(-1)?;
    // SAFETY: RtlSubscribeWnfStateChangeNotification has this signature since Windows 8.
    let f: Subscribe = unsafe { std::mem::transmute(f) };
    let mut sub = std::ptr::null_mut();
    // SAFETY: the callback is a plain function and the context outlives the process' use of it.
    let st = unsafe { f(&mut sub, name, stamp, cb, context, std::ptr::null(), 0, 0) };
    if st < 0 { Err(st) } else { Ok(()) }
}

/// Every well-known state name listed in the registry.
fn well_known() -> Vec<u64> {
    let mut names = Vec::new();
    let mut key = HKEY::default();
    // SAFETY: opens a read-only key into a local handle, closed below.
    if unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, w!(r"SYSTEM\CurrentControlSet\Control\Notifications"), None, KEY_READ, &mut key) }.is_err() {
        return names;
    }
    for i in 0.. {
        let mut buf = [0u16; 64];
        let mut len = buf.len() as u32;
        // SAFETY: reads one value name into a local buffer of `len` characters.
        let r = unsafe { RegEnumValueW(key, i, Some(windows_core::PWSTR(buf.as_mut_ptr())), &mut len, None, None, None, None) };
        if r.is_err() {
            break;
        }
        if let Ok(v) = u64::from_str_radix(&String::from_utf16_lossy(&buf[..len as usize]), 16) {
            names.push(v);
        }
    }
    // SAFETY: closes the key opened above.
    unsafe { let _ = RegCloseKey(key); }
    names.extend([SHEL_TOAST_PUBLISHED, SHEL_NOTIFICATIONS]);
    names.sort_unstable();
    names.dedup();
    names
}

/// Dev probe: stamps at 0 s, `quiet` s and `quiet + after` s; writes every name that moved.
pub fn scan(out: &std::path::Path, quiet: u64, after: u64) {
    let names = well_known();
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let t0 = std::time::Instant::now();
    {
        let events = events.clone();
        let _ = std::thread::spawn(move || watch_dir(t0, &events));
    }
    let snap = |names: &[u64]| -> Vec<Option<u32>> { names.iter().map(|n| query(*n).ok().map(|(s, _)| s)).collect() };
    let a = snap(&names);
    std::thread::sleep(std::time::Duration::from_secs(quiet));
    let b = snap(&names);
    std::thread::sleep(std::time::Duration::from_secs(after));
    let c = snap(&names);
    let Ok(mut f) = std::fs::File::create(out) else { return };
    let readable = a.iter().filter(|s| s.is_some()).count();
    let _ = writeln!(f, "names {} readable {readable}", names.len());
    for n in &names {
        if let Err(st) = query(*n) {
            let _ = writeln!(f, "{n:016x} unreadable 0x{st:08x}");
        }
    }
    for e in events.lock().map(|v| v.clone()).unwrap_or_default() {
        let _ = writeln!(f, "dir {e}");
    }
    for (i, n) in names.iter().enumerate() {
        if a[i] != b[i] || b[i] != c[i] {
            let data = query(*n).map(|(_, d)| d.iter().take(48).map(|x| format!("{x:02x}")).collect::<String>()).unwrap_or_default();
            let tag = if a[i] == b[i] { "TOAST-ONLY" } else { "noise" };
            let _ = writeln!(f, "{n:016x} {tag} {:?} {:?} {:?} {data}", a[i], b[i], c[i]);
        }
    }
}

/// Dev probe: every change notification of the notification platform's database folder.
fn watch_dir(t0: std::time::Instant, events: &std::sync::Mutex<Vec<String>>) {
    use windows::Win32::Storage::FileSystem::*;
    let dir = std::env::var("LOCALAPPDATA").unwrap_or_default() + r"\Microsoft\Windows\Notifications";
    // SAFETY: opens the folder for change notifications; the handle lives as long as the probe.
    let h = unsafe {
        CreateFileW(
            &windows_core::HSTRING::from(dir.as_str()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    };
    let Ok(h) = h else {
        if let Ok(mut v) = events.lock() { v.push(format!("open failed {dir}")); }
        return;
    };
    let filter = FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME | FILE_NOTIFY_CHANGE_ATTRIBUTES | FILE_NOTIFY_CHANGE_SIZE
        | FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_LAST_ACCESS | FILE_NOTIFY_CHANGE_CREATION | FILE_NOTIFY_CHANGE_SECURITY;
    let mut buf = vec![0u32; 4096];
    loop {
        let mut got = 0u32;
        // SAFETY: a synchronous read into a local, dword-aligned buffer.
        let ok = unsafe { ReadDirectoryChangesW(h, buf.as_mut_ptr() as _, (buf.len() * 4) as u32, true, filter, Some(&mut got), None, None) };
        if ok.is_err() || got == 0 {
            if let Ok(mut v) = events.lock() { v.push(format!("{:.2} read failed or overflow", t0.elapsed().as_secs_f64())); }
            return;
        }
        let mut off = 0usize;
        loop {
            // SAFETY: walks the FILE_NOTIFY_INFORMATION records the call wrote.
            let rec = unsafe { &*((buf.as_ptr() as *const u8).add(off) as *const FILE_NOTIFY_INFORMATION) };
            let len = rec.FileNameLength as usize / 2;
            // SAFETY: the name has FileNameLength bytes after the record header.
            let name = unsafe { std::slice::from_raw_parts(rec.FileName.as_ptr(), len) };
            if let Ok(mut v) = events.lock() {
                v.push(format!("{:.2} action {} {}", t0.elapsed().as_secs_f64(), rec.Action.0, String::from_utf16_lossy(name)));
            }
            if rec.NextEntryOffset == 0 {
                break;
            }
            off += rec.NextEntryOffset as usize;
        }
    }
}
