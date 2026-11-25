//! The logon loop: start explorust, wait for its ready signal, watch it, and at the first problem
//! start explorer.exe for the rest of the session and tell the user once (`notice`). Also falls
//! back on the hotkey, on Shift held at logon or when explorust.exe is missing, without a word.

use crate::notice::{self, Notice};
use crate::launch_timing;
use crate::policy::{AtLogon, MAX_STREAK, PROBE, Watchdog, at_logon};
use crate::proc::{Child, Event, wait_any};
use crate::startup_retry::SessionStartup;
use crate::taskbar::{self, Start};
use crate::{folders, hotkey, log, reg};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub use launch_timing::FALLBACK_EVENT;
use crate::switch_events::{CANCEL_FOR_RESTORE, EXITING_EXPLORER, RELEASE_EXPLORER, WATCHES_EXPLORER_HANDOFF};

enum End {
    /// The switch began rollback before the supervisor could launch the shell.
    HandoffAborted,
    /// Something went wrong: explorer takes over and the user is told.
    Problem(String),
    /// Asked for (hotkey, `--fallback`, `--restore`, a clean exit): explorer, no message.
    Fallback(String),
    /// explorust exited with code 5 (`explorust --reload`).
    Reload,
    /// All render targets in this process need a fresh graphics device.
    GraphicsLost,
}

/// The exit code of `explorust --reload`.
const RELOAD: u32 = 5;
/// explorust's exit code when it found a problem itself (a part that crashed twice or hangs, the
/// keyboard helper dying): its reason is in `problem.txt`.
const PROBLEM: u32 = 6;

/// `adopt`: the pid of a running explorust whose guard died; it is watched instead of starting one.
pub fn run(adopt: Option<(u32, Option<String>)>, mut rollback: Option<Event>, explorer_ready: Option<&Event>) -> u32 {
    let live = rollback.is_some();
    let explorer = |notice| explorer_forever(notice, explorer_ready);
    let fallback = match Event::named(FALLBACK_EVENT) {
        Ok(e) => e,
        Err(e) if live => {
            log::line(&format!("guard: live handoff cannot start without the fallback event: {e}"));
            return 1;
        }
        Err(e) => {
            log::line(&format!("guard: {e}; starting explorer"));
            return explorer(None);
        }
    };
    // Create the fallback event before checking rollback. A watcher starting recovery can
    // either prevent this start or signal fallback through the event held by this guard.
    if rollback.as_ref().is_some_and(Event::signalled) {
        return 1;
    }
    hotkey::spawn(fallback.0);
    let exe = shell_exe();
    let build = reg::windows_build();
    let mut attempt = 0u32;
    let mut adopted = None;
    let mut startup = SessionStartup::default();
    let mut graphics_restarted_at = None;
    if let Some((pid, name)) = adopt {
        log::line(&format!("guard adopts explorust pid {pid}: its guard ended"));
        match Child::open(pid) {
            Ok(child) => adopted = Some((child, name)),
            Err(e) => log::line(&format!("adopt: {e}; starting a new explorust")),
        }
    } else {
        log::line(&format!("guard start: windows {build}, shell {}", exe.display()));
        if !live && hotkey::shift_held() {
            log::line("Shift held at logon: plain explorer");
            return explorer(None);
        }
        if !live {
            // A logon counts by its outcome: only one that ended without a problem clears the streak.
            if !reg::last_logon_had_problem() && reg::problem_streak() != 0 {
                log::line("the last logon ended without a problem: problem streak reset");
                reg::set_problem_streak(0);
            }
            reg::set_logon_had_problem(false);
            let streak = reg::problem_streak();
            if at_logon(streak) == AtLogon::TurnOff {
                return explorer(Some(turn_off(&format!("explorust ran into a problem at {streak} sign-ins in a row"))));
            }
        }
        if !exe.exists() {
            let why = format!("{} is missing", exe.display());
            return explorer(Some(if live { Notice::Problem(why) } else { turn_off(&why) }));
        }
    }
    loop {
        let self_test = reg::last_good_build().as_deref() != Some(build.as_str());
        let end = match adopted.take() {
            Some((child, Some(name))) => match PhaseEvents::open(&name) {
                Ok(phases) => supervise(&child, &fallback, &phases, self_test, &build, &exe, &mut startup),
                Err(e) => {
                    child.kill();
                    End::Problem(e)
                }
            },
            Some((child, None)) => watch(&child, &fallback, TaskbarWatch::new()),
            None => {
                attempt += 1;
                run_once(&exe, attempt, self_test, &build, &fallback, &mut startup, rollback.take())
            }
        };
        let why = match end {
            End::HandoffAborted => return 1,
            End::Reload => {
                swap_next(&exe);
                continue;
            }
            End::Fallback(why) => {
                let _ = fallback.signal();
                log::line(&format!("fallback: {why}"));
                return explorer(None);
            }
            End::GraphicsLost => {
                let now = Instant::now();
                let recovery = launch_timing::NATIVE_BUDGET + launch_timing::IMMERSIVE_BUDGET;
                if graphics_restarted_at.is_none_or(|then| now.duration_since(then) >= recovery) {
                    graphics_restarted_at = Some(now);
                    log::line("graphics device lost: restarting explorust");
                    continue;
                }
                "graphics device lost again during shell recovery".into()
            }
            End::Problem(why) => why,
        };
        let _ = fallback.signal();
        if live {
            log::line(&format!("problem after a live switch: {why}; plain explorer for this session"));
            return explorer(Some(Notice::Problem(why)));
        }
        let streak = reg::problem_streak() + 1;
        reg::set_problem_streak(streak);
        reg::set_logon_had_problem(true);
        log::line(&format!("problem: {why}; plain explorer for this session (problem logons in a row: {streak})"));
        if streak >= MAX_STREAK {
            return explorer(Some(turn_off(&format!("{why} (a problem at {streak} sign-ins in a row)"))));
        }
        return explorer(Some(Notice::Problem(why)));
    }
}

