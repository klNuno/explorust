//! Files streams bounded batches. If compaction moves names between batches, resume by NTFS
//! record number so the service can release the table lock before sending each page.

use super::{IDX, NONE, ROOT, SEP, Table, fold, meta_key, word_start};
use crate::proto::Hit;
use memchr::memmem;
use std::ops::Range;

pub(super) struct Alias {
    parent: u32,
    name: String,
    folded: String,
}

pub struct ScopedScan {
    table: u64,
    scope: u32,
    records: Range<usize>,
    arena: Option<ArenaScan>,
    seen: Vec<u64>,
    alias: (u32, usize),
}

struct ArenaScan {
    revision: u64,
    next: usize,
    end: usize,
}

const SLICE: usize = 32_768;

impl Table {
    /// Keep long names that the table cannot already guarantee. When several links remain
    /// outside pruned folders, keep the primary too: the journal may report a different one.
    pub fn set_aliases(&mut self, rec: u32, mut names: Vec<(u32, String)>) {
        if names.len() > 1 {
            names.retain(|(parent, _)| !self.pruned_parent(*parent));
        } else {
            names.clear();
        }
        names.sort_unstable();
        names.dedup();
        if names.is_empty()
            || names.len() == 1 && self.is_primary_alias(rec, names[0].0, &names[0].1)
        {
            self.aliases.remove(&rec);
        } else {
            self.aliases.insert(
                rec,
                names
                    .into_iter()
                    .map(|(parent, name)| Alias {
                        parent,
                        folded: fold::fold(&name),
                        name,
                    })
                    .collect(),
            );
        }
    }

    fn is_primary_alias(&self, rec: u32, parent: u32, name: &str) -> bool {
        self.slots
            .get(rec as usize)
            .is_some_and(|slot| slot.parent == parent)
            && self.name(rec).as_deref() == Some(name)
    }

    pub fn begin_scoped_scan(&self, root: &str, path: &str) -> Option<ScopedScan> {
        if !self.journal_current || !self.initial_links_complete || !self.pending_links.is_empty() {
            return None;
        }
        let path = path.trim_end_matches('\\');
        let scope = if path.eq_ignore_ascii_case(root.trim_end_matches('\\')) {
            ROOT
        } else {
            self.scoped_record(root, path)?
        };
        Some(ScopedScan {
            table: self.id(),
            scope,
            records: 0..self.slots.len(),
            arena: Some(ArenaScan {
                revision: self.arena_revision,
                next: 0,
                end: self.starts.len(),
            }),
            seen: Vec::new(),
            alias: (0, 0),
        })
    }

    fn scoped_record(&self, root: &str, path: &str) -> Option<u32> {
        let leaf = fold::fold(path.rsplit_once('\\')?.1);
        if leaf.is_empty() || leaf.as_bytes().contains(&SEP) {
            return None;
        }
        let needle = [leaf.as_bytes(), &[SEP]].concat();
        let finder = memmem::Finder::new(&needle);
        let lower = path.to_lowercase();
        let mut scope = None;
        for names in self
            .name_indices()
            .into_iter()
            .filter(|names| !names.is_empty())
        {
            let mut at = self.starts[names.start] as usize;
            let end = self.name_range(names.end - 1).1 + 1;
            let mut i = names.start;
            while let Some(pos) = finder.find(&self.fold[at..end]) {
                let matched = at + pos;
                i = self.seek(i, names.end, matched);
                at = matched + needle.len();
                let rec = self.recs[i];
                if matched == self.starts[i] as usize
                    && rec != NONE
                    && self.is_dir(rec)
                    && self
                        .path(rec, root)
                        .is_some_and(|p| p.to_lowercase() == lower)
                    && scope.replace(rec).is_some()
                {
                    return None;
                }
            }
        }
        scope
    }
}

impl ScopedScan {
    /// The index deliberately omits WinSxS descendants. Search just those subtrees, including
    /// the scope itself when it is one of the pruned roots.
    pub fn gaps(&self, table: &Table, root: &str) -> Vec<String> {
        table
            .prune_roots
            .iter()
            .copied()
            .filter(|&rec| {
                table.is_dir(rec) && (rec == self.scope || table.descends_from(rec, self.scope))
            })
            .filter_map(|rec| table.path(rec, root))
            .collect()
    }

