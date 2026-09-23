use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};

use inphzugzwang_core::Move;

use crate::{MATE, MAX_PLY};

const ENTRIES_PER_CLUSTER: usize = 4;
const CLUSTER_BYTES: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Bound {
    Upper = 1,
    Lower = 2,
    Exact = 3,
}

#[derive(Clone, Copy)]
pub(super) struct Record {
    pub mv: Move,
    pub score: i32,
    pub eval: i32,
    pub depth: u8,
    pub bound: Bound,
    pub pv: bool,
    pub(super) age: u8,
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
        let age = self.age.load(Ordering::Relaxed) & 63;
        let occupied = self.clusters[..sample]
            .iter()
            .flat_map(|cluster| cluster.entries.iter())
            .filter(|entry| {
                let data = entry.data.load(Ordering::Relaxed);
                data != 0 && (data >> 58) as u8 == age
            })
            .count();
        (occupied * 1000 / (sample * ENTRIES_PER_CLUSTER)) as u16
    }

    pub(super) fn probe(&self, key: u64, ply: usize) -> Option<Record> {
        self.cluster(key).entries.iter().find_map(|entry| {
            let data = entry.data.load(Ordering::Relaxed);
            if data == 0 || entry.key_xor_data.load(Ordering::Relaxed) ^ data != key {
                return None;
            }
            Some(unpack(data, ply))
        })
    }

    pub(super) fn store(&self, key: u64, ply: usize, mut record: Record) {
        let cluster = self.cluster(key);
        let age = self.age.load(Ordering::Relaxed) & 63;
        record.age = age;
        let mut replacement = &cluster.entries[0];
        let mut lowest_quality = i32::MAX;
        for entry in &cluster.entries {
            let data = entry.data.load(Ordering::Relaxed);
            if data == 0 {
                replacement = entry;
                break;
            }
            if entry.key_xor_data.load(Ordering::Relaxed) ^ data == key {
                let previous = unpack(data, ply);
                if previous.depth > record.depth && record.bound != Bound::Exact {
                    return;
                }
                replacement = entry;
                break;
            }
            let stored_age = (data >> 58) as u8;
            let age_distance = age.wrapping_sub(stored_age) & 63;
            let quality = ((data >> 48) & 127) as i32 - 8 * i32::from(age_distance);
            if quality < lowest_quality {
                lowest_quality = quality;
                replacement = entry;
            }
        }
        let data = pack(record, ply);
        replacement.data.store(data, Ordering::Relaxed);
        replacement
            .key_xor_data
            .store(key ^ data, Ordering::Relaxed);
    }

    fn cluster(&self, key: u64) -> &Cluster {
        let index = ((key as u128 * self.clusters.len() as u128) >> 64) as usize;
        &self.clusters[index]
    }
}

fn pack(record: Record, ply: usize) -> u64 {
    let score = if record.score >= MATE - MAX_PLY as i32 {
        record.score + ply as i32
    } else if record.score <= -MATE + MAX_PLY as i32 {
        record.score - ply as i32
    } else {
        record.score
    };
    let flags = u64::from(record.depth.min(127))
        | ((record.bound as u64) << 7)
        | (u64::from(record.pv) << 9)
        | (u64::from(record.age & 63) << 10);
    u64::from(record.mv.raw())
        | (u64::from(score as i16 as u16) << 16)
        | (u64::from(record.eval.clamp(i16::MIN as i32, i16::MAX as i32) as i16 as u16) << 32)
        | (flags << 48)
}

fn unpack(data: u64, ply: usize) -> Record {
    let flags = data >> 48;
    let mut score = (data >> 16) as u16 as i16 as i32;
    if score >= MATE - MAX_PLY as i32 {
        score -= ply as i32;
    } else if score <= -MATE + MAX_PLY as i32 {
        score += ply as i32;
    }
    Record {
        mv: Move::from_raw(data as u16),
        score,
        eval: (data >> 32) as u16 as i16 as i32,
        depth: (flags & 127) as u8,
        bound: match (flags >> 7) & 3 {
            1 => Bound::Upper,
            2 => Bound::Lower,
            _ => Bound::Exact,
        },
        pv: flags & (1 << 9) != 0,
        age: ((flags >> 10) & 63) as u8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mate_scores_track_distance_from_root() {
        let record = Record {
            mv: Move::NULL,
            score: MATE - 9,
            eval: -42,
            depth: 12,
            bound: Bound::Exact,
            pv: true,
            age: 0,
        };
        let packed = pack(record, 4);
        assert_eq!(unpack(packed, 7).score, MATE - 12);
        assert_eq!(unpack(packed, 7).eval, -42);
        let loss = Record {
            score: -MATE + 9,
            ..record
        };
        assert_eq!(unpack(pack(loss, 4), 7).score, -MATE + 12);
    }

    #[test]
    fn probes_reject_other_keys_and_age_changes_hashfull() {
        let table = TranspositionTable::new(1).unwrap();
        let record = Record {
            mv: Move::NULL,
            score: 37,
            eval: -13,
            depth: 6,
            bound: Bound::Lower,
            pv: false,
            age: 0,
        };
        table.store(0x1234_5678, 2, record);
        assert_eq!(table.probe(0x1234_5678, 2).unwrap().score, 37);
        assert!(table.probe(0x1234_5679, 2).is_none());
        table.next_generation();
        assert_eq!(table.hashfull(), 0);
    }
}
