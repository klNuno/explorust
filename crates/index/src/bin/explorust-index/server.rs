//! The index: one table per fixed NTFS volume, loaded from the MFT then kept current from the
//! journal on a thread per volume, and the pipe that answers queries from them.

use crate::{log, pipe, volume};
use fsindex::fold;
use fsindex::proto::{Hit, Reply};
use fsindex::table::Table;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::GetCurrentProcess;

pub struct Vol {
    pub root: String,
    pub table: RwLock<Table>,
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
            let t = v.load(j.NextUsn)?;
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            let (n, bytes) = (t.len(), t.bytes());
            *vol.table.write().unwrap_or_else(|p| p.into_inner()) = t;
            log::line(&format!("{letter}: MFT read: {n} names in {ms:.0} ms, table {:.1} MB ({:.1} bytes a name)", bytes as f64 / 1048576.0, bytes as f64 / n.max(1) as f64));
            if first {
                let _ = loaded.send(());
                first = false;
            }
            Ok(v.follow(j.UsnJournalID, j.NextUsn, &vol.table))
        })();
        match r {
            Ok(e) => log::line(&format!("{letter}: journal read stopped ({e}), reloading")),
            Err(e) => log::line(&format!("{letter}: {e}, retry in 30 s")),
        }
        if first {
            let _ = loaded.send(());
            first = false;
        }
        std::thread::sleep(Duration::from_secs(30));
    }
}

fn answer(vols: &[Arc<Vol>], q: &str, max: u16) -> Reply {
    let t = Instant::now();
    let fq = fold::fold(q.trim());
    let mut all: Vec<(u64, Hit)> = Vec::new();
    let mut names = 0;
    for v in vols {
        let tb = v.table.read().unwrap_or_else(|p| p.into_inner());
        names += tb.len();
        // A few extra: some hits have no path (a folder of theirs is metadata).
        for f in tb.query(&fq, max as usize + 8) {
            if let Some(path) = tb.path(f.rec, &v.root) {
                all.push((f.key, Hit { path, folder: tb.is_dir(f.rec), noisy: f.noisy(), rank: f.rank() }));
            }
        }
    }
    all.sort_by_key(|(k, _)| *k);
    all.truncate(max as usize);
    Reply { hits: all.into_iter().map(|(_, h)| h).collect(), micros: t.elapsed().as_micros() as u32, names: names as u32 }
}

/// Runs the index; `ready` is called once every volume's first load ended (the service reports
/// running at once and answers from what is loaded).
pub fn run(ready: Option<&dyn Fn()>) {
    let letters = volume::fixed_ntfs();
    log::line(&format!("index: fixed NTFS volumes {letters:?}"));
    let t0 = Instant::now();
    let (tx, rx) = std::sync::mpsc::channel();
    let vols: Vec<Arc<Vol>> = letters
        .iter()
        .map(|&c| {
            let v = Arc::new(Vol { root: format!("{c}:\\"), table: RwLock::new(Table::new()) });
            let (v2, tx) = (v.clone(), tx.clone());
            let _ = std::thread::Builder::new().name(format!("index-{c}")).spawn(move || keep(c, v2, tx));
            v
        })
        .collect();
    let vols = Arc::new(vols);
    let v2 = vols.clone();
    let n = letters.len();
    let _ = std::thread::Builder::new().name("index-ready".into()).spawn(move || {
        for _ in 0..n {
            let _ = rx.recv();
        }
        let (private, ws) = memory();
        let names: usize = v2.iter().map(|v| v.table.read().map(|t| t.len()).unwrap_or(0)).sum();
        log::line(&format!("index: ready, {names} names on {n} volumes in {:.0} ms, process private {private:.1} MB, working set {ws:.1} MB", t0.elapsed().as_secs_f64() * 1000.0));
    });
    if let Some(f) = ready {
        f();
    }
    let v3 = vols.clone();
    pipe::serve(Arc::new(move |q: &str, max: u16| answer(&v3, q, max)));
}
