//! Move-ordering and evaluation-correction statistics laid out, initialised and updated
//! as in Stockfish 19 (GPL-3.0, https://github.com/official-stockfish/Stockfish).

use inphzugzwang_core::{Move, Piece};

use crate::tt::prefetch;

const MAIN_LIMIT: i32 = 7183;
const CAPTURE_LIMIT: i32 = 10_692;
const CONTINUATION_LIMIT: i32 = 30_000;
const PAWN_LIMIT: i32 = 8192;
pub(super) const CORRECTION_LIMIT: i32 = 1024;
const TT_MOVE_LIMIT: i32 = 8192;
/// Plies near the root with a history of their own.
pub(super) const LOW_PLY: usize = 5;
const MOVE_SLOTS: usize = 1 << 16;
const PAWN_SLOTS: usize = 8192;
const CORRECTION_SLOTS: usize = 1 << 16;
/// A piece and its destination: twelve pieces by 64 squares.
pub(super) const PIECE_SQUARES: usize = 12 * 64;
/// Continuation tables, one per earlier move by check state, capture, piece and square,
/// plus a sentinel read for plies without a move and never updated.
const CONTINUATION_TABLES: usize = 2 * 2 * PIECE_SQUARES + 1;
pub(super) const SENTINEL: usize = CONTINUATION_TABLES - 1;
/// The continuation-correction row read for plies without a move.
pub(super) const NO_PIECE_SQUARE: usize = PIECE_SQUARES;

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

/// The four structure corrections kept per slot and side.
#[derive(Clone, Copy)]
pub(super) enum Correction {
    Pawn = 0,
    Minor = 1,
    WhiteNonPawn = 2,
    BlackNonPawn = 3,
}

pub(super) struct Histories {
    main: Box<[i16]>,
    low_ply: Box<[i16]>,
    capture: Box<[i16]>,
    continuation: Box<[i16]>,
    pawn: Box<[i16]>,
    correction: Box<[[[i16; 4]; 2]]>,
    continuation_correction: Box<[i16]>,
    tt_move: i16,
}

impl Histories {
    pub(super) fn new() -> Self {
        Self {
            main: vec![-5; 2 * MOVE_SLOTS].into_boxed_slice(),
            low_ply: vec![102; LOW_PLY * MOVE_SLOTS].into_boxed_slice(),
            capture: vec![-742; PIECE_SQUARES * 8].into_boxed_slice(),
            continuation: vec![-586; CONTINUATION_TABLES * PIECE_SQUARES].into_boxed_slice(),
            pawn: vec![-1338; PAWN_SLOTS * PIECE_SQUARES].into_boxed_slice(),
            correction: vec![[[-5; 4]; 2]; CORRECTION_SLOTS].into_boxed_slice(),
            continuation_correction: vec![5; (PIECE_SQUARES + 1) * PIECE_SQUARES]
                .into_boxed_slice(),
            tt_move: 0,
        }
    }

    /// Tables of no size, standing in for statistics handed back to the table.
    pub(super) fn empty() -> Self {
        Self {
            main: Box::default(),
            low_ply: Box::default(),
            capture: Box::default(),
            continuation: Box::default(),
            pawn: Box::default(),
            correction: Box::default(),
            continuation_correction: Box::default(),
            tt_move: 0,
        }
    }

    /// At the start of each search, as in Stockfish: the butterfly history shrinks toward
    /// zero and the low-ply history starts over.
    pub(super) fn start_search(&mut self) {
        for entry in self.main.iter_mut() {
            *entry = (i32::from(*entry) * 729 / 1024) as i16;
        }
        self.low_ply.fill(102);
    }

    fn move_index(side: usize, mv: Move) -> usize {
        side * MOVE_SLOTS + usize::from(mv.raw())
    }

    pub(super) fn main(&self, side: usize, mv: Move) -> i32 {
        i32::from(self.main[Self::move_index(side, mv)])
    }

