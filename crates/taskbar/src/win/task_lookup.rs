//! Resolves window identities away from the message loop. Shell and WinEvent notifications
//! often report the same window more than once, so keep one outstanding read per window.

use super::{
    app_resolver::Resolver,
    log,
    tasks::{Task, Window, candidate},
};
use std::{collections::HashMap, io, sync::mpsc, thread};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

pub const MESSAGE: u32 = WM_APP + 6;

pub struct Lookup {
    jobs: mpsc::Sender<Job>,
    results: mpsc::Receiver<(Job, Option<Task>)>,
    pending: HashMap<isize, Pending>,
    next_serial: u64,
}

#[derive(Clone, Copy)]
struct Job {
    window: Window,
    serial: u64,
}

struct Pending {
    serial: u64,
    again: bool,
    flash: bool,
}

pub struct ResultRow {
    pub window: Window,
    pub task: Option<Task>,
    pub again: bool,
    pub flash: bool,
}

impl Lookup {
    pub fn start(hwnd: HWND) -> io::Result<Self> {
        let (jobs, requests) = mpsc::channel::<Job>();
        let (return_result, results) = mpsc::channel();
        let bar = hwnd.0 as isize;
        thread::Builder::new()
            .name("taskbar-app-id".into())
            .spawn(move || {
                // SAFETY: COM is initialized and used only on this worker.
                let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
                {
                    let resolver = initialized.then(Resolver::new).and_then(|result| {
                        result
                            .map_err(|e| {
                                log::line(&format!("application resolver unavailable: {e}"))
                            })
                            .ok()
                    });
                    for job in requests {
                        let hwnd = HWND(job.window.handle as _);
                        let task = (candidate(hwnd) == Some(job.window))
                            .then(|| job.window.resolve(resolver.as_ref()))
                            .flatten();
                        if return_result.send((job, task)).is_err() {
                            break;
                        }
                        // SAFETY: posts a wakeup with no borrowed data to the taskbar.
                        let _ = unsafe {
                            PostMessageW(Some(HWND(bar as _)), MESSAGE, WPARAM(0), LPARAM(0))
                        };
                    }
                }
                if initialized {
                    // SAFETY: the resolver is dropped and the worker is finished with COM.
                    unsafe { CoUninitialize() };
                }
            })?;
        Ok(Self {
            jobs,
            results,
            pending: HashMap::new(),
            next_serial: 0,
        })
    }

    pub fn request(&mut self, window: Window) {
        if let Some(pending) = self.pending.get_mut(&window.handle) {
            pending.again = true;
            return;
        }
        let serial = self.next_serial;
        self.next_serial += 1;
        if self.jobs.send(Job { window, serial }).is_ok() {
            self.pending.insert(
                window.handle,
                Pending {
                    serial,
                    again: false,
                    flash: false,
                },
            );
        }
    }

    pub fn forget(&mut self, handle: isize) {
        self.pending.remove(&handle);
    }

    pub fn set_flash(&mut self, handle: isize, flash: bool) {
        if let Some(pending) = self.pending.get_mut(&handle) {
            pending.flash = flash;
        }
    }

    pub fn receive(&mut self) -> Option<ResultRow> {
        while let Ok((job, task)) = self.results.try_recv() {
            let handle = job.window.handle;
            if self
                .pending
                .get(&handle)
                .is_some_and(|p| p.serial == job.serial)
            {
                let pending = self.pending.remove(&handle).unwrap();
                return Some(ResultRow {
                    window: job.window,
                    task,
                    again: pending.again,
                    flash: pending.flash,
                });
            }
        }
        None
    }
}
