use std::collections::BTreeMap;

const INLINE_BITS: usize = 6;
const MASK: u8 = 1 << 7;
const PAIR: u8 = MASK + (1 << INLINE_BITS);
const EXTERNAL: u8 = u8::MAX;

/// Most names only need a few case marks near the start. Long mixed-case names keep their
/// original bitmap separately, and search only reads it when reconstructing a path.
#[derive(Default)]
pub(super) struct Case {
    pub marks: Vec<u8>,
    pub(super) external: BTreeMap<usize, Box<[u8]>>,
}

impl Case {
    pub fn mark(&mut self, name: usize, bytes: usize, at: usize) {
        let marks = &mut self.marks[name];
        match *marks {
            0 if at < usize::from(MASK - 1) => {
                *marks = at as u8 + 1;
                return;
            }
            1..=6 if at < INLINE_BITS => {
                *marks = MASK | (1 << (*marks - 1)) | (1 << at);
                return;
            }
            1 if at <= usize::from(EXTERNAL - PAIR) => {
                *marks = PAIR + (at as u8 - 1);
                return;
            }
            MASK..PAIR if at < INLINE_BITS => {
                *marks |= 1 << at;
                return;
            }
            _ => {}
        }
        let bits = self.external.entry(name).or_insert_with(|| {
            let mut bits = vec![0; bytes.div_ceil(8)].into_boxed_slice();
            for start in 0..bytes.min(usize::from(MASK - 1)) {
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
        if self.marks[name] == EXTERNAL {
            self.external.remove(&name);
        }
    }

    pub fn compact(&mut self, src: usize, dst: usize) {
        if src != dst {
            self.marks[dst] = self.marks[src];
            if self.marks[src] == EXTERNAL {
                let bits = self.external.remove(&src).unwrap();
                self.external.insert(dst, bits);
            }
        }
    }

    pub fn bytes(&self) -> usize {
        self.marks.capacity()
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

fn inline_change(marks: u8, at: usize) -> bool {
    match marks {
        0..MASK => at + 1 == usize::from(marks),
        MASK..PAIR => at < INLINE_BITS && marks & (1 << at) != 0,
        PAIR..EXTERNAL => at == 0 || at == usize::from(marks - PAIR) + 1,
        EXTERNAL => false,
    }
}
