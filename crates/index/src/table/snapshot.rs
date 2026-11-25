//! Versioned copy of the settled MFT, before journal updates and background metadata work.
//! Only the volume worker's private cache uses this format.

use super::{PREFIX_WORDS, Records, SEP, Slots, Table};
use bytemuck::{Pod, cast_slice, cast_slice_mut};
use crc32fast::Hasher;
use std::io::{self, Read, Write};

const VERSION: &[u8; 8] = b"EXPMFT05";
const PREVIOUS_VERSION: &[u8; 8] = b"EXPMFT04";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapshotCursor {
    pub journal: u64,
    pub next: i64,
    /// A zero identifier means the boot could not be identified.
    pub boot: [u8; 16],
}

impl SnapshotCursor {
    pub fn verified_in(self, boot: [u8; 16]) -> bool {
        boot != [0; 16] && self.boot == boot
    }
}

pub struct Snapshot {
    pub names: Table,
    pub cursor: SnapshotCursor,
    pub sequences: Vec<u16>,
    pub pruned: Vec<u64>,
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid MFT snapshot")
}

struct Output<W> {
    writer: W,
    hash: Hasher,
}

impl<W: Write> Output<W> {
    fn bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.hash.update(bytes);
        self.writer.write_all(bytes)
    }

    fn count(&mut self, count: usize) -> io::Result<()> {
        self.bytes(
            &u32::try_from(count)
                .map_err(io::Error::other)?
                .to_le_bytes(),
        )
    }

    fn arena<T: Pod>(&mut self, data: &[T]) -> io::Result<()> {
        self.count(data.len())?;
        self.bytes(cast_slice(data))
    }
}

struct Input<R> {
    reader: R,
    hash: Hasher,
    remaining: u64,
}

impl<R: Read> Input<R> {
    fn bytes(&mut self, bytes: &mut [u8]) -> io::Result<()> {
        self.remaining = self
            .remaining
            .checked_sub(bytes.len() as u64)
            .ok_or_else(invalid)?;
        self.reader.read_exact(bytes)?;
        self.hash.update(bytes);
        Ok(())
    }

    fn word<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        let mut word = [0; N];
        self.bytes(&mut word)?;
        Ok(word)
    }

    fn count(&mut self) -> io::Result<usize> {
        Ok(u32::from_le_bytes(self.word()?) as usize)
    }

    fn arena<T: Pod>(&mut self) -> io::Result<Vec<T>> {
        let count = self.count()?;
        let bytes = count.checked_mul(size_of::<T>()).ok_or_else(invalid)?;
        if bytes as u64 > self.remaining {
            return Err(invalid());
        }
        let mut values = Vec::new();
        values.try_reserve_exact(count).map_err(io::Error::other)?;
        values.resize(count, T::zeroed());
        self.bytes(cast_slice_mut(&mut values))?;
        Ok(values)
    }

    fn finish(mut self) -> io::Result<()> {
        let mut checksum = [0; 4];
        self.reader.read_exact(&mut checksum)?;
        if self.remaining != 0 || self.hash.finalize() != u32::from_le_bytes(checksum) {
            return Err(invalid());
        }
        Ok(())
    }
}

impl Snapshot {
    pub fn write(
        writer: impl Write,
        names: &Table,
        cursor: SnapshotCursor,
        sequences: &[u16],
        pruned: &[u64],
    ) -> io::Result<()> {
        if names.dead != 0 || names.compaction.is_some() || names.journal_current {
            return Err(invalid());
        }
        let mut out = Output {
            writer,
            hash: Hasher::new(),
        };
        out.bytes(VERSION)?;
        out.bytes(&cursor.journal.to_le_bytes())?;
        out.bytes(&cursor.next.to_le_bytes())?;
        out.bytes(&cursor.boot)?;
        match &names.slots {
            Slots::Packed(slots) => {
                out.bytes(&[0])?;
                out.arena(slots)?;
            }
            Slots::Wide(slots) => {
                out.bytes(&[1])?;
                out.arena(slots)?;
            }
        }
        out.arena(&names.case.marks)?;
        out.arena(&names.fold)?;
        out.arena(&names.prefixes)?;
        out.arena(&names.starts)?;
        match &names.recs {
            Records::Packed(records) => {
                out.bytes(&[0])?;
                out.arena(records)?;
            }
            Records::Wide(records) => {
                out.bytes(&[1])?;
                out.arena(records)?;
            }
        }
        out.arena(&names.meta.words)?;
        out.count(names.meta.outliers.len())?;
        for (&i, &time) in &names.meta.outliers {
            out.count(i)?;
            out.bytes(&time.to_le_bytes())?;
        }
        out.arena(&names.prune_roots)?;
        out.count(names.case.external.len())?;
        for (&i, bits) in &names.case.external {
            out.count(i)?;
            out.arena(bits.as_ref())?;
        }
        out.arena(sequences)?;
        out.arena(pruned)?;
        out.writer.write_all(&out.hash.finalize().to_le_bytes())
    }

