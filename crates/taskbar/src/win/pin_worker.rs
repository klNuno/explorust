use super::{actions::Launch, log};
use shelldata::{
    Image, icons,
    items::{self, PinItem},
    taskband,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::time::Instant;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::Shell::IShellItem;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

pub const MESSAGE: u32 = WM_APP + 5;

pub struct PinData {
    pub name: String,
    pub launch: Launch,
    pub file_explorer: bool,
    pub icon: Option<Image>,
}

pub enum Update {
    Slots(usize),
    Resolved(usize, Option<(String, PinData)>),
    Icon(usize, Image),
}

pub type Inbox = Arc<Mutex<Vec<Update>>>;

#[derive(Clone)]
struct Sink {
    hwnd: isize,
    inbox: Weak<Mutex<Vec<Update>>>,
}

impl Sink {
    fn post(&self, update: Update) -> bool {
        let Some(inbox) = self.inbox.upgrade() else {
            return false;
        };
        inbox.lock().unwrap().push(update);
        unsafe { PostMessageW(Some(HWND(self.hwnd as _)), MESSAGE, WPARAM(0), LPARAM(0)).is_ok() }
    }
}

pub fn start(hwnd: HWND, px: i32, inbox: &Inbox) {
    let sink = Sink {
        hwnd: hwnd.0 as isize,
        inbox: Arc::downgrade(inbox),
    };
    spawn_sta("taskbar-pin-list", move || {
        let pidls = taskband::pin_pidls(&items::taskband_blob());
        load_pins(sink, pidls, px, items::resolve_pin, |job, px| {
            let item = items::item_from_pidl(&job.pidl)?;
            image_for_key(&item, &job.key, px)
        });
    });
}

struct IconJob {
    slot: usize,
    key: String,
    pidl: Vec<u8>,
}

struct PendingPins {
    pidls: Vec<Vec<u8>>,
    local: Vec<bool>,
    next: AtomicUsize,
}

fn load_pins(
    sink: Sink,
    pidls: Vec<Vec<u8>>,
    px: i32,
    resolve: impl Fn(Vec<u8>) -> Option<PinItem> + Send + Sync + 'static,
    read_icon: impl Fn(&IconJob, i32) -> Option<Image> + Send + Sync + 'static,
) {
    let count = pidls.len();
    if !sink.post(Update::Slots(count)) || count == 0 {
        return;
    }
    log::line(&format!("published {count} pin slots"));

    // Publish local shortcut launch targets before providers can occupy all resolution workers.
    let folder = items::LocalPinFolder::taskbar();
    let prepared: Vec<_> = pidls
        .iter()
        .enumerate()
        .map(|(slot, pidl)| {
            folder.as_ref().is_some_and(|folder| folder.contains(pidl))
                && publish_local(&sink, slot, pidl)
        })
        .collect();
    let pending = Arc::new(PendingPins {
        pidls,
        local: prepared,
        next: AtomicUsize::new(0),
    });
    let (send, receive) = mpsc::channel();
    let receive = Arc::new(Mutex::new(receive));
    let read_icon = Arc::new(read_icon);
    let resolve = Arc::new(resolve);
    for _ in 0..count.min(4) {
        let icon_sink = sink.clone();
        let receive = receive.clone();
        let read_icon = read_icon.clone();
        spawn_sta("taskbar-pin-icon", move || {
            while icon_sink.inbox.strong_count() > 0 {
                let Ok(job): Result<IconJob, _> = receive.lock().unwrap().recv() else {
                    break;
                };
                let start = Instant::now();
                let icon = read_icon(&job, px);
                let result = if icon.is_some() { "loaded" } else { "fallback" };
                log::line(&format!(
                    "pin slot {} icon {result} in {} ms",
                    job.slot,
                    start.elapsed().as_millis()
                ));
                if let Some(icon) = icon
                    && !icon_sink.post(Update::Icon(job.slot, icon))
                {
                    break;
                }
            }
        });

        let sink = sink.clone();
        let pending = pending.clone();
        let send = send.clone();
        let resolve = resolve.clone();
        spawn_sta("taskbar-pin-identity", move || {
            while sink.inbox.strong_count() > 0 {
                let slot = pending.next.fetch_add(1, Ordering::Relaxed);
                let Some(pidl) = pending.pidls.get(slot) else {
                    break;
                };
                let local = pending.local[slot] || publish_local(&sink, slot, pidl);
                let start = Instant::now();
                let Some(item) = resolve(pidl.clone()) else {
                    if !local && !sink.post(Update::Resolved(slot, None)) {
                        break;
                    }
                    continue;
                };
                let data = PinData {
                    name: item.name,
                    launch: Launch::Pidl(item.pidl.clone()),
                    file_explorer: false,
                    icon: None,
                };
                if !sink.post(Update::Resolved(slot, Some((item.key.clone(), data)))) {
                    break;
                }
                log::line(&format!(
                    "pin slot {slot} resolved in {} ms",
                    start.elapsed().as_millis()
                ));
                let _ = send.send(IconJob {
                    slot,
                    key: item.key,
                    pidl: item.pidl,
                });
            }
        });
    }
}

fn publish_local(sink: &Sink, slot: usize, pidl: &[u8]) -> bool {
    let Some(shortcut) = items::local_shortcut(pidl) else {
        return false;
    };
    let key = shortcut.path.to_lowercase();
    let data = PinData {
        name: shortcut.name,
        launch: Launch::Path(shortcut.path),
        file_explorer: shortcut.explorer,
        icon: None,
    };
    sink.post(Update::Resolved(slot, Some((key, data))))
}

fn spawn_sta(name: &'static str, run: impl FnOnce() + Send + 'static) {
    let thread = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            // SAFETY: initialize COM on this worker and balance successful initialization.
            if let Err(error) = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok() } {
                log::line(&format!("{name}: COM initialization failed: {error}"));
                return;
            }
            run();
            unsafe { CoUninitialize() };
        });
    if let Err(error) = thread {
        log::line(&format!("{name} could not start: {error}"));
    }
}

