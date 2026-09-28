use std::mem::MaybeUninit;

use crate::{Bitboard, Color, Piece, PieceType, Square};

/// Threat changes one move can record before the record counts as overflowed.
const THREAT_CAPACITY: usize = 128;

/// What one move changed on the board, recorded while the move is made so that the
/// network's accumulators can follow it without comparing positions: the pieces removed
/// and placed, the king move if any, the pawns before and after, and every change of one
/// piece attacking another, as Stockfish 19's `DirtyPiece`, `DirtyThreats` and
/// `DirtyPawnPairs` hold them.
///
/// A threat is encoded as the attacker's square in bits 16 to 21, the target's square in
/// bits 8 to 13, the attacker's and the target's piece codes in bits 4 to 7 and 0 to 3,
/// and bit 31 set when the threat appears rather than disappears. Piece codes are
/// Stockfish's: 1 to 6 for White's pawn to king, 9 to 14 for Black's.
pub struct Dirty {
    removed: [(Piece, Square); 2],
    removed_len: usize,
    added: [(Piece, Square); 2],
    added_len: usize,
    king: Option<(Color, Square, Square)>,
    castle: bool,
    pawns: [[Bitboard; 2]; 2],
    threats: [MaybeUninit<u32>; THREAT_CAPACITY],
    threats_len: usize,
    overflowed: bool,
}

/// Bit set in a recorded threat that appears.
pub const THREAT_ADDED: u32 = 1 << 31;

impl Dirty {
    pub fn new() -> Self {
        let placeholder = (
            Piece {
                color: Color::White,
                kind: PieceType::Pawn,
            },
            Square(0),
        );
        Self {
            removed: [placeholder; 2],
            removed_len: 0,
            added: [placeholder; 2],
            added_len: 0,
            king: None,
            castle: false,
            pawns: [[Bitboard::EMPTY; 2]; 2],
            // SAFETY: an array of `MaybeUninit` needs no initialisation.
            threats: unsafe {
                MaybeUninit::<[MaybeUninit<u32>; THREAT_CAPACITY]>::uninit().assume_init()
            },
            threats_len: 0,
            overflowed: false,
        }
    }

    pub(crate) fn begin(&mut self, pawns: [Bitboard; 2]) {
        self.removed_len = 0;
        self.added_len = 0;
        self.king = None;
        self.castle = false;
        self.pawns = [pawns; 2];
        self.threats_len = 0;
        self.overflowed = false;
    }

    pub(crate) fn remove(&mut self, piece: Piece, square: Square) {
        self.removed[self.removed_len] = (piece, square);
        self.removed_len += 1;
    }

    pub(crate) fn add(&mut self, piece: Piece, square: Square) {
        self.added[self.added_len] = (piece, square);
        self.added_len += 1;
    }

    pub(crate) fn finish(&mut self, king: Option<(Color, Square, Square)>, castle: bool) {
        self.king = king;
        self.castle = castle;
    }

    pub(crate) fn set_pawns_after(&mut self, pawns: [Bitboard; 2]) {
        self.pawns[1] = pawns;
    }

    pub(crate) fn threat(
        &mut self,
        added: bool,
        attacker: Piece,
        from: Square,
        target: Piece,
        to: Square,
    ) {
        if self.threats_len == THREAT_CAPACITY {
            self.overflowed = true;
            return;
        }
        let code = |piece: Piece| 8 * piece.color.index() as u32 + piece.kind.index() as u32 + 1;
        self.threats[self.threats_len].write(
            (u32::from(added) << 31)
                | (u32::from(from.0) << 16)
                | (u32::from(to.0) << 8)
                | (code(attacker) << 4)
                | code(target),
        );
        self.threats_len += 1;
    }

    /// Pieces the move took off their squares, the mover included.
    pub fn removed(&self) -> &[(Piece, Square)] {
        &self.removed[..self.removed_len]
    }

    /// Pieces the move put on squares, the mover or its promotion included.
    pub fn added(&self) -> &[(Piece, Square)] {
        &self.added[..self.added_len]
    }

    /// For a king move, its colour, origin and destination.
    pub fn king(&self) -> Option<(Color, Square, Square)> {
        self.king
    }

    pub fn is_castle(&self) -> bool {
        self.castle
    }

    /// Each colour's pawns before the move and after it.
    pub fn pawns(&self) -> [[Bitboard; 2]; 2] {
        self.pawns
    }

    /// The threat changes in the order they were recorded; a threat may appear and
    /// disappear within one move.
    pub fn threats(&self) -> &[u32] {
        // SAFETY: the first `threats_len` items were written by `threat`.
        unsafe { std::slice::from_raw_parts(self.threats.as_ptr().cast(), self.threats_len) }
    }

    /// Whether more threat changes happened than the record holds.
    pub fn overflowed(&self) -> bool {
        self.overflowed
    }
}

impl Default for Dirty {
    fn default() -> Self {
        Self::new()
    }
}
