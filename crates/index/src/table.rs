//! The names of one volume in a compact table. Each MFT record number indexes an 8-byte slot
//! (parent record, name index with flags); per name, `meta` holds the noisy bit and recency, so a
//! query reads only sequential arrays until its best hits are known; the names live once, folded (`fold`, each
//! name after a `\n`), with one bit per byte (`case`) marking the characters to raise back. A prefix
//! query is one pass over the name starts, a substring query one `memmem` of the query over the
//! folded arena; an offset maps back to its name through `starts` (sorted, binary search).

use crate::fold;
use memchr::memmem;

pub const SEP: u8 = b'\n';
/// The root directory's MFT record number on NTFS.
pub const ROOT: u32 = 5;
const NONE: u32 = u32::MAX;
const DIR: u32 = 1 << 31;
const NOISY: u32 = 1 << 30;
/// The record or one of its folders is noisy: set from the parent on insert, and for the whole
/// table by `settle` after the MFT load (records come in record order, children before parents).
const UNDER: u32 = 1 << 29;
const IDX: u32 = (1 << 29) - 1;

#[derive(Clone, Copy)]
struct Slot {
    parent: u32,
    /// Name index in the arenas plus DIR and NOISY, or NONE for a free slot.
    name: u32,
}

const FREE: Slot = Slot { parent: 0, name: NONE };
/// A record inside a pruned tree: no name kept, and its children are pruned too.
const PRUNED: Slot = Slot { parent: NONE, name: NONE };
/// Folders whose contents are left out of the table: the component store holds a large share of a
/// Windows volume's names (long ones), none of them anything a user opens.
const PRUNE_NAMES: [&str; 1] = ["winsxs"];
/// In `meta`: the name's record or a folder of it is noisy; the other bits are the last USN of
/// the file shifted right 16 bits (larger is more recent).
const META_NOISY: u32 = 1 << 31;

/// Folder names whose contents rank after everything else (Windows Search leaves them out).
const NOISY_NAMES: [&str; 9] = ["windows", "appdata", "$recycle.bin", "winsxs", "node_modules", ".git", "programdata", "system volume information", "target"];

#[derive(Default)]
pub struct Table {
    slots: Vec<Slot>,
    case: Vec<u8>,
    fold: Vec<u8>,
    starts: Vec<u32>,
    recs: Vec<u32>,
    meta: Vec<u32>,
    live: usize,
    dead: usize,
    /// Records of the folders named in PRUNE_NAMES (their contents are not kept).
    prune_roots: Vec<u32>,
}

/// A match before its path is built.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Found {
    /// Sort key: noisy, then rank (0 name prefix, 1 word prefix, 2 inside), then most recent.
    pub key: u64,
    pub rec: u32,
}

impl Found {
    pub fn rank(&self) -> u8 {
        ((self.key >> 32) & 0xFF) as u8
    }
    pub fn noisy(&self) -> bool {
        self.key >> 40 != 0
    }
}

fn key(noisy: bool, rank: u8, usn: u32) -> u64 {
    ((noisy as u64) << 40) | ((rank as u64) << 32) | (u32::MAX - usn) as u64
}

fn word_start(prev: u8) -> bool {
    matches!(prev, b' ' | b'.' | b'-' | b'_' | b'(' | b'[' | b'+' | b',')
}

impl Table {
    pub fn new() -> Table {
        Table { case: vec![0], fold: vec![SEP], ..Default::default() }
    }

    pub fn len(&self) -> usize {
        self.live
    }

    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// Heap bytes held by the table.
    pub fn bytes(&self) -> usize {
        self.slots.capacity() * size_of::<Slot>() + self.case.capacity() + self.fold.capacity() + (self.starts.capacity() + self.recs.capacity() + self.meta.capacity()) * 4
    }

    pub fn shrink(&mut self) {
        self.slots.shrink_to_fit();
        self.case.shrink_to_fit();
        self.fold.shrink_to_fit();
        self.starts.shrink_to_fit();
        self.recs.shrink_to_fit();
        self.meta.shrink_to_fit();
    }

    fn name_range(&self, i: usize) -> (usize, usize) {
        let s = self.starts[i] as usize;
        let e = self.starts.get(i + 1).map_or(self.fold.len(), |&n| n as usize) - 1;
        (s, e)
    }

