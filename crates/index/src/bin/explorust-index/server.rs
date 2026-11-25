//! The index: one table per fixed NTFS volume, loaded from the MFT then kept current from the
//! journal on a thread per volume, and the pipe that answers queries from them.

use crate::hydration::Hydration;
use crate::{log, metadata, pipe, volume};
use fsindex::fold;
use fsindex::proto::{self, Hit, Reply, Request};
use memchr::memmem;
use std::fs::File;
use std::io;
use fsindex::table::{SortKey, Table};
use parking_lot::RwLock as VolumeLock;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::{GetCurrentProcess, GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN};

pub struct Vol {
    pub root: String,
    pub table: Arc<VolumeLock<Table>>,
}

fn memory() -> (f64, f64) {
    let mut c = PROCESS_MEMORY_COUNTERS_EX { cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
    // SAFETY: fills a local struct of the size given.
    let _ = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut c as *mut _ as *mut PROCESS_MEMORY_COUNTERS, c.cb) };
    (c.PrivateUsage as f64 / 1048576.0, c.WorkingSetSize as f64 / 1048576.0)
}

/// Loads and follows one volume forever (reloads when the journal is lost).
fn keep(letter: char, vol: Arc<Vol>, loaded: std::sync::mpsc::Sender<()>) {
    let mut first = true;
    loop {
        let r = (|| -> windows_core::Result<windows_core::Error> {
            let v = volume::Volume::open(letter)?;
            let j = v.journal()?;
            let t0 = Instant::now();
            let volume::Mft { names, sequences, pruned } = v.load(j.NextUsn)?;
            let id = names.id();
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            let (n, bytes) = (names.len(), names.bytes());
            let hydration = Hydration::publish(vol.table.clone(), names);
            log::line(&format!("{letter}: MFT read: {n} names in {ms:.0} ms, table {:.1} MB ({:.1} bytes a name)", bytes as f64 / 1048576.0, bytes as f64 / n.max(1) as f64));
            if first {
                let _ = loaded.send(());
                first = false;
            }
            if !pruned.is_empty() {
                match v.duplicate() {
                    Ok(handle) => links_later(letter, handle, pruned, vol.clone(), id),
                    Err(e) => log::line(&format!("{letter}: hard links: duplicate volume handle failed: {e}")),
                }
            }
            metadata::spawn(letter, v.device_path(), sequences, hydration);
            Ok(v.follow(j.UsnJournalID, j.NextUsn, &vol.table))
        })();
        match r {
            Ok(e) => log::line(&format!("{letter}: journal read stopped ({e}), reloading")),
            Err(e) => log::line(&format!("{letter}: {e}, retry in 30 s")),
        }
        if !volume::fixed_ntfs().contains(&letter) {
            log::line(&format!("{letter}: volume gone, no longer served"));
            VOLS.write().unwrap_or_else(|p| p.into_inner()).retain(|(c, _)| *c != letter);
            if first {
                let _ = loaded.send(());
            }
            return;
        }
        if first {
            let _ = loaded.send(());
            first = false;
        }
        std::thread::sleep(Duration::from_secs(30));
    }
}

/// Looks up the outside names of the pruned files on a background thread of its own, so the
/// follower starts at once (the lookup opens each file by id).
fn links_later(letter: char, handle: volume::Volume, frns: Vec<u64>, vol: Arc<Vol>, id: u64) {
    let _ = std::thread::Builder::new().name(format!("index-{letter}-links")).spawn(move || {
        // SAFETY: lowers this thread's CPU and I/O priority for the lookup.
        let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) };
        let t0 = Instant::now();
        let (looked, added, failed) = volume::add_links(handle.handle(), &frns, &vol.table, id);
        let mut table = vol.table.write();
        if table.id() != id {
            return;
        }
        table.shrink();
        let (bytes, names) = (table.bytes(), table.len());
        drop(table);
        log::line(&format!(
            "{letter}: hard links: {looked} files inside WinSxS looked up in {:.0} ms, {added} names outside it added, {failed} not opened; {names} names, table {:.1} MB",
            t0.elapsed().as_secs_f64() * 1000.0,
            bytes as f64 / 1048576.0
        ));
    });
}

fn answer(vols: &[Arc<Vol>], q: &str, max: u16, scope: Option<&str>) -> (Reply, bool) {
    let t = Instant::now();
    let fq = fold::fold(q.trim());
    let mut all: Vec<(SortKey, Hit)> = Vec::new();
    let mut names = 0;
    let mut covered = false;
    for v in vols {
        if scope.is_some_and(|path| !path.get(..3).is_some_and(|drive| drive.eq_ignore_ascii_case(&v.root))) {
            continue;
        }
        let tb = v.table.read();
        names += tb.len();
        let found = if let Some(path) = scope {
            let Some(found) = tb.query_scope(&fq, max as usize, &v.root, path) else { break };
            covered = true;
            found
        } else {
            // A few extra: some hits have no path (a folder of theirs is metadata).
            tb.query(&fq, max as usize + 8)
        };
        for f in found {
            if let Some(path) = tb.path(f.rec, &v.root) {
                all.push((f.key, Hit { path, folder: tb.is_dir(f.rec), noisy: f.noisy(), rank: f.rank(), modified: f.modified() }));
            }
        }
    }
    all.sort_by_key(|(k, _)| *k);
    all.truncate(max as usize);
    (Reply { hits: all.into_iter().map(|(_, h)| h).collect(), micros: t.elapsed().as_micros() as u32, names: names as u32 }, covered)
}

