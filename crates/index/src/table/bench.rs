use super::*;
use std::hint::black_box;
use std::time::Instant;

#[cfg(target_os = "linux")]
mod snapshot;

// cargo test --release -p fsindex --lib seven_million_names -- --ignored --nocapture
#[test]
#[ignore = "release benchmark with 7 million names"]
fn seven_million_names() {
    let vocabulary = [
        "report",
        "setup",
        "image",
        "Microsoft",
        "config",
        "data",
        "readme",
        "photo",
        "build",
        "notes",
        "Été",
        "Documents",
        "node_modules",
        "videos",
        "work",
        "packed",
    ];
    let extensions = ["txt", "dll", "png", "json", "exe", "md", "rs", "mp4"];
    let count = 7_000_000u32;
    let mut table = Table::new();
    let begin = Instant::now();
    table.upsert(ROOT, ROOT, ".", true, false, 0);
    for i in 0..count - 2 {
        let rec = 16 + i + i / 27;
        let word = vocabulary[i as usize % vocabulary.len()];
        let dir = i < 140_000;
        let parent = if i < 32 {
            ROOT
        } else if dir {
            16 + i / 9 + i / 9 / 27
        } else {
            let folder = (i.wrapping_mul(18371) % 139_968) + 32;
            16 + folder + folder / 27
        };
        let name = if dir {
            format!("{word} folder {i}")
        } else {
            let suffix = vocabulary[(i >> 9) as usize % vocabulary.len()];
            let ext = extensions[((i >> 5) ^ i) as usize % extensions.len()];
            format!("{word}_{suffix}-{i}.{ext}")
        };
        // Unhydrated files retain zero. The rest span several years at FILETIME precision.
        let modified = if dir || i % 13 == 0 {
            0
        } else {
            133_138_374_000_000_000 + i64::from(i) * 234_567_891
        };
        table.upsert(rec, parent, &name, dir, i % 91 == 0, modified);
    }
    let needle = "uniquenarrowneedle";
    let known = 16 + count + count / 27;
    table.upsert(known, ROOT, &format!("human-{needle}.txt"), false, false, 0);
    let built = begin.elapsed();
    table.settle();
    table.shrink();
    table.journal_current = true;
    table.finish_initial_links();
    assert_eq!(table.len(), count as usize);
    eprintln!(
        "build_ms={:.3} ready_ms={:.3} names={} arena_bytes={:?} private_kib={:?}",
        built.as_secs_f64() * 1000.0,
        begin.elapsed().as_secs_f64() * 1000.0,
        table.len(),
        table.arena_bytes(),
        private_kib()
    );

    let scope = table.path(16 + 3_521 + 3_521 / 27, "C:\\").unwrap();
    let finder = memmem::Finder::new(b"report");
    let mut timings = [Vec::new(), Vec::new(), Vec::new()];
    for i in 0..=20 {
        let start = Instant::now();
        let narrow = table.query(black_box(needle), 32);
        let narrow_ms = start.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(narrow.len(), 1);
        assert_eq!(table.path(narrow[0].rec, "C:\\"), table.path(known, "C:\\"));
        if i == 0 {
            eprintln!(
                "first_narrow_answer_ms={:.3}",
                begin.elapsed().as_secs_f64() * 1000.0
            );
        }
        let start = Instant::now();
        let broad = table.query(black_box("r"), 32);
        let paths = broad
            .iter()
            .filter_map(|hit| table.path(hit.rec, "C:\\"))
            .collect::<Vec<_>>();
        let broad_ms = start.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(paths.len(), 32);
        let start = Instant::now();
        let mut scan = table.begin_scoped_scan("C:\\", &scope).unwrap();
        let mut hits = Vec::with_capacity(128);
        loop {
            let done = scan.step(&table, "C:\\", &finder, 128, &mut hits).unwrap();
            if done || hits.len() == 128 {
                break;
            }
        }
        let scoped_ms = start.elapsed().as_secs_f64() * 1000.0;
        assert!(!hits.is_empty() && hits.iter().all(|hit| hit.path.starts_with(&scope)));
        if i != 0 {
            for (samples, ms) in timings.iter_mut().zip([narrow_ms, broad_ms, scoped_ms]) {
                samples.push(ms);
            }
        }
        if i == 0 {
            eprintln!(
                "answers narrow={} broad={} scoped={} broad_first={} scope={scope}",
                narrow.len(),
                broad.len(),
                hits.len(),
                paths[0]
            );
        }
    }
    for (test, mut samples) in ["narrow", "broad", "scoped_first_page"]
        .into_iter()
        .zip(timings)
    {
        samples.sort_unstable_by(f64::total_cmp);
        eprintln!(
            "{test} p50_ms={:.3} p95_ms={:.3} min_ms={:.3} max_ms={:.3}",
            (samples[9] + samples[10]) / 2.0,
            samples[18],
            samples[0],
            samples[19]
        );
    }

    #[cfg(target_os = "linux")]
    if let Some(path) = std::env::var_os("EXPLORUST_SNAPSHOT_BENCH") {
        table.journal_current = false;
        snapshot::reload(table, std::path::Path::new(&path), needle, known);
    }
}

fn private_kib() -> Option<usize> {
    let smaps = std::fs::read_to_string("/proc/self/smaps_rollup").ok()?;
    Some(
        smaps
            .lines()
            .filter(|line| line.starts_with("Private_Clean:") || line.starts_with("Private_Dirty:"))
            .filter_map(|line| line.split_whitespace().nth(1)?.parse::<usize>().ok())
            .sum(),
    )
}