    pub(super) fn update_main(&mut self, side: usize, mv: Move, bonus: i32) {
        gravity(
            &mut self.main[Self::move_index(side, mv)],
            bonus,
            MAIN_LIMIT,
        );
    }

    pub(super) fn low_ply(&self, ply: usize, mv: Move) -> i32 {
        i32::from(self.low_ply[Self::move_index(ply, mv)])
    }

    pub(super) fn update_low_ply(&mut self, ply: usize, mv: Move, bonus: i32) {
        gravity(
            &mut self.low_ply[Self::move_index(ply, mv)],
            bonus,
            MAIN_LIMIT,
        );
    }

    /// `victim` is the captured kind's index plus one, or zero for none.
    pub(super) fn capture(&self, piece_square: usize, victim: usize) -> i32 {
        i32::from(self.capture[piece_square * 8 + victim])
    }

    pub(super) fn update_capture(&mut self, piece_square: usize, victim: usize, bonus: i32) {
        gravity(
            &mut self.capture[piece_square * 8 + victim],
            bonus,
            CAPTURE_LIMIT,
        );
    }

    pub(super) fn continuation(&self, table: usize, piece_square: usize) -> i32 {
        i32::from(self.continuation[table * PIECE_SQUARES + piece_square])
    }

    pub(super) fn update_continuation(&mut self, table: usize, piece_square: usize, bonus: i32) {
        gravity(
            &mut self.continuation[table * PIECE_SQUARES + piece_square],
            bonus,
            CONTINUATION_LIMIT,
        );
    }

    fn pawn_index(pawn_key: u64, piece_square: usize) -> usize {
        (pawn_key as usize & (PAWN_SLOTS - 1)) * PIECE_SQUARES + piece_square
    }

    pub(super) fn pawn(&self, pawn_key: u64, piece_square: usize) -> i32 {
        i32::from(self.pawn[Self::pawn_index(pawn_key, piece_square)])
    }

    pub(super) fn update_pawn(&mut self, pawn_key: u64, piece_square: usize, bonus: i32) {
        gravity(
            &mut self.pawn[Self::pawn_index(pawn_key, piece_square)],
            bonus,
            PAWN_LIMIT,
        );
    }

    pub(super) fn correction(&self, kind: Correction, key: u64, side: usize) -> i32 {
        i32::from(self.correction[key as usize & (CORRECTION_SLOTS - 1)][side][kind as usize])
    }

    pub(super) fn update_correction(
        &mut self,
        kind: Correction,
        key: u64,
        side: usize,
        bonus: i32,
    ) {
        gravity(
            &mut self.correction[key as usize & (CORRECTION_SLOTS - 1)][side][kind as usize],
            bonus,
            CORRECTION_LIMIT,
        );
    }

    /// Starts loading the corrections a position with these keys reads, and the
    /// continuation corrections of `piece_square` in `rows`.
    pub(super) fn prefetch_corrections(
        &self,
        keys: [u64; 4],
        rows: [usize; 2],
        piece_square: usize,
    ) {
        for key in keys {
            prefetch(&self.correction[key as usize & (CORRECTION_SLOTS - 1)]);
        }
        for row in rows {
            prefetch(&self.continuation_correction[row * PIECE_SQUARES + piece_square]);
        }
    }

    /// `row` is the piece and square of the earlier move, `NO_PIECE_SQUARE` for none.
    pub(super) fn continuation_correction(&self, row: usize, piece_square: usize) -> i32 {
        i32::from(self.continuation_correction[row * PIECE_SQUARES + piece_square])
    }

    pub(super) fn update_continuation_correction(
        &mut self,
        row: usize,
        piece_square: usize,
        bonus: i32,
    ) {
        gravity(
            &mut self.continuation_correction[row * PIECE_SQUARES + piece_square],
            bonus,
            CORRECTION_LIMIT,
        );
    }

    pub(super) fn tt_move(&self) -> i32 {
        i32::from(self.tt_move)
    }

    pub(super) fn update_tt_move(&mut self, bonus: i32) {
        gravity(&mut self.tt_move, bonus, TT_MOVE_LIMIT);
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
