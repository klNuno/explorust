use super::{DIR, IDX, NONE, PRUNED, Table, reserve_append};
use crate::usn::{ATTR_DIRECTORY, ATTR_HIDDEN, ATTR_SYSTEM, Record};
use parking_lot::{RwLock, RwLockWriteGuard};

const BATCH: usize = 1024;
const WORD_BITS: usize = u64::BITS as usize;

/// Refresh the published names against an unfiltered MFT, using the existing arenas. The caller
/// keeps the journal writer stopped and replays from its pre-scan cursor after this returns.
pub fn reconcile<E>(
    table: &RwLock<Table>,
    sequences: &mut Vec<u16>,
    pruned: &mut Vec<u64>,
    mut enumerate: impl FnMut(bool, &mut dyn FnMut(&[Record])) -> Result<(), E>,
) -> Result<usize, E> {
    table.write().prune_roots.clear();
    pruned.clear();
    let mut seen = vec![0u64; sequences.len().div_ceil(WORD_BITS)];
    let mut corrections = 0;
    let mut directory_edits = Vec::new();

    // Record order is unrelated to directory depth. Refresh directories first so the second
    // pass can skip names still inside WinSxS and recover files whose directories moved out.
    for directories in [true, false] {
        enumerate(directories, &mut |records| {
            for records in records.chunks(BATCH) {
                let mut names = table.write();
                for r in records {
                    let dir = r.attrs & ATTR_DIRECTORY != 0;
                    if dir != directories || !(16..u64::from(u32::MAX)).contains(&r.rec) {
                        continue;
                    }
                    let i = r.rec as usize;
                    let word = i / WORD_BITS;
                    if sequences.len() <= i {
                        reserve_append(sequences, i + 1 - sequences.len(), true);
                        sequences.resize(i + 1, 0);
                    }
                    if seen.len() <= word {
                        seen.resize(word + 1, 0);
                    }
                    seen[word] |= 1 << (i % WORD_BITS);
                    let (live, offsets) = (names.live, names.starts.len());
                    apply(&mut names, r, dir, sequences[i], pruned);
                    sequences[i] = r.seq;
                    // Replacing a name appends an offset, while exposing or hiding one
                    // changes the live count. Compaction runs only outside this batch.
                    if (live, offsets) != (names.live, names.starts.len()) {
                        if dir {
                            directory_edits.push((i, names.live <= live));
                        } else {
                            corrections += 1;
                        }
                    }
                }
                RwLockWriteGuard::unlock_fair(names);
            }
            loop {
                let mut names = table.write();
                let more = names.compact_pending_slice();
                RwLockWriteGuard::unlock_fair(names);
                if !more {
                    break;
                }
            }
        })?;
        if directories {
            corrections += settle_directories(table, &seen, std::mem::take(&mut directory_edits));
        }
    }

    // Absence is only meaningful once both passes reach EOF. The journal supplies creations,
    // renames and deletes that raced the scan because no live follower can skip past the cursor.
    let records = table.read().slots.len().max(sequences.len());
    for start in (0..records).step_by(BATCH) {
        let mut names = table.write();
        for i in start..(start + BATCH).min(records) {
            if !was_seen(&seen, i) {
                if let Some(slot) = names.slots.get(i)
                    && (slot.name != NONE || slot.parent == NONE) {
                    corrections += usize::from(slot.name != NONE);
                    names.remove(i as u32);
                }
                if let Some(sequence) = sequences.get_mut(i) {
                    *sequence = 0;
                }
            }
        }
        RwLockWriteGuard::unlock_fair(names);
    }
    loop {
        let mut names = table.write();
        let more = (names.dead != 0 || names.compaction.is_some()) && names.compact_slice(BATCH);
        RwLockWriteGuard::unlock_fair(names);
        if !more {
            break;
        }
    }
    Ok(corrections)
}

fn was_seen(seen: &[u64], i: usize) -> bool {
    seen.get(i / WORD_BITS)
        .is_some_and(|word| word & (1 << (i % WORD_BITS)) != 0)
}

fn settle_directories(table: &RwLock<Table>, seen: &[u64], mut edits: Vec<(usize, bool)>) -> usize {
    // Count a directory after pruning so a temporarily restored name that remains pruned
    // does not register as an offline change.
    edits.sort_unstable_by_key(|&(record, _)| record);
    let mut edits = edits.into_iter().peekable();
    let mut corrections = 0;
    let slots = table.read().slots.len();
    for start in (0..slots).step_by(BATCH) {
        let mut names = table.write();
        for i in start..(start + BATCH).min(slots) {
            let before = edits.next_if(|&(record, _)| record == i);
            let slot = names.slots.at(i);
            if slot.name == NONE || slot.name & DIR == 0 || !was_seen(seen, i) {
                corrections += usize::from(before.is_some_and(|(_, named)| named));
                continue;
            }
            if names.walk_pruned(i as u32) {
                corrections += usize::from(before.is_none_or(|(_, named)| named));
                names.remove(i as u32);
                names.slots.set(i, PRUNED);
            } else {
                corrections += usize::from(before.is_some());
                let noisy = names.walk_noisy(i as u32);
                names.meta.set_noisy((slot.name & IDX) as usize, noisy);
            }
        }
        RwLockWriteGuard::unlock_fair(names);
    }
    corrections
}

