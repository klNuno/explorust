//! One worker orders SetSite calls, tracks the live rule file and retains sessions until the UI
//! thread acknowledges their rollback. The diagnostics callback and the rollback share a tree.

use super::abi::*;
use super::config::{self, Changes};
use super::owner::{self, Loader};
use super::report;
use super::rules::Rules;
use super::state::{DispatchProgress, SiteChange, SiteQueue, StyleOwner};
use super::watch::{Tree, Watch};
use std::os::windows::io::OwnedHandle;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
use windows::UI::Core::{CoreDispatcher, CoreDispatcherPriority, DispatchedHandler};
use windows::Win32::Foundation::{CloseHandle, E_FAIL, E_INVALIDARG, HANDLE, WAIT_FAILED, WAIT_OBJECT_0};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects};
use windows_core::{IInspectable, IUnknown, Interface, PCWSTR};

/// The diagnostics site is free-threaded. XAML elements are accessed through the UI dispatcher.
pub struct Site(pub IUnknown);
// SAFETY: the diagnostics runtime supports advising its site from an MTA worker.
unsafe impl Send for Site {}

struct Wake(HANDLE);
// SAFETY: the owned event can be waited on and signaled from any thread.
unsafe impl Send for Wake {}
unsafe impl Sync for Wake {}

impl Wake {
    fn signal(&self) {
        // SAFETY: the event remains open while self lives.
        let _ = unsafe { SetEvent(self.0) };
    }
}

