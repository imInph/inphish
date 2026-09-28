//! Transposition table with Stockfish 19's replacement rules (GPL-3.0,
//! https://github.com/official-stockfish/Stockfish), stored as two lockless words per
//! entry: the data, and the key xor the data, so that a torn write fails the key check.

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Mutex;

use inphzugzwang_core::Move;

use crate::{is_decisive, Memory, DEPTH_NONE};

const ENTRIES_PER_CLUSTER: usize = 4;
const CLUSTER_BYTES: usize = 64;
const GENERATION_MASK: u8 = 31;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Bound {
    /// Only the static evaluation is stored.
    None = 0,
    Upper = 1,
    Lower = 2,
    Exact = 3,
}

impl Bound {
    /// Whether a stored score with this bound can stand for a score on the `lower` side:
    /// at least as high when `lower`, at most as high otherwise.
    pub(super) fn covers(self, lower: bool) -> bool {
        self as u8 & if lower { 2 } else { 1 } != 0
    }
}

#[derive(Clone, Copy)]
pub(super) struct Record {
    pub mv: Move,
    /// Score as stored, relative to the node for mate and tablebase scores.
    pub score: i32,
    pub eval: i32,
    pub depth: i32,
    pub bound: Bound,
    pub pv: bool,
}

// Data word: move 0-15, score 16-31, evaluation 32-47, depth less DEPTH_NONE 48-55
// (zero marks an empty entry), bound 56-57, principal variation 58, generation 59-63.

fn pack(record: &Record, generation: u8) -> u64 {
    let depth = (record.depth - DEPTH_NONE).clamp(1, 255) as u64;
    u64::from(record.mv.raw())
        | (u64::from(record.score as i16 as u16) << 16)
        | (u64::from(record.eval as i16 as u16) << 32)
        | (depth << 48)
        | ((record.bound as u64) << 56)
        | (u64::from(record.pv) << 58)
        | (u64::from(generation & GENERATION_MASK) << 59)
}

fn unpack(data: u64) -> Record {
    Record {
        mv: Move::from_raw(data as u16),
        score: i32::from((data >> 16) as u16 as i16),
        eval: i32::from((data >> 32) as u16 as i16),
        depth: ((data >> 48) & 255) as i32 + DEPTH_NONE,
        bound: match (data >> 56) & 3 {
            0 => Bound::None,
            1 => Bound::Upper,
            2 => Bound::Lower,
            _ => Bound::Exact,
        },
        pv: data & (1 << 58) != 0,
    }
}

fn depth8(data: u64) -> i32 {
    ((data >> 48) & 255) as i32
}

fn generation_of(data: u64) -> u8 {
    (data >> 59) as u8 & GENERATION_MASK
}

struct Entry {
    key_xor_data: AtomicU64,
    data: AtomicU64,
}

impl Default for Entry {
    fn default() -> Self {
        Self {
            key_xor_data: AtomicU64::new(0),
            data: AtomicU64::new(0),
        }
    }
}

impl Entry {
    /// The entry's data if it holds `key`.
    fn load(&self, key: u64) -> Option<u64> {
        let data = self.data.load(Ordering::Relaxed);
        (depth8(data) != 0 && self.key_xor_data.load(Ordering::Relaxed) ^ data == key)
            .then_some(data)
    }

    fn write(&self, key: u64, data: u64) {
        self.data.store(data, Ordering::Relaxed);
        self.key_xor_data.store(key ^ data, Ordering::Relaxed);
    }
}

#[repr(align(64))]
struct Cluster {
    entries: [Entry; ENTRIES_PER_CLUSTER],
}

impl Default for Cluster {
    fn default() -> Self {
        Self {
            entries: std::array::from_fn(|_| Entry::default()),
        }
    }
}

/// What time management carries from one search of a game to the next.
#[derive(Clone, Copy)]
pub(super) struct Previous {
    /// Best score and its running average from the last search, if there was one.
    pub score: Option<(i32, i32)>,
    pub time_reduction: f64,
    /// Stockfish's `originalTimeAdjust`, fixed on the first timed move of a game.
    pub time_adjust: Option<f64>,
}

