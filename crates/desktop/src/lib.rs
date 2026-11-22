//! explorust's desktop: a `Progman` window at the bottom of the Z-order that paints the wallpaper
//! from Windows' settings, Windows' own desktop shell view inside it (icons, context menu,
//! rename, drag and drop, change notifications), and the command window `explorust.desktop`
//! (ShowDesktop for Win+D and Win+M, Shutdown).
//!
//! Contract with the shell (`crates/shell/src/components.rs`): `run()` is called on a thread of
//! its own, creates its windows, runs its message loop and returns on `bus::Command::Shutdown` or
//! `WM_QUIT`. A panic is caught and logged by the shell, which starts `run()` again.

pub mod layout;
pub mod toggle;
pub mod transcoded;

#[cfg(any(target_os = "windows", test))]
mod click_activation;
#[cfg(any(target_os = "windows", test))]
mod view_layer;

#[cfg(target_os = "windows")]
mod browser;
#[cfg(target_os = "windows")]
mod command;
#[cfg(target_os = "windows")]
mod log;
#[cfg(target_os = "windows")]
mod progman;
#[cfg(target_os = "windows")]
mod reg;
#[cfg(target_os = "windows")]
mod screen;
#[cfg(target_os = "windows")]
mod showdesktop;
#[cfg(target_os = "windows")]
mod view;
#[cfg(target_os = "windows")]
mod wallpaper;
#[cfg(target_os = "windows")]
mod workerw;

/// Runs the desktop until shutdown.
pub fn run() {
    #[cfg(target_os = "windows")]
    progman::run();
}
