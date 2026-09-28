//! Staged move picker after Stockfish 15.1's `MovePicker` (GPL-3.0,
//! https://github.com/official-stockfish/Stockfish). Moves come out in stages so that a
//! node cut off early never generates or sorts the rest: the table move, captures that
//! do not lose material, killers and the counter move, quiet moves by their histories,
//! then the losing captures. Check evasions, ProbCut captures and quiescence moves have
//! stages of their own. The generators produce legal moves only, so no stage needs a
//! legality check except for moves remembered from elsewhere.

use inphzugzwang_core::{
    bishop_attacks, knight_attacks, pawn_attacks, rook_attacks, Bitboard, Move, MoveFlag, MoveList,
    PieceType, Position, Square,
};

use crate::history::{piece_square, Histories};

/// Stockfish's middlegame piece values, the scale of capture scores and exchange limits.
pub(super) const MIDGAME: [i32; 6] = [126, 781, 825, 1276, 2538, 0];

pub(super) fn is_capture(mv: Move) -> bool {
    mv.flag() & 4 != 0
}

/// Neither a capture nor a promotion.
pub(super) fn is_quiet(mv: Move) -> bool {
    !is_capture(mv) && mv.promotion().is_none()
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

/// Capture-history slot of the captured kind: its index plus one, zero for none.
pub(super) fn victim_slot(victim: Option<PieceType>) -> usize {
    victim.map_or(0, |kind| kind.index() + 1)
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
    Refutation,
    QuietInit,
    Quiet,
    BadCapture,
    EvasionTable,
    EvasionInit,
    Evasion,
    ProbCutTable,
    ProbCutInit,
    ProbCut,
    QuiescenceTable,
    QuiescenceInit,
    QuiescenceCapture,
    QuietCheckInit,
    QuietCheck,
    Done,
}

impl Stage {
    fn after(self) -> Self {
        match self {
            Self::MainTable => Self::CaptureInit,
            Self::EvasionTable => Self::EvasionInit,
            Self::ProbCutTable => Self::ProbCutInit,
            Self::QuiescenceTable => Self::QuiescenceInit,
            other => other,
        }
    }
}

pub(super) struct Picker {
    stage: Stage,
    table_move: Move,
    refutations: [Move; 3],
    refutation: usize,
    depth: i32,
    threshold: i32,
    recapture: Option<Square>,
    /// Continuation tables of the moves one, two, four and six plies earlier.
    continuations: [usize; 4],
    list: MoveList,
    current: usize,
    bad_end: usize,
    quiets: MoveList,
    quiet_current: usize,
    /// Own pieces attacked by an enemy piece of lesser value, set when quiets are scored.
    pub(super) threatened: Bitboard,
}

impl Picker {
    fn new(stage: Stage, table_move: Move, depth: i32, continuations: [usize; 4]) -> Self {
        Self {
            stage: if table_move == Move::NULL {
                stage.after()
            } else {
                stage
            },
            table_move,
            refutations: [Move::NULL; 3],
            refutation: 0,
            depth,
            threshold: 0,
            recapture: None,
            continuations,
            list: MoveList::new(),
            current: 0,
            bad_end: 0,
            quiets: MoveList::new(),
            quiet_current: 0,
            threatened: Bitboard::EMPTY,
        }
    }

    /// For the main search. `table_move` must be legal or null.
    pub(super) fn main(
        position: &Position,
        table_move: Move,
        depth: i32,
        killers: [Move; 2],
        counter: Move,
        continuations: [usize; 4],
    ) -> Self {
        let stage = if position.checkers().0 != 0 {
            Stage::EvasionTable
        } else {
            Stage::MainTable
        };
        let mut picker = Self::new(stage, table_move, depth, continuations);
        picker.refutations = [killers[0], killers[1], counter];
        picker
    }

    /// For ProbCut: captures whose exchange gains at least `threshold`.
    pub(super) fn probcut(position: &Position, table_move: Move, threshold: i32) -> Self {
        let table_move = if table_move != Move::NULL
            && is_capture(table_move)
            && position.see_ge(table_move, threshold)
        {
            table_move
        } else {
            Move::NULL
        };
        let mut picker = Self::new(Stage::ProbCutTable, table_move, 0, [0; 4]);
        picker.threshold = threshold;
        picker
    }

    /// For the quiescence search at `depth` (zero or below).
    pub(super) fn quiescence(
        position: &Position,
        table_move: Move,
        depth: i32,
        recapture: Option<Square>,
        continuations: [usize; 4],
    ) -> Self {
        let stage = if position.checkers().0 != 0 {
            Stage::EvasionTable
        } else {
            Stage::QuiescenceTable
        };
        let mut picker = Self::new(stage, table_move, depth, continuations);
        picker.recapture = recapture;
        picker
    }

    pub(super) fn next(
        &mut self,
        position: &Position,
        histories: &Histories,
        skip_quiets: bool,
    ) -> Option<Move> {
        loop {
            match self.stage {
                Stage::MainTable
                | Stage::EvasionTable
                | Stage::ProbCutTable
                | Stage::QuiescenceTable => {
                    self.stage = self.stage.after();
                    return Some(self.table_move);
                }
                Stage::CaptureInit | Stage::ProbCutInit | Stage::QuiescenceInit => {
                    position.generate_tactical_moves(&mut self.list);
                    self.score_captures(position, histories);
                    partial_insertion_sort(&mut self.list, i32::MIN);
                    self.current = 0;
                    self.bad_end = 0;
                    self.stage = match self.stage {
                        Stage::CaptureInit => Stage::GoodCapture,
                        Stage::ProbCutInit => Stage::ProbCut,
                        _ => Stage::QuiescenceCapture,
                    };
                }
                Stage::GoodCapture => {
                    while self.current < self.list.len() {
                        let entry = self.list.entries()[self.current];
                        self.current += 1;
                        if entry.mv == self.table_move {
                            continue;
                        }
                        if position.see_ge(entry.mv, -69 * entry.score / 1024) {
                            return Some(entry.mv);
                        }
                        // A losing capture waits at the front of the list for the last stage.
                        self.list.entries_mut()[self.bad_end] = entry;
                        self.bad_end += 1;
                    }
                    self.refutation = 0;
                    self.stage = Stage::Refutation;
                }
                Stage::Refutation => {
                    while self.refutation < self.refutations.len() {
                        let index = self.refutation;
                        self.refutation += 1;
                        let mv = self.refutations[index];
                        let repeated =
                            index == 2 && (mv == self.refutations[0] || mv == self.refutations[1]);
                        if mv != Move::NULL
                            && mv != self.table_move
                            && !repeated
                            && is_quiet(mv)
                            && position.is_legal_move(mv)
                        {
                            return Some(mv);
                        }
                        self.refutations[index] = Move::NULL;
                    }
                    self.stage = Stage::QuietInit;
                }
                Stage::QuietInit => {
                    if !skip_quiets {
                        position.generate_quiet_moves(&mut self.quiets);
                        self.score_quiets(position, histories);
                        partial_insertion_sort(&mut self.quiets, -3000 * self.depth);
                    }
                    self.quiet_current = 0;
                    self.stage = Stage::Quiet;
                }
                Stage::Quiet => {
                    if !skip_quiets {
                        while self.quiet_current < self.quiets.len() {
                            let mv = self.quiets.get(self.quiet_current);
                            self.quiet_current += 1;
                            if mv != self.table_move && !self.refutations.contains(&mv) {
                                return Some(mv);
                            }
                        }
                    }
                    self.current = 0;
                    self.stage = Stage::BadCapture;
                }
                Stage::BadCapture => {
                    if self.current < self.bad_end {
                        self.current += 1;
                        return Some(self.list.get(self.current - 1));
                    }
                    self.stage = Stage::Done;
                }
                Stage::EvasionInit => {
                    position.generate_moves(&mut self.list);
                    self.score_evasions(position, histories);
                    self.current = 0;
                    self.stage = Stage::Evasion;
                }
                Stage::Evasion => {
                    while self.current < self.list.len() {
                        let entries = self.list.entries_mut();
                        let best = (self.current..entries.len())
                            .max_by_key(|&index| (entries[index].score, std::cmp::Reverse(index)))
                            .expect("a move remains");
                        entries.swap(self.current, best);
                        let mv = entries[self.current].mv;
                        self.current += 1;
                        if mv != self.table_move {
                            return Some(mv);
                        }
                    }
                    self.stage = Stage::Done;
                }
                Stage::ProbCut => {
                    while self.current < self.list.len() {
                        let mv = self.list.get(self.current);
                        self.current += 1;
                        if mv != self.table_move && position.see_ge(mv, self.threshold) {
                            return Some(mv);
                        }
                    }
                    self.stage = Stage::Done;
                }
                Stage::QuiescenceCapture => {
                    while self.current < self.list.len() {
                        let mv = self.list.get(self.current);
                        self.current += 1;
                        if mv != self.table_move
                            && (self.depth > -5 || Some(mv.to()) == self.recapture)
                        {
                            return Some(mv);
                        }
                    }
                    self.stage = if self.depth == 0 {
                        Stage::QuietCheckInit
                    } else {
                        Stage::Done
                    };
                }
                Stage::QuietCheckInit => {
                    position.generate_quiet_moves(&mut self.quiets);
                    self.quiet_current = 0;
                    self.stage = Stage::QuietCheck;
                }
                Stage::QuietCheck => {
                    while self.quiet_current < self.quiets.len() {
                        let mv = self.quiets.get(self.quiet_current);
                        self.quiet_current += 1;
                        if mv != self.table_move && position.gives_check(mv) {
                            return Some(mv);
                        }
                    }
                    self.stage = Stage::Done;
                }
                Stage::Done => return None,
            }
        }
    }

    /// Most valuable victim first, adjusted by the capture history.
    fn score_captures(&mut self, position: &Position, histories: &Histories) {
        for entry in self.list.entries_mut() {
            let victim = victim(position, entry.mv);
            entry.score = 6 * victim.map_or(0, |kind| MIDGAME[kind.index()])
                + histories.capture(moved_square(position, entry.mv), victim_slot(victim));
        }
    }

    /// Histories, a bonus for moving a piece attacked by a lesser one to a square that
    /// lesser attackers do not reach, and a bonus for giving check.
    fn score_quiets(&mut self, position: &Position, histories: &Histories) {
        let us = position.side_to_move();
        let them = us.other();
        let occupied = position.occupied();
        let attacks = |kind: PieceType, of: fn(Square, Bitboard) -> Bitboard| {
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
        self.threatened = (position.pieces(us, PieceType::Queen) & by_rook)
            | (position.pieces(us, PieceType::Rook) & by_minor)
            | ((position.pieces(us, PieceType::Knight) | position.pieces(us, PieceType::Bishop))
                & by_pawn);
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
        let [one, two, four, six] = self.continuations;
        for entry in self.quiets.entries_mut() {
            let mv = entry.mv;
            let (from, to) = (mv.from(), mv.to());
            let kind = position.piece_at(from).expect("a move has a mover").kind;
            let square = moved_square(position, mv);
            let escape = if self.threatened.contains(from) {
                match kind {
                    PieceType::Queen if !by_rook.contains(to) => 50_000,
                    PieceType::Rook if !by_minor.contains(to) => 25_000,
                    _ if !by_pawn.contains(to) => 15_000,
                    _ => 0,
                }
            } else {
                0
            };
            entry.score = 2 * histories.main(side, mv)
                + 2 * histories.continuation(one, square)
                + histories.continuation(two, square)
                + histories.continuation(four, square)
                + histories.continuation(six, square)
                + escape
                + if checks[kind.index()].contains(to) {
                    16_384
                } else {
                    0
                };
        }
    }

    /// Captures by victim and then the least valuable attacker, ahead of all quiet moves,
    /// which follow their butterfly and last-move histories.
    fn score_evasions(&mut self, position: &Position, histories: &Histories) {
        let side = position.side_to_move().index();
        let previous = self.continuations[0];
        for entry in self.list.entries_mut() {
            let mv = entry.mv;
            entry.score = match victim(position, mv) {
                Some(victim) => {
                    let mover = position.piece_at(mv.from()).expect("a move has a mover");
                    MIDGAME[victim.index()] - mover.kind.index() as i32 - 1 + (1 << 28)
                }
                None => {
                    histories.main(side, mv)
                        + histories.continuation(previous, moved_square(position, mv))
                }
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
