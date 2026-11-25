use super::*;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter};
use std::os::fd::AsRawFd;
use std::path::Path;

pub(super) fn reload(table: Table, path: &Path, needle: &str, known: u32) {
    let expected = table.path(known, "C:\\").unwrap();
    let cursor = SnapshotCursor {
        journal: 17,
        next: 23,
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
