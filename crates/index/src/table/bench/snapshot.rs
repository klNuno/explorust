use super::*;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter};
use std::os::fd::AsRawFd;
use std::path::Path;

pub(super) fn reconcile_names(mut table: Table, count: u32, known: u32, needle: &str) {
    use crate::usn::{ATTR_DIRECTORY, ATTR_HIDDEN, Record};
    use parking_lot::RwLock;
    use std::sync::atomic::{AtomicBool, Ordering};

    table.remove(ROOT);
    table.compact();
    table.shrink();
    let mut sequences = vec![7; table.slots.len()];
    let table = RwLock::new(table);
    let before = private_kib().unwrap();
    let reset_high_water = fs::write("/proc/self/clear_refs", b"5").is_ok();
    let done = AtomicBool::new(false);
    let (elapsed, sampled_peak) = std::thread::scope(|scope| {
        let sampler = scope.spawn(|| {
            let mut peak = before;
            while !done.load(Ordering::Relaxed) {
                peak = peak.max(private_kib().unwrap());
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            peak.max(private_kib().unwrap())
        });
        let clock = Instant::now();
        let mut records = Vec::with_capacity(1024);
        reconcile(&table, &mut sequences, &mut Vec::new(), |dirs, consume| {
            let files = if dirs { 0..140_000 } else { 140_000..count - 2 };
            for i in files {
                let (rec, parent, mut name, dir) = synthetic_name(i);
                if i == count - 3 {
                    name = "renamed-offline.txt".to_owned();
                }
                records.push(Record {
                    rec: rec.into(),
                    seq: 7,
                    parent: parent.into(),
                    usn: 0,
                    reason: 0,
                    attrs: if dir { ATTR_DIRECTORY } else { 0 } | if i % 91 == 0 { ATTR_HIDDEN } else { 0 },
                    name,
                });
                if records.len() == records.capacity() {
                    consume(&records);
                    records.clear();
                }
            }
            consume(&records);
            records.clear();
            Ok::<_, ()>(())
        }).unwrap();
        let elapsed = clock.elapsed();
        done.store(true, Ordering::Relaxed);
        (elapsed, sampler.join().unwrap())
    });
    let table = table.read();
    assert_eq!(table.len(), (count - 2) as usize);
    assert_eq!(table.query("renamed-offline.txt", 1).len(), 1);
    assert!(table.query(needle, 1).is_empty());
    assert_eq!(sequences[known as usize], 0);
    let status = fs::read_to_string("/proc/self/status").unwrap();
    let high_water = status.lines().find(|line| line.starts_with("VmHWM:")).unwrap();
    eprintln!("reconcile_ms={:.3} private_before_kib={before} private_peak_kib={sampled_peak} reset_high_water={reset_high_water} {high_water} verified_removed=1 verified_renamed=1", elapsed.as_secs_f64() * 1000.0);
}

pub(super) fn reload(table: Table, path: &Path, needle: &str, known: u32) {
    let expected = table.path(known, "C:\\").unwrap();
    let cursor = SnapshotCursor {
        journal: 17,
        next: 23,
        boot: [1; 16],
    };
    let clock = Instant::now();
    let mut writer = BufWriter::new(File::create_new(path).unwrap());
    Snapshot::write(
        &mut writer,
        &table,
        cursor,
        &vec![0; table.slots.len()],
        &[],
    )
    .unwrap();
    let file = writer.into_inner().unwrap();
    file.sync_all().unwrap();
    let bytes = file.metadata().unwrap().len();
    eprintln!(
        "snapshot_write_ms={:.3} snapshot_bytes={bytes}",
        clock.elapsed().as_secs_f64() * 1000.0
    );
    drop(file);
    drop(table);

    for round in 1..=5 {
        let file = File::open(path).unwrap();
        evict(&file);
        let disk_before = disk_reads();
        let clock = Instant::now();
        let snapshot = Snapshot::read_if(BufReader::new(file), bytes, |c| c == cursor)
            .unwrap()
            .unwrap();
        let reload_ms = clock.elapsed().as_secs_f64() * 1000.0;
        let hits = snapshot.names.query(needle, 32);
        assert_eq!(hits.len(), 1);
        assert_eq!(
            snapshot.names.path(hits[0].rec, "C:\\").as_deref(),
            Some(expected.as_str())
        );
        assert_eq!(snapshot.sequences.len(), snapshot.names.slots.len());
        let first_ms = clock.elapsed().as_secs_f64() * 1000.0;
        let physical = disk_reads()
            .zip(disk_before)
            .map(|(a, b)| a.saturating_sub(b));
        eprintln!(
            "snapshot_round={round} reload_ms={reload_ms:.3} first_narrow_answer_ms={first_ms:.3} physical_read_bytes={physical:?}"
        );
    }
    fs::remove_file(path).unwrap();
}

fn evict(file: &File) {
    unsafe extern "C" {
        fn posix_fadvise(fd: i32, offset: i64, len: i64, advice: i32) -> i32;
    }
    // SAFETY: the live file descriptor belongs to this benchmark. DONTNEED applies only to it.
    assert_eq!(unsafe { posix_fadvise(file.as_raw_fd(), 0, 0, 4) }, 0);
}

fn disk_reads() -> Option<u64> {
    let data = fs::read_to_string("/proc/self/io").ok()?;
    data.lines()
        .find_map(|line| line.strip_prefix("read_bytes: ")?.parse().ok())
}
