use super::{NONE, reserve_append};

const MAX_PACKED: u64 = 0xFF_FFFE;

/// Reverse lookups need three bytes on most volumes. A higher record number widens
/// this column once so even unusually sparse MFTs retain the complete record number.
pub(super) enum Records {
    Packed(Vec<[u8; 3]>),
    Wide(Vec<u32>),
}

impl Default for Records {
    fn default() -> Self {
        Self::Packed(Vec::new())
    }
}

impl Records {
    pub fn cover(&mut self, highest: u64) {
        if highest > MAX_PACKED && let Self::Packed(records) = self {
            let mut wide = Vec::with_capacity(records.len() + 1);
            wide.extend(records.iter().copied().map(decode));
            *self = Self::Wide(wide);
        }
    }

    pub fn get(&self, i: usize) -> u32 {
        match self {
            Self::Packed(records) => decode(records[i]),
            Self::Wide(records) => records[i],
        }
    }

    pub fn set(&mut self, i: usize, rec: u32) {
        match self {
            Self::Packed(records) => records[i] = encode(rec),
            Self::Wide(records) => records[i] = rec,
        }
    }

    pub fn push(&mut self, rec: u32, compact: bool) {
        self.cover(u64::from(rec));
        match self {
            Self::Packed(records) => {
                reserve_append(records, 1, compact);
                records.push(encode(rec));
            }
            Self::Wide(records) => {
                reserve_append(records, 1, compact);
                records.push(rec);
            }
        }
    }

    pub fn bytes(&self) -> usize {
        match self {
            Self::Packed(records) => records.capacity() * size_of::<[u8; 3]>(),
            Self::Wide(records) => records.capacity() * size_of::<u32>(),
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Packed(records) => records.len(),
            Self::Wide(records) => records.len(),
        }
    }

    pub fn shrink_to_fit(&mut self) {
        match self {
            Self::Packed(records) => records.shrink_to_fit(),
            Self::Wide(records) => records.shrink_to_fit(),
        }
    }

    pub fn truncate(&mut self, n: usize) {
        match self {
            Self::Packed(records) => records.truncate(n),
            Self::Wide(records) => records.truncate(n),
        }
    }
}

fn encode(rec: u32) -> [u8; 3] {
    debug_assert!(u64::from(rec) <= MAX_PACKED || rec == NONE);
    let [a, b, c, _] = rec.wrapping_add(1).to_le_bytes();
    [a, b, c]
}

fn decode([a, b, c]: [u8; 3]) -> u32 {
    u32::from_le_bytes([a, b, c, 0]).wrapping_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widening_preserves_maximum_packed_record_and_deleted_entries() {
        let highest = MAX_PACKED as u32;
        let mut records = Records::default();
        records.push(5, false);
        records.push(highest, false);
        records.set(0, NONE);
        assert_eq!(records.get(1), highest);
        records.push(highest + 1, false);
        assert_eq!(records.get(0), NONE);
        assert_eq!(records.get(1), highest);
        assert_eq!(records.get(2), highest + 1);
    }

    #[test]
    fn known_large_mft_starts_wide_even_with_few_names() {
        let mut records = Records::default();
        records.cover(MAX_PACKED + 1);
        records.push(7, false);
        assert!(matches!(records, Records::Wide(_)));
        assert_eq!(records.get(0), 7);
    }
}