    /// Returns `None` if this volume was reloaded; otherwise `true` after exhausting the scan.
    /// Each call visits a bounded number of records and appends at most `limit` hits in total.
    pub fn step(
        &mut self,
        table: &Table,
        root: &str,
        finder: &memmem::Finder<'_>,
        limit: usize,
        hits: &mut Vec<Hit>,
    ) -> Option<bool> {
        if table.id() != self.table || !table.journal_current {
            return None;
        }
        if let Some(mut arena) = self
            .arena
            .take()
            .filter(|arena| arena.revision == table.arena_revision)
        {
            if self.arena_step(&mut arena, table, root, finder, limit, hits) {
                self.records.start = self.records.end;
                self.seen = Vec::new();
            } else {
                self.arena = Some(arena);
                return Some(false);
            }
        }
        // If a compaction moved the name arena between slices, resume by record number.
        // The bitmap prevents emitting records the fast scan already returned.
        let end = self.records.end.min(self.records.start + SLICE);
        while self.records.start < end && hits.len() < limit {
            let rec = self.records.next().unwrap();
            let slot = table.slots[rec];
            if slot.name == NONE
                || self
                    .seen
                    .get(rec / 64)
                    .is_some_and(|word| word & (1 << (rec % 64)) != 0)
            {
                continue;
            }
            let i = (slot.name & IDX) as usize;
            let (a, b) = table.name_range(i);
            if let Some(at) = finder.find(&table.fold[a..b])
                && let Some(hit) = self.primary_hit(table, root, i, rank(&table.fold, a, a + at))
            {
                hits.push(hit);
            }
        }
        if !self.records.is_empty() || hits.len() == limit {
            return Some(false);
        }
        let mut remaining = SLICE;
        for (&rec, aliases) in table.aliases.range(self.alias.0..) {
            let start = if rec == self.alias.0 { self.alias.1 } else { 0 };
            for (i, alias) in aliases.iter().enumerate().skip(start) {
                if remaining == 0 || hits.len() == limit {
                    return Some(false);
                }
                remaining -= 1;
                self.alias = (rec, i + 1);
                let name = alias.folded.as_bytes();
                if let Some(at) = finder.find(name)
                    && !table.pruned_parent(alias.parent)
                    && (alias.parent == self.scope || table.descends_from(alias.parent, self.scope))
                    && let Some(mut path) = table.path(alias.parent, root)
                {
                    let rank = rank(name, 0, at);
                    let primary = table
                        .slots
                        .get(rec as usize)
                        .filter(|slot| slot.name != NONE);
                    if table.is_primary_alias(rec, alias.parent, &alias.name) {
                        continue;
                    }
                    if !path.ends_with('\\') {
                        path.push('\\');
                    }
                    path.push_str(&alias.name);
                    let meta = primary.map_or(0, |slot| table.meta[(slot.name & IDX) as usize]);
                    let key = meta_key(meta, rank);
                    hits.push(Hit {
                        path,
                        folder: false,
                        noisy: key.0 >= 4,
                        rank,
                        modified: key.1.0,
                    });
                }
            }
        }
        Some(true)
    }

    fn arena_step(
        &mut self,
        arena: &mut ArenaScan,
        table: &Table,
        root: &str,
        finder: &memmem::Finder<'_>,
        limit: usize,
        hits: &mut Vec<Hit>,
    ) -> bool {
        if arena.next >= arena.end {
            return true;
        }
        let Some(names) = table
            .name_indices()
            .into_iter()
            .find(|range| range.end > arena.next && range.start < arena.end && !range.is_empty())
        else {
            return true;
        };
        let first = arena.next.max(names.start);
        let end = arena.end.min(names.end).min(first + SLICE);
        let byte_end = table.name_range(end - 1).1 + 1;
        let mut at = table.starts[first] as usize;
        let mut i = first;
        while let Some(offset) = finder.find(&table.fold[at..byte_end]) {
            let matched = at + offset;
            i = table.seek(i, end, matched);
            let (a, b) = table.name_range(i);
            at = b + 1;
            if let Some(hit) = self.primary_hit(table, root, i, rank(&table.fold, a, matched)) {
                let rec = table.recs[i] as usize;
                if self.seen.len() <= rec / 64 {
                    self.seen.resize(rec / 64 + 1, 0);
                }
                self.seen[rec / 64] |= 1 << (rec % 64);
                hits.push(hit);
                if hits.len() == limit {
                    arena.next = i + 1;
                    return false;
                }
            }
        }
        arena.next = end;
        arena.next == arena.end
    }

    fn primary_hit(&self, table: &Table, root: &str, i: usize, rank: u8) -> Option<Hit> {
        let rec = table.recs[i];
        if rec == NONE
            || rec == self.scope
            || (self.scope != ROOT && !table.descends_from(rec, self.scope))
        {
            return None;
        }
        let path = table.path(rec, root)?;
        let (key, modified) = meta_key(table.meta[i], rank);
        Some(Hit {
            path,
            folder: table.is_dir(rec),
            noisy: key >= 4,
            rank,
            modified: modified.0,
        })
    }
}

