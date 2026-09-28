//! Staged move picker after Stockfish 19's `MovePicker` (GPL-3.0,
//! https://github.com/official-stockfish/Stockfish). Moves come out in stages so that a
//! node cut off early never generates or sorts the rest: the table move, captures that
//! do not lose too much, quiet moves with good histories, the losing captures, then the
//! remaining quiet moves. Check evasions, ProbCut captures and the captures of the
//! quiescence search have stages of their own. The generators produce legal moves only,
//! so no stage needs a legality check.

use inphzugzwang_core::{
    bishop_attacks, knight_attacks, pawn_attacks, rook_attacks, Bitboard, Move, MoveFlag, MoveList,
    PieceType, Position,
};

use crate::history::{piece_square, Histories, LOW_PLY};

/// Stockfish's piece values, the scale of capture scores and exchange thresholds.
pub(super) const PIECE_VALUES: [i32; 6] = [208, 781, 825, 1276, 2538, 0];

pub(super) fn is_capture(mv: Move) -> bool {
    mv.flag() & 4 != 0
}

/// A capture or a queen promotion: the moves generated with the captures.
pub(super) fn is_capture_stage(mv: Move) -> bool {
    is_capture(mv) || mv.promotion() == Some(PieceType::Queen)
}

/// The kind a capture removes.
pub(super) fn victim(position: &Position, mv: Move) -> Option<PieceType> {
    if mv.flag() == MoveFlag::EnPassant as u8 {
        Some(PieceType::Pawn)
    } else if is_capture(mv) {
        position.piece_at(mv.to()).map(|piece| piece.kind)
    } else {
        None
    }
}

/// The kind on a move's destination, which Stockfish's ordering and pruning take for the
/// captured piece before the move is made: none for en passant.
pub(super) fn target(position: &Position, mv: Move) -> Option<PieceType> {
    position.piece_at(mv.to()).map(|piece| piece.kind)
}

/// Capture-history slot of the captured kind: its index plus one, zero for none.
pub(super) fn victim_slot(victim: Option<PieceType>) -> usize {
    victim.map_or(0, |kind| kind.index() + 1)
}

pub(super) fn victim_value(victim: Option<PieceType>) -> i32 {
    victim.map_or(0, |kind| PIECE_VALUES[kind.index()])
}

