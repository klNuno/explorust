use fsindex::table::Table;
use parking_lot::RwLock;
use std::ops::Range;
use std::sync::Arc;

pub const BATCH: usize = 256;

/// The initial date worker watches journal changes from the instant its names are published.
pub struct Hydration {
    table: Arc<RwLock<Table>>,
    generation: u64,
    revision: u64,
}

impl Hydration {
    pub fn watch(table: Arc<RwLock<Table>>) -> Self {
        let mut names = table.write();
        let generation = names.id();
        let revision = names.touches();
        names.watch_touches(true);
        drop(names);
        Self { table, generation, revision }
    }

    pub fn is_current(&self) -> bool {
        self.table.read().id() == self.generation
    }

    pub fn pending(&self, records: Range<usize>) -> Option<Vec<u32>> {
        let table = self.table.read();
        (table.id() == self.generation).then(|| {
            records.map(|rec| rec as u32).filter(|&rec| self.missing(&table, rec)).collect()
        })
    }

    pub fn apply(&self, dates: &[(u32, i64)]) -> Option<usize> {
        let mut table = self.table.write();
        if table.id() != self.generation {
            return None;
        }
        let mut applied = 0;
        for &(rec, modified) in dates {
            if self.missing(&table, rec) {
                applied += usize::from(table.set_modified(rec, modified));
            }
        }
        Some(applied)
    }

    #[cfg(target_os = "windows")]
    pub fn apply_aliases(&self, entries: impl IntoIterator<Item = (u32, Vec<(u32, String)>)>) -> bool {
        let mut table = self.table.write();
        if table.id() != self.generation {
            return false;
        }
        for (rec, aliases) in entries {
            table.set_initial_aliases(rec, aliases);
        }
        true
    }

    #[cfg(target_os = "windows")]
    pub fn finish_links(&self, alias_parents: impl IntoIterator<Item = u32>) -> bool {
        let mut table = self.table.write();
        if table.id() != self.generation {
            return false;
        }
        table.mark_alias_parents(alias_parents);
        table.finish_initial_links();
        true
    }

    fn missing(&self, table: &Table, rec: u32) -> bool {
        // A touched record may now refer to a different file, even if its date is still unknown.
        table.needs_modified(rec) && !table.touched_since(rec, self.revision)
    }
}

impl Drop for Hydration {
    fn drop(&mut self) {
        let mut table = self.table.write();
        if table.id() == self.generation {
            table.watch_touches(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fsindex::table::ROOT;

    fn publish(table: Arc<RwLock<Table>>, names: Table) -> Hydration {
        *table.write() = names;
        Hydration::watch(table)
    }

    fn load_names() -> Table {
        let mut table = Table::new();
        table.upsert(20, ROOT, "notes-old.txt", false, false, 0);
        table.upsert(21, ROOT, "notes-new.txt", false, false, 0);
        table
    }

    #[test]
    fn names_are_searchable_before_hydration() {
        let table = Arc::new(RwLock::new(Table::new()));
        let hydration = publish(table.clone(), load_names());
        assert_eq!(hydration.pending(20..20 + BATCH), Some(vec![20, 21]));
        let (begin, wait) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            wait.recv().unwrap();
            hydration.apply(&[(20, 10), (21, 20)])
        });

        let hits = table.read().query("notes", 2);
        begin.send(()).unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|h| h.modified() == 0));
        assert_eq!(worker.join().unwrap(), Some(2));
        assert_eq!(table.read().query("notes", 1)[0].rec, 21);
    }

    #[test]
    fn hydration_keeps_journal_updates_and_drops_old_generations() {
        let table = Arc::new(RwLock::new(Table::new()));
        let hydration = publish(table.clone(), load_names());
        {
            let mut table = table.write();
            table.touch(20);
            table.set_modified(20, 5);
            table.touch(21);
            table.remove(21);
            table.upsert(21, ROOT, "replacement.txt", false, false, 0);
        }
        assert_eq!(hydration.pending(20..22), Some(Vec::new()));
        assert_eq!(hydration.apply(&[(20, 100), (21, 200)]), Some(0));
        assert_eq!(table.read().query("notes", 1)[0].modified(), 5);
        assert_eq!(table.read().query("replacement", 1)[0].modified(), 0);

        let next = publish(table.clone(), load_names());
        assert!(!hydration.is_current());
        assert_eq!(hydration.pending(20..22), None);
        assert_eq!(hydration.apply(&[(20, 100)]), None);
        assert_eq!(table.read().query("notes", 1)[0].modified(), 0);
        drop(hydration);
        table.write().touch(20);
        assert_eq!(next.apply(&[(20, 100), (21, 200)]), Some(1));
    }
}
