use super::log;
use shelldata::{
    Image, icons,
    items::{self, PinItem},
    taskband,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

pub const MESSAGE: u32 = WM_APP + 5;

pub struct PinData {
    pub name: String,
    pub pidl: Vec<u8>,
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
    if let Err(error) = std::thread::Builder::new()
        .name("taskbar-pin-list".into())
        .spawn(move || {
            let start = Instant::now();
            let pidls = taskband::pin_pidls(&items::taskband_blob());
            let count = pidls.len();
            if !sink.post(Update::Slots(count)) {
                return;
            }
            log::line(&format!(
                "published {count} pin slots in {} ms",
                start.elapsed().as_millis()
            ));
            let work = Arc::new((pidls, AtomicUsize::new(0)));
            // A provider can block one STA. Keep other pins moving with bounded parallelism.
            for _ in 0..count.min(4) {
                let sink = sink.clone();
                let work = work.clone();
                if let Err(error) = std::thread::Builder::new()
                    .name("taskbar-pin".into())
                    .spawn(move || {
                        if let Err(error) =
                            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok() }
                        {
                            log::line(&format!("pin COM initialization failed: {error}"));
                            return;
                        }
                        loop {
                            let slot = work.1.fetch_add(1, Ordering::Relaxed);
                            let Some(pidl) =
                                work.0.get(slot).filter(|_| sink.inbox.strong_count() > 0)
                            else {
                                break;
                            };
                            let start = Instant::now();
                            let Some(item) = items::resolve_pin(pidl.clone()) else {
                                if !sink.post(Update::Resolved(slot, None)) {
                                    break;
                                }
                                continue;
                            };
                            let data = PinData {
                                name: item.name.clone(),
                                pidl: item.pidl.clone(),
                                icon: None,
                            };
                            if !sink.post(Update::Resolved(slot, Some((item.key.clone(), data)))) {
                                break;
                            }
                            log::line(&format!(
                                "pin slot {slot} resolved in {} ms",
                                start.elapsed().as_millis()
                            ));
                            let start = Instant::now();
                            let icon = image(&item, px);
                            log::line(&format!(
                                "pin slot {slot} icon {} in {} ms",
                                if icon.is_some() { "loaded" } else { "fallback" },
                                start.elapsed().as_millis()
                            ));
                            if let Some(icon) = icon
                                && !sink.post(Update::Icon(slot, icon))
                            {
                                break;
                            }
                        }
                        unsafe { CoUninitialize() };
                    })
                {
                    log::line(&format!("pin worker could not start: {error}"));
                }
            }
        })
    {
        log::line(&format!("pin list worker could not start: {error}"));
    }
}

pub fn image(item: &PinItem, px: i32) -> Option<Image> {
    icons::icon_for_item(&item.item, px).or_else(|| {
        if item.key.contains('\\') {
            icons::icon_for_path(&item.key, px)
        } else {
            items::apps_folder_item(&item.key).and_then(|app| icons::icon_for_item(&app, px))
        }
    })
}
