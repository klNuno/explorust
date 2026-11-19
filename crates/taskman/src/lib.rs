//! explorust Task Manager: a small exe with processes grouped by app, a performance page with
//! graphs and the startup apps, sampled once a second from NtQuerySystemInformation and PDH.

pub mod model;
pub mod parse;
#[cfg(target_os = "windows")]
mod win;

#[cfg(target_os = "windows")]
pub use win::run;