    fn changed(&self, at: usize) -> bool {
        self.case[at >> 3] & (1 << (at & 7)) != 0
    }

    fn mark(&mut self, at: usize) {
        if self.case.len() <= at >> 3 {
            self.case.resize((at >> 3) + 1, 0);
        }
        self.case[at >> 3] |= 1 << (at & 7);
    }

    fn grow_case(&mut self) {
        let need = self.fold.len().div_ceil(8);
        if self.case.len() < need {
            self.case.resize(need, 0);
        }
    }

    pub fn name(&self, rec: u32) -> Option<String> {
        let s = self.slots.get(rec as usize)?;
        if s.name == NONE {
            return None;
        }
        let (a, b) = self.name_range((s.name & IDX) as usize);
        let f = std::str::from_utf8(&self.fold[a..b]).ok()?;
        Some(crate::fold::unfold(f, |i| self.changed(a + i)))
    }

    pub fn is_dir(&self, rec: u32) -> bool {
        self.slots.get(rec as usize).is_some_and(|s| s.name != NONE && s.name & DIR != 0)
    }

    /// Adds or updates record `rec`: a new name or parent replaces the old one.
    pub fn upsert(&mut self, rec: u32, parent: u32, name: &str, dir: bool, attrs_noisy: bool, usn: i64) {
        if rec == NONE || name.is_empty() || name.contains('\n') {
            return;
        }
        let r = rec as usize;
        if r >= self.slots.len() {
            self.slots.resize(r + 1, FREE);
        }
        if self.prune_roots.contains(&parent) || self.slots.get(parent as usize).is_some_and(|p| p.parent == NONE && p.name == NONE) {
            self.remove(rec);
            self.slots[r] = PRUNED;
            return;
        }
        if dir && PRUNE_NAMES.iter().any(|n| n.eq_ignore_ascii_case(name)) && !self.prune_roots.contains(&rec) {
            self.prune_roots.push(rec);
        }
        let usn = ((usn.max(0) as u64) >> 16).min((META_NOISY - 1) as u64) as u32;
        let noisy = attrs_noisy || name.starts_with('$') || NOISY_NAMES.iter().any(|n| n.eq_ignore_ascii_case(name));
        let under = noisy || self.slots.get(parent as usize).is_some_and(|p| p.name != NONE && p.name & UNDER != 0);
        let flags = if dir { DIR } else { 0 } | if noisy { NOISY } else { 0 } | if under { UNDER } else { 0 };
        let old = self.slots[r];
        let mut old_usn = 0;
        if old.name != NONE {
            let oi = (old.name & IDX) as usize;
            if old.parent == parent && self.name(rec).as_deref() == Some(name) {
                self.slots[r].name = (old.name & IDX) | flags;
                let m = &mut self.meta[oi];
                *m = (*m & !META_NOISY).max(usn) | if under { META_NOISY } else { 0 };
                return;
            }
            old_usn = self.meta[oi] & !META_NOISY;
            self.recs[(old.name & IDX) as usize] = NONE;
            self.dead += 1;
            self.live -= 1;
        }
        let idx = self.starts.len() as u32;
        self.starts.push(self.fold.len() as u32);
        self.recs.push(rec);
        self.meta.push(usn.max(old_usn) | if under { META_NOISY } else { 0 });
        let base = self.fold.len();
        let mut marks = Vec::new();
        fold::push_folded(name, &mut self.fold, |i| marks.push(base + i));
        self.fold.push(SEP);
        self.grow_case();
        for m in marks {
            self.mark(m);
        }
        self.slots[r] = Slot { parent, name: idx | flags };
        self.live += 1;
    }

    pub fn remove(&mut self, rec: u32) {
        let Some(s) = self.slots.get_mut(rec as usize) else { return };
        if s.name == NONE {
            return;
        }
        self.recs[(s.name & IDX) as usize] = NONE;
        *s = FREE;
        self.dead += 1;
        self.live -= 1;
        if self.dead > 200_000 && self.dead > self.live {
            self.compact();
        }
    }

