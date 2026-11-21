//! Files and folders from explorust's own index, the `explorust-index` service (MFT and USN
//! journal of every fixed NTFS volume, `crates/index`): asked over its pipe on a worker thread on
//! every key, beside the Windows Search worker (`index.rs`); its hits merge into the same file
//! groups (`search::merge_files`). Per key timings are logged, with p50 and p95 every 20 keys.

use crate::index::{Batch, FileHit, post};
use crate::{apps, log, search};
use fsindex::client::Client;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Instant;
use windows::Win32::Foundation::HWND;
use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::UI::Shell::{IShellItemImageFactory, SHFILEINFOW, SHGFI_TYPENAME, SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW, SHCreateItemFromParsingName};
use windows_core::HSTRING;

const MAX: u16 = 24;
/// The source of the second batch of a query: icons and type names for rows already shown.
pub const ICONS: &str = "mft-icons";
/// Rows that can show: five per group, folders and files.
const WITH_ICONS: usize = 10;

pub struct Mft {
    tx: Sender<(u64, String)>,
    /// The service answered the last query: a key waits for its rows only then.
    up: Arc<AtomicBool>,
}

impl Mft {
    pub fn start(hwnd: HWND, icon_px: i32) -> Mft {
        let (tx, rx) = channel();
        let target = hwnd.0 as isize;
        let up = Arc::new(AtomicBool::new(false));
        let u = up.clone();
        let _ = std::thread::Builder::new().name("start-mft".into()).spawn(move || worker(rx, HWND(target as _), icon_px, u));
        Mft { tx, up }
    }

    pub fn up(&self) -> bool {
        self.up.load(Ordering::Relaxed)
    }

    pub fn query(&self, generation: u64, query: &str) {
        let _ = self.tx.send((generation, query.to_string()));
    }
}

fn type_text(path: &str, folder: bool) -> String {
    let mut info = SHFILEINFOW::default();
    let attrs = if folder { FILE_ATTRIBUTE_DIRECTORY } else { FILE_ATTRIBUTE_NORMAL };
    // SAFETY: fills a local struct; USEFILEATTRIBUTES keeps it off the disk.
    let ok = unsafe { SHGetFileInfoW(&HSTRING::from(path), attrs, Some(&mut info), size_of::<SHFILEINFOW>() as u32, SHGFI_TYPENAME | SHGFI_USEFILEATTRIBUTES) };
    if ok == 0 {
        return String::new();
    }
    let n = info.szTypeName.iter().position(|&c| c == 0).unwrap_or(info.szTypeName.len());
    String::from_utf16_lossy(&info.szTypeName[..n])
}

fn percentile(v: &[f32], p: f32) -> f32 {
    let mut s = v.to_vec();
    s.sort_by(f32::total_cmp);
    s[((s.len() - 1) as f32 * p).round() as usize]
}

type Rows = (u64, Vec<(String, bool, u8)>, Instant);

/// Icons and type names of the rows already shown, newest key only (older ones are dropped).
fn icons(rx: Receiver<Rows>, hwnd: HWND, icon_px: i32) {
    // SAFETY: COM for this worker thread (shell items for icons), for its whole life.
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    while let Ok(mut job) = rx.recv() {
        while let Ok(j) = rx.try_recv() {
            job = j;
        }
        let (generation, rows, t) = job;
        let n = rows.len();
        let icons: Vec<FileHit> = rows
            .into_iter()
            .enumerate()
            .map(|(i, (path, folder, rank))| {
                let icon = if i < WITH_ICONS {
                    // SAFETY: a shell item for a path the service just listed.
                    unsafe { SHCreateItemFromParsingName::<_, _, IShellItemImageFactory>(&HSTRING::from(path.as_str()), None) }
                        .ok()
                        .and_then(|f| apps::image(&f, icon_px))
                } else {
                    None
                };
                let name = path.rsplit('\\').next().unwrap_or(&path).to_string();
                FileHit { type_text: type_text(&path, folder), name, path, folder, icon, rank, modified: 0 }
            })
            .collect();
        let ms = t.elapsed().as_secs_f32() * 1000.0;
        post(hwnd, Batch { source: ICONS, generation, hits: icons, done: false, ms });
        log::line(&format!("mft icons: {n} rows at {ms:.1} ms after the query"));
    }
}

fn worker(rx: Receiver<(u64, String)>, hwnd: HWND, icon_px: i32, up: Arc<AtomicBool>) {
    let mut client = Client::default();
    let mut trips: Vec<f32> = Vec::new();
    let mut down = false;
    let (icons_tx, icons_rx) = channel();
    let target = hwnd.0 as isize;
    let _ = std::thread::Builder::new().name("start-mft-icons".into()).spawn(move || icons(icons_rx, HWND(target as _), icon_px));
    while let Ok(mut job) = rx.recv() {
        while let Ok(j) = rx.try_recv() {
            job = j;
        }
        let (generation, q) = job;
        if q.trim().is_empty() {
            continue;
        }
        let t = Instant::now();
        let reply = match client.query(&q, MAX) {
            Ok(r) => r,
            Err(e) => {
                up.store(false, Ordering::Relaxed);
                if !down {
                    log::line(&format!("mft: explorust-index not reachable: {e}"));
                    down = true;
                }
                continue;
            }
        };
        up.store(true, Ordering::Relaxed);
        if down {
            log::line("mft: explorust-index reachable");
            down = false;
        }
        let trip = t.elapsed().as_secs_f32() * 1000.0;
        let n = reply.hits.len();
        // The rows go out at once with no icon; icons and type names follow in a second batch.
        // The service orders by rank and last write before capping the list.
        let hits: Vec<FileHit> = reply
            .hits
            .iter()
            .map(|h| {
                let name = h.path.rsplit('\\').next().unwrap_or(&h.path).to_string();
                let rank = search::file_rank(&name, &q) + if h.noisy { 4 } else { 0 };
                FileHit { type_text: String::new(), name, path: h.path.clone(), folder: h.folder, icon: None, rank, modified: h.modified }
            })
            .collect();
        let ms = t.elapsed().as_secs_f32() * 1000.0;
        let rows: Vec<(String, bool, u8)> = hits.iter().map(|h| (h.path.clone(), h.folder, h.rank)).collect();
        post(hwnd, Batch { source: "mft", generation, hits, done: false, ms });
        // Icons load on their own thread: a key's query never waits behind the previous key's icons.
        let _ = icons_tx.send((generation, rows, t));
        trips.push(trip);
        log::line(&format!(
            "mft \"{q}\": {n} hits, service {:.2} ms, pipe round trip {trip:.2} ms, rows posted at {ms:.2} ms ({} names)",
            reply.micros as f32 / 1000.0,
            reply.names
        ));
        if trips.len() % 20 == 0 {
            log::line(&format!("mft: {} queries, round trip p50 {:.2} ms, p95 {:.2} ms", trips.len(), percentile(&trips, 0.5), percentile(&trips, 0.95)));
        }
    }
}
