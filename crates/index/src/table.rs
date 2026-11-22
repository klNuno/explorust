//! The names of one volume in a compact table. Each MFT record number indexes an 8-byte slot
//! (parent record, name index with flags); per name, `meta` holds the noisy bit and recency, so a
//! query reads only sequential arrays until its best hits are known; the names live once, folded (`fold`, each
//! name after a `\n`), with one bit per byte (`case`) marking the characters to raise back. A prefix
//! query is one pass over the name starts, a substring query one `memmem` of the query over the
//! folded arena; an offset maps back to its name through `starts` (sorted, binary search).

use crate::fold;
use memchr::memmem;
use std::cmp::Reverse;
use std::collections::HashSet;

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
/// In `meta`: the name or a folder of it is noisy; the remaining bits hold last write time.
const META_NOISY: u64 = 1 << 63;

/// Folder names whose contents rank after everything else (Windows Search leaves them out).
const NOISY_NAMES: [&str; 9] = ["windows", "appdata", "$recycle.bin", "winsxs", "node_modules", ".git", "programdata", "system volume information", "target"];

#[derive(Default)]
pub struct Table {
    slots: Vec<Slot>,
    case: Vec<u8>,
    fold: Vec<u8>,
    starts: Vec<u32>,
    recs: Vec<u32>,
    meta: Vec<u64>,
    live: usize,
    dead: usize,
    /// The initial build grows geometrically until the table is trimmed for publication.
    compact_growth: bool,
    /// Records of the folders named in PRUNE_NAMES (their contents are not kept).
    prune_roots: Vec<u32>,
    /// Scoped coverage starts only after the initial scan, journal catch-up and alias discovery.
    pub journal_current: bool,
    pub initial_links_complete: bool,
    /// Parents seen with an alias this single-name table cannot guarantee. Keep them through
    /// the current scan so an old observation still routes the scope to Windows Search.
    alias_parents: HashSet<u32>,
    /// A journal change may have introduced a name in any directory until its links are read.
    pending_links: HashSet<u32>,
    /// Files pruned since the last `take_pruned_files`: a hard link of theirs may sit outside
    /// the pruned tree (System32 binaries are links into WinSxS), looked up by the service.
    pruned_files: Vec<u32>,
    /// This table's number: a table made by a reload has another, so work begun on the old one
    /// is not written into it.
    id: u64,
    /// Links lookups running on this table, and the records the journal touched while one ran:
    /// a lookup's result for such a record is stale.
    watchers: usize,
    /// Record -> the touch count when the journal last changed it; `touches` counts them.
    touched: std::collections::HashMap<u32, u64>,
    touches: u64,
}

/// What a record holds, compared across a hard link lookup made without the lock (compaction
/// renumbers names, so the name itself is compared).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecState {
    Free,
    Pruned,
    Named(u32, String),
}

static TABLES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// A match before its path is built.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Found {
    pub key: SortKey,
    pub rec: u32,
}

impl Found {
    pub fn rank(&self) -> u8 {
        self.key.0 & 3
    }
    pub fn noisy(&self) -> bool {
        self.key.0 >= 4
    }

    pub fn modified(&self) -> u64 {
        let (_, Reverse(modified)) = self.key;
        modified
    }
}

/// Clean before noisy, then match rank, then newest first at full FILETIME precision.
pub type SortKey = (u8, Reverse<u64>);

fn word_start(prev: u8) -> bool {
    matches!(prev, b' ' | b'.' | b'-' | b'_' | b'(' | b'[' | b'+' | b',')
}

/// The default Vec growth can double every arena at once on the first journal insert after
/// `shrink`. Give large arrays headroom for future writes without committing another table.
fn reserve_append<T>(vec: &mut Vec<T>, count: usize, compact: bool) {
    let required = vec.len() + count;
    if required > vec.capacity() {
        if compact {
            let headroom = (vec.capacity() / 16).max(4096 / size_of::<T>());
            vec.reserve_exact((vec.capacity() + headroom).max(required) - vec.len());
        } else {
            vec.reserve(count);
        }
    }
}