    /// Rewrites the arenas without the names of deleted or renamed records.
    pub fn compact(&mut self) {
        let mut fold = Vec::with_capacity(self.fold.len().saturating_sub(self.dead * 8));
        fold.push(SEP);
        let mut case = vec![0u8; self.case.len()];
        let mut starts = Vec::with_capacity(self.live);
        let mut recs = Vec::with_capacity(self.live);
        let mut meta = Vec::with_capacity(self.live);
        for i in 0..self.starts.len() {
            let rec = self.recs[i];
            if rec == NONE {
                continue;
            }
            let (a, b) = self.name_range(i);
            let slot = &mut self.slots[rec as usize];
            slot.name = (slot.name & !IDX) | starts.len() as u32;
            starts.push(fold.len() as u32);
            recs.push(rec);
            meta.push(self.meta[i]);
            let base = fold.len();
            for k in a..b {
                if self.changed(k) {
                    let at = base + k - a;
                    case[at >> 3] |= 1 << (at & 7);
                }
            }
            fold.extend_from_slice(&self.fold[a..=b]);
        }
        case.truncate(fold.len().div_ceil(8));
        (self.case, self.fold, self.starts, self.recs, self.meta, self.dead) = (case, fold, starts, recs, meta, 0);
    }

    /// Sets the inherited noisy bit of every record from its folders and drops the names inside
    /// pruned folders (after the MFT load); returns how many names were dropped.
    pub fn settle(&mut self) -> usize {
        let mut pruned = 0;
        if !self.prune_roots.is_empty() {
            for r in 0..self.slots.len() as u32 {
                if self.slots[r as usize].name != NONE && self.walk_pruned(r) {
                    self.recs[(self.slots[r as usize].name & IDX) as usize] = NONE;
                    self.slots[r as usize] = PRUNED;
                    self.dead += 1;
                    self.live -= 1;
                    pruned += 1;
                }
            }
            if pruned > 0 {
                self.compact();
            }
        }
        for r in 0..self.slots.len() as u32 {
            if self.slots[r as usize].name == NONE {
                continue;
            }
            let u = self.walk_noisy(r);
            let s = &mut self.slots[r as usize];
            s.name = if u { s.name | UNDER } else { s.name & !UNDER };
            let m = &mut self.meta[(s.name & IDX) as usize];
            *m = if u { *m | META_NOISY } else { *m & !META_NOISY };
        }
        pruned
    }

    /// True when a folder above the record is a prune root.
    fn walk_pruned(&self, rec: u32) -> bool {
        let mut r = rec;
        for _ in 0..64 {
            let Some(s) = self.slots.get(r as usize) else { return false };
            if s.name == NONE || r == ROOT || s.parent == r {
                return false;
            }
            let p = self.slots.get(s.parent as usize).copied().unwrap_or(FREE);
            if self.prune_roots.contains(&s.parent) || (p.parent == NONE && p.name == NONE) {
                return true;
            }
            r = s.parent;
        }
        false
    }

    /// True when the record or one of its folders is noisy, by walking up.
    fn walk_noisy(&self, mut rec: u32) -> bool {
        for _ in 0..64 {
            let Some(s) = self.slots.get(rec as usize) else { return false };
            if s.name == NONE {
                return false;
            }
            if s.name & NOISY != 0 {
                return true;
            }
            if rec == ROOT || s.parent == rec {
                return false;
            }
            rec = s.parent;
        }
        false
    }

