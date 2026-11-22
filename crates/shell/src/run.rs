//! Argument parsing and the process lifetime: host thread up, optional self-test, ready signal,
//! then wait for a quit request or the host's end.

use crate::startup_retry::SessionStartup;
use crate::{actions, components, events, guardwatch, keyhost, launch_timing, log, minimized, problem, selftest};
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
struct Args {
    ready_event: Option<String>,
    staged_ready: bool,
    guard: Option<u32>,
    self_test: bool,
    quit: bool,
    reload: bool,
}

fn parse() -> Result<Args, String> {
    let mut args = Args { ready_event: None, staged_ready: false, guard: None, self_test: false, quit: false, reload: false };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--ready-event" => args.ready_event = Some(it.next().ok_or("--ready-event needs a name")?),
            "--staged-ready" => args.staged_ready = true,
            "--guard" => args.guard = it.next().and_then(|p| p.parse().ok()),
            "--self-test" => args.self_test = true,
            "--quit" => args.quit = true,
            "--reload" => args.reload = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(args)
}

struct PhaseEvents {
    native: events::Owned,
    immersive: events::Owned,
    verified: events::Owned,
}

impl PhaseEvents {
    fn open(name: &str) -> Result<Self, String> {
        // Retain the handles so a replacement guard can observe the same phase after adoption.
        Ok(Self {
            native: events::Owned::create(&launch_timing::native_event(name))?,
            immersive: events::Owned::create(&launch_timing::immersive_event(name))?,
            verified: events::Owned::create(name)?,
        })
    }
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
    let exits = ExitRequests { quit, reload };
    let offered = args.ready_event.as_deref()
        .and_then(|name| events::Owned::open_for_signal(&launch_timing::staged_event(name)));
    let negotiated = offered.is_some();
    let staged = args.ready_event.as_deref().filter(|_| args.staged_ready || negotiated);
    let phases = match staged.map(PhaseEvents::open).transpose() {
        Ok(phases) => phases,
        Err(e) => {
            log::line(&format!("readiness events: {e}"));
            return HOST_FAILED;
        }
    };
    if let Some(event) = offered && let Err(e) = event.signal() {
        log::line(&format!("staged readiness negotiation: {e}"));
        return HOST_FAILED;
    }
    if let Some(pid) = args.guard {
        guardwatch::spawn(pid, staged.map(str::to_string));
    }
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
    if let Err(code) = exits.wait_reply(&registered_rx, launch_timing::REGISTRATION, HOST_FAILED, "shell window registration") {
        return code;
    }
    // Dark menus and shell views for every part (uxtheme's app mode is process-wide).
    files::dark_mode_for_process();
    minimized::hide();
    components::spawn_parts(false);
    actions::spawn();
    keyhost::start();
    if let Err(code) = exits.wait_parts(false) {
        return code;
    }
    log::line(&format!("native desktop ready in {} ms", launched.elapsed().as_millis()));
    components::spawn_watchdog(false);
    if let Some(phases) = &phases {
        signal(&phases.native, "native ready");
    }
    let mut startup = SessionStartup::default();
    if args.guard.is_some() && !negotiated {
        // Earlier guards either leave startup to the shell or launch a single unobserved runner.
        match std::env::current_exe() {
            Ok(exe) => startup.desktop_ready(&exe),
            Err(e) => log::line(&format!("session startup claim unacknowledged: {e}. Will retry on reload.")),
        }
    }
    let legacy_ready_sent = launch_timing::legacy_ready_at_native(args.guard.is_some(), phases.is_some(), args.self_test)
        && signal_legacy_ready(args.ready_event.as_deref());
    match exits.wait_reply(&ready_rx, launch_timing::HOST, HOST_FAILED, "immersive host startup") {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            log::line(&format!("uwp host failed: {e}"));
            return shutdown(HOST_FAILED);
        }
        Err(code) => return code,
    }
    log::line("uwp host ready");
    components::spawn_parts(true);
    if let Err(code) = exits.wait_parts(true) {
        return code;
    }
    components::spawn_watchdog(true);
    if let Some(phases) = &phases {
        signal(&phases.immersive, "immersive ready");
    }
    if args.self_test {
        let (tx, rx) = mpsc::sync_channel(1);
        if let Err(e) = std::thread::Builder::new().name("self-test".into()).spawn(move || {
            let _ = tx.send(selftest::calculator());
        }) {
            log::line(&format!("self-test thread: {e}"));
            return shutdown(SELF_TEST_FAILED);
        }
        match exits.wait_reply(&rx, launch_timing::SELF_TEST, SELF_TEST_FAILED, "self-test") {
            Ok(Ok(ms)) => log::line(&format!("self-test passed: Calculator frame shown in {ms} ms")),
            Ok(Err(e)) => {
                log::line(&format!("self-test failed: {e}"));
                return shutdown(SELF_TEST_FAILED);
            }
            Err(code) => return code,
        }
    }
    if let Some(code) = exits.requested(0) {
        return code;
    }
    if let Some(phases) = &phases {
        signal(&phases.verified, "build verdict");
    } else if !legacy_ready_sent {
        signal_legacy_ready(args.ready_event.as_deref());
    }
    loop {
        if let Some(code) = exits.requested(250) {
            return code;
        }
        // The host thread ended, or panicked (its sender dropped): the UWP frames are gone.
        if !matches!(done_rx.try_recv(), Err(mpsc::TryRecvError::Empty)) {
            log::line("uwp host thread ended: stopping");
            return shutdown(HOST_FAILED);
        }
    }
}