impl Default for Previous {
    fn default() -> Self {
        Self {
            score: None,
            time_reduction: 0.85,
            time_adjust: None,
        }
    }
}

/// The shared transposition table, which also keeps each search thread's statistics
/// and the time-management state between moves, so that all are cleared together.
pub struct TranspositionTable {
    clusters: Box<[Cluster]>,
    generation: AtomicU8,
    memories: Mutex<Vec<Memory>>,
    previous: Mutex<Previous>,
}

impl TranspositionTable {
    pub fn new(megabytes: u32) -> Option<Self> {
        let count = (megabytes as usize).checked_mul(1024 * 1024 / CLUSTER_BYTES)?;
        if count == 0 {
            return None;
        }
        let mut clusters = Vec::new();
        clusters.try_reserve_exact(count).ok()?;
        clusters.resize_with(count, Cluster::default);
        Some(Self {
            clusters: clusters.into_boxed_slice(),
            generation: AtomicU8::new(0),
            memories: Mutex::new(Vec::new()),
            previous: Mutex::new(Previous::default()),
        })
    }

    /// Starts a new search generation, against which entries age.
    pub fn next_generation(&self) {
        let next = (self.generation() + 1) & GENERATION_MASK;
        self.generation.store(next, Ordering::Relaxed);
    }

    fn generation(&self) -> u8 {
        self.generation.load(Ordering::Relaxed)
    }

    fn relative_age(&self, data: u64) -> i32 {
        i32::from(self.generation().wrapping_sub(generation_of(data)) & GENERATION_MASK)
    }

    /// Permille of sampled entries written in the current generation.
    pub fn hashfull(&self) -> u16 {
        let sample = self.clusters.len().min(1000);
        let occupied = self.clusters[..sample]
            .iter()
            .flat_map(|cluster| cluster.entries.iter())
            .filter(|entry| {
                let data = entry.data.load(Ordering::Relaxed);
                depth8(data) != 0 && self.relative_age(data) == 0
            })
            .count();
        (occupied * 1000 / (sample * ENTRIES_PER_CLUSTER)) as u16
    }

    /// Statistics kept from an earlier search, or new ones.
    pub(super) fn take_memory(&self) -> Memory {
        self.memories
            .lock()
            .ok()
            .and_then(|mut memories| memories.pop())
            .unwrap_or_else(Memory::new)
    }

    /// Keeps a search thread's statistics for the next search.
    pub(super) fn keep_memory(&self, memory: Memory) {
        if let Ok(mut memories) = self.memories.lock() {
            memories.push(memory);
        }
    }

    pub(super) fn previous(&self) -> Previous {
        self.previous
            .lock()
            .map_or_else(|_| Previous::default(), |previous| *previous)
    }

    pub(super) fn set_previous(&self, previous: Previous) {
        if let Ok(mut stored) = self.previous.lock() {
            *stored = previous;
        }
    }

    pub(super) fn probe(&self, key: u64) -> Option<Record> {
        self.cluster(key)
            .entries
            .iter()
            .find_map(|entry| entry.load(key))
            .map(unpack)
    }

    /// Stockfish's `TTEntry::save`: a result without a move keeps the stored move, and a
    /// result replaces the stored one of the same position when it is exact, from a newer
    /// search, or not clearly shallower. A kept, non-exact decisive entry of depth 5 or
    /// more loses a ply instead, which helps elementary mates. Another position's result
    /// replaces the entry of least depth after ageing.
    pub(super) fn store(&self, key: u64, record: Record) {
        let cluster = self.cluster(key);
        let generation = self.generation();
        for entry in &cluster.entries {
            let Some(data) = entry.load(key) else {
                continue;
            };
            let previous = unpack(data);
            let mv = if record.mv == Move::NULL {
                previous.mv
            } else {
                record.mv
            };
            let replace = record.bound == Bound::Exact
                || record.depth - DEPTH_NONE + 2 * i32::from(record.pv) > depth8(data) - 4
                || self.relative_age(data) != 0;
            if replace {
                entry.write(key, pack(&Record { mv, ..record }, generation));
            } else {
                let mut data = (data & !0xFFFF) | u64::from(mv.raw());
                if depth8(data) + DEPTH_NONE >= 5
                    && previous.bound != Bound::Exact
                    && is_decisive(previous.score)
                {
                    data -= 1 << 48;
                }
                entry.write(key, data);
            }
            return;
        }
        let mut replacement = &cluster.entries[0];
        let mut lowest = i32::MAX;
        for entry in &cluster.entries {
            let data = entry.data.load(Ordering::Relaxed);
            let value = depth8(data) - 8 * self.relative_age(data);
            if value < lowest {
                lowest = value;
                replacement = entry;
            }
        }
        replacement.write(key, pack(&record, generation));
    }

