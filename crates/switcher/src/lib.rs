//! explorust's Alt+Tab. The hotkey hook of the shell sends `SwitcherNext` (lParam 1 with Shift),
//! `SwitcherCommit` on Alt release and `SwitcherCancel` on Esc to `explorust.switcher`. The first
//! Tab snapshots the Alt+Tab windows (taskbar-eligible, in Z-order, the foreground one first) and
//! selects the next one; when Alt is still held 100 ms later a topmost panel shows, centered like
//! Windows 11's: acrylic backdrop, one tile per window with its app icon, title and a live DWM
//! thumbnail, the selection ring gliding between tiles, hover and click to select, a close button.
//! Commit brings the selected window to the foreground (restoring it when minimized), Cancel
//! leaves the original foreground in place. Task View and virtual desktops come later.
//!
//! Contract with the shell (`crates/shell/src/components.rs`): `run()` is called on a thread of
//! its own, creates its windows (its command window has the class `bus::class_of(Owner::Switcher)`),
//! runs its message loop and returns on `bus::Command::Shutdown` or `WM_QUIT`.

pub mod layout;
pub mod model;

#[cfg(target_os = "windows")]
mod activate;
#[cfg(target_os = "windows")]
mod log;
#[cfg(target_os = "windows")]
mod scene;
#[cfg(target_os = "windows")]
mod window;
#[cfg(target_os = "windows")]
mod windows_list;

/// Runs the switcher until shutdown.
pub fn run() {
    #[cfg(target_os = "windows")]
    window::run();
}
