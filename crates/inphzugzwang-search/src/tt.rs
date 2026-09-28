use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};

use inphzugzwang_core::Move;

const ENTRIES_PER_CLUSTER: usize = 4;
const CLUSTER_BYTES: usize = 64;
/// Stored depths are offset so that the quiescence depths and `DEPTH_NONE` fit in seven
/// unsigned bits, and a stored depth is never zero, which marks an empty entry.
const DEPTH_OFFSET: i32 = -7;
pub(super) const DEPTH_NONE: i32 = -6;
const DEPTH_MAX: i32 = 127 + DEPTH_OFFSET;

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
    /// The entry's data if it holds `key`. Key and data are written as two words, so a
    /// torn write from another thread fails the check instead of mixing positions.
    fn load(&self, key: u64) -> Option<u64> {
        let data = self.data.load(Ordering::Relaxed);
        (data != 0 && self.key_xor_data.load(Ordering::Relaxed) ^ data == key).then_some(data)
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

pub struct TranspositionTable {
    clusters: Box<[Cluster]>,
    age: AtomicU8,
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
            age: AtomicU8::new(0),
        })
    }

    pub fn next_generation(&self) {
        self.age.fetch_add(1, Ordering::Relaxed);
    }

    pub fn hashfull(&self) -> u16 {
        let sample = self.clusters.len().min(1000);
        let age = self.age() as u64;
        let occupied = self.clusters[..sample]
            .iter()
            .flat_map(|cluster| cluster.entries.iter())
            .filter(|entry| {
                let data = entry.data.load(Ordering::Relaxed);
                data != 0 && data >> 58 == age
            })
            .count();
        (occupied * 1000 / (sample * ENTRIES_PER_CLUSTER)) as u16
    }

    pub(super) fn probe(&self, key: u64) -> Option<Record> {
        self.cluster(key)
            .entries
            .iter()
            .find_map(|entry| entry.load(key))
            .map(unpack)
    }

    /// Stores a result the way Stockfish does: a result without a move keeps the move
    /// already stored for the position, and a shallower non-exact result for the same
    /// position does not replace a clearly deeper one. Otherwise the entry of the same
    /// position, an empty one, or the one of least depth after ageing is replaced.
    pub(super) fn store(&self, key: u64, record: Record) {
        let cluster = self.cluster(key);
        let age = self.age();
        let depth = record.depth.clamp(DEPTH_NONE, DEPTH_MAX);
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
                || depth + 2 * i32::from(record.pv) > previous.depth - 4;
            let data = if replace {
                pack(
                    &Record {
                        mv,
                        depth,
                        ..record
                    },
                    age,
                )
            } else if mv != previous.mv {
                (data & !0xFFFF) | u64::from(mv.raw())
            } else {
                return;
            };
            entry.write(key, data);
            return;
        }
        let mut replacement = &cluster.entries[0];
        let mut lowest_quality = i32::MAX;
        for entry in &cluster.entries {
            let data = entry.data.load(Ordering::Relaxed);
            if data == 0 {
                replacement = entry;
                break;
            }
            let age_distance = i32::from(age.wrapping_sub((data >> 58) as u8) & 63);
            let quality = ((data >> 48) & 127) as i32 - 8 * age_distance;
            if quality < lowest_quality {
                lowest_quality = quality;
                replacement = entry;
            }
        }
        replacement.write(key, pack(&Record { depth, ..record }, age));
    }

    fn age(&self) -> u8 {
        self.age.load(Ordering::Relaxed) & 63
    }

    fn cluster(&self, key: u64) -> &Cluster {
        let index = ((key as u128 * self.clusters.len() as u128) >> 64) as usize;
        &self.clusters[index]
    }
}

fn pack(record: &Record, age: u8) -> u64 {
    let flags = (record.depth - DEPTH_OFFSET) as u64
        | ((record.bound as u64) << 7)
        | (u64::from(record.pv) << 9)
        | (u64::from(age & 63) << 10);
    u64::from(record.mv.raw())
        | (u64::from(record.score as i16 as u16) << 16)
        | (u64::from(record.eval as i16 as u16) << 32)
        | (flags << 48)
}

fn unpack(data: u64) -> Record {
    let flags = data >> 48;
    Record {
        mv: Move::from_raw(data as u16),
        score: i32::from((data >> 16) as u16 as i16),
        eval: i32::from((data >> 32) as u16 as i16),
        depth: (flags & 127) as i32 + DEPTH_OFFSET,
        bound: match (flags >> 7) & 3 {
            0 => Bound::None,
            1 => Bound::Upper,
            2 => Bound::Lower,
            _ => Bound::Exact,
        },
        pv: flags & (1 << 9) != 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(depth: i32, bound: Bound, mv: Move) -> Record {
        Record {
            mv,
            score: -29_000,
            eval: 31,
            depth,
            bound,
            pv: false,
        }
    }

    #[test]
    fn records_round_trip_including_quiescence_depths() {
        for depth in [DEPTH_NONE, -1, 0, 1, 60, DEPTH_MAX] {
            for bound in [Bound::None, Bound::Upper, Bound::Lower, Bound::Exact] {
                let original = Record {
                    pv: depth % 2 == 0,
                    ..record(depth, bound, Move::from_raw(0x1234))
                };
                let unpacked = unpack(pack(&original, 5));
                assert_eq!(unpacked.depth, depth);
                assert_eq!(unpacked.bound, bound);
                assert_eq!(unpacked.pv, original.pv);
                assert_eq!(unpacked.score, original.score);
                assert_eq!(unpacked.eval, original.eval);
                assert_eq!(unpacked.mv, original.mv);
                assert_ne!(pack(&original, 5), 0);
            }
        }
    }

    #[test]
    fn stores_keep_moves_and_deeper_results() {
        let table = TranspositionTable::new(1).unwrap();
        let key = 0x1234_5678;
        let mv = Move::from_raw(0x0421);
        table.store(key, record(10, Bound::Lower, mv));
        table.store(key, record(3, Bound::Upper, Move::NULL));
        let kept = table.probe(key).unwrap();
        assert_eq!((kept.depth, kept.bound, kept.mv), (10, Bound::Lower, mv));
        table.store(key, record(3, Bound::Exact, Move::NULL));
        let exact = table.probe(key).unwrap();
        assert_eq!((exact.depth, exact.bound, exact.mv), (3, Bound::Exact, mv));
        assert!(table.probe(key + 1).is_none());
        table.next_generation();
        assert_eq!(table.hashfull(), 0);
    }
}
