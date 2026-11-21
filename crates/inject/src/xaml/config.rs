//! The live TAP reads its configuration from the same directory as the loader.

use super::rules::Rules;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::HANDLE;
use windows::core::{BOOL, HSTRING, PCWSTR};

pub fn directory() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("explorust")
}

fn read(path: &Path) -> io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn style_text(dir: &Path) -> io::Result<Option<String>> {
    let style = read(&dir.join("settings-style.txt"))?;
    if dir.join("settings-xaml-dump").try_exists()? {
        Ok(Some(format!("{}\n!dump\n", style.unwrap_or_default())))
    } else {
        Ok(style)
    }
}

pub fn live_rules(dir: &Path) -> io::Result<Rules> {
    let switches = read(&dir.join("parts-off.txt"))?.unwrap_or_default();
    if crate::target::switched_off(&switches) || super::switched_off(&switches) {
        return Ok(Rules::default());
    }
    Ok(Rules::parse(&style_text(dir)?.unwrap_or_default()))
}

// These three functions are in kernel32. Keep the binding local to this watch.
#[link(name = "kernel32")]
unsafe extern "system" {
    fn FindFirstChangeNotificationW(path: PCWSTR, subtree: BOOL, filter: u32) -> HANDLE;
    fn FindNextChangeNotification(notification: HANDLE) -> BOOL;
    fn FindCloseChangeNotification(notification: HANDLE) -> BOOL;
}

pub struct Changes(pub HANDLE);

impl Changes {
    pub fn new(dir: &Path) -> Option<Self> {
        const FILE_NAME: u32 = 0x01;
        const LAST_WRITE: u32 = 0x10;
        let path = HSTRING::from(dir.as_os_str());
        // SAFETY: the input is a live, null-terminated path. A successful handle belongs to us.
        let h = unsafe { FindFirstChangeNotificationW(PCWSTR(path.as_ptr()), false.into(), FILE_NAME | LAST_WRITE) };
        (!h.is_invalid()).then_some(Self(h))
    }

    pub fn rearm(&self) -> bool {
        // SAFETY: the notification handle is owned by self.
        unsafe { FindNextChangeNotification(self.0) }.as_bool()
    }
}

impl Drop for Changes {
    fn drop(&mut self) {
        // SAFETY: this handle was returned by FindFirstChangeNotificationW.
        let _ = unsafe { FindCloseChangeNotification(self.0) };
    }
}