/// Deletes the Shell value so the next logon is plain Windows. The notice says what happened: off
/// for good, or, when the value could not be deleted, a problem whose streak stays so the next
/// logon tries again.
fn turn_off(why: &str) -> Notice {
    match reg::delete_shell() {
        Ok(_) => {
            reg::set_problem_streak(0);
            reg::set_logon_had_problem(false);
            log::line(&format!("explorust turned off: {why}; the Shell value is deleted"));
            Notice::TurnedOff(why.to_string())
        }
        Err(e) => {
            // Latched: the next logon must not clear the streak, so it tries to turn off again.
            reg::set_logon_had_problem(true);
            log::line(&format!("explorust should turn off ({why}) but the Shell value stays: {e}"));
            Notice::Problem(format!("{why}; turning explorust off failed: {e}"))
        }
    }
}

struct PhaseEvents {
    staged: Event,
    native: Event,
    immersive: Event,
    verified: Event,
}

impl PhaseEvents {
    fn open(name: &str) -> Result<Self, String> {
        Ok(Self {
            staged: Event::named(&launch_timing::staged_event(name))?,
            native: Event::named(&launch_timing::native_event(name))?,
            immersive: Event::named(&launch_timing::immersive_event(name))?,
            verified: Event::named(name)?,
        })
    }
}

