//! Move-ordering statistics, laid out and updated as in Stockfish 15.1 (GPL-3.0,
//! https://github.com/official-stockfish/Stockfish): butterfly history by side and
//! from-to squares, capture history by moving piece, destination and captured kind, and
//! continuation history of a move given the move played one, two, four or six plies
//! earlier.

use inphzugzwang_core::{Move, Piece};

const MAIN_LIMIT: i32 = 7183;
const CAPTURE_LIMIT: i32 = 10_692;
const CONTINUATION_LIMIT: i32 = 29_952;
/// A piece and its destination: twelve pieces by 64 squares.
pub(super) const PIECE_SQUARES: usize = 12 * 64;
/// Continuation tables, one per earlier move by check state, capture, piece and square,
/// plus a sentinel read for plies without a move and never updated.
const CONTINUATION_TABLES: usize = 2 * 2 * PIECE_SQUARES + 1;
pub(super) const SENTINEL: usize = CONTINUATION_TABLES - 1;

pub(super) fn piece_index(piece: Piece) -> usize {
    piece.color.index() * 6 + piece.kind.index()
}

/// A piece arriving on a square, as indexed in the continuation tables.
pub(super) fn piece_square(piece: Piece, to: usize) -> usize {
    piece_index(piece) * 64 + to
}

/// The continuation table for replies to a move of `piece_square`.
pub(super) fn continuation_table(in_check: bool, capture: bool, piece_square: usize) -> usize {
    (usize::from(in_check) * 2 + usize::from(capture)) * PIECE_SQUARES + piece_square
}

/// Moves `entry` toward `bonus`, less so the closer it already is to the limit, so that
/// entries stay within the limit and recent results weigh more than old ones.
fn gravity(entry: &mut i16, bonus: i32, limit: i32) {
    let bonus = bonus.clamp(-limit, limit);
    let value = i32::from(*entry);
    *entry = (value + bonus - value * bonus.abs() / limit) as i16;
}

pub(super) struct Histories {
    main: Box<[i16]>,
    capture: Box<[i16]>,
    continuation: Box<[i16]>,
    counters: Box<[Move]>,
}

impl Histories {
    pub(super) fn new() -> Self {
        let mut continuation = vec![-71; CONTINUATION_TABLES * PIECE_SQUARES];
        continuation[SENTINEL * PIECE_SQUARES..].fill(0);
        Self {
            main: vec![0; 2 * 64 * 64].into_boxed_slice(),
            capture: vec![0; PIECE_SQUARES * 7].into_boxed_slice(),
            continuation: continuation.into_boxed_slice(),
            counters: vec![Move::NULL; PIECE_SQUARES].into_boxed_slice(),
        }
    }

    fn main_index(side: usize, mv: Move) -> usize {
        side * 4096 + mv.from().index() * 64 + mv.to().index()
    }

    /// `victim` is the captured kind's index plus one, or zero for a queen promotion
    /// without capture.
    fn capture_index(piece_square: usize, victim: usize) -> usize {
        piece_square * 7 + victim
    }

    pub(super) fn main(&self, side: usize, mv: Move) -> i32 {
        i32::from(self.main[Self::main_index(side, mv)])
    }

    pub(super) fn update_main(&mut self, side: usize, mv: Move, bonus: i32) {
        gravity(
            &mut self.main[Self::main_index(side, mv)],
            bonus,
            MAIN_LIMIT,
        );
    }

    pub(super) fn capture(&self, piece_square: usize, victim: usize) -> i32 {
        i32::from(self.capture[Self::capture_index(piece_square, victim)])
    }

    pub(super) fn update_capture(&mut self, piece_square: usize, victim: usize, bonus: i32) {
        gravity(
            &mut self.capture[Self::capture_index(piece_square, victim)],
            bonus,
            CAPTURE_LIMIT,
        );
    }

    pub(super) fn continuation(&self, table: usize, piece_square: usize) -> i32 {
        i32::from(self.continuation[table * PIECE_SQUARES + piece_square])
    }

    pub(super) fn update_continuation(&mut self, table: usize, piece_square: usize, bonus: i32) {
        debug_assert_ne!(table, SENTINEL);
        gravity(
            &mut self.continuation[table * PIECE_SQUARES + piece_square],
            bonus,
            CONTINUATION_LIMIT,
        );
    }

    pub(super) fn counter(&self, previous: usize) -> Move {
        self.counters[previous]
    }

    pub(super) fn set_counter(&mut self, previous: usize, mv: Move) {
        self.counters[previous] = mv;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gravity_stays_within_the_limit() {
        let mut entry = 0;
        for _ in 0..1000 {
            gravity(&mut entry, 1594, MAIN_LIMIT);
            assert!(i32::from(entry) <= MAIN_LIMIT);
        }
        assert!(i32::from(entry) > MAIN_LIMIT - 10);
        for _ in 0..1000 {
            gravity(&mut entry, -40_000, CONTINUATION_LIMIT);
            assert!(i32::from(entry) >= -CONTINUATION_LIMIT);
        }
    }
}