    /// Check journal identity and cursor bounds before allocating the table.
    pub fn read_if(
        reader: impl Read,
        size: u64,
        usable: impl FnOnce(SnapshotCursor) -> bool,
    ) -> io::Result<Option<Self>> {
        let remaining = size.checked_sub(4).ok_or_else(invalid)?;
        let mut input = Input {
            reader,
            hash: Hasher::new(),
            remaining,
        };
        let version = input.word::<8>()?;
        if &version != VERSION && &version != PREVIOUS_VERSION {
            return Err(invalid());
        }
        let cursor = SnapshotCursor {
            journal: u64::from_le_bytes(input.word()?),
            next: i64::from_le_bytes(input.word()?),
            boot: if &version == VERSION { input.word()? } else { [0; 16] },
        };
        if !usable(cursor) {
            return Ok(None);
        }
        let mut names = Table::new();
        names.slots = match input.word()? {
            [0] => Slots::Packed(input.arena()?),
            [1] => Slots::Wide(input.arena()?),
            _ => return Err(invalid()),
        };
        names.case.marks = input.arena()?;
        names.fold = input.arena()?;
        names.prefixes = input.arena()?;
        if !names.prefixes.is_empty() && names.prefixes.len() != PREFIX_WORDS {
            return Err(invalid());
        }
        names.starts = input.arena()?;
        names.recs = match input.word()? {
            [0] => Records::Packed(input.arena()?),
            [1] => Records::Wide(input.arena()?),
            _ => return Err(invalid()),
        };
        names.meta.words = input.arena()?;
        let outliers = input.count()?;
        if outliers > names.meta.words.len() {
            return Err(invalid());
        }
        for _ in 0..outliers {
            let i = input.count()?;
            if i >= names.meta.words.len() {
                return Err(invalid());
            }
            names.meta.outliers.insert(i, u64::from_le_bytes(input.word()?));
        }
        names.prune_roots = input.arena()?;
        let external = input.count()?;
        if external > names.case.marks.len() {
            return Err(invalid());
        }
        for _ in 0..external {
            let i = input.count()?;
            if i >= names.case.marks.len() {
                return Err(invalid());
            }
            let bits = input.arena::<u8>()?;
            names.case.external.insert(i, bits.into_boxed_slice());
        }
        let sequences = input.arena()?;
        let pruned = input.arena()?;
        input.finish()?;
        let count = names.recs.len();
        if names.starts.len() != count
            || names.case.marks.len() != count
            || names.meta.words.len() != count
            || names.fold.first() != Some(&SEP)
            || names.fold.last() != Some(&SEP)
        {
            return Err(invalid());
        }
        names.live = count;
        names.compact_growth = true;
        Ok(Some(Self {
            names,
            cursor,
            sequences,
            pruned,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::ROOT;

    #[test]
    fn snapshot_restores_case_ranking_and_pruned_records() {
        let mut names = Table::new();
        names.upsert(17, ROOT, "Work", true, false, 0);
        names.upsert(18, 17, "ÉTÉ really long TITLE.txt", false, false, 7);
        names.upsert(19, ROOT, "WinSxS", true, false, 0);
        names.upsert(20, 19, "skipped.txt", false, false, 2);
        names.settle();
        let cursor = SnapshotCursor {
            journal: 7,
            next: 42,
            boot: [42; 16],
        };
        let mut bytes = Vec::new();
        Snapshot::write(&mut bytes, &names, cursor, &[7; 21], &[20]).unwrap();
        let read = |bytes: &[u8]| Snapshot::read_if(bytes, bytes.len() as u64, |c| c == cursor);
        let snapshot = read(&bytes).unwrap().unwrap();
        assert_eq!(snapshot.cursor, cursor);
        assert!(snapshot.cursor.verified_in(cursor.boot));
        assert!(!snapshot.cursor.verified_in([43; 16]));
        assert!(!SnapshotCursor { boot: [0; 16], ..cursor }.verified_in([0; 16]));
        assert_eq!(snapshot.sequences, [7; 21]);
        assert_eq!(snapshot.pruned, [20]);
        assert_eq!(snapshot.names.state(20), crate::table::RecState::Pruned);
        assert_eq!(snapshot.names.path(18, "C:\\"), names.path(18, "C:\\"));
        assert_eq!(snapshot.names.query("été", 5), names.query("été", 5));
        assert!(
            Snapshot::read_if(&bytes[..40], bytes.len() as u64, |_| false)
                .unwrap()
                .is_none()
        );

        let last = bytes.len() - 1;
        assert!(read(&bytes[..last]).is_err());
        bytes[last] ^= 1;
        assert!(read(&bytes).is_err());

        let mut legacy = bytes[..last - 3].to_vec();
        legacy[..8].copy_from_slice(PREVIOUS_VERSION);
        legacy.drain(24..40);
        legacy.extend_from_slice(&crc32fast::hash(&legacy).to_le_bytes());
        let previous = Snapshot::read_if(&legacy[..], legacy.len() as u64, |_| true).unwrap().unwrap();
        assert!(!previous.cursor.verified_in(cursor.boot));
        assert_eq!(previous.names.path(18, "C:\\"), names.path(18, "C:\\"));
    }
}