fn run_once(exe: &Path, attempt: u32, self_test: bool, build: &str, fallback: &Event, startup: &mut SessionStartup, rollback: Option<Event>) -> End {
    let name = format!(r"Local\explorust-ready-{}-{attempt}", std::process::id());
    let phases = match PhaseEvents::open(&name) {
        Ok(phases) => phases,
        Err(e) => return End::Fallback(e),
    };
    let explicit_staged = match std::fs::File::open(exe).and_then(launch_timing::needs_staged_flag) {
        Ok(supported) => supported,
        Err(e) => {
            log::line(&format!("could not check staged readiness support: {e}"));
            false
        }
    };
    // The launch still offers the named event. Only explicitly compatible binaries
    // get the older command-line flag, which pre-stage shells reject.
    let mut args = format!("--ready-event {name} --guard {}", std::process::id());
    if explicit_staged {
        args.push(' ');
        args.push_str(launch_timing::STAGED_FLAG);
    }
    if self_test {
        args.push_str(" --self-test");
    }
    let _ = std::fs::remove_file(problem_file());
    if rollback.is_some_and(|event| {
        event.signalled() || Event::is_signalled(CANCEL_FOR_RESTORE) || taskbar::explorer_window().is_some()
    }) {
        log::line("guard: live handoff rolled back before shell launch");
        return End::HandoffAborted;
    }
    let child = match Child::spawn(exe, &args) {
        Ok(c) => c,
        Err(e) => return End::Problem(format!("explorust did not start: {e}")),
    };
    log::line(&format!("started explorust pid {} ({args})", child.pid));
    if explicit_staged {
        return supervise(&child, fallback, &phases, self_test, build, exe, startup);
    }
    let legacy = launch_timing::legacy_budget(self_test);
    match wait_any(&[fallback.0, child.handle, phases.staged.0, phases.verified.0], Some(legacy.as_millis() as u32)) {
        Some(0) => {
            child.kill();
            End::Fallback("hotkey or --fallback during readiness negotiation".into())
        }
        Some(1) => ended(&child, Some("readiness negotiation")),
        Some(2) => supervise(&child, fallback, &phases, self_test, build, exe, startup),
        Some(_) => {
            log::line("explorust ready (legacy protocol)");
            record_ready(self_test, build);
            watch(&child, fallback, TaskbarWatch::new())
        }
        None => {
            let _ = fallback.signal();
            child.kill();
            End::Problem(format!("explorust did not negotiate or signal readiness after {} s", legacy.as_secs()))
        }
    }
}

fn supervise(child: &Child, fallback: &Event, phases: &PhaseEvents, self_test: bool, build: &str, exe: &Path, startup: &mut SessionStartup) -> End {
    if let Err(end) = wait_phase(child, fallback, &phases.native, "native desktop", launch_timing::NATIVE_BUDGET, None) {
        return end;
    }
    log::line("explorust native desktop ready");
    startup.desktop_ready(exe);
    let mut taskbar = TaskbarWatch::new();
    for (event, label, limit) in [
        (&phases.immersive, "immersive host", launch_timing::IMMERSIVE_BUDGET),
        (&phases.verified, "build verdict", launch_timing::VERDICT_BUDGET),
    ] {
        if let Err(end) = wait_phase(child, fallback, event, label, limit, Some(&mut taskbar)) {
            return end;
        }
        log::line(&format!("explorust {label} ready"));
    }
    record_ready(self_test, build);
    watch(child, fallback, taskbar)
}

fn record_ready(self_test: bool, build: &str) {
    folders::handler(true);
    if self_test && let Err(e) = reg::set_last_good_build(build) {
        log::line(&format!("could not record the good build: {e}"));
    }
}

fn wait_phase(child: &Child, fallback: &Event, phase: &Event, label: &str, budget: Duration, mut taskbar: Option<&mut TaskbarWatch>) -> Result<(), End> {
    let start = Instant::now();
    loop {
        let remaining = budget.saturating_sub(start.elapsed());
        let wait = if taskbar.is_some() { remaining.min(PROBE) } else { remaining };
        match wait_any(&[fallback.0, child.handle, phase.0], Some(wait.as_millis() as u32)) {
            Some(0) => {
                child.kill();
                return Err(End::Fallback(format!("hotkey or --fallback during {label}")));
            }
            Some(1) => return Err(ended(child, Some(label))),
            Some(_) => return Ok(()),
            None if start.elapsed() >= budget => {
                let _ = fallback.signal();
                child.kill();
                return Err(End::Problem(format!("explorust {label} was not ready after {} s", budget.as_secs())));
            }
            None => {
                if let Some(end) = taskbar.as_deref_mut().and_then(|watch| watch.probe(child, fallback)) {
                    return Err(end);
                }
            }
        }
    }
}

struct TaskbarWatch {
    started: Instant,
    dog: Option<Watchdog>,
}

impl TaskbarWatch {
    fn new() -> Self {
        let enabled = !parts_off().iter().any(|p| p == "taskbar");
        Self { started: Instant::now(), dog: enabled.then(Watchdog::default) }
    }

    fn probe(&mut self, child: &Child, fallback: &Event) -> Option<End> {
        let at = self.started.elapsed();
        let why = self.dog.as_mut()?.probe(taskbar::probe(child.pid), at)?;
        let _ = fallback.signal();
        child.kill();
        Some(End::Problem(why))
    }
}