fn rank(name: &[u8], start: usize, matched: usize) -> u8 {
    if matched == start {
        0
    } else if word_start(name[matched - 1]) {
        1
    } else {
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn ready_table() -> Table {
        let mut table = Table::new();
        table.journal_current = true;
        table.finish_initial_links();
        table
    }

    fn drain(scan: &mut ScopedScan, table: &Table, query: &str, limit: usize) -> BTreeSet<String> {
        let finder = memmem::Finder::new(query);
        let mut paths = BTreeSet::new();
        loop {
            let mut hits = Vec::new();
            let done = scan.step(table, "C:\\", &finder, limit, &mut hits).unwrap();
            assert!(hits.len() <= limit);
            for hit in hits {
                assert!(paths.insert(hit.path), "duplicate hit");
            }
            if done {
                return paths;
            }
        }
    }

    #[test]
    fn indexed_pages_include_known_aliases_and_only_delegate_pruned_subtrees() {
        let mut table = ready_table();
        table.upsert(16, ROOT, "Work", true, false, 0);
        table.upsert(17, ROOT, "Work-old", true, false, 0);
        table.upsert(18, 16, "WinSxS", true, false, 0);
        table.upsert(19, 17, "report-primary.txt", false, false, 0);
        table.upsert(20, 16, "REPORT-local.txt", false, false, 0);
        table.mark_alias_parents([16, 17, 18]);
        table.set_aliases(
            19,
            vec![
                (17, "report-primary.txt".into()),
                (16, "report-alias.txt".into()),
                (18, "report-pruned.txt".into()),
            ],
        );
        table.set_aliases(
            20,
            vec![
                (16, "REPORT-local.txt".into()),
                (16, "report-local-alias.txt".into()),
            ],
        );
        let mut scan = table.begin_scoped_scan("C:\\", r"c:\work").unwrap();
        assert_eq!(scan.gaps(&table, "C:\\"), [r"C:\Work\WinSxS"]);
        assert_eq!(
            drain(&mut scan, &table, "report", 1),
            [
                r"C:\Work\REPORT-local.txt",
                r"C:\Work\report-alias.txt",
                r"C:\Work\report-local-alias.txt"
            ]
            .map(String::from)
            .into_iter()
            .collect()
        );

        let mut scan = table.begin_scoped_scan("C:\\", r"C:\Work\WinSxS").unwrap();
        assert_eq!(scan.gaps(&table, "C:\\"), [r"C:\Work\WinSxS"]);
        assert!(drain(&mut scan, &table, "report", 200).is_empty());
    }

    #[test]
    fn more_than_two_pages_survive_index_compaction_between_chunks() {
        let mut table = ready_table();
        table.upsert(16, ROOT, "Folder", true, false, 0);
        table.upsert(17, 16, "delete me", false, false, 0);
        for i in 20..451 {
            table.upsert(i, 16, &format!("Report-{i}.txt"), false, false, 0);
        }
        let mut scan = table.begin_scoped_scan("C:\\", r"C:\Folder").unwrap();
        let mut first = Vec::new();
        assert_eq!(
            scan.step(
                &table,
                "C:\\",
                &memmem::Finder::new("report"),
                200,
                &mut first
            ),
            Some(false)
        );
        assert_eq!(first.len(), 200);
        table.remove(17);
        table.compact();
        let rest = drain(&mut scan, &table, "report", 200);
        assert_eq!(rest.len(), 231);
        assert!(first.iter().all(|hit| !rest.contains(&hit.path)));
    }

    #[test]
    fn a_normal_timestamp_update_preserves_aliases_and_old_layout_cannot_overwrite_renames() {
        let mut table = ready_table();
        table.initial_links_complete = false;
        table.watch_touches(true);
        table.upsert(16, ROOT, "Report.txt", false, false, 0);
        table.touch(16);
        table.set_initial_aliases(
            16,
            vec![
                (ROOT, "Report.txt".into()),
                (ROOT, "Report-alias.txt".into()),
            ],
        );
        table.upsert(17, ROOT, "Report-new.txt", false, false, 0);
        table.require_link_lookup(17);
        table.set_aliases(
            17,
            vec![
                (ROOT, "Report-new.txt".into()),
                (ROOT, "Report-new-alias.txt".into()),
            ],
        );
        table.finish_link_lookup(17);
        table.set_initial_aliases(
            17,
            vec![
                (ROOT, "Report-old.txt".into()),
                (ROOT, "Report-old-alias.txt".into()),
            ],
        );
        table.finish_initial_links();
        let mut scan = table.begin_scoped_scan("C:\\", "C:\\").unwrap();
        assert_eq!(
            drain(&mut scan, &table, "report", 200),
            [
                r"C:\Report.txt",
                r"C:\Report-alias.txt",
                r"C:\Report-new.txt",
                r"C:\Report-new-alias.txt"
            ]
            .map(String::from)
            .into_iter()
            .collect()
        );
    }
}
