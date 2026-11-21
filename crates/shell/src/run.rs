//! Argument parsing and the process lifetime: host thread up, optional self-test, ready signal,
//! then wait for a quit request or the host's end.

use crate::{actions, components, events, guardwatch, keyhost, log, minimized, problem, selftest};
use std::sync::mpsc;
use std::time::Duration;
use windows::Win32::UI::WindowsAndMessaging::GetShellWindow;

pub const CLEAN: i32 = 0;
pub const HOST_FAILED: i32 = 2;
pub const OTHER_SHELL: i32 = 3;
pub const SELF_TEST_FAILED: i32 = 4;
/// `--reload`: the guard swaps in `explorust.next.exe` if present and starts explorust again.
pub const RELOAD: i32 = 5;
/// A problem found inside (`problem`): the guard starts explorer.exe and tells the user.
pub const PROBLEM: i32 = 6;
/// How long the parts get to show their command windows before the shell says it is ready.
const PARTS_READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// How long the host gets to publish the shell window and start the controller. A first run on a
/// new Windows build downloads twinui's PDB (tens of MB) before it is ready.
const HOST_READY_TIMEOUT: Duration = Duration::from_secs(100);

struct Args {
    ready_event: Option<String>,
    guard: Option<u32>,
    self_test: bool,
    quit: bool,
    reload: bool,
}

fn parse() -> Result<Args, String> {
    let mut args = Args { ready_event: None, guard: None, self_test: false, quit: false, reload: false };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--ready-event" => args.ready_event = Some(it.next().ok_or("--ready-event needs a name")?),
            "--guard" => args.guard = it.next().and_then(|p| p.parse().ok()),
            "--self-test" => args.self_test = true,
            "--quit" => args.quit = true,
            "--reload" => args.reload = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(args)
}

pub fn main() -> i32 {
    let args = match parse() {
        Ok(a) => a,
        Err(e) => {
            log::line(&e);
            return HOST_FAILED;
        }
    };
    if args.quit || args.reload {
        let event = if args.quit { events::QUIT_EVENT } else { events::RELOAD_EVENT };
        return match events::set_existing(event) {
            Ok(()) => CLEAN,
            Err(e) => {
                log::line(&format!("--quit/--reload: no running explorust ({e})"));
                1
            }
        };
    }
    let code = shell(&args);
    log::line(&format!("exit {code}"));
    code
}

fn shell(args: &Args) -> i32 {
    log::line(&format!("start (self-test: {}, ready event: {:?})", args.self_test, args.ready_event));
    // SAFETY: a plain query of the session's shell window.
    if !unsafe { GetShellWindow() }.is_invalid() {
        log::line("another shell owns the shell window: exiting");
        return OTHER_SHELL;
    }
    components::log_panics();
    let made = events::Owned::create(events::QUIT_EVENT).and_then(|q| Ok((q, events::Owned::create(events::RELOAD_EVENT)?)));
    let (quit, reload) = match made {
        Ok(q) => q,
        Err(e) => {
            log::line(&e);
            return HOST_FAILED;
        }
    };
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let spawned = std::thread::Builder::new().name("uwp-host".into()).spawn(move || {
        let code = uwp_host::run_embedded_host(ready_tx, true);
        log::line(&format!("uwp host ended: {code:?}"));
        let _ = done_tx.send(());
    });
    if let Err(e) = spawned {
        log::line(&format!("uwp host thread: {e}"));
        return HOST_FAILED;
    }
    match ready_rx.recv_timeout(HOST_READY_TIMEOUT) {
        Ok(Ok(())) => log::line("uwp host ready"),
        Ok(Err(e)) => {
            log::line(&format!("uwp host failed: {e}"));
            return HOST_FAILED;
        }
        Err(_) => {
            log::line(&format!("uwp host not ready after {} s", HOST_READY_TIMEOUT.as_secs()));
            return HOST_FAILED;
        }
    }
    if args.self_test {
        match selftest::calculator() {
            Ok(ms) => log::line(&format!("self-test passed: Calculator frame shown in {ms} ms")),
            Err(e) => {
                log::line(&format!("self-test failed: {e}"));
                return SELF_TEST_FAILED;
            }
        }
    }
    // Dark menus and shell views for every part (uxtheme's app mode is process-wide).
    files::dark_mode_for_process();
    minimized::hide();
    actions::spawn();
    keyhost::start();
    components::spawn_all();
    // Ready only once every part is up: a part that cannot start is a problem now, not later.
    match components::wait_ready(PARTS_READY_TIMEOUT) {
        Ok(()) => components::spawn_watchdog(),
        Err(why) => problem::report(why),
    }
    if let Some(pid) = args.guard {
        guardwatch::spawn(pid);
    }
    if let Some(name) = args.ready_event.as_ref().filter(|_| !problem::pending()) {
        if let Err(e) = events::set_existing(name) {
            log::line(&format!("ready event: {e}"));
        }
    }
    loop {
        if quit.wait(250) {
            log::line("quit requested");
            keyhost::stop();
            components::shutdown_all();
            minimized::restore();
            return CLEAN;
        }
        if reload.wait(0) {
            log::line("reload requested");
            keyhost::stop();
            components::shutdown_all();
            return RELOAD;
        }
        if let Some(why) = problem::take() {
            log::line(&format!("stopping on a problem: {why}"));
            keyhost::stop();
            components::shutdown_all();
            minimized::restore();
            return PROBLEM;
        }
        // The host thread ended, or panicked (its sender dropped): the UWP frames are gone.
        if !matches!(done_rx.try_recv(), Err(mpsc::TryRecvError::Empty)) {
            log::line("uwp host thread ended: stopping");
            keyhost::stop();
            components::shutdown_all();
            minimized::restore();
            return HOST_FAILED;
        }
    }
}