fn ended(child: &Child, starting: Option<&str>) -> End {
    let code = child.exit_code();
    match code {
        0 if starting.is_none() => End::Fallback("explorust exited cleanly".into()),
        RELOAD => End::Reload,
        launch_timing::GRAPHICS_LOST => End::GraphicsLost,
        _ => {
            let reason = problem_reason(code).unwrap_or_else(|| format!("exit code {code:#x}"));
            match starting {
                Some(phase) => End::Problem(format!("explorust stopped during {phase}: {reason}")),
                None => End::Problem(reason),
            }
        }
    }
}

/// Watches a ready explorust until it ends, its taskbar stops answering, or the escape hatch.
fn watch(child: &Child, fallback: &Event, mut taskbar: TaskbarWatch) -> End {
    loop {
        match wait_any(&[fallback.0, child.handle], Some(PROBE.as_millis() as u32)) {
            Some(0) => {
                child.kill();
                return End::Fallback("hotkey or --fallback".into());
            }
            Some(_) => return ended(child, None),
            None => {
                if let Some(end) = taskbar.probe(child, fallback) {
                    return end;
                }
            }
        }
    }
}

/// Where explorust writes why it stopped with `PROBLEM` (same folder as the logs).
fn problem_file() -> PathBuf {
    log::dir().join("problem.txt")
}

fn problem_reason(code: u32) -> Option<String> {
    if code != PROBLEM {
        return None;
    }
    let text = std::fs::read_to_string(problem_file()).ok()?;
    let _ = std::fs::remove_file(problem_file());
    Some(text.trim().to_string()).filter(|t| !t.is_empty())
}