    /// The path of `rec` below the volume root, with `\` separators, or None when a folder of it
    /// is missing.
    pub fn path(&self, rec: u32, root: &str) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        let mut r = rec;
        for _ in 0..256 {
            if r == ROOT {
                let mut p = root.to_string();
                for (i, n) in parts.iter().rev().enumerate() {
                    if i > 0 {
                        p.push('\\');
                    }
                    p.push_str(n);
                }
                return Some(p);
            }
            let s = self.slots.get(r as usize)?;
            if s.name == NONE {
                return None;
            }
            parts.push(self.name(r)?);
            if s.parent == r {
                return None;
            }
            r = s.parent;
        }
        None
    }

    /// The index of the name holding offset `pos`, searching forward from name `from` (matches
    /// come in order, mostly close together): gallop, then binary search.
    fn seek(&self, from: usize, pos: usize) -> usize {
        let n = self.starts.len();
        if n == 0 {
            return 0;
        }
        let (mut lo, mut step) = (from.min(n - 1), 1usize);
        let mut hi = lo;
        while hi < n && self.starts[hi] as usize <= pos {
            lo = hi;
            hi += step;
            step *= 2;
        }
        let hi = hi.min(n);
        lo + self.starts[lo..hi].partition_point(|&s| s as usize <= pos).saturating_sub(1)
    }

    /// The best `max` matches of the folded query `q`, sorted.
    pub fn query(&self, q: &str, max: usize) -> Vec<Found> {
        let q = q.as_bytes();
        if q.is_empty() || q.contains(&SEP) || max == 0 {
            return Vec::new();
        }
        let mut top = Top::new(max);
        // Name prefixes: one pass over the name starts, cheaper than memmem when most names match.
        let (q0, n) = (q[0], q.len());
        for (i, &st) in self.starts.iter().enumerate() {
            let st = st as usize;
            if self.fold[st] == q0 && self.fold.get(st..st + n) == Some(q) && self.recs[i] != NONE {
                top.offer(meta_key(self.meta[i], 0), i);
            }
        }
        // A word or inner match ranks after every clean name prefix: skip when those fill the list.
        // Past INNER_CAP inner matches the rest is left out (one-letter queries on a big volume).
        if !top.full_below(key(false, 1, 0)) {
            let finder = memmem::Finder::new(q);
            let (mut at, mut i, mut seen) = (0, 0, 0usize);
            while let Some(p) = finder.find(&self.fold[at..]) {
                let m = at + p;
                i = self.seek(i, m);
                let (s, e) = self.name_range(i);
                at = e + 1;
                if m == s || self.recs[i] == NONE {
                    continue;
                }
                let rank = if word_start(self.fold[m - 1]) { 1 } else { 2 };
                top.offer(meta_key(self.meta[i], rank), i);
                seen += 1;
                if seen >= INNER_CAP {
                    break;
                }
            }
        }
        let mut out: Vec<Found> = top.heap.into_iter().map(|(k, i)| Found { key: k, rec: self.recs[i] }).collect();
        out.sort_unstable();
        out
    }
}

/// Inner matches looked at, at most, per query.
const INNER_CAP: usize = 60_000;

fn meta_key(meta: u32, rank: u8) -> u64 {
    key(meta & META_NOISY != 0, rank, meta & !META_NOISY)
}

/// The best `max` keys seen so far (a max-heap: the worst kept on top).
struct Top {
    heap: std::collections::BinaryHeap<(u64, usize)>,
    max: usize,
}

impl Top {
    fn new(max: usize) -> Top {
        Top { heap: std::collections::BinaryHeap::with_capacity(max + 1), max }
    }

    fn offer(&mut self, k: u64, i: usize) {
        if self.heap.len() < self.max {
            self.heap.push((k, i));
        } else if self.heap.peek().is_some_and(|w| k < w.0) {
            self.heap.pop();
            self.heap.push((k, i));
        }
    }

