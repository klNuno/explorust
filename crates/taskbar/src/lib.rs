//! explorust's the taskbar: Shell_TrayWnd, task buttons, tray, clock, AppBars, Win+number, Win+X. Placeholder until step 3 of `.agents/TODO.md`.
//!
//! Contract with the shell (`crates/shell/src/components.rs`): `run()` is called on a thread of
//! its own, creates its windows (its command window has the class `bus::class_of(Owner::..)`),
//! runs its message loop and returns on `bus::Command::Shutdown` or `WM_QUIT`. A panic is caught
//! and logged by the shell, which starts `run()` again (at most 3 times in 5 minutes).

/// Runs the component until shutdown. The placeholder returns at once.
pub fn run() {}