fn signal_legacy_ready(name: Option<&str>) -> bool {
    let Some(name) = name else { return false };
    match events::set_existing(name) {
        Ok(()) => true,
        Err(e) => {
            log::line(&format!("ready event: {e}"));
            false
        }
    }
}

fn signal(event: &events::Owned, phase: &str) {
    if let Err(e) = event.signal() {
        log::line(&format!("{phase} event: {e}"));
    }
}

struct ExitRequests {
    quit: events::Owned,
    reload: events::Owned,
}

impl ExitRequests {
    fn wait_parts(&self, immersive: bool) -> Result<(), i32> {
        let started = Instant::now();
        let parts = components::enabled(immersive);
        loop {
            if let Some(code) = self.requested(0) {
                return Err(code);
            }
            let missing: Vec<_> = parts.iter().map(|(_, owner)| *owner)
                .filter(|&owner| !components::ready(owner)).map(bus::Owner::name).collect();
            if missing.is_empty() {
                return Ok(());
            }
            if started.elapsed() >= launch_timing::PARTS {
                problem::report(format!("the {} part did not answer within {} s", missing.join(", "), launch_timing::PARTS.as_secs()));
                return Err(self.requested(0).unwrap_or_else(|| shutdown(PROBLEM)));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn wait_reply<T>(&self, receiver: &mpsc::Receiver<T>, limit: Duration, failure: i32, phase: &str) -> Result<T, i32> {
        let started = Instant::now();
        loop {
            if let Some(code) = self.requested(0) {
                return Err(code);
            }
            let remaining = limit.saturating_sub(started.elapsed());
            match receiver.recv_timeout(remaining.min(Duration::from_millis(100))) {
                Ok(value) => return Ok(value),
                Err(mpsc::RecvTimeoutError::Timeout) if started.elapsed() < limit => {}
                Err(e) => {
                    log::line(&format!("{phase} did not finish within {} s: {e}", limit.as_secs()));
                    return Err(shutdown(failure));
                }
            }
        }
    }

    fn requested(&self, wait: u32) -> Option<i32> {
        let code = if self.quit.wait(wait) {
            log::line("quit requested");
            CLEAN
        } else if self.reload.wait(0) {
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
}

fn shutdown(code: i32) -> i32 {
    keyhost::stop();
    components::shutdown_all();
    if code != RELOAD {
        minimized::restore();
    }
    code
}