pub fn image(item: &PinItem, px: i32) -> Option<Image> {
    image_for_key(&item.item, &item.key, px)
}

fn image_for_key(item: &IShellItem, key: &str, px: i32) -> Option<Image> {
    icons::icon_for_item(item, px).or_else(|| {
        if key.contains('\\') {
            icons::icon_for_path(key, px)
        } else {
            items::apps_folder_item(key).and_then(|app| icons::icon_for_item(&app, px))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::Condvar;
    use std::time::Duration;
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree, IPersistFile,
    };
    use windows::Win32::UI::Shell::{ILGetSize, IShellLinkW, SHParseDisplayName, ShellLink};
    use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, HWND_MESSAGE};
    use windows_core::{HSTRING, Interface, w};

    fn shortcut(path: &Path, target: &str, args: &str) -> Vec<u8> {
        unsafe {
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            link.SetPath(&HSTRING::from(target)).unwrap();
            link.SetArguments(&HSTRING::from(args)).unwrap();
            let file: IPersistFile = link.cast().unwrap();
            let path = HSTRING::from(path.to_str().unwrap());
            file.Save(&path, true).unwrap();
            let mut pidl = std::ptr::null_mut();
            SHParseDisplayName(&path, None, &mut pidl, 0, None).unwrap();
            let bytes =
                std::slice::from_raw_parts(pidl.cast::<u8>(), ILGetSize(Some(pidl)) as usize)
                    .to_vec();
            CoTaskMemFree(Some(pidl.cast()));
            bytes
        }
    }

    #[derive(Default)]
    struct Hold(Mutex<bool>, Condvar);

    impl Hold {
        fn wait(&self) {
            let lock = self.0.lock().unwrap();
            let _ = self
                .1
                .wait_timeout_while(lock, Duration::from_secs(20), |released| !*released)
                .unwrap();
        }

        fn release(&self) {
            *self.0.lock().unwrap() = true;
            self.1.notify_all();
        }
    }

    struct Fixture(Vec<PathBuf>);

    impl Drop for Fixture {
        fn drop(&mut self) {
            for path in &self.0 {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    struct Mailbox(HWND);

    impl Drop for Mailbox {
        fn drop(&mut self) {
            let _ = unsafe { DestroyWindow(self.0) };
        }
    }

    fn wait_for<T>(mut probe: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(value) = probe() {
                return value;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the pin fixture"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn four_held_icons_do_not_block_a_local_pins_identity_or_launch() {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
        let folder = PathBuf::from(std::env::var_os("APPDATA").unwrap())
            .join(r"Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar");
        std::fs::create_dir_all(&folder).unwrap();
        let system = std::env::var("SystemRoot").unwrap();
        let tag = format!("explorust-pins-{}", std::process::id());
        let marker = std::env::temp_dir().join(format!("{tag}.txt"));
        let mut fixture = Fixture(vec![marker.clone()]);
        let paths: Vec<_> = (0..5)
            .map(|slot| folder.join(format!("{tag}-{slot}.lnk")))
            .collect();
        fixture.0.extend(paths.iter().cloned());
        let command = format!("/d /c echo launched > \"{}\"", marker.display());
        let pidls: Vec<_> = paths
            .iter()
            .map(|path| shortcut(path, &format!(r"{system}\System32\cmd.exe"), &command))
            .collect();
        let local_folder = items::LocalPinFolder::taskbar().unwrap();
        assert!(
            pidls.iter().all(|pidl| local_folder.contains(pidl)),
            "standard TaskBar shortcut PIDLs must use the local pass"
        );
        let fifth = pidls[4].clone();
        let inbox = Inbox::default();
        let hwnd = Mailbox(unsafe {
            CreateWindowExW(
                Default::default(),
                w!("STATIC"),
                w!(""),
                Default::default(),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                None,
                None,
            )
            .unwrap()
        });
        let sink = Sink {
            hwnd: hwnd.0.0 as isize,
            inbox: Arc::downgrade(&inbox),
        };
        let hold_identity = Arc::new(Hold::default());
        let hold_icons = Arc::new(Hold::default());
        let (icon_started, icon_jobs) = mpsc::channel();

        let identity = hold_identity.clone();
        let icons = hold_icons.clone();
        load_pins(
            sink,
            pidls,
            24,
            move |pidl| {
                if pidl == fifth {
                    identity.wait();
                }
                items::resolve_pin(pidl)
            },
            move |job, _| {
                icon_started.send(job.slot).unwrap();
                icons.wait();
                None
            },
        );

        let mut held: Vec<_> = (0..4)
            .map(|_| icon_jobs.recv_timeout(Duration::from_secs(5)).unwrap())
            .collect();
        held.sort_unstable();
        assert_eq!(held, [0, 1, 2, 3]);
        let fallback = inbox
            .lock()
            .unwrap()
            .iter()
            .find_map(|update| match update {
                Update::Resolved(4, Some((key, data)))
                    if matches!(data.launch, Launch::Path(_)) =>
                {
                    assert_eq!(key, &paths[4].to_string_lossy().to_lowercase());
                    Some(data.launch.clone())
                }
                _ => None,
            })
            .expect("the fifth pin already has a validated launch target");
        super::super::actions::launch(fallback);
        wait_for(|| {
            std::fs::read_to_string(&marker)
                .ok()
                .filter(|text| text.contains("launched"))
        });

        hold_identity.release();
        wait_for(|| {
            inbox
                .lock()
                .unwrap()
                .iter()
                .find_map(|update| match update {
                    Update::Resolved(4, Some((_, data)))
                        if matches!(data.launch, Launch::Pidl(_)) =>
                    {
                        Some(())
                    }
                    _ => None,
                })
        });
        assert!(
            icon_jobs.try_recv().is_err(),
            "the fifth icon is still queued behind four held reads"
        );
        hold_icons.release();
        assert_eq!(icon_jobs.recv_timeout(Duration::from_secs(5)).unwrap(), 4);
        println!(
            "Four held icon reads, fifth pin launched through its shortcut and resolved before its icon started"
        );
        unsafe { CoUninitialize() };
    }
}
