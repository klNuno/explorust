//! explorust's taskbar: `Shell_TrayWnd` on the primary monitor's bottom edge, with Start, task
//! buttons from live windows (grouped by AppUserModelID, pins first) and the clock. It reserves
//! the work area, answers the shell hook (minimize animations land on the buttons) and the bus
//! commands Win+1..9, Win+T and Shutdown.
//!
//! Contract with the shell (`crates/shell/src/components.rs`): `run()` is called on a thread of
//! its own, creates its windows (its command window has the class `bus::class_of(Owner::Taskbar)`),
//! runs its message loop and returns on `bus::Command::Shutdown` or `WM_QUIT`. A panic is caught
//! and logged by the shell, which starts `run()` again (at most 3 times in 5 minutes).

pub mod appbar;
pub mod geom;
pub mod jump;
pub mod model;
pub mod right;
pub mod settings;
pub mod taskband;
pub mod theme;
pub mod thumbs;
pub mod tray;
pub mod winx;

#[cfg(target_os = "windows")]
mod win;

/// Runs the taskbar until shutdown.
/// Power items of the Win+X menu only log the call they would make (the dev exe's default).
pub fn set_dry_power(dry: bool) {
    #[cfg(target_os = "windows")]
    win::set_dry_power(dry);
    #[cfg(not(target_os = "windows"))]
    let _ = dry;
}

pub fn run() {
    #[cfg(target_os = "windows")]
    win::run();
}