fn pages(pipe: &mut File, request: &Request) -> io::Result<()> {
    let Some(path) = request.scope.as_deref() else { return Ok(()) };
    let volume = VOLS.read().ok().and_then(|volumes| {
        volumes.iter().map(|(_, v)| v).find(|v| path.get(..3).is_some_and(|drive| drive.eq_ignore_ascii_case(&v.root))).cloned()
    });
    let Some(volume) = volume else { return proto::write_reply(pipe, &Reply::default(), request.version) };
    let t = Instant::now();
    let folded = fold::fold(request.query.trim());
    if folded.is_empty() || folded.as_bytes().contains(&fsindex::table::SEP) {
        return proto::write_page(pipe, &Reply::default(), false, false);
    }
    let finder = memmem::Finder::new(folded.as_bytes());
    let (mut scan, names, gaps) = {
        let table = volume.table.read();
        let Some(scan) = table.begin_scoped_scan(&volume.root, path) else {
            drop(table);
            return proto::write_reply(pipe, &Reply::default(), request.version);
        };
        let gaps = scan.gaps(&table, &volume.root);
        (scan, table.len() as u32, gaps)
    };
    let limit = usize::from(request.max.max(1));
    let mut reply = Reply { hits: Vec::with_capacity(limit), names, micros: 0 };
    loop {
        if !pipe::client_present(pipe) {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let done = {
            let table = volume.table.read();
            scan.step(&table, &volume.root, &finder, limit, &mut reply.hits)
        };
        let Some(done) = done else {
            return Err(io::ErrorKind::ConnectionAborted.into());
        };
        if done || reply.hits.len() == limit {
            reply.micros = t.elapsed().as_micros().min(u128::from(u32::MAX)) as u32;
            proto::write_page(pipe, &reply, !done || !gaps.is_empty(), false)?;
            reply.hits.clear();
        }
        if done { break }
    }
    let mut gaps = gaps.into_iter().peekable();
    while gaps.peek().is_some() {
        reply.hits.extend(gaps.by_ref().take(limit).map(|path| Hit { path, folder: true, noisy: false, rank: 0, modified: 0 }));
        proto::write_page(pipe, &reply, gaps.peek().is_some(), true)?;
        reply.hits.clear();
    }
    Ok(())
}

/// The volumes being served; a fixed volume that arrives later joins through `rescan`.
static VOLS: RwLock<Vec<(char, Arc<Vol>)>> = RwLock::new(Vec::new());
/// Reports each new volume's first load (the startup summary counts them).
static LOADED: Mutex<Option<std::sync::mpsc::Sender<()>>> = Mutex::new(None);

fn add(c: char) -> Arc<Vol> {
    let v = Arc::new(Vol { root: format!("{c}:\\"), table: Arc::new(VolumeLock::new(Table::new())) });
    let (tx, _rx) = std::sync::mpsc::channel();
    let tx = LOADED.lock().ok().and_then(|l| l.clone()).unwrap_or(tx);
    let v2 = v.clone();
    let _ = std::thread::Builder::new().name(format!("index-{c}")).spawn(move || keep(c, v2, tx));
    VOLS.write().unwrap_or_else(|p| p.into_inner()).push((c, v.clone()));
    v
}

/// Starts following the fixed NTFS volumes not served yet (a disk attached after start); returns
/// how many were added.
pub fn rescan() -> usize {
    let have: Vec<char> = VOLS.read().map(|v| v.iter().map(|(c, _)| *c).collect()).unwrap_or_default();
    let new: Vec<char> = volume::fixed_ntfs().into_iter().filter(|c| !have.contains(c)).collect();
    for &c in &new {
        log::line(&format!("index: new fixed NTFS volume {c}:"));
        add(c);
    }
    new.len()
}

/// Runs the index; `ready` is called at once (the service reports running and answers from what
/// is loaded); the startup summary is logged when every volume's first load ended.
pub fn run(ready: Option<&dyn Fn()>) {
    let letters = volume::fixed_ntfs();
    log::line(&format!("index: fixed NTFS volumes {letters:?}"));
    let t0 = Instant::now();
    let (tx, rx) = std::sync::mpsc::channel();
    if let Ok(mut l) = LOADED.lock() {
        *l = Some(tx);
    }
    for &c in &letters {
        add(c);
    }
    let n = letters.len();
    let _ = std::thread::Builder::new().name("index-ready".into()).spawn(move || {
        for _ in 0..n {
            let _ = rx.recv();
        }
        let (private, ws) = memory();
        let names: usize = VOLS.read().map(|v| v.iter().map(|(_, v)| v.table.read().len()).sum()).unwrap_or(0);
        log::line(&format!("index: ready, {names} names on {n} volumes in {:.0} ms, process private {private:.1} MB, working set {ws:.1} MB", t0.elapsed().as_secs_f64() * 1000.0));
        // Later arrivals log their own MFT read line.
        loop {
            if rx.recv().is_err() {
                break;
            }
            let (private, ws) = memory();
            log::line(&format!("index: volume added, process private {private:.1} MB, working set {ws:.1} MB"));
        }
    });
    if let Some(f) = ready {
        f();
    }
    pipe::serve(Arc::new(move |q: &str, max: u16, scope: Option<&str>| {
        let vols: Vec<Arc<Vol>> = VOLS.read().map(|v| v.iter().map(|(_, v)| v.clone()).collect()).unwrap_or_default();
        answer(&vols, q, max, scope)
    }), pages);
}