fn apply(names: &mut Table, r: &Record, dir: bool, old_sequence: u16, pruned: &mut Vec<u64>) {
    let rec = r.rec as u32;
    let i = rec as usize;
    let parent = r.parent.min(u64::from(u32::MAX)) as u32;
    if old_sequence != r.seq || r.name.is_empty() || r.name.contains('\n') {
        names.remove(rec);
    }
    if !dir && names.pruned_parent(parent) {
        if i >= names.slots.len() {
            names.slots.resize(i + 1, names.compact_growth);
        }
        if names.slots.at(i) != PRUNED {
            names.remove(rec);
            names.slots.set(i, PRUNED);
        }
        pruned.push(r.rec | (u64::from(r.seq) << 48));
    } else {
        let noisy = r.attrs & (ATTR_HIDDEN | ATTR_SYSTEM) != 0;
        names.upsert_record(rec, parent, &r.name, dir, noisy, 0, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::{ROOT, RecState, Snapshot, SnapshotCursor};
    use crate::usn;

    fn record(rec: u32, parent: u32, name: &str, dir: bool) -> Record {
        let mut result = None;
        usn::parse(
            &usn::encode(
                u64::from(rec) | (7 << 48),
                parent.into(),
                0,
                0,
                if dir { ATTR_DIRECTORY } else { 0 },
                name,
            ),
            |r| result = Some(r),
        );
        result.unwrap()
    }

    #[test]
    fn repairs_offline_creates_renames_deletes_and_unprunes_a_tree() {
        let mut table = Table::new();
        table.upsert(20, ROOT, "before.txt", false, false, 0);
        table.upsert(22, ROOT, "deleted.txt", false, false, 0);
        table.upsert(30, ROOT, "WinSxS", true, false, 0);
        table.upsert(31, 30, "unpruned.txt", false, false, 0);
        table.upsert(40, ROOT, "Windows", true, false, 0);
        table.upsert(41, 40, "WinSxS", true, false, 0);
        table.upsert(42, 41, "Manifest", true, false, 0);
        let mut bytes = Vec::new();
        let cursor = SnapshotCursor {
            journal: 7,
            next: 42,
            boot: [1; 16],
        };
        Snapshot::write(&mut bytes, &table, cursor, &[7; 43], &[31]).unwrap();
        let snapshot = Snapshot::read_if(&bytes[..], bytes.len() as u64, |_| true)
            .unwrap()
            .unwrap();
        let table = RwLock::new(snapshot.names);
        let mut sequences = snapshot.sequences;
        let mut pruned = snapshot.pruned;
        let records = [
            record(20, ROOT, "after.txt", false),
            record(25, ROOT, "created.txt", false),
            record(30, ROOT, "Visible", true),
            record(31, 30, "unpruned.txt", false),
            record(40, ROOT, "Windows", true),
            record(41, 40, "WinSxS", true),
            record(42, 41, "Manifest", true),
        ];
        let corrections = reconcile(&table, &mut sequences, &mut pruned, |_, apply| {
            apply(&records);
            Ok::<_, ()>(())
        })
        .unwrap();
        assert_eq!(corrections, 5);
        let table = table.read();
        assert_eq!(table.state(22), RecState::Free);
        assert_eq!(table.path(20, "C:\\").as_deref(), Some("C:\\after.txt"));
        assert_eq!(table.path(25, "C:\\").as_deref(), Some("C:\\created.txt"));
        assert_eq!(
            table.path(31, "C:\\").as_deref(),
            Some("C:\\Visible\\unpruned.txt")
        );
        assert_eq!(sequences[22], 0);
        assert_eq!(table.state(42), RecState::Pruned);
        assert!(pruned.is_empty());
    }

    #[test]
    fn interrupted_enumeration_keeps_records_it_has_not_seen() {
        let mut names = Table::new();
        names.upsert(20, ROOT, "keep.txt", false, false, 0);
        names.upsert(22, ROOT, "Keep Folder", true, false, 0);
        let names = RwLock::new(names);
        let mut calls = 0;
        let result = reconcile(&names, &mut vec![7; 23], &mut Vec::new(), |_, apply| {
            apply(&[record(21, ROOT, "new.txt", false)]);
            calls += 1;
            if calls == 1 { Ok(()) } else { Err(()) }
        });
        assert_eq!(result, Err(()));
        assert!(names.read().has_name(20));
        assert!(names.read().has_name(22));
    }
}
