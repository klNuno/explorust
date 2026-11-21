//! Argument parsing and the process lifetime: host thread up, optional self-test, ready signal,
//! then wait for a quit request or the host's end.

use crate::{actions, components, events, guardwatch, keyhost, log, minimized, problem, selftest, startup};
use std::sync::mpsc;
use std::time::{Duration, Instant};
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
    let launched = Instant::now();
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
    if let Err(e) = uwp_host::configure_dpi_awareness() {
        log::line(&format!("process DPI: {e}"));
        return HOST_FAILED;
    }
    let (registered_tx, registered_rx) = mpsc::sync_channel(1);
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let spawned = std::thread::Builder::new().name("uwp-host".into()).spawn(move || {
        let code = uwp_host::run_shell_host(registered_tx, ready_tx);
        log::line(&format!("uwp host ended: {code:?}"));
        let _ = done_tx.send(());
    });
    if let Err(e) = spawned {
        log::line(&format!("uwp host thread: {e}"));
        return HOST_FAILED;
    }
    if registered_rx.recv_timeout(PARTS_READY_TIMEOUT).is_err() {
        log::line("uwp host did not register the shell window");
        return HOST_FAILED;
    }
    // Dark menus and shell views for every part (uxtheme's app mode is process-wide).
    files::dark_mode_for_process();
    minimized::hide();
    components::spawn_parts(false);
    actions::spawn();
    keyhost::start();
    startup::after_taskbar(launched, PARTS_READY_TIMEOUT, args.guard.is_some());
    let host_started = Instant::now();
    loop {
        if let Some(code) = requested_exit(&quit, &reload, 0) {
            return code;
        }
        match ready_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(())) => break,
            Ok(Err(e)) => {
                log::line(&format!("uwp host failed: {e}"));
                return shutdown(HOST_FAILED);
            }
            Err(mpsc::RecvTimeoutError::Timeout) if host_started.elapsed() < HOST_READY_TIMEOUT => {}
            Err(e) => {
                log::line(&format!("uwp host did not become ready: {e}"));
                return shutdown(HOST_FAILED);
            }
        }
    }
    log::line("uwp host ready");
    components::spawn_parts(true);
    if args.self_test {
        match selftest::calculator() {
            Ok(ms) => log::line(&format!("self-test passed: Calculator frame shown in {ms} ms")),
            Err(e) => {
                log::line(&format!("self-test failed: {e}"));
                return shutdown(SELF_TEST_FAILED);
            }
        }
    }
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
        if let Some(code) = requested_exit(&quit, &reload, 250) {
            return code;
        }
        // The host thread ended, or panicked (its sender dropped): the UWP frames are gone.
        if !matches!(done_rx.try_recv(), Err(mpsc::TryRecvError::Empty)) {
            log::line("uwp host thread ended: stopping");
            return shutdown(HOST_FAILED);
        }
    }
}

fn requested_exit(quit: &events::Owned, reload: &events::Owned, wait: u32) -> Option<i32> {
    let code = if quit.wait(wait) {
        log::line("quit requested");
        CLEAN
    } else if reload.wait(0) {
        log::line("reload requested");
        RELOAD
    } else if let Some(why) = problem::take() {
        log::line(&format!("stopping on a problem: {why}"));
        PROBLEM
    } else {
        return None;
    };
    Some(shutdown(code))
}

fn shutdown(code: i32) -> i32 {
    keyhost::stop();
    components::shutdown_all();
    if code != RELOAD {
        minimized::restore();
    }
    code
}