/// Parts set aside in `parts-off.txt` (the shell reads the same file).
fn parts_off() -> Vec<String> {
    std::fs::read_to_string(log::dir().join("parts-off.txt"))
        .map(|s| s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default()
}

/// A reload: `explorust.next.exe` beside explorust.exe, if present, replaces it before the restart.
fn swap_next(exe: &Path) {
    let next = exe.with_file_name("explorust.next.exe");
    if !next.exists() {
        // Installed in Program Files this guard cannot write beside explorust.exe: the elevated
        // deploy (tools/g1-deploy.sh) has already swapped the new exe in.
        log::line(&format!("reload: restarting {}", exe.display()));
        return;
    }
    // The old process has exited, but its image can stay mapped for a moment.
    for _ in 0..20 {
        match std::fs::rename(&next, exe) {
            Ok(()) => {
                log::line("reload: explorust.next.exe swapped in");
                return;
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    log::line("reload: explorust.exe still locked after 2 s, restarting the old one");
}

fn shell_exe() -> PathBuf {
    let me = std::env::current_exe().unwrap_or_default();
    me.with_file_name("explorust.exe")
}

pub fn explorer_path() -> PathBuf {
    use windows::Win32::UI::Shell::FOLDERID_Windows;
    crate::install::known(&FOLDERID_Windows).unwrap_or_else(|| PathBuf::from(r"C:\Windows")).join("explorer.exe")
}

/// Keeps a plain explorer.exe as the shell for the rest of the session: started (or an explorer
/// whose taskbar already answers, adopted), confirmed by its answering taskbar, restarted when it
/// ends. A start that fails (no process, no taskbar within 60 s, or a taskbar that lives under 30 s)
/// counts; after 3 in a row the user is told once how to start it by hand, and the guard keeps
/// trying, waiting longer between tries (up to a minute). `notice` shows once a taskbar answers.
/// The escape hotkey is released first.
pub(crate) fn explorer_forever(notice: Option<Notice>, mut explorer_ready: Option<&Event>) -> u32 {
    // Explorer has its own shortcuts: the escape to it has no use now and is given back.
    hotkey::release();
    let release = Event::named(RELEASE_EXPLORER).ok();
    let _handoff = Event::named(WATCHES_EXPLORER_HANDOFF).ok();
    folders::handler(false);
    let mut notice = notice;
    let path = explorer_path();
    let mut failures = 0u32;
    let mut told = false;
    loop {
        if Event::is_signalled(RELEASE_EXPLORER) {
            return released_explorer();
        }
        if failures > 0 {
            if failures >= 3 && !told {
                told = true;
                log::line(&format!("explorer failed to come up {failures} times in a row: telling the user"));
                notice::show_later(Notice::NoShell);
            }
            std::thread::sleep(Duration::from_secs((u64::from(failures) * 5).min(60)));
        }
        let pid = match taskbar::explorer() {
            Some(pid) => {
                log::line(&format!("explorer pid {pid} owns the taskbar"));
                pid
            }
            None => {
                let Some(child) = spawn_explorer(&path) else {
                    failures += 1;
                    continue;
                };
                let t0 = Instant::now();
                match taskbar::wait_explorer(&child, Duration::from_secs(60)) {
                    Start::Up(pid) => {
                        log::line(&format!("explorer taskbar answers (pid {pid}, started pid {}) after {} ms", child.pid, t0.elapsed().as_millis()));
                        pid
                    }
                    Start::Exited(code) => {
                        log::line(&format!("explorer pid {} ended before its taskbar came, code {code:#x}", child.pid));
                        failures += 1;
                        continue;
                    }
                    Start::Timeout => {
                        log::line(&format!("explorer pid {} has no taskbar after 60 s: restarting it", child.pid));
                        child.kill();
                        failures += 1;
                        continue;
                    }
                }
            }
        };
        if let Some(event) = explorer_ready.take() {
            if let Err(error) = event.signal() {
                log::line(&format!("guard: could not report Explorer ready: {error}"));
            }
        }
        if let Some(n) = notice.take() {
            notice::show_later(n);
        }
        let up = Instant::now();
        match Child::open(pid) {
            Ok(shell) => {
                if let Some(event) = &release {
                    if wait_any(&[event.0, shell.handle], None) == Some(0) {
                        return released_explorer();
                    }
                } else {
                    wait_any(&[shell.handle], None);
                }
                if Event::is_signalled(EXITING_EXPLORER) {
                    return released_explorer();
                }
                log::line(&format!("explorer pid {pid} ended after {} s, code {:#x}", up.elapsed().as_secs(), shell.exit_code()));
            }
            Err(e) => log::line(&format!("explorer pid {pid}: {e}")),
        }
        if up.elapsed() < Duration::from_secs(30) {
            failures += 1;
        } else {
            failures = 0;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn released_explorer() -> u32 {
    log::line("guard: released Explorer for a live switch");
    0
}

/// Starts explorer.exe, three tries 2 s apart.
fn spawn_explorer(path: &Path) -> Option<Child> {
    for i in 1..=3 {
        match Child::spawn(path, "") {
            Ok(c) => {
                log::line(&format!("started explorer pid {}", c.pid));
                return Some(c);
            }
            Err(e) => log::line(&format!("explorer start {i}/3: {e}")),
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "fixture process for the Windows readiness test"]
    fn idle_shell() {
        std::thread::sleep(Duration::from_secs(10));
    }

    #[test]
    fn immersive_wait_is_separate_and_escape_takes_priority() {
        let prefix = format!(r"Local\explorust-phase-test-{}", std::process::id());
        let phases = PhaseEvents::open(&prefix).unwrap();
        let fallback = Event::named(&format!("{prefix}-fallback")).unwrap();
        let child = Child::spawn(
            &std::env::current_exe().unwrap(),
            "--exact supervise::tests::idle_shell --ignored",
        ).unwrap();
        phases.native.signal().unwrap();
        assert!(wait_phase(&child, &fallback, &phases.native, "native", Duration::ZERO, None).is_ok());
        let name = launch_timing::immersive_event(&prefix);
        let delay = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(250));
            Event::set_existing(&name).unwrap();
        });
        let ready = wait_phase(&child, &fallback, &phases.immersive, "immersive", Duration::from_secs(2), None);
        delay.join().unwrap();
        assert!(ready.is_ok());

        phases.verified.signal().unwrap();
        fallback.signal().unwrap();
        assert!(matches!(
            wait_phase(&child, &fallback, &phases.verified, "build verdict", Duration::from_secs(1), None),
            Err(End::Fallback(_))
        ));
        assert!(child.ended());
    }
}
