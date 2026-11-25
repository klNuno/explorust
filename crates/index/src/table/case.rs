use std::collections::BTreeMap;

const INLINE_BITS: usize = 14;
const PAIR: u16 = 1 << INLINE_BITS;
const EXTERNAL: u16 = 1 << 15;
const POSITION: u16 = (1 << 7) - 1;

/// Most names only need a few case marks near the start. Long mixed-case names keep their
/// original bitmap separately, and search only reads it when reconstructing a path.
#[derive(Default)]
pub(super) struct Case {
    pub marks: Vec<u16>,
    pub(super) external: BTreeMap<usize, Box<[u8]>>,
}

impl Case {
    pub fn mark(&mut self, name: usize, bytes: usize, at: usize) {
        let marks = &mut self.marks[name];
        if *marks < PAIR {
            if at < INLINE_BITS {
                *marks |= 1 << at;
                return;
            }
            if at < usize::from(POSITION) && marks.count_ones() <= 1 {
                let first = if *marks == 0 {
                    0
                } else {
                    marks.trailing_zeros() as u16 + 1
                };
                *marks = PAIR | ((first << 7) | (at as u16 + 1));
                return;
            }
        } else if *marks < EXTERNAL && *marks >> 7 & POSITION == 0 && at < usize::from(POSITION) {
            *marks |= (at as u16 + 1) << 7;
            return;
        }
        let bits = self.external.entry(name).or_insert_with(|| {
            let mut bits = vec![0; bytes.div_ceil(8)].into_boxed_slice();
            for start in 0..bytes.min(usize::from(POSITION)) {
                if inline_change(*marks, start) {
                    bits[start >> 3] |= 1 << (start & 7);
                }
            }
            bits
        });
        *marks = EXTERNAL;
        bits[at >> 3] |= 1 << (at & 7);
    }

    pub fn unfold(&self, name: usize, folded: &str) -> String {
        let marks = self.marks[name];
        if marks == 0 {
            return folded.to_owned();
        }
        if marks == EXTERNAL {
            let bits = &self.external[&name];
            crate::fold::unfold(folded, |at| bits[at >> 3] & (1 << (at & 7)) != 0)
        } else {
            crate::fold::unfold(folded, |at| inline_change(marks, at))
        }
    }

    pub fn remove(&mut self, name: usize) {
        if self.marks[name] & EXTERNAL != 0 {
            self.external.remove(&name);
        }
    }

    pub fn compact(&mut self, src: usize, dst: usize) {
        if src != dst {
            self.marks[dst] = self.marks[src];
            if self.marks[src] & EXTERNAL != 0 {
                let bits = self.external.remove(&src).unwrap();
                self.external.insert(dst, bits);
            }
        }
    }

    pub fn bytes(&self) -> usize {
        self.marks.capacity() * size_of::<u16>()
            + self
                .external
                .values()
                .map(|bits| {
                    // The payload and key/value storage, before B-tree node overhead.
                    size_of::<usize>() + size_of::<Box<[u8]>>() + bits.len()
                })
                .sum::<usize>()
    }
}

fn inline_change(marks: u16, at: usize) -> bool {
    if marks < PAIR {
        at < INLINE_BITS && marks & (1 << at) != 0
    } else {
        let offset = at + 1;
        offset == usize::from(marks & POSITION) || offset == usize::from(marks >> 7 & POSITION)
    }
}