    /// Lowers the stored depth of `key`'s entry, marking it less useful.
    pub(super) fn penalize(&self, key: u64, plies: i32) {
        for entry in &self.cluster(key).entries {
            if let Some(data) = entry.load(key) {
                let depth = (depth8(data) - plies).max(0) as u64;
                entry.write(key, (data & !(255 << 48)) | (depth << 48));
                return;
            }
        }
    }

    /// Fetches the cluster of `key` into the cache ahead of a probe.
    pub(super) fn prefetch(&self, key: u64) {
        let cluster = std::ptr::from_ref(self.cluster(key));
        #[cfg(target_arch = "x86_64")]
        // SAFETY: prefetching has no effect on memory and the pointer is valid.
        unsafe {
            std::arch::x86_64::_mm_prefetch(cluster.cast::<i8>(), std::arch::x86_64::_MM_HINT_T0);
        }
        #[cfg(target_arch = "aarch64")]
        // SAFETY: as above.
        unsafe {
            std::arch::asm!(
                "prfm pldl1keep, [{0}]",
                in(reg) cluster,
                options(nostack, readonly, preserves_flags)
            );
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        let _ = cluster;
    }

    fn cluster(&self, key: u64) -> &Cluster {
        let index = ((u128::from(key) * self.clusters.len() as u128) >> 64) as usize;
        &self.clusters[index]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(depth: i32, bound: Bound, mv: Move, score: i32) -> Record {
        Record {
            mv,
            score,
            eval: 31,
            depth,
            bound,
            pv: false,
        }
    }

    #[test]
    fn records_round_trip() {
        for depth in [DEPTH_NONE + 1, -1, 0, 1, 60, 245] {
            for bound in [Bound::None, Bound::Upper, Bound::Lower, Bound::Exact] {
                let original = Record {
                    pv: depth % 2 == 0,
                    ..record(depth, bound, Move::from_raw(0x1234), -29_000)
                };
                let unpacked = unpack(pack(&original, 17));
                assert_eq!(
                    (unpacked.depth, unpacked.bound, unpacked.pv, unpacked.score),
                    (depth, bound, original.pv, original.score)
                );
                assert_eq!((unpacked.eval, unpacked.mv), (original.eval, original.mv));
                assert_eq!(generation_of(pack(&original, 17)), 17);
            }
        }
    }

    #[test]
    fn stores_keep_moves_and_deeper_results() {
        let table = TranspositionTable::new(1).unwrap();
        let key = 0x1234_5678;
        let mv = Move::from_raw(0x0421);
        table.store(key, record(10, Bound::Lower, mv, 40));
        table.store(key, record(3, Bound::Upper, Move::NULL, 12));
        let kept = table.probe(key).unwrap();
        assert_eq!((kept.depth, kept.bound, kept.mv), (10, Bound::Lower, mv));
        table.store(key, record(3, Bound::Exact, Move::NULL, 12));
        let exact = table.probe(key).unwrap();
        assert_eq!((exact.depth, exact.bound, exact.mv), (3, Bound::Exact, mv));
        table.penalize(key, 1);
        assert_eq!(table.probe(key).unwrap().depth, 2);
        assert!(table.probe(key + 1).is_none());
        table.next_generation();
        assert_eq!(table.hashfull(), 0);
        table.store(key, record(1, Bound::Upper, Move::NULL, 5));
        assert_eq!(table.probe(key).unwrap().depth, 1, "a new search replaces");
    }
}
