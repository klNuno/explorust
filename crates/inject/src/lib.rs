//! explorust's injector: styles Settings and the file dialogs of other apps like the rest of the
//! shell (dark theme, Mica or acrylic from DWM), replacing what Windhawk did on the owner's PC.
//!
//! Design (least invasive that reaches medium-integrity apps): explorust watches window shows with
//! an out-of-context WinEvent hook (no code in any other process). For a target window it first
//! tries the DWM attributes from outside; only what needs the host's own thread (the dark theme of
//! a dialog's controls, or a frame DWM refuses from outside) gets the DLL, through a
//! `WH_GETMESSAGE` hook on that one thread: Windows loads `explorust_inject.dll` into that process
//! alone, the hook procedure styles the window on its own UI thread, and explorust removes the hook
//! when the window is destroyed or explorust exits, so the DLL leaves the host. Every entry point
//! catches panics. Kill switch: a line `inject` in `%LOCALAPPDATA%\explorust\parts-off.txt`.

pub mod target;

#[cfg(target_os = "windows")]
mod dark;
#[cfg(target_os = "windows")]
mod hook;
#[cfg(target_os = "windows")]
mod loader;
#[cfg(target_os = "windows")]
mod log;

#[cfg(target_os = "windows")]
pub use loader::{run, shutdown};

/// Name of the loader's message-only window; the DLL reports to it.
pub const WINDOW_CLASS: &str = "explorust.inject";
/// Registered message posted to a target window: wParam = `target::Kind`.
pub const MSG_STYLE: &str = "explorust.inject.style";
/// Registered message the DLL posts back: wParam = microseconds spent styling, lParam = host pid.
pub const MSG_DONE: &str = "explorust.inject.done";
/// The DLL's file name, next to explorust.exe.
pub const DLL_NAME: &str = "explorust_inject.dll";
/// The exported hook procedure.
pub const HOOK_EXPORT: &[u8] = b"explorust_inject_hook\0";

/// `%LOCALAPPDATA%\explorust\parts-off.txt` says `inject`.
pub fn switched_off() -> bool {
    let base = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    std::fs::read_to_string(base.join("explorust").join("parts-off.txt"))
        .map(|s| target::switched_off(&s))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    /// The DLL spells these names as wide literals; they must stay equal to the constants.
    #[test]
    fn names_match_the_dll_literals() {
        assert_eq!(super::MSG_STYLE, "explorust.inject.style");
        assert_eq!(super::MSG_DONE, "explorust.inject.done");
        assert_eq!(super::WINDOW_CLASS, "explorust.inject");
    }
}
