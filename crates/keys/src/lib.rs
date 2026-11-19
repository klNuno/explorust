//! Windows' shell hotkeys, which explorer owns and which do nothing without it: Win alone, Win+S,
//! Win+Tab, Win+N, Win+X, Win+1..9 and the rest, plus Alt+Tab. A low-level keyboard hook on its own
//! thread turns them into bus commands. One code path, two hosts: explorust runs it in-process in a
//! dev build, and the installed shell runs it in `explorust-keys.exe`, built with UI access so keys
//! reach it over elevated windows. That exe starts nothing: Win+R, Win+I and the other shell
//! actions are posted to explorust (`Command::ShellAction`), so every app keeps a plain token.
//! Ctrl+Alt+Shift+F12, the guard's escape, is caught here too and sets the guard's fallback event.
//! The same host answers `Command::Activate` (window class `explorust.keys`): the parts ask it to
//! bring a window forward, which explorust alone cannot do over an elevated window.

pub mod chords;
#[cfg(target_os = "windows")]
mod activate;
#[cfg(target_os = "windows")]
mod hook;
#[cfg(target_os = "windows")]
pub use hook::spawn;
