//! explorust.exe: the shell process the guard starts. Today it hosts the UWP shell controller
//! (`uwp-host`), so Settings and Store apps work with no explorer.exe; the taskbar, Start and the
//! rest join it step by step.
//!
//! - `--ready-event <name>`: set that named event once the host is up (and the self-test passed).
//! - `--guard <pid>`: the guard that started it; if it ends, explorust starts one that adopts it.
//! - `--self-test`: before signalling ready, open Calculator and wait for its frame on screen.
//! - `--prefetch-symbols`: populate this user's cache for the installed Windows build and exit.
//! - `--quit`: ask the running explorust of this session to exit cleanly (code 0).
//! - `--reload`: ask it to exit with code 5; the guard then swaps in `explorust.next.exe` beside it,
//!   if there is one, and starts explorust again (the guest's deploy loop).
//!
//! Keys: installed in Program Files, explorust starts `explorust-keys.exe` (the UI-access hook,
//! `keyhost`); a dev build hooks the keyboard in-process.
//!
//! Exit codes: 0 clean quit, 2 the UWP host failed, 3 another shell owns the session, 4 self-test
//! failed, 5 reload. The guard restarts explorust on any code but 0.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
mod actions;
#[cfg(target_os = "windows")]
mod components;
#[cfg(target_os = "windows")]
mod comsec;
#[cfg(target_os = "windows")]
mod events;
#[cfg(target_os = "windows")]
mod guardwatch;
#[cfg(target_os = "windows")]
mod keyhost;
#[cfg(target_os = "windows")]
mod log;
#[cfg(target_os = "windows")]
mod minimized;
mod problem;
#[cfg(target_os = "windows")]
mod run;
#[cfg(target_os = "windows")]
mod selftest;
#[cfg(target_os = "windows")]
mod startup;
#[cfg(any(target_os = "windows", test))]
mod startup_rules;

#[cfg(target_os = "windows")]
fn main() {
    if std::env::args().nth(1).as_deref() == Some("--prefetch-symbols") {
        std::process::exit(if uwp_host::prefetch() { 0 } else { 1 });
    }
    // Before any COM call of the process (AppContainer hosts call back into twinui here).
    match comsec::init() {
        Ok(()) => log::line("COM security set (explorer's SDDL, AppContainers allowed)"),
        Err(e) => log::line(&format!("COM security not set: {e}")),
    }
    std::process::exit(run::main());
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("explorust runs on Windows only");
    std::process::exit(1);
}
