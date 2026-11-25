use super::reserve_append;
use std::collections::BTreeMap;

const NOISY: u32 = 1 << 31;
const TIME: u32 = NOISY - 1;
const OUTLIER: u32 = TIME;
const EPOCH: u64 = 116_444_736_000_000_000; // Unix epoch in FILETIME ticks.
const STEP: u64 = 20_000_000;
pub(super) const META_NOISY: u64 = 1 << 63;

/// Current dates use two-second buckets. Preserve unusual dates (before 1970 or
/// past 2106) exactly so they still sort and display correctly.
#[derive(Default)]
pub(super) struct Metadata {
    pub words: Vec<u32>,
    pub outliers: BTreeMap<usize, u64>,
}

impl Metadata {
    pub fn get(&self, i: usize) -> u64 {
        let word = self.words[i];
        let time = match word & TIME {
            0 => 0,
            OUTLIER => self.outliers[&i],
            bucket => EPOCH + u64::from(bucket - 1) * STEP,
        };
        time | (u64::from(word & NOISY) << 32)
    }

    pub fn push(&mut self, modified: u64, noisy: bool, compact: bool) {
        let i = self.words.len();
        reserve_append(&mut self.words, 1, compact);
        self.words.push(0);
        self.set(i, modified, noisy);
    }

    pub fn set(&mut self, i: usize, modified: u64, noisy: bool) {
        let word = &mut self.words[i];
        if *word & TIME == OUTLIER {
            self.outliers.remove(&i);
        }
        let time = if modified == 0 {
            0
        } else if let Some(delta) = modified.checked_sub(EPOCH)
            && delta / STEP < u64::from(OUTLIER - 1)
        {
            (delta / STEP) as u32 + 1
        } else {
            self.outliers.insert(i, modified);
            OUTLIER
        };
        *word = time | if noisy { NOISY } else { 0 };
    }

    pub fn noisy(&self, i: usize) -> bool {
        self.words[i] & NOISY != 0
    }

    pub fn needs_modified(&self, i: usize) -> bool {
        self.words[i] & TIME == 0
    }

    pub fn set_noisy(&mut self, i: usize, noisy: bool) {
        self.words[i] = (self.words[i] & TIME) | if noisy { NOISY } else { 0 };
    }

    pub fn clear(&mut self, i: usize) {
        if self.words[i] & TIME == OUTLIER {
            self.outliers.remove(&i);
        }
        self.words[i] = 0;
    }

    pub fn compact(&mut self, src: usize, dst: usize) {
        if src != dst {
            self.clear(dst);
            self.words[dst] = self.words[src];
            if let Some(modified) = self.outliers.remove(&src) {
                self.outliers.insert(dst, modified);
            }
            self.words[src] = 0;
        }
    }

    pub fn bytes(&self) -> usize {
        self.words.capacity() * size_of::<u32>()
            + self.outliers.len() * size_of::<(usize, u64)>()
    }
}