    /// Full of keys all better than `k`.
    fn full_below(&self, k: u64) -> bool {
        self.heap.len() >= self.max && self.heap.peek().is_some_and(|w| w.0 < k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Table {
        let mut t = Table::new();
        t.upsert(ROOT, ROOT, ".", true, false, 0);
        t.upsert(30, ROOT, "Users", true, false, 10);
        t.upsert(31, 30, "agent", true, false, 10);
        t.upsert(32, 31, "Documents", true, false, 10);
        t.upsert(33, 32, "Rapport final.docx", false, false, 5 << 16);
        t.upsert(34, 32, "final-notes.txt", false, false, 9 << 16);
        t.upsert(35, 31, "AppData", true, true, 10);
        t.upsert(36, 35, "final cache.bin", false, false, 99 << 16);
        t.upsert(37, 32, "unfinalized.md", false, false, 1 << 16);
        t
    }

    #[test]
    fn paths() {
        let t = sample();
        assert_eq!(t.path(33, "C:\\").as_deref(), Some("C:\\Users\\agent\\Documents\\Rapport final.docx"));
        assert_eq!(t.path(30, "C:\\").as_deref(), Some("C:\\Users"));
        assert!(t.is_dir(32));
    }

    #[test]
    fn ranking() {
        let t = sample();
        let got: Vec<(u32, u8, bool)> = t.query("final", 10).iter().map(|f| (f.rec, f.rank(), f.noisy())).collect();
        // Prefix first, then word prefix, then inside; the AppData file last despite being newest.
        assert_eq!(got, vec![(34, 0, false), (33, 1, false), (37, 2, false), (36, 0, true)]);
        assert_eq!(t.query("FINAL", 10).len(), 0, "the caller folds the query");
        assert_eq!(t.query("final", 2).len(), 2);
    }

    #[test]
    fn rename_delete_compact() {
        let mut t = sample();
        t.upsert(34, 31, "Plan.txt", false, false, 20 << 16);
        assert!(t.query("final-notes", 5).is_empty());
        assert_eq!(t.query("plan", 5)[0].rec, 34);
        assert_eq!(t.path(34, "C:\\").as_deref(), Some("C:\\Users\\agent\\Plan.txt"));
        t.remove(33);
        assert!(t.query("rapport", 5).is_empty());
        let before = t.len();
        t.compact();
        assert_eq!(t.len(), before);
        assert_eq!(t.query("plan", 5)[0].rec, 34);
        assert_eq!(t.path(37, "C:\\").as_deref(), Some("C:\\Users\\agent\\Documents\\unfinalized.md"));
        assert_eq!(t.name(36).as_deref(), Some("final cache.bin"));
        assert_eq!(t.name(32).as_deref(), Some("Documents"));
    }

    #[test]
    fn winsxs_contents_pruned() {
        let mut t = sample();
        t.upsert(40, ROOT, "Windows", true, false, 1);
        // MFT order: a child can come before its folders.
        t.upsert(43, 42, "final.manifest", false, false, 1);
        t.upsert(42, 41, "amd64_final_31bf3856ad364e35", true, false, 1);
        t.upsert(41, 40, "WinSxS", true, false, 1);
        let before = t.len() - 2;
        assert_eq!(t.settle(), 2);
        assert_eq!(t.len(), before);
        assert!(t.name(41).is_some(), "the folder itself stays");
        assert!(t.query("amd64", 5).is_empty());
        // Journal changes inside the pruned tree stay out.
        t.upsert(44, 42, "final2.manifest", false, false, 2);
        t.upsert(45, 41, "x86_final", true, false, 2);
        t.upsert(46, 45, "final3.dll", false, false, 2);
        assert_eq!(t.len(), before);
        assert_eq!(t.query("final", 10).len(), 4);
        // A record moved out of the tree comes back.
        t.upsert(46, 32, "final3.dll", false, false, 3);
        assert_eq!(t.path(46, "C:\\").as_deref(), Some("C:\\Users\\agent\\Documents\\final3.dll"));
    }

    #[test]
    fn million_names_memory_and_speed() {
        // A synthetic volume: 1 M names in 20 k folders, about Windows' average name length.
        let mut t = Table::new();
        t.upsert(ROOT, ROOT, ".", true, false, 0);
        let words = ["report", "setup", "image", "Microsoft", "config", "data", "readme", "photo", "build", "notes"];
        let exts = ["txt", "dll", "png", "json", "exe", "md"];
        let n = 1_000_000u32;
        for i in 0..n {
            let rec = 16 + i;
            let dir = i < 20_000;
            let parent = if dir { ROOT } else { 16 + (i % 20_000) };
            let name = if dir {
                format!("{} folder {i}", words[i as usize % 10])
            } else {
                format!("{}_{}{}.{}", words[i as usize % 10], words[(i / 10) as usize % 10], i, exts[i as usize % 6])
            };
            t.upsert(rec, parent, &name, dir, false, i as i64 * 1000);
        }
        t.settle();
        t.shrink();
        let mb = t.bytes() as f64 / 1048576.0;
        eprintln!("table: {} names, {:.1} MB", t.len(), mb);
        assert!(mb < 60.0, "{mb} MB");
        let start = std::time::Instant::now();
        for q in ["r", "re", "rep", "repo", "report", "readme_photo5", "photo7"] {
            let tq = std::time::Instant::now();
            let f = t.query(q, 24);
            eprintln!("query {q}: {} in {:.2} ms", f.len(), tq.elapsed().as_secs_f64() * 1000.0);
            assert!(!f.is_empty());
        }
        eprintln!("all queries {:.1} ms", start.elapsed().as_secs_f64() * 1000.0);
    }
}
