use fsindex::table::Table;
use std::ops::Range;
use std::sync::{Arc, RwLock};

pub const BATCH: usize = 256;

/// The initial date worker watches journal changes from the instant its names are published.
pub struct Hydration {
    table: Arc<RwLock<Table>>,
    generation: u64,
    revision: u64,
}

impl Hydration {
    pub fn publish(table: Arc<RwLock<Table>>, mut names: Table) -> Self {
        let generation = names.id();
        let revision = names.touches();
        names.watch_touches(true);
        *table.write().unwrap_or_else(|p| p.into_inner()) = names;
        Self { table, generation, revision }
    }

    pub fn is_current(&self) -> bool {
        self.table.read().unwrap_or_else(|p| p.into_inner()).id() == self.generation
    }

    pub fn pending(&self, records: Range<usize>) -> Option<Vec<u32>> {
        let table = self.table.read().unwrap_or_else(|p| p.into_inner());
        (table.id() == self.generation).then(|| {
            records.map(|rec| rec as u32).filter(|&rec| self.missing(&table, rec)).collect()
        })
    }

    pub fn apply(&self, dates: &[(u32, i64)]) -> Option<usize> {
        let mut table = self.table.write().unwrap_or_else(|p| p.into_inner());
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

    fn missing(&self, table: &Table, rec: u32) -> bool {
        // A touched record may now refer to a different file, even if its date is still unknown.
        table.needs_modified(rec) && !table.touched_since(rec, self.revision)
    }
}

impl Drop for Hydration {
    fn drop(&mut self) {
        let mut table = self.table.write().unwrap_or_else(|p| p.into_inner());
        if table.id() == self.generation {
            table.watch_touches(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fsindex::table::ROOT;

    fn load_names() -> Table {
        let mut table = Table::new();
        table.upsert(20, ROOT, "notes-old.txt", false, false, 0);
        table.upsert(21, ROOT, "notes-new.txt", false, false, 0);
        table
    }

    #[test]
    fn names_are_searchable_before_hydration() {
        let table = Arc::new(RwLock::new(Table::new()));
        let hydration = Hydration::publish(table.clone(), load_names());
        assert_eq!(hydration.pending(20..20 + BATCH), Some(vec![20, 21]));
        let (begin, wait) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            wait.recv().unwrap();
            hydration.apply(&[(20, 10), (21, 20)])
        });

        let hits = table.read().unwrap().query("notes", 2);
        begin.send(()).unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|h| h.modified() == 0));
        assert_eq!(worker.join().unwrap(), Some(2));
        assert_eq!(table.read().unwrap().query("notes", 1)[0].rec, 21);
    }

    #[test]
    fn hydration_keeps_journal_updates_and_drops_old_generations() {
        let table = Arc::new(RwLock::new(Table::new()));
        let hydration = Hydration::publish(table.clone(), load_names());
        {
            let mut table = table.write().unwrap();
            table.touch(20);
            table.set_modified(20, 5);
            table.touch(21);
            table.remove(21);
            table.upsert(21, ROOT, "replacement.txt", false, false, 0);
        }
        assert_eq!(hydration.pending(20..22), Some(Vec::new()));
        assert_eq!(hydration.apply(&[(20, 100), (21, 200)]), Some(0));
        assert_eq!(table.read().unwrap().query("notes", 1)[0].modified(), 5);
        assert_eq!(table.read().unwrap().query("replacement", 1)[0].modified(), 0);

        let next = Hydration::publish(table.clone(), load_names());
        assert!(!hydration.is_current());
        assert_eq!(hydration.pending(20..22), None);
        assert_eq!(hydration.apply(&[(20, 100)]), None);
        assert_eq!(table.read().unwrap().query("notes", 1)[0].modified(), 0);
        drop(hydration);
        table.write().unwrap().touch(20);
        assert_eq!(next.apply(&[(20, 100), (21, 200)]), Some(1));
    }
}
