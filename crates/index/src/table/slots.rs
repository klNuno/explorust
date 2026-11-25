use super::{DIR, IDX, NOISY, NONE, reserve_append};

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct Slot {
    pub parent: u32,
    /// Name index in the arenas plus DIR and NOISY, or NONE for a free slot.
    pub name: u32,
}

pub(super) const FREE: Slot = Slot { parent: 0, name: NONE };
/// A record inside a pruned tree: no name kept, and its children are pruned too.
pub(super) const PRUNED: Slot = Slot { parent: NONE, name: NONE };

const BITS: u32 = 23;
const MASK: u32 = (1 << BITS) - 1;
const FREE_PACKED: [u8; 6] = encode(FREE);

pub(super) enum Slots {
    Packed(Vec<[u8; 6]>),
    Wide(Vec<Slot>),
}

impl Default for Slots {
    fn default() -> Self {
        Self::Packed(Vec::new())
    }
}

impl Slots {
    pub fn set(&mut self, record: usize, slot: Slot) {
        let parent_wide = slot.parent >= MASK && slot.parent != NONE;
        let name_wide = slot.name & IDX >= MASK && slot.name != NONE;
        if (parent_wide || name_wide) && let Self::Packed(slots) = self {
            *self = Self::Wide(slots.iter().copied().map(decode).collect());
        }
        match self {
            Self::Packed(slots) => slots[record] = encode(slot),
            Self::Wide(slots) => slots[record] = slot,
        }
    }

    pub fn at(&self, record: usize) -> Slot {
        match self {
            Self::Packed(slots) => decode(slots[record]),
            Self::Wide(slots) => slots[record],
        }
    }

    pub fn get(&self, record: usize) -> Option<Slot> {
        match self {
            Self::Packed(slots) => slots.get(record).copied().map(decode),
            Self::Wide(slots) => slots.get(record).copied(),
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Packed(slots) => slots.len(),
            Self::Wide(slots) => slots.len(),
        }
    }

    pub fn bytes(&self) -> usize {
        match self {
            Self::Packed(slots) => slots.capacity() * size_of::<[u8; 6]>(),
            Self::Wide(slots) => slots.capacity() * size_of::<Slot>(),
        }
    }

    pub fn resize(&mut self, len: usize, compact: bool) {
        let more = len - self.len();
        match self {
            Self::Packed(slots) => {
                reserve_append(slots, more, compact);
                slots.resize(len, FREE_PACKED);
            }
            Self::Wide(slots) => {
                reserve_append(slots, more, compact);
                slots.resize(len, FREE);
            }
        }
    }

    pub fn shrink_to_fit(&mut self) {
        match self {
            Self::Packed(slots) => slots.shrink_to_fit(),
            Self::Wide(slots) => slots.shrink_to_fit(),
        }
    }
}

const fn encode(slot: Slot) -> [u8; 6] {
    let parent = slot.parent & MASK;
    let name = if slot.name == NONE {
        MASK
    } else {
        slot.name & IDX | (slot.name & (DIR | NOISY)) >> 7
    };
    let [a, b, c, d, e, f, _, _] = ((parent as u64) | ((name as u64) << BITS)).to_le_bytes();
    [a, b, c, d, e, f]
}

fn decode([a, b, c, d, e, f]: [u8; 6]) -> Slot {
    let packed = u64::from_le_bytes([a, b, c, d, e, f, 0, 0]);
    let parent = packed as u32 & MASK;
    let name = (packed >> BITS) as u32;
    Slot {
        parent: if parent == MASK { NONE } else { parent },
        name: if name == MASK { NONE } else { name & MASK | (name & !MASK) << 7 },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn growth_and_widening_preserve_free_pruned_and_flagged_slots() {
        let mut slots = Slots::default();
        slots.resize(5, false);
        let named = Slot { parent: MASK - 1, name: DIR | NOISY | (MASK - 1) };
        slots.set(1, named);
        slots.set(2, PRUNED);
        assert_eq!(slots.at(0), FREE);
        assert_eq!(slots.at(1), named);
        assert_eq!(slots.at(2), PRUNED);
        let wide = Slot { parent: MASK, name: DIR | MASK };
        slots.set(4, wide);
        assert!(matches!(&slots, Slots::Wide(_)));
        assert_eq!(slots.at(0), FREE);
        assert_eq!(slots.at(1), named);
        assert_eq!(slots.at(2), PRUNED);
        assert_eq!(slots.at(4), wide);
    }
}
