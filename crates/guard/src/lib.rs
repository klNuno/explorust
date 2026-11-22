//! Session, setup and recovery routines shared with the standalone switch command.

pub mod account;
mod apps;
mod folders;
mod hotkey;
pub mod install;
pub mod launch_timing;
pub mod log;
mod manage;
mod notice;
mod package;
pub mod policy;
mod prefetch;
pub mod proc;
pub mod reg;
#[path = "../../shell/src/reopen.rs"]
pub mod reopen;
mod setup;
mod setup_mode;
mod startup_retry;
mod supervise;
pub mod switch_events;
pub mod explorer_launch;
pub mod taskbar;
pub mod ui;
mod uninstall;

pub use manage::bring_back_explorer_except;
pub use supervise::explorer_path;
pub use command::run;
mod command;