impl Drop for Wake {
    fn drop(&mut self) {
        // SAFETY: final owner of an event created by CreateEventW.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

struct Worker {
    tx: mpsc::Sender<SiteChange<Site>>,
    wake: Arc<Wake>,
}

static WORKER: Mutex<Option<Worker>> = Mutex::new(None);

pub fn enqueue(change: SiteChange<Site>) -> windows_core::Result<()> {
    let mut slot = WORKER.lock().unwrap_or_else(|e| e.into_inner());
    if slot.is_none() {
        // SAFETY: unnamed, auto-reset event with default attributes.
        let wake = Arc::new(Wake(unsafe { CreateEventW(None, false, false, PCWSTR::null()) }?));
        let (tx, rx) = mpsc::channel();
        let signal = wake.clone();
        std::thread::Builder::new()
            .name("explorust-xaml".into())
            .spawn(move || run(&rx, &signal))
            .map_err(|_| E_FAIL)?;
        *slot = Some(Worker { tx, wake });
    }
    let worker = slot.as_ref().unwrap();
    worker.tx.send(change).map_err(|_| E_FAIL)?;
    worker.wake.signal();
    Ok(())
}

fn run(rx: &mpsc::Receiver<SiteChange<Site>>, wake: &Arc<Wake>) {
    // SAFETY: the worker owns its COM apartment and uninitializes it on exit.
    if let Err(e) = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok() {
        report::line(format!("tap: worker COM setup failed: {e}"));
        return;
    }
    run_sessions(rx, wake);
    unsafe { CoUninitialize() };
}

fn run_sessions(rx: &mpsc::Receiver<SiteChange<Site>>, wake: &Arc<Wake>) {
    let mut queue = SiteQueue::default();
    let mut active: Option<Session> = None;
    loop {
        for change in rx.try_iter() {
            queue.update(change, active.as_ref().map(|s| s.tap));
        }

        let mut timeout = INFINITE;
        if let Some(s) = active.as_mut() {
            let step = s.advance(queue.retiring, wake);
            if step == Step::Retired {
                active = None;
                queue.retiring = false;
                // A SetSite call may have arrived during unadvise or UI rollback.
                continue;
            }
            timeout = match step {
                Step::Idle => INFINITE,
                Step::Wait(ms) => ms,
                Step::Retired => unreachable!(),
            };
        }
        if let Some((tap, site)) = queue.take_ready() {
            match Session::start(tap, &site.0) {
                Ok(s) => active = Some(s),
                Err(e) => report::line(format!("tap: not started: {e}")),
            }
            continue;
        }

        let mut events = [wake.0, HANDLE::default(), HANDLE::default(), HANDLE::default()];
        let mut len = 1;
        let mut directory_event = None;
        if let Some(s) = &active {
            events[len] = owner::handle(&s.owner_wake);
            len += 1;
            if let Some(process) = &s.owner.process {
                events[len] = process.handle();
                len += 1;
            }
            if let Some(changes) = &s.changes {
                directory_event = Some(WAIT_OBJECT_0.0 + len as u32);
                events[len] = changes.0;
                len += 1;
            }
        }
        // SAFETY: the worker holds each handle until the wait returns.
        let event = unsafe { WaitForMultipleObjects(&events[..len], false, timeout) };
        if let Some(s) = active.as_mut() {
            if Some(event.0) == directory_event {
                s.changed();
            } else if event == WAIT_FAILED {
                s.changes = None;
            }
        }
    }
}

#[derive(PartialEq, Eq)]
enum Step {
    Idle,
    Wait(u32),
    Retired,
}

struct Session {
    tap: u64,
    svc: IVisualTreeService,
    diag: IXamlDiagnostics,
    cb: IVisualTreeServiceCallback2,
    tree: Arc<Tree>,
    dir: PathBuf,
    image: String,
    owner: StyleOwner<Arc<Loader>>,
    owner_wake: OwnedHandle,
    changes: Option<Changes>,
    desired: Arc<Rules>,
    committed: Option<Arc<Rules>>,
    refresh: bool,
    advised: bool,
    pending: Option<Operation>,
    retry_at: Option<Instant>,
}

impl Session {
    fn start(tap: u64, site: &IUnknown) -> windows_core::Result<Self> {
        let start = Instant::now();
        let svc: IVisualTreeService = site.cast()?;
        let diag: IXamlDiagnostics = site.cast()?;
        let mut data = std::ptr::null_mut();
        // SAFETY: initialization data is an owned BSTR returned through this pointer.
        let data = unsafe {
            let hr = diag.GetInitializationData(&mut data);
            let text = bstr(data);
            SysFreeString(data);
            hr.ok()?;
            text
        };
        let (dir, image) = data.split_once('\n').ok_or(E_INVALIDARG)?;
        let dir = PathBuf::from(dir);
        let owner_wake = owner::tap_wake()?;
        let changes = Changes::new(&dir);
        report::line(format!("tap: site set in pid {}", std::process::id()));
        let tree = Arc::new(Tree::new(svc.clone(), diag.clone(), start));
        let cb: IVisualTreeServiceCallback2 = Watch::new(tree.clone()).into();
        // SAFETY: MTA initialized by the worker. The session retains the service and callback.
        let hr = unsafe { svc.AdviseVisualTreeChange(cb.as_raw()) };
        report::line(format!("tap: advise {:#010x} in {} ms", hr.0, start.elapsed().as_millis()));
        hr.ok()?;
        Ok(Self {
            tap,
            svc,
            diag,
            cb,
            tree,
            dir,
            image: image.into(),
            owner: StyleOwner::default(),
            owner_wake,
            changes,
            desired: Arc::new(Rules::default()),
            committed: None,
            refresh: true,
            advised: true,
            pending: None,
            retry_at: None,
        })
    }

    fn changed(&mut self) {
        if self.changes.as_ref().is_some_and(|c| !c.rearm()) {
            self.changes = None;
        }
        self.refresh = true;
    }

    fn refresh_owner(&mut self) {
        let rolled_back = self.pending.is_none() && self.committed.as_ref().is_some_and(|r| r.idle());
        if self.owner.refresh(|p| p.alive(), || Loader::connect(&self.image).map(Arc::new), rolled_back) {
            self.refresh = true;
            if self.owner.process.is_some() {
                report::line("tap: shell owner connected".into());
            } else {
                self.tree.pause();
                self.desired = Arc::new(Rules::default());
                report::line("tap: shell owner exited, rolling back".into());
            }
        }
    }

    fn advance(&mut self, retiring: bool, wake: &Arc<Wake>) -> Step {
        self.refresh_owner();
        if let Some(at) = self.retry_at {
            if Instant::now() < at {
                return Step::Wait(wait_until(at));
            }
            self.retry_at = None;
        }
        if let Some(job) = self.pending.as_ref() {
            match job.result() {
                None => return Step::Wait(job.wait_ms()),
                Some(ok) => {
                    let job = self.pending.take().unwrap();
                    if !ok {
                        self.committed = None;
                        return self.retry();
                    }
                    let Some(rules) = job.rules else { return Step::Retired };
                    self.committed = Some(rules);
                    // A replacement loader may have announced itself before this rollback finished.
                    self.refresh_owner();
                }
            }
        }

        if retiring {
            if self.advised {
                // SAFETY: same worker and retained callback that advised this session.
                let hr = unsafe { self.svc.UnadviseVisualTreeChange(self.cb.as_raw()) };
                if hr.is_err() {
                    report::line(format!("tap: unadvise {:#010x}, will retry", hr.0));
                    return self.retry();
                }
                self.advised = false;
                self.tree.pause();
            }
            return self.dispatch(None, wake);
        }

        if self.changes.is_none() {
            self.changes = Changes::new(&self.dir);
            self.refresh = true;
        }
        if self.refresh && self.owner.process.is_some() {
            match config::live_rules(&self.dir) {
                Ok(rules) => {
                    if rules != *self.desired {
                        self.desired = Arc::new(rules);
                    }
                    self.refresh = false;
                }
                Err(e) => {
                    report::line(format!("tap: cannot read style: {e}"));
                    return self.retry();
                }
            }
        }
        if self.committed.as_ref() != Some(&self.desired) {
            self.dispatch(Some(self.desired.clone()), wake)
        } else if self.changes.is_none() {
            Step::Wait(1000)
        } else {
            self.tree.flush();
            Step::Idle
        }
    }

    fn retry(&mut self) -> Step {
        // A log write can also wake the directory watcher. Keep that wake from accelerating retries.
        self.retry_at = Some(Instant::now() + Duration::from_secs(1));
        Step::Wait(1000)
    }

    fn dispatch(&mut self, rules: Option<Arc<Rules>>, wake: &Arc<Wake>) -> Step {
        let mut raw = std::ptr::null_mut();
        // SAFETY: an out pointer to an owned reference on success.
        let hr = unsafe { self.diag.GetDispatcher(&mut raw) };
        let dispatcher = hr.ok().and_then(|_| {
            if raw.is_null() {
                return Err(E_FAIL.into());
            }
            // SAFETY: GetDispatcher returned this owned IInspectable reference.
            unsafe { IInspectable::from_raw(raw) }.cast::<CoreDispatcher>()
        });
        let owner = self.owner.process.clone();
        match dispatcher.and_then(|d| Operation::dispatch(&d, self.tree.clone(), rules, owner, wake.clone())) {
            Ok(job) => {
                let wait = Step::Wait(job.wait_ms());
                self.pending = Some(job);
                wait
            }
            Err(e) => {
                report::line(format!("tap: UI dispatch failed, will retry: {e}"));
                self.retry()
            }
        }
    }
}

struct Operation {
    rules: Option<Arc<Rules>>,
    progress: Arc<Mutex<DispatchProgress>>,
    deadline: Instant,
}

impl Operation {
    fn dispatch(
        d: &CoreDispatcher,
        tree: Arc<Tree>,
        rules: Option<Arc<Rules>>,
        owner: Option<Arc<Loader>>,
        wake: Arc<Wake>,
    ) -> windows_core::Result<Self> {
        let progress = Arc::new(Mutex::new(DispatchProgress::Queued));
        let queued = progress.clone();
        let desired = rules.clone();
        d.RunAsync(
            CoreDispatcherPriority::Normal,
            &DispatchedHandler::new(move || {
                {
                    let mut p = queued.lock().unwrap_or_else(|e| e.into_inner());
                    if !p.begin() {
                        return Ok(());
                    }
                }
                let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    tree.update(desired.as_ref(), || owner.as_ref().is_some_and(|p| p.alive()))
                })).unwrap_or(false);
                *queued.lock().unwrap_or_else(|e| e.into_inner()) = DispatchProgress::Done(done);
                wake.signal();
                Ok(())
            }),
        )?;
        Ok(Self {
            rules,
            progress,
            deadline: Instant::now() + Duration::from_secs(5),
        })
    }

    fn result(&self) -> Option<bool> {
        let mut p = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        let result = p.result(Instant::now() >= self.deadline);
        if matches!(*p, DispatchProgress::Cancelled) {
            report::line("tap: UI dispatch did not start in 5 s, will retry".into());
        }
        result
    }

    fn wait_ms(&self) -> u32 {
        match *self.progress.lock().unwrap_or_else(|e| e.into_inner()) {
            DispatchProgress::Queued => wait_until(self.deadline),
            _ => INFINITE,
        }
    }
}

fn wait_until(deadline: Instant) -> u32 {
    deadline.saturating_duration_since(Instant::now()).as_millis() as u32 + 1
}