/// The moving piece and its destination as indexed in the history tables.
pub(super) fn moved_square(position: &Position, mv: Move) -> usize {
    piece_square(
        position.piece_at(mv.from()).expect("a move has a mover"),
        mv.to().index(),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    MainTable,
    CaptureInit,
    GoodCapture,
    QuietInit,
    GoodQuiet,
    BadCapture,
    BadQuiet,
    EvasionTable,
    EvasionInit,
    Evasion,
    ProbCutTable,
    ProbCutInit,
    ProbCut,
    QuiescenceTable,
    QuiescenceInit,
    QuiescenceCapture,
    Done,
}

impl Stage {
    fn after_table(self) -> Self {
        match self {
            Self::MainTable => Self::CaptureInit,
            Self::EvasionTable => Self::EvasionInit,
            Self::ProbCutTable => Self::ProbCutInit,
            Self::QuiescenceTable => Self::QuiescenceInit,
            other => other,
        }
    }
}

/// Quiet moves scoring at or below this are left until after the losing captures.
const GOOD_QUIET: i32 = -14_000;

pub(super) struct Picker {
    stage: Stage,
    table_move: Move,
    depth: i32,
    ply: usize,
    threshold: i32,
    pawn_key: u64,
    /// Continuation tables of the moves one to six plies earlier.
    continuations: [usize; 6],
    skip_quiets: bool,
    captures: MoveList,
    current: usize,
    bad_end: usize,
    quiets: MoveList,
    quiet_current: usize,
}

impl Picker {
    fn new(stage: Stage, table_move: Move, depth: i32) -> Self {
        Self {
            stage: if table_move == Move::NULL {
                stage.after_table()
            } else {
                stage
            },
            table_move,
            depth,
            ply: 0,
            threshold: 0,
            pawn_key: 0,
            continuations: [0; 6],
            skip_quiets: false,
            captures: MoveList::new(),
            current: 0,
            bad_end: 0,
            quiets: MoveList::new(),
            quiet_current: 0,
        }
    }

    /// For the main search at `depth` above zero, or the quiescence search at or below
    /// zero. `table_move` must be legal or null.
    pub(super) fn main(
        position: &Position,
        table_move: Move,
        depth: i32,
        ply: usize,
        continuations: [usize; 6],
    ) -> Self {
        let stage = if position.checkers().0 != 0 {
            Stage::EvasionTable
        } else if depth > 0 {
            Stage::MainTable
        } else {
            Stage::QuiescenceTable
        };
        let mut picker = Self::new(stage, table_move, depth);
        picker.ply = ply;
        picker.pawn_key = position.pawn_key();
        picker.continuations = continuations;
        picker
    }

    /// For ProbCut: captures and queen promotions whose exchange gains `threshold`.
    pub(super) fn probcut(table_move: Move, threshold: i32) -> Self {
        let table_move = if is_capture_stage(table_move) {
            table_move
        } else {
            Move::NULL
        };
        let mut picker = Self::new(Stage::ProbCutTable, table_move, 0);
        picker.threshold = threshold;
        picker
    }

    pub(super) fn skip_quiet_moves(&mut self) {
        self.skip_quiets = true;
    }

    pub(super) fn next(&mut self, position: &Position, histories: &Histories) -> Option<Move> {
        loop {
            match self.stage {
                Stage::MainTable
                | Stage::EvasionTable
                | Stage::ProbCutTable
                | Stage::QuiescenceTable => {
                    self.stage = self.stage.after_table();
                    return Some(self.table_move);
                }
                Stage::CaptureInit | Stage::ProbCutInit | Stage::QuiescenceInit => {
                    position.generate_tactical_moves(&mut self.captures);
                    self.score_captures(position, histories);
                    partial_insertion_sort(&mut self.captures, i32::MIN);
                    self.current = 0;
                    self.bad_end = 0;
                    self.stage = match self.stage {
                        Stage::CaptureInit => Stage::GoodCapture,
                        Stage::ProbCutInit => Stage::ProbCut,
                        _ => Stage::QuiescenceCapture,
                    };
                }
                Stage::GoodCapture => {
                    while self.current < self.captures.len() {
                        let entry = self.captures.entries()[self.current];
                        self.current += 1;
                        if entry.mv == self.table_move {
                            continue;
                        }
                        if position.see_ge(entry.mv, -entry.score / 18) {
                            return Some(entry.mv);
                        }
                        // A losing capture waits at the front of the list for a later stage.
                        self.captures.entries_mut()[self.bad_end] = entry;
                        self.bad_end += 1;
                    }
                    self.stage = Stage::QuietInit;
                }
                Stage::QuietInit => {
                    if !self.skip_quiets {
                        position.generate_quiet_moves(&mut self.quiets);
                        self.score_quiets(position, histories);
                        partial_insertion_sort(&mut self.quiets, -3560 * self.depth);
                    }
                    self.quiet_current = 0;
                    self.stage = Stage::GoodQuiet;
                }
                Stage::GoodQuiet => {
                    if !self.skip_quiets {
                        while self.quiet_current < self.quiets.len() {
                            let entry = self.quiets.entries()[self.quiet_current];
                            self.quiet_current += 1;
                            if entry.mv != self.table_move && entry.score > GOOD_QUIET {
                                return Some(entry.mv);
                            }
                        }
                    }
                    self.current = 0;
                    self.stage = Stage::BadCapture;
                }
                Stage::BadCapture => {
                    if self.current < self.bad_end {
                        self.current += 1;
                        return Some(self.captures.get(self.current - 1));
                    }
                    self.quiet_current = 0;
                    self.stage = Stage::BadQuiet;
                }
                Stage::BadQuiet => {
                    if !self.skip_quiets {
                        while self.quiet_current < self.quiets.len() {
                            let entry = self.quiets.entries()[self.quiet_current];
                            self.quiet_current += 1;
                            if entry.mv != self.table_move && entry.score <= GOOD_QUIET {
                                return Some(entry.mv);
                            }
                        }
                    }
                    self.stage = Stage::Done;
                }
                Stage::EvasionInit => {
                    position.generate_moves(&mut self.captures);
                    self.score_evasions(position, histories);
                    partial_insertion_sort(&mut self.captures, i32::MIN);
                    self.current = 0;
                    self.stage = Stage::Evasion;
                }
                Stage::Evasion | Stage::QuiescenceCapture => {
                    while self.current < self.captures.len() {
                        let mv = self.captures.get(self.current);
                        self.current += 1;
                        if mv != self.table_move {
                            return Some(mv);
                        }
                    }
                    self.stage = Stage::Done;
                }
                Stage::ProbCut => {
                    while self.current < self.captures.len() {
                        let mv = self.captures.get(self.current);
                        self.current += 1;
                        if mv != self.table_move && position.see_ge(mv, self.threshold) {
                            return Some(mv);
                        }
                    }
                    self.stage = Stage::Done;
                }
                Stage::Done => return None,
            }
        }
    }

    /// Capture history plus seven times the victim's value.
    fn score_captures(&mut self, position: &Position, histories: &Histories) {
        for entry in self.captures.entries_mut() {
            let target = target(position, entry.mv);
            entry.score = histories.capture(moved_square(position, entry.mv), victim_slot(target))
                + 7 * victim_value(target);
        }
    }

    /// Histories, a bonus for a safe check, a term for leaving or entering squares that
    /// lesser enemy pieces attack, and the low-ply history near the root.
    fn score_quiets(&mut self, position: &Position, histories: &Histories) {
        let us = position.side_to_move();
        let them = us.other();
        let occupied = position.occupied();
        let attacks = |kind: PieceType, of: fn(inphzugzwang_core::Square, Bitboard) -> Bitboard| {
            position
                .pieces(them, kind)
                .fold(Bitboard::EMPTY, |all, square| all | of(square, occupied))
        };
        let by_pawn = position
            .pieces(them, PieceType::Pawn)
            .fold(Bitboard::EMPTY, |all, square| {
                all | pawn_attacks(them, square)
            });
        let by_minor = by_pawn
            | attacks(PieceType::Knight, |square, _| knight_attacks(square))
            | attacks(PieceType::Bishop, bishop_attacks);
        let by_rook = by_minor | attacks(PieceType::Rook, rook_attacks);
        let threat_by_lesser = [
            Bitboard::EMPTY,
            by_pawn,
            by_pawn,
            by_minor,
            by_rook,
            Bitboard::EMPTY,
        ];
        let king = position.king(them);
        let diagonal = bishop_attacks(king, occupied);
        let straight = rook_attacks(king, occupied);
        let checks = [
            pawn_attacks(them, king),
            knight_attacks(king),
            diagonal,
            straight,
            diagonal | straight,
            Bitboard::EMPTY,
        ];
        let side = us.index();
        let [one, two, three, four, _, six] = self.continuations;
        let (ply, pawn_key) = (self.ply, self.pawn_key);
        for entry in self.quiets.entries_mut() {
            let mv = entry.mv;
            let (from, to) = (mv.from(), mv.to());
            let kind = position.piece_at(from).expect("a move has a mover").kind;
            let square = moved_square(position, mv);
            let mut score = 2 * histories.main(side, mv)
                + 2 * histories.pawn(pawn_key, square)
                + histories.continuation(one, square)
                + histories.continuation(two, square)
                + histories.continuation(three, square)
                + histories.continuation(four, square)
                + histories.continuation(six, square);
            if checks[kind.index()].contains(to) && position.see_ge(mv, -75) {
                score += 16_384;
            }
            let lesser = threat_by_lesser[kind.index()];
            score += PIECE_VALUES[kind.index()]
                * 20
                * (i32::from(lesser.contains(from)) - i32::from(lesser.contains(to)));
            if ply < LOW_PLY {
                score += 8 * histories.low_ply(ply, mv) / (1 + ply as i32);
            }
            entry.score = score;
        }
    }

    /// Captures and queen promotions by victim ahead of all quiet moves, which follow
    /// their butterfly and last-move histories.
    fn score_evasions(&mut self, position: &Position, histories: &Histories) {
        let side = position.side_to_move().index();
        let previous = self.continuations[0];
        for entry in self.captures.entries_mut() {
            let mv = entry.mv;
            entry.score = if is_capture_stage(mv) {
                victim_value(target(position, mv)) + (1 << 28)
            } else {
                histories.main(side, mv)
                    + histories.continuation(previous, moved_square(position, mv))
            };
        }
    }
}

/// Sorts the moves scoring at least `limit` to the front, highest first, leaving the
/// order of the rest unspecified.
fn partial_insertion_sort(list: &mut MoveList, limit: i32) {
    let moves = list.entries_mut();
    let mut sorted_end = 0;
    for index in 1..moves.len() {
        if moves[index].score >= limit {
            let moving = moves[index];
            sorted_end += 1;
            moves[index] = moves[sorted_end];
            let mut slot = sorted_end;
            while slot > 0 && moves[slot - 1].score < moving.score {
                moves[slot] = moves[slot - 1];
                slot -= 1;
            }
            moves[slot] = moving;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use inphzugzwang_core::START_FEN;

    fn all(picker: &mut Picker, position: &Position, histories: &Histories) -> Vec<u16> {
        let mut moves = Vec::new();
        while let Some(mv) = picker.next(position, histories) {
            moves.push(mv.raw());
        }
        moves
    }

    #[test]
    fn every_legal_move_comes_out_once() {
        let histories = Histories::new();
        let mut position = Position::from_fen(START_FEN).unwrap();
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        for _ in 0..300 {
            let legal = position.legal_moves();
            if legal.is_empty() {
                position = Position::from_fen(START_FEN).unwrap();
                continue;
            }
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let table = if seed.is_multiple_of(3) {
                legal.get(seed as usize % legal.len())
            } else {
                Move::NULL
            };
            let mut expected: Vec<u16> = legal.iter().map(|mv| mv.raw()).collect();
            expected.sort_unstable();
            let mut picked = all(
                &mut Picker::main(&position, table, 5, 2, [0; 6]),
                &position,
                &histories,
            );
            picked.sort_unstable();
            assert_eq!(picked, expected, "{}", position.fen());
            if position.checkers().0 == 0 {
                let mut tactical: Vec<u16> = legal
                    .iter()
                    .filter(|&mv| is_capture_stage(mv) || mv == table)
                    .map(|mv| mv.raw())
                    .collect();
                tactical.sort_unstable();
                let mut picked = all(
                    &mut Picker::main(&position, table, 0, 2, [0; 6]),
                    &position,
                    &histories,
                );
                picked.sort_unstable();
                assert_eq!(picked, tactical, "{}", position.fen());
            }
            position.make(legal.get((seed >> 8) as usize % legal.len()));
        }
    }
}
