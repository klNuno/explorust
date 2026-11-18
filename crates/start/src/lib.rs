//! explorust's Start: a resident window, cloaked until the Win key (bus `StartToggle`) or Win+S
//! (bus `Search`) shows it in one frame, with Windows 11's layout: search box, pinned grid with
//! pages, Recommended, the user and power buttons, and All apps. Apps come from
//! `shell:AppsFolder`, pins from `%LOCALAPPDATA%\explorust\start-pins.json`.
//!
//! Contract with the shell (`crates/shell/src/components.rs`): `run()` is called on a thread of
//! its own, creates its windows (its command window has the class `bus::class_of(Owner::Start)`),
//! runs its message loop and returns on `bus::Command::Shutdown` or `WM_QUIT`.

pub mod layout;
pub mod model;
pub mod strings;

#[cfg(target_os = "windows")]
mod apps;
#[cfg(target_os = "windows")]
mod log;
#[cfg(target_os = "windows")]
mod menu;
#[cfg(target_os = "windows")]
mod power;
#[cfg(target_os = "windows")]
mod recent;
#[cfg(target_os = "windows")]
mod scene;
#[cfg(target_os = "windows")]
mod window;

#[cfg(target_os = "windows")]
pub use power::set_dry_run as set_dry_power;

/// Runs Start until shutdown.
pub fn run() {
    #[cfg(target_os = "windows")]
    window::run();
}
