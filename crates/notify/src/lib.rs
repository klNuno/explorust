//! explorust's toasts (step notify-1); the notification center and quick settings come next.
//!
//! Without explorer nobody starts ShellExperienceHost, which draws Windows' toasts, yet the
//! notification platform still accepts and stores every toast. This part reads them through the
//! documented `UserNotificationListener` (it answers an unpackaged process when the user allows
//! notification access) and draws each new one as a toast card above the taskbar's right side,
//! Windows 11's look, 5 s on screen (paused while hovered), with Windows' notification sound. A
//! click activates the sender, the close button dismisses it, Do Not Disturb and the per-app
//! banner settings are honoured. Research and evidence: `.agents/results/notify-1.md`.
//!
//! Contract with the shell (`crates/shell/src/components.rs`): `run()` is called on a thread of
//! its own, creates its windows (its command window has the class `bus::class_of(Owner::Notify)`),
//! runs its message loop and returns on `bus::Command::Shutdown` or `WM_QUIT`.

pub mod model;

#[cfg(target_os = "windows")]
mod log;
#[cfg(target_os = "windows")]
mod win;

/// Runs the component until shutdown.
pub fn run() {
    #[cfg(target_os = "windows")]
    win::run();
}