impl Table {
    pub fn new() -> Table {
        Table { case: vec![0], fold: vec![SEP], id: TABLES.fetch_add(1, std::sync::atomic::Ordering::Relaxed), ..Default::default() }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn state(&self, rec: u32) -> RecState {
        match self.slots.get(rec as usize) {
            None => RecState::Free,
            Some(s) if s.name != NONE => RecState::Named(s.parent, self.name(rec).unwrap_or_default()),
            Some(s) if s.parent == NONE => RecState::Pruned,
            Some(_) => RecState::Free,
        }
    }

    /// A file whose links were looked up and none is outside the pruned trees: its kept name is
    /// gone (renamed or unlinked into the tree), so it leaves the table as a pruned record.
    pub fn prune(&mut self, rec: u32) {
        if self.slots.get(rec as usize).is_some_and(|s| s.name != NONE && s.name & DIR == 0) {
            self.remove(rec);
            self.slots[rec as usize] = PRUNED;
        }
    }

    /// A links lookup begins (`true`) or ends on this table; the touched set lives while one runs.
    pub fn watch_touches(&mut self, on: bool) {
        if on {
            self.watchers += 1;
        } else {
            self.watchers = self.watchers.saturating_sub(1);
            if self.watchers == 0 {
                self.touched = Default::default();
            }
        }
    }

    /// The journal changed `rec` (called for every record it applies).
    pub fn touch(&mut self, rec: u32) {
        if self.watchers > 0 {
            self.touches += 1;
            self.touched.insert(rec, self.touches);
        }
    }

    /// Where the journal stands, for a lookup to snapshot with its records.
    pub fn touches(&self) -> u64 {
        self.touches
    }

    /// The journal changed `rec` after a lookup's snapshot taken at `since`.
    pub fn touched_since(&self, rec: u32, since: u64) -> bool {
        self.touched.get(&rec).is_some_and(|&n| n > since)
    }

    /// A rename or hard-link change can invalidate the kept name or expose a new alias.
    pub fn recheck_links(&mut self, rec: u32) {
        if self.slots.get(rec as usize).is_some_and(|s| s.parent == NONE || s.name != NONE && s.name & DIR == 0)
            && !self.pruned_files.contains(&rec) {
            self.pruned_files.push(rec);
        }
    }

    pub fn require_link_lookup(&mut self, rec: u32) {
        self.pending_links.insert(rec);
    }

    pub fn finish_link_lookup(&mut self, rec: u32) {
        self.pending_links.remove(&rec);
    }

    pub fn mark_alias_parents(&mut self, parents: impl IntoIterator<Item = u32>) {
        self.alias_parents.extend(parents);
    }

    pub fn len(&self) -> usize {
        self.live
    }

    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// Heap bytes held by the table.
    pub fn bytes(&self) -> usize {
        self.slots.capacity() * size_of::<Slot>() + self.case.capacity() + self.fold.capacity() + (self.starts.capacity() + self.recs.capacity()) * 4 + self.meta.capacity() * 8
    }

    pub fn shrink(&mut self) {
        self.compact_growth = true;
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

    pub fn has_name(&self, rec: u32) -> bool {
        self.slots.get(rec as usize).is_some_and(|s| s.name != NONE)
    }

    pub fn needs_modified(&self, rec: u32) -> bool {
        self.slots.get(rec as usize).is_some_and(|s| s.name != NONE && self.meta[(s.name & IDX) as usize] & !META_NOISY == 0)
    }

    /// Updates a named file's last write time. An unavailable read preserves the known time.
    pub fn set_modified(&mut self, rec: u32, modified: i64) -> bool {
        let Some(s) = self.slots.get(rec as usize).filter(|s| s.name != NONE && modified > 0) else { return false };
        let m = &mut self.meta[(s.name & IDX) as usize];
        *m = modified as u64 | (*m & META_NOISY);
        true
    }

    /// Files pruned since the last call (their record numbers), for the hard link lookup.
    pub fn take_pruned_files(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.pruned_files)
    }

    /// True when a name under `parent` would be left out (a prune root or inside one).
    pub fn pruned_parent(&self, parent: u32) -> bool {
        self.prune_roots.contains(&parent) || self.slots.get(parent as usize).is_some_and(|p| p.parent == NONE && p.name == NONE)
    }

    /// Adds or updates record `rec`: a new name or parent replaces the old one. A name inside a
    /// pruned tree does not replace a kept name outside it: a file with hard links on both sides
    /// keeps its outside one (the journal reports changes under either name).
    pub fn upsert(&mut self, rec: u32, parent: u32, name: &str, dir: bool, attrs_noisy: bool, modified: i64) {
        if rec == NONE || name.is_empty() || name.contains('\n') {
            return;
        }
        let r = rec as usize;
        if r >= self.slots.len() {
            let count = r + 1 - self.slots.len();
            reserve_append(&mut self.slots, count, self.compact_growth);
            self.slots.resize(r + 1, FREE);
        }
        if self.pruned_parent(parent) {
            if !dir && self.slots[r].name != NONE && !self.walk_pruned(rec) {
                self.set_modified(rec, modified);
                return;
            }
            self.remove(rec);
            self.slots[r] = PRUNED;
            if !dir {
                self.pruned_files.push(rec);
            }
            return;
        }
        if dir && PRUNE_NAMES.iter().any(|n| n.eq_ignore_ascii_case(name)) && !self.prune_roots.contains(&rec) {
            self.prune_roots.push(rec);
        }
        let noisy = attrs_noisy || name.starts_with('$') || NOISY_NAMES.iter().any(|n| n.eq_ignore_ascii_case(name));
        let under = noisy || self.slots.get(parent as usize).is_some_and(|p| p.name != NONE && p.name & UNDER != 0);
        let flags = if dir { DIR } else { 0 } | if noisy { NOISY } else { 0 } | if under { UNDER } else { 0 };
        let old = self.slots[r];
        let mut previous = 0;
        if old.name != NONE {
            let oi = (old.name & IDX) as usize;
            previous = self.meta[oi] & !META_NOISY;
            if old.parent == parent && self.name(rec).as_deref() == Some(name) {
                self.slots[r].name = (old.name & IDX) | flags;
                self.meta[oi] = write_time(modified, previous) | if under { META_NOISY } else { 0 };
                return;
            }
            self.recs[(old.name & IDX) as usize] = NONE;
            self.dead += 1;
            self.live -= 1;
        }
        let idx = self.starts.len() as u32;
        reserve_append(&mut self.starts, 1, self.compact_growth);
        reserve_append(&mut self.recs, 1, self.compact_growth);
        reserve_append(&mut self.meta, 1, self.compact_growth);
        self.starts.push(self.fold.len() as u32);
        self.recs.push(rec);
        self.meta.push(write_time(modified, previous) | if under { META_NOISY } else { 0 });
        let base = self.fold.len();
        let count = name.len() + 1;
        reserve_append(&mut self.fold, count, self.compact_growth);
        let case_len = (base + count).div_ceil(8);
        let extra_case = case_len - self.case.len();
        reserve_append(&mut self.case, extra_case, self.compact_growth);
        self.case.resize(case_len, 0);
        fold::push_folded(name, &mut self.fold, |i| {
            let at = base + i;
            self.case[at >> 3] |= 1 << (at & 7);
        });
        self.fold.push(SEP);
        self.slots[r] = Slot { parent, name: idx | flags };
        self.live += 1;
        self.compact_if_needed();
    }

    pub fn remove(&mut self, rec: u32) {
        let Some(s) = self.slots.get_mut(rec as usize) else { return };
        if s.name == NONE {
            // A deleted file inside a pruned tree is free, not pruned (a lookup then sees the change).
            *s = FREE;
            return;
        }
        self.recs[(s.name & IDX) as usize] = NONE;
        *s = FREE;
        self.dead += 1;
        self.live -= 1;
        self.compact_if_needed();
    }

    fn compact_if_needed(&mut self) {
        // A busy journal can rename files indefinitely without issuing any deletes. Reclaim
        // the old names in either case, after enough edits to amortize scanning the arena.
        if self.dead >= 200_000 && self.dead > self.live / 8 {
            self.compact();
        }
    }

    /// Rewrites the arenas in place without the names of deleted or renamed records.
    pub fn compact(&mut self) {
        if self.dead == 0 {
            return;
        }
        let mut kept = 0;
        let mut base = usize::from(self.fold.first() == Some(&SEP));
        for i in 0..self.starts.len() {
            let rec = self.recs[i];
            if rec == NONE {
                continue;
            }
            let (a, b) = self.name_range(i);
            if base != a {
                // The write cursor trails the read cursor, so later case bits remain intact.
                // Copy the separator's zero bit as well, for the next name appended there.
                for (at, source) in (base..).zip(a..=b) {
                    let changed = u8::from(self.changed(source));
                    let mask = 1 << (at & 7);
                    let byte = &mut self.case[at >> 3];
                    *byte = (*byte & !mask) | (changed << (at & 7));
                }
                self.fold.copy_within(a..=b, base);
            }
            let slot = &mut self.slots[rec as usize];
            slot.name = (slot.name & !IDX) | kept as u32;
            self.starts[kept] = base as u32;
            self.recs[kept] = rec;
            self.meta[kept] = self.meta[i];
            base += b + 1 - a;
            kept += 1;
        }
        self.fold.truncate(base);
        self.case.truncate(base.div_ceil(8));
        if base % 8 != 0 && let Some(last) = self.case.last_mut() {
            *last &= (1 << (base % 8)) - 1;
        }
        self.starts.truncate(kept);
        self.recs.truncate(kept);
        self.meta.truncate(kept);
        self.dead = 0;
        self.shrink();
    }

    /// Sets the inherited noisy bit of every record from its folders and drops the names inside
    /// pruned folders (after the MFT load); returns how many names were dropped.
    pub fn settle(&mut self) -> usize {
        let mut pruned = 0;
        if !self.prune_roots.is_empty() {
            for r in 0..self.slots.len() as u32 {
                if self.slots[r as usize].name != NONE && self.walk_pruned(r) {
                    if self.slots[r as usize].name & DIR == 0 {
                        self.pruned_files.push(r);
                    }
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
        self.query_where(q, max, None)
    }

    /// Query only the descendants of a known scope. A prune root or any ancestor of one has
    /// incomplete coverage. Resolve folder names to record IDs before ranking any matches.
    pub fn query_scope(&self, q: &str, max: usize, root: &str, path: &str) -> Option<Vec<Found>> {
        if !self.journal_current || !self.initial_links_complete || !self.pending_links.is_empty() {
            return None;
        }
        let path = path.trim_end_matches('\\');
        let lower_path = path.to_lowercase();
        let scope = if path.eq_ignore_ascii_case(root.trim_end_matches('\\')) {
            ROOT
        } else {
            let leaf = path.rsplit_once('\\')?.1;
            let folded = fold::fold(leaf);
            let mut matching = self.starts.iter().enumerate().filter_map(|(i, _)| {
                let rec = self.recs[i];
                if rec == NONE || !self.is_dir(rec) {
                    return None;
                }
                let (start, end) = self.name_range(i);
                (&self.fold[start..end] == folded.as_bytes()
                    && self.path(rec, root).is_some_and(|p| p.to_lowercase() == lower_path)).then_some(rec)
            });
            let scope = matching.next()?;
            // NTFS directories can opt in to case-sensitive lookup. An ambiguous scope needs
            // the native search provider rather than the first matching MFT record.
            if matching.next().is_some() {
                return None;
            }
            scope
        };
        if self.prune_roots.iter().any(|&rec| {
            self.is_dir(rec) && (rec == scope || self.descends_from(rec, scope) || self.descends_from(scope, rec))
        }) {
            return None;
        }
        if self.alias_parents.iter().any(|&parent| parent == scope || self.descends_from(parent, scope)) {
            return None;
        }
        Some(self.query_where(q, max, Some(scope)))
    }

    fn descends_from(&self, mut rec: u32, ancestor: u32) -> bool {
        for _ in 0..256 {
            if rec == ROOT {
                return false;
            }
            let Some(slot) = self.slots.get(rec as usize).filter(|s| s.name != NONE) else { return false };
            rec = slot.parent;
            if rec == ancestor {
                return true;
            }
        }
        false
    }

    fn query_where(&self, q: &str, max: usize, scope: Option<u32>) -> Vec<Found> {
        let q = q.as_bytes();
        if q.is_empty() || q.contains(&SEP) || max == 0 {
            return Vec::new();
        }
        let mut top = Top::new(max);
        // Name prefixes: one pass over the name starts, cheaper than memmem when most names match.
        let (q0, n) = (q[0], q.len());
        for (i, &st) in self.starts.iter().enumerate() {
            let st = st as usize;
            if self.fold[st] == q0 && self.fold.get(st..st + n) == Some(q) && self.recs[i] != NONE
                && scope.is_none_or(|ancestor| self.descends_from(self.recs[i], ancestor)) {
                top.offer(meta_key(self.meta[i], 0), i);
            }
        }
        // A word or inner match ranks after every clean name prefix: skip when those fill the list.
        // Past INNER_CAP inner matches the rest is left out (one-letter queries on a big volume).
        if !top.full_below((1, Reverse(u64::MAX))) {
            let finder = memmem::Finder::new(q);
            let (mut at, mut i, mut seen) = (0, 0, 0usize);
            while let Some(p) = finder.find(&self.fold[at..]) {
                let m = at + p;
                i = self.seek(i, m);
                let (s, e) = self.name_range(i);
                at = e + 1;
                if m == s || self.recs[i] == NONE || scope.is_some_and(|ancestor| !self.descends_from(self.recs[i], ancestor)) {
                    continue;
                }
                let rank = if word_start(self.fold[m - 1]) { 1 } else { 2 };
                top.offer(meta_key(self.meta[i], rank), i);
                seen += 1;
                if scope.is_none() && seen >= INNER_CAP {
                    break;
                }
            }
        }
        let mut out: Vec<Found> = top.heap.into_iter().map(|(k, i)| Found { key: k, rec: self.recs[i] }).collect();
        out.sort_unstable();
        out
    }
}

#[cfg(test)]
mod scope_tests {
    use super::*;

    #[test]
    fn scoped_results_require_their_own_volume_and_filter_before_ranking() {
        let mut table = Table::new();
        table.upsert(11, ROOT, "Projects", true, false, 0);
        table.upsert(12, ROOT, "Projects-old", true, false, 0);
        table.upsert(13, 11, "old report.txt", false, false, 10);
        table.upsert(14, 12, "report.txt", false, false, 20);
        assert_eq!(table.query("report", 1)[0].rec, 14);
        assert!(table.query_scope("report", 1, "D:\\", "D:\\Projects").is_none());
        table.journal_current = true;
        assert!(table.query_scope("report", 1, "D:\\", "D:\\Projects").is_none());
        table.initial_links_complete = true;
        assert_eq!(table.query_scope("report", 1, "D:\\", "d:\\projects\\").unwrap()[0].rec, 13);
        assert!(table.query_scope("report", 1, "C:\\", "D:\\Projects").is_none());
        assert!(table.query_scope("report", 1, "D:\\", "D:\\missing").is_none());
    }

    #[test]
    fn prune_roots_and_their_ancestors_have_incomplete_coverage() {
        let mut table = Table::new();
        table.upsert(11, ROOT, "Windows", true, false, 0);
        table.upsert(12, 11, "WinSxS", true, false, 0);
        table.upsert(13, 11, "System32", true, false, 0);
        table.journal_current = true;
        table.initial_links_complete = true;
        for scope in ["C:\\", "C:\\Windows", "C:\\Windows\\WinSxS", "C:\\Windows\\WinSxS\\sub"] {
            assert!(table.query_scope("foo", 1, "C:\\", scope).is_none(), "{scope}");
        }
        assert!(table.query_scope("foo", 1, "C:\\", "C:\\Windows\\System32").unwrap().is_empty());
    }

    #[test]
    fn case_sensitive_folders_with_the_same_folded_path_are_not_covered() {
        let mut table = Table::new();
        table.upsert(11, ROOT, "Projects", true, false, 0);
        table.upsert(12, ROOT, "projects", true, false, 0);
        table.journal_current = true;
        table.initial_links_complete = true;
        assert!(table.query_scope("report", 1, "D:\\", "D:\\Projects").is_none());
    }

    #[test]
    fn scopes_with_alternate_names_or_unresolved_link_changes_are_uncovered() {
        let mut table = Table::new();
        table.upsert(20, ROOT, "Work", true, false, 0);
        table.upsert(21, 20, "First", true, false, 0);
        table.upsert(22, ROOT, "Second", true, false, 0);
        table.upsert(23, ROOT, "Other", true, false, 0);
        table.upsert(30, 21, "first.txt", false, false, 0);
        table.upsert(30, 22, "second.txt", false, false, 0);
        table.journal_current = true;
        table.initial_links_complete = true;

        table.require_link_lookup(30);
        assert!(table.query_scope("first", 10, "D:\\", "D:\\Other").is_none());
        table.mark_alias_parents([21, 22]);
        table.finish_link_lookup(30);
        for scope in ["D:\\Work", "D:\\Work\\First", "D:\\Second"] {
            assert!(table.query_scope("first", 10, "D:\\", scope).is_none(), "{scope}");
        }
        assert!(table.query_scope("first", 10, "D:\\", "D:\\Other").unwrap().is_empty());
    }
}

/// Inner matches looked at, at most, per query.
const INNER_CAP: usize = 60_000;

fn write_time(modified: i64, previous: u64) -> u64 {
    if modified > 0 { modified as u64 } else { previous }
}

fn meta_key(meta: u64, rank: u8) -> SortKey {
    (rank | (((meta >> 63) as u8) << 2), Reverse(meta & !META_NOISY))
}

/// The best `max` keys seen so far (a max-heap: the worst kept on top).
struct Top {
    heap: std::collections::BinaryHeap<(SortKey, usize)>,
    max: usize,
}

impl Top {
    fn new(max: usize) -> Top {
        Top { heap: std::collections::BinaryHeap::with_capacity(max + 1), max }
    }

    fn offer(&mut self, k: SortKey, i: usize) {
        if self.heap.len() < self.max {
            self.heap.push((k, i));
        } else if self.heap.peek().is_some_and(|w| k < w.0) {
            self.heap.pop();
            self.heap.push((k, i));
        }
    }

    /// Full of keys all better than `k`.
    fn full_below(&self, k: SortKey) -> bool {
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
    fn newest_last_write_wins_before_cut() {
        let mut t = sample();
        let older = 133_800_000_000_000_000;
        t.upsert(51, 32, "clock-old.txt", false, false, older);
        t.upsert(52, 32, "clock-new.txt", false, false, older + 1);
        t.upsert(53, 35, "clock-cache.txt", false, false, older + 2);
        t.compact();
        assert_eq!(t.query("clock", 1)[0].rec, 52);
        // SetFileTime can move a file backwards. A later rename without a fresh read keeps it.
        assert!(t.set_modified(52, older - 1));
        t.upsert(52, 32, "clock-renamed.txt", false, false, 0);
        let got = t.query("clock", 2);
        assert_eq!(got.iter().map(|f| (f.rec, f.modified())).collect::<Vec<_>>(), vec![(51, older as u64), (52, (older - 1) as u64)]);
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
    fn hard_link_outside_pruned_tree() {
        let mut t = sample();
        t.upsert(40, ROOT, "Windows", true, false, 1);
        t.upsert(50, 40, "System32", true, false, 1);
        t.upsert(43, 42, "kernelbase.dll", false, false, 1);
        t.upsert(42, 41, "amd64_kernelbase_31bf3856ad364e35", true, false, 1);
        t.upsert(41, 40, "WinSxS", true, false, 1);
        t.settle();
        assert_eq!(t.take_pruned_files(), vec![43], "files only, not the pruned folder");
        assert!(t.take_pruned_files().is_empty());
        // The service found the file's other link, in System32.
        t.upsert(43, 50, "kernelbase.dll", false, false, 0);
        assert_eq!(t.path(43, "C:\\").as_deref(), Some("C:\\Windows\\System32\\kernelbase.dll"));
        // A journal record under its WinSxS name keeps the System32 one.
        t.upsert(43, 42, "kernelbase.dll", false, false, 2);
        assert_eq!(t.path(43, "C:\\").as_deref(), Some("C:\\Windows\\System32\\kernelbase.dll"));
        assert!(t.take_pruned_files().is_empty());
        // A new file inside the tree is reported for the lookup.
        t.upsert(44, 42, "new.dll", false, false, 3);
        assert_eq!(t.take_pruned_files(), vec![44]);
        assert!(t.pruned_parent(42) && t.pruned_parent(41) && !t.pruned_parent(50));
    }

    #[test]
    fn rename_into_pruned_tree_drops_the_gone_name() {
        let mut t = sample();
        t.upsert(40, ROOT, "Windows", true, false, 1);
        t.upsert(42, 40, "WinSxS", true, false, 1);
        t.settle();
        // An ordinary file of Documents renamed into WinSxS: the journal names it there.
        t.upsert(33, 42, "Rapport final.docx", false, false, 2);
        t.recheck_links(33);
        assert_eq!(t.take_pruned_files(), vec![33]);
        let before = t.state(33);
        assert!(matches!(&before, RecState::Named(32, n) if n == "Rapport final.docx"));
        // The lookup found no link outside the tree.
        t.prune(33);
        assert_eq!(t.state(33), RecState::Pruned);
        assert!(t.query("rapport", 5).is_empty());
        assert!(t.path(33, "C:\\").is_none());
        // Folders are never pruned this way; a fresh table has another id.
        t.prune(32);
        assert!(t.name(32).is_some());
        assert_ne!(Table::new().id(), t.id());
    }

    #[test]
    fn journal_changes_during_a_links_lookup_win() {
        let mut t = sample();
        t.upsert(40, ROOT, "Windows", true, false, 1);
        t.upsert(42, 40, "WinSxS", true, false, 1);
        t.upsert(50, 42, "ntdll.dll", false, false, 1);
        t.settle();
        assert_eq!(t.state(50), RecState::Pruned);
        // A lookup snapshots the pruned file, then the journal deletes it.
        t.watch_touches(true);
        let was = t.state(50);
        let since = t.touches();
        t.touch(50);
        t.remove(50);
        assert_ne!(t.state(50), was, "a deleted pruned record is free, not pruned");
        assert!(t.touched_since(50, since) && !t.touched_since(33, since));
        // A lookup that starts after the change reads the new state: not stale for it.
        assert!(!t.touched_since(50, t.touches()));
        t.watch_touches(false);
        assert!(!t.touched_since(50, since), "the set ends with the last lookup");
        t.touch(33);
        assert!(!t.touched_since(33, 0), "no lookup running: nothing kept");
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

    #[test]
    fn first_journal_update_does_not_double_the_table() {
        let mut t = Table::new();
        for rec in 16..32_784 {
            t.upsert(rec, ROOT, "Some File.txt", false, false, i64::from(rec));
        }
        t.shrink();
        let initial = t.bytes();
        t.upsert(16, ROOT, "Renamed File.txt", false, false, 0);
        t.upsert(32_784, ROOT, "Another File.txt", false, false, 10);
        let updated = t.bytes();
        assert!(updated <= initial + initial / 8, "{initial} bytes grew to {updated}");
        assert_eq!(t.query("renamed", 1)[0].modified(), 16);
        assert_eq!(t.name(32_784).as_deref(), Some("Another File.txt"));
    }

    #[test]
    fn compaction_preserves_unicode_case_and_clears_bits_for_the_next_append() {
        let mut t = Table::new();
        t.upsert(16, ROOT, "DELETE-A", false, false, 0);
        t.upsert(17, ROOT, "ÉTÉ-ß.txt", false, false, 11);
        t.upsert(18, ROOT, "REMOVE-ME.txt", false, false, 0);
        t.upsert(19, ROOT, "ΣΊΣΥΦΟΣ.txt", false, true, 23);
        t.remove(16);
        t.remove(18);
        t.compact();
        t.upsert(20, ROOT, "lower.txt", false, false, 0);
        for (record, name) in [(17, "ÉTÉ-ß.txt"), (19, "ΣΊΣΥΦΟΣ.txt"), (20, "lower.txt")] {
            assert_eq!(t.name(record).as_deref(), Some(name));
        }
        let hit = t.query("σίσυφοσ", 1)[0];
        assert_eq!((hit.rec, hit.modified(), hit.noisy()), (19, 23, true));
    }

    #[test]
    fn rename_only_journal_reclaims_old_names() {
        let mut t = Table::new();
        for i in 0..200_010 {
            let name = if i % 2 == 0 { "renamed.txt" } else { "original.txt" };
            t.upsert(16, ROOT, name, false, false, i64::from(i));
        }
        assert!(t.bytes() < 32 * 1024);
        assert_eq!(t.name(16).as_deref(), Some("original.txt"));
        assert_eq!(t.query("original", 1)[0].modified(), 200_009);
        assert!(t.query("renamed", 1).is_empty());
    }

    #[test]
    #[ignore = "release-mode benchmark with 7.7 million names"]
    fn journal_memory_and_query_benchmark() {
        use std::hint::black_box;
        use std::time::Instant;

        fn memory(label: &str, volumes: &[Table]) {
            let names: usize = volumes.iter().map(Table::len).sum();
            let bytes: usize = volumes.iter().map(Table::bytes).sum();
            eprintln!("{label}: {names} names, {bytes} table bytes ({:.1} B/name)", bytes as f64 / names as f64);
            if let Ok(s) = std::fs::read_to_string("/proc/self/smaps_rollup") {
                for line in s.lines().filter(|s| s.starts_with("Rss:") || s.starts_with("Private_")) {
                    eprintln!("{line}");
                }
            }
            #[cfg(windows)]
            {
                use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
                use windows::Win32::System::Threading::GetCurrentProcess;
                let size = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
                let mut c = PROCESS_MEMORY_COUNTERS_EX { cb: size, ..Default::default() };
                // SAFETY: the API fills the structure size supplied here for this process.
                unsafe { GetProcessMemoryInfo(GetCurrentProcess(), (&mut c as *mut PROCESS_MEMORY_COUNTERS_EX).cast(), size) }.unwrap();
                eprintln!("private bytes {}, commit {}, peak commit {}, working set {}", c.PrivateUsage, c.PagefileUsage, c.PeakPagefileUsage, c.WorkingSetSize);
            }
        }

        let start = Instant::now();
        let mut volumes = Vec::new();
        for count in [4_900_000u32, 2_000_000, 800_000] {
            let mut table = Table::new();
            let folders = count / 100;
            for i in 0..count {
                let rec = 16 + i + i / 8;
                let dir = i < folders;
                let folder = i % folders;
                let parent = if dir { ROOT } else { 16 + folder + folder / 8 };
                let word = ["Report", "setup", "image", "Microsoft", "config", "data", "readme", "photo", "build", "notes"][i as usize % 10];
                let name = if dir { format!("{word} folder {i}") } else { format!("{word}_archive_{:07}_{i:07}.txt", i / 1000) };
                table.upsert(rec, parent, &name, dir, false, 133_800_000_000_000_000 + i64::from(i));
            }
            table.settle();
            table.shrink();
            volumes.push(table);
        }
        eprintln!("build: {:.2} s", start.elapsed().as_secs_f64());
        memory("published", &volumes);
        for table in &mut volumes {
            let count = table.len() as u32;
            table.upsert(16 + count + count / 8, 16, "renamed final archive.txt", false, false, 1);
        }
        memory("after journal insert", &volumes);

        for query in ["r", "rep", "report_archive_0000000", "archive_0004031", "renamed final", "missing file"] {
            let mut samples = [0; 9];
            let mut checksum = 0u64;
            for duration in &mut samples {
                let start = Instant::now();
                checksum = 0;
                for table in &volumes {
                    for hit in table.query(black_box(query), 24) {
                        checksum = checksum.wrapping_add(u64::from(hit.rec) ^ hit.modified());
                    }
                }
                black_box(checksum);
                *duration = start.elapsed().as_micros();
            }
            samples.sort_unstable();
            eprintln!("{query:?}: median {:.2} ms, max {:.2} ms, checksum {checksum}", samples[4] as f64 / 1000.0, samples[8] as f64 / 1000.0);
        }
    }
}
