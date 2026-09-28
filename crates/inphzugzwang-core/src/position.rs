use std::fmt;
use std::sync::OnceLock;

use crate::attacks::{between, line, ray_pass};
use crate::{bishop_attacks, king_attacks, knight_attacks, pawn_attacks, rook_attacks};
use crate::{
    Bitboard, CastlingRights, Color, Dirty, Move, MoveFlag, MoveList, Piece, PieceType, Square,
};

/// Which legal moves a generation pass produces.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Generate {
    All,
    Tactical,
    Quiet,
}

/// Which threats through a square `Position::record_threats` records: all, none, or
/// those on rays not holding every square of the mask.
#[derive(Clone, Copy)]
enum Rays {
    All,
    Unless(u64),
    None,
}

pub const START_FEN: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";
const MAX_HISTORY: usize = 16_384;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct State {
    pieces: [Bitboard; 6],
    colors: [Bitboard; 2],
    mailbox: [Option<Piece>; 64],
    side: Color,
    castling: CastlingRights,
    ep: Option<Square>,
    halfmove: u16,
    fullmove: u32,
    key: u64,
    pawn_key: u64,
    non_pawn_keys: [u64; 2],
    minor_key: u64,
    checkers: Bitboard,
    pinned: Bitboard,
    reversible_start: usize,
    plies_from_null: u16,
    /// Plies back to an earlier occurrence of this position within reach of the fifty-move
    /// counter, negative when that occurrence was itself a repetition, zero for none.
    repetition: i16,
    /// The piece the move into this position captured.
    captured: Option<Piece>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FenError(&'static str);

impl fmt::Display for FenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for FenError {}

pub struct Position {
    state: State,
    history: Vec<State>,
}

impl Clone for Position {
    fn clone(&self) -> Self {
        let mut history = Vec::with_capacity(self.history.len().saturating_add(256));
        history.extend_from_slice(&self.history);
        Self {
            state: self.state,
            history,
        }
    }
}

impl Position {
    pub fn startpos() -> Self {
        Self::from_fen(START_FEN).expect("built-in starting FEN is valid")
    }

    pub fn from_fen(fen: &str) -> Result<Self, FenError> {
        let fields: Vec<&str> = fen.split_whitespace().collect();
        if fields.len() != 6 {
            return Err(FenError("FEN needs six fields"));
        }
        let mut state = State {
            pieces: [Bitboard::EMPTY; 6],
            colors: [Bitboard::EMPTY; 2],
            mailbox: [None; 64],
            side: match fields[1] {
                "w" => Color::White,
                "b" => Color::Black,
                _ => return Err(FenError("invalid side to move")),
            },
            castling: CastlingRights::NONE,
            ep: None,
            halfmove: fields[4]
                .parse()
                .map_err(|_| FenError("invalid halfmove clock"))?,
            fullmove: fields[5]
                .parse()
                .map_err(|_| FenError("invalid fullmove number"))?,
            key: 0,
            pawn_key: 0,
            non_pawn_keys: [0; 2],
            minor_key: 0,
            checkers: Bitboard::EMPTY,
            pinned: Bitboard::EMPTY,
            reversible_start: 0,
            plies_from_null: 0,
            repetition: 0,
            captured: None,
        };
        if state.fullmove == 0 {
            return Err(FenError("fullmove number must be positive"));
        }
        let ranks: Vec<&str> = fields[0].split('/').collect();
        if ranks.len() != 8 {
            return Err(FenError("FEN board needs eight ranks"));
        }
        for (fen_rank, text) in ranks.iter().enumerate() {
            let rank = 7 - fen_rank as u8;
            let mut file = 0_u8;
            for symbol in text.chars() {
                if let Some(empty) = symbol.to_digit(10) {
                    if empty == 0 || empty > 8 || file as u32 + empty > 8 {
                        return Err(FenError("invalid empty-square count"));
                    }
                    file += empty as u8;
                } else {
                    let piece = Piece::from_fen(symbol).ok_or(FenError("invalid piece"))?;
                    if file >= 8 {
                        return Err(FenError("rank exceeds eight squares"));
                    }
                    if piece.kind == PieceType::Pawn && (rank == 0 || rank == 7) {
                        return Err(FenError("pawn on back rank"));
                    }
                    place(&mut state, Square::new(file, rank), piece);
                    file += 1;
                }
            }
            if file != 8 {
                return Err(FenError("rank has wrong square count"));
            }
        }
        for color in [Color::White, Color::Black] {
            if (state.pieces[PieceType::King.index()] & state.colors[color.index()]).count() != 1 {
                return Err(FenError("each side needs one king"));
            }
            if (state.pieces[PieceType::Pawn.index()] & state.colors[color.index()]).count() > 8
                || state.colors[color.index()].count() > 16
            {
                return Err(FenError("too many pieces for one side"));
            }
        }
        if fields[2] != "-" {
            if fields[2].len() > 4 {
                return Err(FenError("too many castling rights"));
            }
            for symbol in fields[2].chars() {
                let color = if symbol.is_ascii_uppercase() {
                    Color::White
                } else {
                    Color::Black
                };
                let king = king_square(&state, color);
                if king.rank() != color.back_rank() {
                    return Err(FenError("castling king is off its back rank"));
                }
                let rook_file = match symbol {
                    'K' | 'k' => (king.file() + 1..8).rev().find(|&file| {
                        state.mailbox[Square::new(file, color.back_rank()).0 as usize]
                            == Some(Piece {
                                color,
                                kind: PieceType::Rook,
                            })
                    }),
                    'Q' | 'q' => (0..king.file()).find(|&file| {
                        state.mailbox[Square::new(file, color.back_rank()).0 as usize]
                            == Some(Piece {
                                color,
                                kind: PieceType::Rook,
                            })
                    }),
                    'A'..='H' => Some(symbol as u8 - b'A'),
                    'a'..='h' => Some(symbol as u8 - b'a'),
                    _ => return Err(FenError("invalid castling right")),
                }
                .ok_or(FenError("castling rook is missing"))?;
                if rook_file == king.file() {
                    return Err(FenError("castling rook shares king file"));
                }
                let rook = Square::new(rook_file, color.back_rank());
                if state.mailbox[rook.0 as usize]
                    != Some(Piece {
                        color,
                        kind: PieceType::Rook,
                    })
                {
                    return Err(FenError("castling rook is missing"));
                }
                let kingside = rook_file > king.file();
                if state.castling.get(color, kingside).is_some() {
                    return Err(FenError("duplicate castling right"));
                }
                state.castling.set(color, kingside, Some(rook));
            }
        }
        if fields[3] != "-" {
            let ep = Square::parse(fields[3]).ok_or(FenError("invalid en passant square"))?;
            let expected_rank = if state.side == Color::White { 5 } else { 2 };
            if ep.rank() != expected_rank || state.mailbox[ep.0 as usize].is_some() {
                return Err(FenError("invalid en passant target"));
            }
            let pawn_rank = if state.side == Color::White { 4 } else { 3 };
            if state.mailbox[Square::new(ep.file(), pawn_rank).0 as usize]
                != Some(Piece {
                    color: state.side.other(),
                    kind: PieceType::Pawn,
                })
            {
                return Err(FenError("en passant pawn is missing"));
            }
            let origin_rank = if state.side == Color::White { 6 } else { 1 };
            if state.mailbox[Square::new(ep.file(), origin_rank).0 as usize].is_some() {
                return Err(FenError("en passant pawn origin is occupied"));
            }
            state.ep = Some(ep);
        }
        let mut position = Self {
            state,
            history: Vec::with_capacity(MAX_HISTORY),
        };
        position.refresh_checks();
        let previous = position.state.side.other();
        if position.is_attacked(
            position.king(previous),
            position.state.side,
            position.occupied(),
            None,
        ) {
            return Err(FenError("side not to move is in check"));
        }
        position.refresh_keys();
        Ok(position)
    }

    pub fn fen(&self) -> String {
        let mut fen = String::new();
        for rank in (0..8).rev() {
            if rank != 7 {
                fen.push('/');
            }
            let mut empty = 0;
            for file in 0..8 {
                match self.state.mailbox[Square::new(file, rank).0 as usize] {
                    Some(piece) => {
                        if empty != 0 {
                            fen.push(char::from(b'0' + empty));
                            empty = 0;
                        }
                        fen.push(piece.fen());
                    }
                    None => empty += 1,
                }
            }
            if empty != 0 {
                fen.push(char::from(b'0' + empty));
            }
        }
        fen.push_str(if self.state.side == Color::White {
            " w "
        } else {
            " b "
        });
        let mut any = false;
        for color in [Color::White, Color::Black] {
            for kingside in [true, false] {
                if let Some(rook) = self.state.castling.get(color, kingside) {
                    any = true;
                    let standard = self.king(color) == Square::new(4, color.back_rank())
                        && rook.file() == if kingside { 7 } else { 0 };
                    let symbol = if standard {
                        match (color, kingside) {
                            (Color::White, true) => 'K',
                            (Color::White, false) => 'Q',
                            (Color::Black, true) => 'k',
                            (Color::Black, false) => 'q',
                        }
                    } else if color == Color::White {
                        char::from(b'A' + rook.file())
                    } else {
                        char::from(b'a' + rook.file())
                    };
                    fen.push(symbol);
                }
            }
        }
        if !any {
            fen.push('-');
        }
        fen.push(' ');
        match self.state.ep {
            Some(ep) => fen.push_str(&ep.to_string()),
            None => fen.push('-'),
        }
        fen.push_str(&format!(" {} {}", self.state.halfmove, self.state.fullmove));
        fen
    }

    pub fn side_to_move(&self) -> Color {
        self.state.side
    }

    pub fn key(&self) -> u64 {
        self.state.key
    }

    pub fn pawn_key(&self) -> u64 {
        self.state.pawn_key
    }

    /// Keys of each side's pieces other than pawns, kings included, as in Stockfish.
    pub fn non_pawn_keys(&self) -> [u64; 2] {
        self.state.non_pawn_keys
    }

    /// Key of both sides' knights and bishops.
    pub fn minor_key(&self) -> u64 {
        self.state.minor_key
    }

    /// The piece the last move captured.
    pub fn captured_piece(&self) -> Option<Piece> {
        self.state.captured
    }

    pub fn plies_from_null(&self) -> u32 {
        u32::from(self.state.plies_from_null)
    }

    pub fn checkers(&self) -> Bitboard {
        self.state.checkers
    }

    pub fn pinned(&self) -> Bitboard {
        self.state.pinned
    }

    pub fn piece_at(&self, square: Square) -> Option<Piece> {
        self.state.mailbox[square.0 as usize]
    }

    pub fn occupied(&self) -> Bitboard {
        self.state.colors[0] | self.state.colors[1]
    }

    pub fn king(&self, color: Color) -> Square {
        king_square(&self.state, color)
    }

    pub fn halfmove_clock(&self) -> u16 {
        self.state.halfmove
    }

    pub fn castling_rights(&self) -> CastlingRights {
        self.state.castling
    }

    /// Plies played since the start of the game, from the FEN full-move number.
    pub fn game_ply(&self) -> u32 {
        2 * (self.state.fullmove - 1) + u32::from(self.state.side == Color::Black)
    }

    /// The square behind a pawn that has just moved two squares, whether or not any pawn
    /// can capture there.
    pub fn en_passant(&self) -> Option<Square> {
        self.state.ep
    }

    fn is_attacked(
        &self,
        target: Square,
        by: Color,
        occupancy: Bitboard,
        removed: Option<Square>,
    ) -> bool {
        let own = self.state.colors[by.index()];
        let excluded = removed.map_or(0, |square| square.bit().0);
        let pieces =
            |kind: PieceType| Bitboard(self.state.pieces[kind.index()].0 & own.0 & !excluded);
        (pawn_attacks(by.other(), target) & pieces(PieceType::Pawn)).0 != 0
            || (knight_attacks(target) & pieces(PieceType::Knight)).0 != 0
            || (king_attacks(target) & pieces(PieceType::King)).0 != 0
            || (bishop_attacks(target, occupancy)
                & (pieces(PieceType::Bishop) | pieces(PieceType::Queen)))
            .0 != 0
            || (rook_attacks(target, occupancy)
                & (pieces(PieceType::Rook) | pieces(PieceType::Queen)))
            .0 != 0
    }

    fn attackers(&self, target: Square, by: Color, occupancy: Bitboard) -> Bitboard {
        let own = self.state.colors[by.index()];
        let of = |kind: PieceType| self.state.pieces[kind.index()] & own;
        (pawn_attacks(by.other(), target) & of(PieceType::Pawn))
            | (knight_attacks(target) & of(PieceType::Knight))
            | (king_attacks(target) & of(PieceType::King))
            | (bishop_attacks(target, occupancy) & (of(PieceType::Bishop) | of(PieceType::Queen)))
            | (rook_attacks(target, occupancy) & (of(PieceType::Rook) | of(PieceType::Queen)))
    }

    fn refresh_checks(&mut self) {
        let king = self.king(self.state.side);
        let enemy = self.state.side.other();
        let occupancy = self.occupied();
        self.state.checkers = self.attackers(king, enemy, occupancy);
        self.state.pinned = Bitboard::EMPTY;
        let sliders = (self.state.pieces[PieceType::Bishop.index()]
            | self.state.pieces[PieceType::Rook.index()]
            | self.state.pieces[PieceType::Queen.index()])
            & self.state.colors[enemy.index()];
        for attacker in sliders {
            let Some(piece) = self.piece_at(attacker) else {
                continue;
            };
            let aligned = if piece.kind == PieceType::Bishop {
                king.file().abs_diff(attacker.file()) == king.rank().abs_diff(attacker.rank())
            } else if piece.kind == PieceType::Rook {
                king.file() == attacker.file() || king.rank() == attacker.rank()
            } else {
                line(king, attacker).0 != 0
            };
            if aligned {
                let between = between(king, attacker) & occupancy;
                if between.count() == 1
                    && (between & self.state.colors[self.state.side.index()]).0 != 0
                {
                    self.state.pinned = self.state.pinned | between;
                }
            }
        }
    }

    pub fn legal_moves(&self) -> MoveList {
        let mut moves = MoveList::new();
        self.generate_legal_moves(Generate::All, &mut moves);
        moves
    }

    pub fn tactical_moves(&self) -> MoveList {
        let mut moves = MoveList::new();
        self.generate_tactical_moves(&mut moves);
        moves
    }

    /// Fills `moves` with the legal moves, replacing its contents. Reusing one list avoids
    /// initialising and copying a fresh one at every node.
    pub fn generate_moves(&self, moves: &mut MoveList) {
        self.generate_legal_moves(Generate::All, moves);
    }

    /// Fills `moves` with the legal captures and queen promotions, or every evasion in check.
    pub fn generate_tactical_moves(&self, moves: &mut MoveList) {
        let mode = if self.state.checkers.0 == 0 {
            Generate::Tactical
        } else {
            Generate::All
        };
        self.generate_legal_moves(mode, moves);
    }

    /// Fills `moves` with the legal moves that `generate_tactical_moves` leaves out when
    /// not in check: non-captures other than queen promotions, castling included.
    pub fn generate_quiet_moves(&self, moves: &mut MoveList) {
        debug_assert_eq!(self.state.checkers.0, 0);
        self.generate_legal_moves(Generate::Quiet, moves);
    }

    fn generate_legal_moves(&self, mode: Generate, moves: &mut MoveList) {
        let tactical_only = mode == Generate::Tactical;
        let quiet_only = mode == Generate::Quiet;
        moves.clear();
        let side = self.state.side;
        let enemy = side.other();
        let own = self.state.colors[side.index()];
        let theirs = self.state.colors[enemy.index()];
        let occupancy = own | theirs;
        let king = self.king(side);
        for to in king_attacks(king) & !own {
            if (tactical_only && !theirs.contains(to)) || (quiet_only && theirs.contains(to)) {
                continue;
            }
            let after = Bitboard((occupancy.0 & !king.bit().0 & !to.bit().0) | to.bit().0);
            if !self.is_attacked(
                to,
                enemy,
                after,
                if theirs.contains(to) { Some(to) } else { None },
            ) {
                moves.push(Move::new(
                    king,
                    to,
                    if theirs.contains(to) {
                        MoveFlag::Capture
                    } else {
                        MoveFlag::Quiet
                    },
                ));
            }
        }
        if self.state.checkers.count() > 1 {
            return;
        }
        let check_mask = if let Some(checker) = self.state.checkers.into_iter().next() {
            checker.bit() | between(king, checker)
        } else {
            Bitboard(!0)
        };
        let pawns = self.state.pieces[PieceType::Pawn.index()] & own;
        for from in pawns {
            let pinned_line = if self.state.pinned.contains(from) {
                line(king, from)
            } else {
                Bitboard(!0)
            };
            let forward = if side == Color::White { 8_i16 } else { -8_i16 };
            let destination = from.0 as i16 + forward;
            if (0..64).contains(&destination) {
                let to = Square(destination as u8);
                if !occupancy.contains(to) {
                    if check_mask.contains(to) && pinned_line.contains(to) {
                        let promotes = to.rank() == 0 || to.rank() == 7;
                        if tactical_only {
                            if promotes {
                                moves.push(Move::new(from, to, MoveFlag::QueenPromotion));
                            }
                        } else if quiet_only && promotes {
                            for flag in [
                                MoveFlag::KnightPromotion,
                                MoveFlag::BishopPromotion,
                                MoveFlag::RookPromotion,
                            ] {
                                moves.push(Move::new(from, to, flag));
                            }
                        } else {
                            self.push_pawn_move(moves, from, to, false);
                        }
                    }
                    let start_rank = if side == Color::White { 1 } else { 6 };
                    if !tactical_only && from.rank() == start_rank {
                        let double = Square((destination + forward) as u8);
                        if !occupancy.contains(double)
                            && check_mask.contains(double)
                            && pinned_line.contains(double)
                        {
                            moves.push(Move::new(from, double, MoveFlag::DoublePush));
                        }
                    }
                }
            }
            if quiet_only {
                continue;
            }
            for to in pawn_attacks(side, from) & theirs & check_mask & pinned_line {
                self.push_pawn_move(moves, from, to, true);
            }
            if let Some(ep) = self.state.ep {
                if pawn_attacks(side, from).contains(ep) {
                    let captured = Square::new(ep.file(), from.rank());
                    let after =
                        Bitboard((occupancy.0 & !from.bit().0 & !captured.bit().0) | ep.bit().0);
                    if !self.is_attacked(king, enemy, after, Some(captured)) {
                        moves.push(Move::new(from, ep, MoveFlag::EnPassant));
                    }
                }
            }
        }
        for kind in [
            PieceType::Knight,
            PieceType::Bishop,
            PieceType::Rook,
            PieceType::Queen,
        ] {
            for from in self.state.pieces[kind.index()] & own {
                let attacks = match kind {
                    PieceType::Knight => knight_attacks(from),
                    PieceType::Bishop => bishop_attacks(from, occupancy),
                    PieceType::Rook => rook_attacks(from, occupancy),
                    PieceType::Queen => {
                        bishop_attacks(from, occupancy) | rook_attacks(from, occupancy)
                    }
                    _ => unreachable!(),
                };
                let allowed = if self.state.pinned.contains(from) {
                    line(king, from)
                } else {
                    Bitboard(!0)
                };
                let targets = match mode {
                    Generate::All => !own,
                    Generate::Tactical => theirs,
                    Generate::Quiet => !occupancy,
                };
                for to in attacks & targets & check_mask & allowed {
                    moves.push(Move::new(
                        from,
                        to,
                        if theirs.contains(to) {
                            MoveFlag::Capture
                        } else {
                            MoveFlag::Quiet
                        },
                    ));
                }
            }
        }
        if !tactical_only && self.state.checkers.0 == 0 {
            self.add_castles(moves);
        }
    }

    fn push_pawn_move(&self, moves: &mut MoveList, from: Square, to: Square, capture: bool) {
        if to.rank() == 0 || to.rank() == 7 {
            let flags = if capture {
                [
                    MoveFlag::KnightCapturePromotion,
                    MoveFlag::BishopCapturePromotion,
                    MoveFlag::RookCapturePromotion,
                    MoveFlag::QueenCapturePromotion,
                ]
            } else {
                [
                    MoveFlag::KnightPromotion,
                    MoveFlag::BishopPromotion,
                    MoveFlag::RookPromotion,
                    MoveFlag::QueenPromotion,
                ]
            };
            for flag in flags {
                moves.push(Move::new(from, to, flag));
            }
        } else {
            moves.push(Move::new(
                from,
                to,
                if capture {
                    MoveFlag::Capture
                } else {
                    MoveFlag::Quiet
                },
            ));
        }
    }

    fn add_castles(&self, moves: &mut MoveList) {
        let side = self.state.side;
        let enemy = side.other();
        let king_from = self.king(side);
        for kingside in [true, false] {
            let Some(rook_from) = self.state.castling.get(side, kingside) else {
                continue;
            };
            let king_to = Square::new(if kingside { 6 } else { 2 }, side.back_rank());
            let rook_to = Square::new(if kingside { 5 } else { 3 }, side.back_rank());
            let empty_board = self.occupied().0 & !king_from.bit().0 & !rook_from.bit().0;
            let king_path = between(king_from, king_to).0 | king_to.bit().0;
            let rook_path = between(rook_from, rook_to).0 | rook_to.bit().0;
            if empty_board & (king_path | rook_path) != 0 {
                continue;
            }
            let step = king_to.file().cmp(&king_from.file());
            let mut file = king_from.file() as i8;
            let delta = match step {
                std::cmp::Ordering::Greater => 1,
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
            };
            let mut safe = true;
            while file != king_to.file() as i8 {
                file += delta;
                let through = Square::new(file as u8, side.back_rank());
                let final_occupancy = if through == king_to {
                    Bitboard(empty_board | king_to.bit().0 | rook_to.bit().0)
                } else {
                    Bitboard(empty_board | through.bit().0)
                };
                if self.is_attacked(through, enemy, final_occupancy, None) {
                    safe = false;
                    break;
                }
            }
            if safe
                && (delta != 0
                    || !self.is_attacked(
                        king_to,
                        enemy,
                        Bitboard(empty_board | king_to.bit().0 | rook_to.bit().0),
                        None,
                    ))
            {
                moves.push(Move::new(
                    king_from,
                    rook_from,
                    if kingside {
                        MoveFlag::KingCastle
                    } else {
                        MoveFlag::QueenCastle
                    },
                ));
            }
        }
    }

    pub fn is_legal(&self, mv: Move) -> bool {
        self.legal_moves().iter().any(|candidate| candidate == mv)
    }

    /// Whether `mv`, typically remembered from another position, is legal here, without
    /// generating the moves except for the rare castling and en passant cases.
    pub fn is_legal_move(&self, mv: Move) -> bool {
        if mv == Move::NULL || !self.is_pseudo_legal(mv) {
            return false;
        }
        if mv.is_castle() || mv.flag() == MoveFlag::EnPassant as u8 {
            return self.is_legal(mv);
        }
        let side = self.state.side;
        let king = self.king(side);
        let (from, to) = (mv.from(), mv.to());
        if from == king {
            let after = Bitboard((self.occupied().0 & !from.bit().0) | to.bit().0);
            let captured = self.state.colors[side.other().index()]
                .contains(to)
                .then_some(to);
            return !self.is_attacked(to, side.other(), after, captured);
        }
        let checkers = self.state.checkers;
        if checkers.count() > 1 {
            return false;
        }
        if let Some(checker) = checkers.into_iter().next() {
            if !(checker.bit() | between(king, checker)).contains(to) {
                return false;
            }
        }
        !self.state.pinned.contains(from) || line(king, from).contains(to)
    }

    pub fn is_pseudo_legal(&self, mv: Move) -> bool {
        let from = mv.from();
        let to = mv.to();
        let Some(piece) = self.piece_at(from) else {
            return false;
        };
        if piece.color != self.state.side || from == to {
            return false;
        }
        let target = self.piece_at(to);
        let occupancy = self.occupied();
        let flag = mv.flag();
        if mv.is_castle() {
            let kingside = flag == MoveFlag::KingCastle as u8;
            let king_to = Square::new(if kingside { 6 } else { 2 }, piece.color.back_rank());
            let rook_to = Square::new(if kingside { 5 } else { 3 }, piece.color.back_rank());
            let empty_board = occupancy.0 & !from.bit().0 & !to.bit().0;
            let path = between(from, king_to).0
                | king_to.bit().0
                | between(to, rook_to).0
                | rook_to.bit().0;
            return piece.kind == PieceType::King
                && self.state.castling.get(piece.color, kingside) == Some(to)
                && target
                    == Some(Piece {
                        color: piece.color,
                        kind: PieceType::Rook,
                    })
                && empty_board & path == 0;
        }
        if target.is_some_and(|target| target.color == piece.color) || flag == 6 || flag == 7 {
            return false;
        }
        if piece.kind == PieceType::Pawn {
            let forward = if piece.color == Color::White {
                1_i8
            } else {
                -1_i8
            };
            let rank_delta = to.rank() as i8 - from.rank() as i8;
            let file_delta = to.file().abs_diff(from.file());
            let promotes = to.rank() == 0 || to.rank() == 7;
            let captures = flag == MoveFlag::Capture as u8 || flag >= 12;
            if flag == MoveFlag::EnPassant as u8 {
                return self.state.ep == Some(to)
                    && target.is_none()
                    && rank_delta == forward
                    && file_delta == 1
                    && self.piece_at(Square::new(to.file(), from.rank()))
                        == Some(Piece {
                            color: piece.color.other(),
                            kind: PieceType::Pawn,
                        });
            }
            if flag == MoveFlag::DoublePush as u8 {
                let start_rank = if piece.color == Color::White { 1 } else { 6 };
                let middle = Square::new(from.file(), (from.rank() + to.rank()) / 2);
                return from.rank() == start_rank
                    && rank_delta == 2 * forward
                    && file_delta == 0
                    && target.is_none()
                    && self.piece_at(middle).is_none();
            }
            if promotes != (flag >= 8) {
                return false;
            }
            if captures {
                rank_delta == forward
                    && file_delta == 1
                    && target.is_some_and(|target| target.color != piece.color)
            } else {
                rank_delta == forward && file_delta == 0 && target.is_none()
            }
        } else {
            if flag != MoveFlag::Quiet as u8 && flag != MoveFlag::Capture as u8 {
                return false;
            }
            if (flag == MoveFlag::Capture as u8) != target.is_some() {
                return false;
            }
            match piece.kind {
                PieceType::Knight => knight_attacks(from).contains(to),
                PieceType::Bishop => bishop_attacks(from, occupancy).contains(to),
                PieceType::Rook => rook_attacks(from, occupancy).contains(to),
                PieceType::Queen => {
                    (bishop_attacks(from, occupancy) | rook_attacks(from, occupancy)).contains(to)
                }
                PieceType::King => king_attacks(from).contains(to),
                PieceType::Pawn => unreachable!(),
            }
        }
    }

    pub fn gives_check(&self, mv: Move) -> bool {
        let side = self.state.side;
        let king = self.king(side.other());
        let own = self.state.colors[side.index()];
        let mut pieces = self.state.pieces.map(|board| board & own);
        let moved = self
            .piece_at(mv.from())
            .expect("check query needs an origin piece");
        let mut occupancy = self.occupied().0 & !mv.from().bit().0;
        pieces[moved.kind.index()] = pieces[moved.kind.index()] & !mv.from().bit();
        if mv.is_castle() {
            let king_to = Square::new(if mv.flag() == 2 { 6 } else { 2 }, side.back_rank());
            let rook_to = Square::new(if mv.flag() == 2 { 5 } else { 3 }, side.back_rank());
            occupancy &= !mv.to().bit().0;
            occupancy |= king_to.bit().0 | rook_to.bit().0;
            pieces[PieceType::King.index()] = pieces[PieceType::King.index()] | king_to.bit();
            pieces[PieceType::Rook.index()] =
                (pieces[PieceType::Rook.index()] & !mv.to().bit()) | rook_to.bit();
        } else {
            if mv.flag() == MoveFlag::EnPassant as u8 {
                occupancy &= !Square::new(mv.to().file(), mv.from().rank()).bit().0;
            }
            occupancy |= mv.to().bit().0;
            pieces[mv.promotion().unwrap_or(moved.kind).index()] =
                pieces[mv.promotion().unwrap_or(moved.kind).index()] | mv.to().bit();
        }
        let occupancy = Bitboard(occupancy);
        (pawn_attacks(side.other(), king) & pieces[PieceType::Pawn.index()]).0 != 0
            || (knight_attacks(king) & pieces[PieceType::Knight.index()]).0 != 0
            || (king_attacks(king) & pieces[PieceType::King.index()]).0 != 0
            || (bishop_attacks(king, occupancy)
                & (pieces[PieceType::Bishop.index()] | pieces[PieceType::Queen.index()]))
            .0 != 0
            || (rook_attacks(king, occupancy)
                & (pieces[PieceType::Rook.index()] | pieces[PieceType::Queen.index()]))
            .0 != 0
    }

    pub fn make(&mut self, mv: Move) {
        self.make_with(mv, None);
    }

    /// Makes `mv` and records in `dirty` what it changed, for the network's updates.
    pub fn make_recorded(&mut self, mv: Move, dirty: &mut Dirty) {
        self.make_with(mv, Some(dirty));
    }

    fn make_with(&mut self, mv: Move, mut dirty: Option<&mut Dirty>) {
        debug_assert!(self.is_legal(mv));
        assert!(
            self.history.len() < MAX_HISTORY,
            "position history exhausted"
        );
        self.history.push(self.state);
        self.state.captured = None;
        self.state.plies_from_null = self.state.plies_from_null.saturating_add(1);
        let side = self.state.side;
        let from = mv.from();
        let to = mv.to();
        let piece = self.piece_at(from).expect("legal move has an origin piece");
        let old_rights = self.state.castling;
        let old_ep = self.state.ep;
        self.state.ep = None;
        self.state.halfmove = self.state.halfmove.saturating_add(1);
        if piece.kind == PieceType::Pawn {
            self.state.halfmove = 0;
        }
        if let Some(dirty) = dirty.as_deref_mut() {
            dirty.begin(self.pawn_boards());
        }
        // The board changes in Stockfish's order, so that the threats recorded along the
        // way add up to exactly the difference between the positions.
        if mv.is_castle() {
            let kingside = mv.flag() == 2;
            let king_to = Square::new(if kingside { 6 } else { 2 }, side.back_rank());
            let rook_to = Square::new(if kingside { 5 } else { 3 }, side.back_rank());
            let rook = Piece {
                color: side,
                kind: PieceType::Rook,
            };
            self.lift(from, Rays::All, &mut dirty);
            self.lift(to, Rays::All, &mut dirty);
            self.put(king_to, piece, Rays::All, &mut dirty);
            self.put(rook_to, rook, Rays::All, &mut dirty);
            if let Some(dirty) = dirty.as_deref_mut() {
                dirty.remove(piece, from);
                dirty.remove(rook, to);
                dirty.add(piece, king_to);
                dirty.add(rook, rook_to);
                dirty.finish(Some((side, from, king_to)), true);
            }
        } else {
            let placed = Piece {
                color: side,
                kind: mv.promotion().unwrap_or(piece.kind),
            };
            let captured = if mv.flag() == MoveFlag::EnPassant as u8 {
                let square = Square::new(to.file(), from.rank());
                let victim = self.lift(square, Rays::All, &mut dirty);
                self.shift(from, to, &mut dirty);
                victim.map(|victim| (victim, square))
            } else if let Some(victim) = self.piece_at(to) {
                self.lift(from, Rays::All, &mut dirty);
                // The destination stays occupied, so no ray through it changes.
                self.lift(to, Rays::None, &mut dirty);
                self.put(to, placed, Rays::None, &mut dirty);
                Some((victim, to))
            } else if placed == piece {
                self.shift(from, to, &mut dirty);
                None
            } else {
                self.lift(from, Rays::All, &mut dirty);
                self.put(to, placed, Rays::All, &mut dirty);
                None
            };
            if captured.is_some() {
                self.state.halfmove = 0;
            }
            self.state.captured = captured.map(|(victim, _)| victim);
            if let Some(dirty) = dirty.as_deref_mut() {
                dirty.remove(piece, from);
                if let Some((victim, square)) = captured {
                    dirty.remove(victim, square);
                }
                dirty.add(placed, to);
                let king = (piece.kind == PieceType::King).then_some((side, from, to));
                dirty.finish(king, false);
            }
            if mv.flag() == MoveFlag::DoublePush as u8 {
                self.state.ep = Some(Square::new(from.file(), (from.rank() + to.rank()) / 2));
            }
        }
        if let Some(dirty) = dirty {
            dirty.set_pawns_after(self.pawn_boards());
        }
        if piece.kind == PieceType::King {
            self.state.castling.set(side, true, None);
            self.state.castling.set(side, false, None);
        }
        for color in [Color::White, Color::Black] {
            for kingside in [true, false] {
                if let Some(rook) = self.state.castling.get(color, kingside) {
                    if from == rook || to == rook {
                        self.state.castling.set(color, kingside, None);
                    }
                }
            }
        }
        if piece.kind == PieceType::Pawn
            || self.state.halfmove == 0
            || self.state.castling != old_rights
        {
            self.state.reversible_start = self.history.len();
        }
        self.state.side = side.other();
        if side == Color::Black {
            self.state.fullmove = self.state.fullmove.saturating_add(1);
        }
        self.refresh_checks();
        self.update_keys_from_move(mv, piece, old_rights, old_ep);
        self.state.repetition = self.find_repetition();
        debug_assert_eq!(self.keys_from_scratch(), self.current_keys());
    }

    fn pawn_boards(&self) -> [Bitboard; 2] {
        let pawns = self.state.pieces[PieceType::Pawn.index()];
        [pawns & self.state.colors[0], pawns & self.state.colors[1]]
    }

    /// Takes the piece off `square`, recording the threats that change.
    fn lift(
        &mut self,
        square: Square,
        rays: Rays,
        dirty: &mut Option<&mut Dirty>,
    ) -> Option<Piece> {
        let piece = self.piece_at(square)?;
        // Stockfish records a piece swapped on its square once it is gone, and any other
        // before it goes; for a swap the two agree, since the square stays occupied.
        if let Some(dirty) = dirty.as_deref_mut() {
            self.record_threats(piece, false, square, rays, dirty);
        }
        remove(&mut self.state, square)
    }

    /// Puts `piece` on the empty `square`, recording the threats that change.
    fn put(&mut self, square: Square, piece: Piece, rays: Rays, dirty: &mut Option<&mut Dirty>) {
        place(&mut self.state, square, piece);
        if let Some(dirty) = dirty.as_deref_mut() {
            self.record_threats(piece, true, square, rays, dirty);
        }
    }

    /// Moves the piece on `from` to the empty `to`: a threat along a ray through both
    /// squares is the same before and after, so neither side of the move records it.
    fn shift(&mut self, from: Square, to: Square, dirty: &mut Option<&mut Dirty>) {
        let both = Rays::Unless((from.bit() | to.bit()).0);
        let piece = self.lift(from, both, dirty).expect("a moved piece");
        self.put(to, piece, both, dirty);
    }

    /// Stockfish 19's `update_piece_threats`: with `piece` on `square` on the board, the
    /// threats it makes and receives, which appear when it is placed (`put`) and vanish
    /// when it is lifted, and with `rays` the threats of sliders through `square` onto
    /// the next piece, which do the opposite.
    fn record_threats(
        &self,
        piece: Piece,
        put: bool,
        square: Square,
        rays: Rays,
        dirty: &mut Dirty,
    ) {
        let pieces = &self.state.pieces;
        let occupied = self.occupied();
        let diagonal = bishop_attacks(square, occupied);
        let straight = rook_attacks(square, occupied);
        let from_square = diagonal | straight;
        let kings = pieces[PieceType::King.index()];
        let targets_all = occupied & !kings;
        let queens = pieces[PieceType::Queen.index()];
        let sliders = ((pieces[PieceType::Bishop.index()] | queens) & diagonal)
            | ((pieces[PieceType::Rook.index()] | queens) & straight);
        // A bishop or rook never targets a queen.
        let can_target = |target: Piece, slider: Piece| {
            target.kind != PieceType::Queen || slider.kind == PieceType::Queen
        };
        let mask = match rays {
            Rays::All => Some(!0),
            Rays::Unless(mask) => Some(mask),
            Rays::None => None,
        };
        let through_sliders = |add_direct: bool, dirty: &mut Dirty| {
            let Some(mask) = mask else {
                return;
            };
            for slider_square in sliders {
                let slider = self.piece_at(slider_square).expect("a slider");
                let ray = ray_pass(slider_square, square);
                let next = ray & from_square & targets_all;
                if next.0 != 0 && ray.0 & mask != mask {
                    let target_square = next.into_iter().next().expect("one square");
                    let target = self.piece_at(target_square).expect("a target");
                    if can_target(target, slider) {
                        dirty.threat(!put, slider, slider_square, target, target_square);
                    }
                }
                if add_direct && can_target(piece, slider) {
                    dirty.threat(put, slider, slider_square, piece, square);
                }
            }
        };
        if piece.kind == PieceType::King {
            through_sliders(false, dirty);
            return;
        }
        let pawns = pieces[PieceType::Pawn.index()];
        let knights = pieces[PieceType::Knight.index()];
        let rooks = pieces[PieceType::Rook.index()];
        let minors_rooks = pawns | knights | pieces[PieceType::Bishop.index()] | rooks;
        let (attacks, targets) = match piece.kind {
            PieceType::Pawn => (pawn_attacks(piece.color, square), knights | rooks),
            PieceType::Knight => (knight_attacks(square), targets_all),
            PieceType::Bishop => (diagonal, minors_rooks),
            PieceType::Rook => (straight, minors_rooks),
            _ => (from_square, targets_all),
        };
        for target_square in attacks & targets {
            let target = self.piece_at(target_square).expect("a target");
            dirty.threat(put, piece, square, target, target_square);
        }
        let mut incoming = knight_attacks(square) & knights;
        if matches!(piece.kind, PieceType::Knight | PieceType::Rook) {
            incoming = incoming
                | (pawn_attacks(Color::White, square) & pawns & self.state.colors[1])
                | (pawn_attacks(Color::Black, square) & pawns & self.state.colors[0]);
        }
        if mask.is_some() {
            through_sliders(true, dirty);
        } else if piece.kind == PieceType::Queen {
            incoming = incoming | (sliders & queens);
        } else {
            incoming = incoming | sliders;
        }
        for source in incoming {
            let attacker = self.piece_at(source).expect("an attacker");
            dirty.threat(put, attacker, source, piece, square);
        }
    }

    fn current_keys(&self) -> (u64, u64, [u64; 2], u64) {
        (
            self.state.key,
            self.state.pawn_key,
            self.state.non_pawn_keys,
            self.state.minor_key,
        )
    }

    /// Stockfish's repetition distance: the nearest earlier equal position at an even
    /// distance of four or more plies, within the fifty-move counter and since the last
    /// null move.
    fn find_repetition(&self) -> i16 {
        let end = usize::from(self.state.halfmove.min(self.state.plies_from_null));
        let count = self.history.len();
        for distance in (4..=end.min(count)).step_by(2) {
            let earlier = &self.history[count - distance];
            if earlier.key == self.state.key {
                let distance = distance as i16;
                return if earlier.repetition != 0 {
                    -distance
                } else {
                    distance
                };
            }
        }
        0
    }

    pub fn unmake(&mut self) {
        self.state = self.history.pop().expect("unmake requires a prior move");
        debug_assert_eq!(self.keys_from_scratch(), self.current_keys());
    }

    pub fn is_threefold(&self) -> bool {
        let mut matches = 1;
        for state in self.history[self.state.reversible_start..]
            .iter()
            .rev()
            .skip(1)
            .step_by(2)
        {
            if state.key == self.state.key {
                matches += 1;
                if matches == 3 {
                    return true;
                }
            }
        }
        false
    }

    pub fn is_fifty_move_draw(&self) -> bool {
        self.state.halfmove >= 100 && (!self.legal_moves().is_empty() || self.state.checkers.0 == 0)
    }

    pub fn is_insufficient_material(&self) -> bool {
        let occupied = self.occupied();
        let pawns = self.state.pieces[PieceType::Pawn.index()];
        let rooks = self.state.pieces[PieceType::Rook.index()];
        let queens = self.state.pieces[PieceType::Queen.index()];
        if (pawns | rooks | queens).0 != 0 {
            return false;
        }
        let knights = self.state.pieces[PieceType::Knight.index()];
        let bishops = self.state.pieces[PieceType::Bishop.index()];
        match occupied.count() {
            2 => true,
            3 => (knights | bishops).count() == 1,
            4 if bishops.count() == 2 && knights.0 == 0 => {
                let white = bishops & self.state.colors[Color::White.index()];
                let black = bishops & self.state.colors[Color::Black.index()];
                if white.count() != 1 || black.count() != 1 {
                    return false;
                }
                let a = white.into_iter().next().expect("single bishop");
                let b = black.into_iter().next().expect("single bishop");
                (a.file() + a.rank()) % 2 == (b.file() + b.rank()) % 2
            }
            _ => false,
        }
    }

    pub fn is_checkmate(&self) -> bool {
        self.state.checkers.0 != 0 && self.legal_moves().is_empty()
    }

    pub fn is_stalemate(&self) -> bool {
        self.state.checkers.0 == 0 && self.legal_moves().is_empty()
    }

    pub fn pieces(&self, color: Color, kind: PieceType) -> Bitboard {
        self.state.pieces[kind.index()] & self.state.colors[color.index()]
    }

    pub fn side_pieces(&self, color: Color) -> Bitboard {
        self.state.colors[color.index()]
    }

    pub fn has_non_pawn_material(&self, color: Color) -> bool {
        let kings_and_pawns =
            self.state.pieces[PieceType::King.index()] | self.state.pieces[PieceType::Pawn.index()];
        (self.state.colors[color.index()] & !kings_and_pawns).0 != 0
    }

    /// Any earlier occurrence since the last irreversible move. Search treats a single
    /// repetition as a draw because the side that could deviate would already have done so.
    pub fn is_repetition(&self) -> bool {
        self.history[self.state.reversible_start..]
            .iter()
            .rev()
            .skip(1)
            .step_by(2)
            .any(|state| state.key == self.state.key)
    }

    /// Stockfish's draw test for a node `ply` plies below the root: the fifty-move rule
    /// unless the last move mated, or a repetition, where one occurrence after the root
    /// suffices and one before it needs a third.
    pub fn is_draw(&self, ply: usize) -> bool {
        if self.state.halfmove > 99
            && (self.state.checkers.0 == 0 || !self.legal_moves().is_empty())
        {
            return true;
        }
        self.state.repetition != 0 && i32::from(self.state.repetition) < ply as i32
    }

    /// Whether a position has repeated since the last capture, pawn move or null move.
    pub fn has_repeated(&self) -> bool {
        let end = usize::from(self.state.halfmove.min(self.state.plies_from_null));
        if self.state.repetition != 0 {
            return true;
        }
        let count = self.history.len();
        (1..=end.saturating_sub(4).min(count))
            .any(|distance| self.history[count - distance].repetition != 0)
    }

    /// Whether the side to move has a move reaching a position that would then count as
    /// a draw by repetition, found with Marcel van Kervinck's cuckoo tables as in
    /// Stockfish: a reversible move of one piece is identified by its key difference.
    pub fn upcoming_repetition(&self, ply: usize) -> bool {
        let end = usize::from(self.state.halfmove.min(self.state.plies_from_null));
        let count = self.history.len();
        if end < 3 || count < end {
            return false;
        }
        let side = hash_word(0x1000);
        let original = self.state.key;
        let mut other = original ^ self.history[count - 1].key ^ side;
        let cuckoo = cuckoo();
        let occupied = self.occupied();
        for distance in (3..=end).step_by(2) {
            other ^=
                self.history[count - distance + 1].key ^ self.history[count - distance].key ^ side;
            if other != 0 {
                continue;
            }
            let earlier = &self.history[count - distance];
            let move_key = original ^ earlier.key;
            if let Some((from, to)) = cuckoo.find(move_key) {
                if (between(from, to) & occupied).0 == 0
                    && (ply > distance || earlier.repetition != 0)
                {
                    return true;
                }
            }
        }
        false
    }

    pub fn make_null(&mut self) {
        debug_assert_eq!(self.state.checkers.0, 0);
        assert!(
            self.history.len() < MAX_HISTORY,
            "position history exhausted"
        );
        let mut key = self.state.key ^ hash_word(0x1000);
        if let Some(ep) = self.hashable_ep() {
            key ^= hash_word(0x2000 + ep.file() as u64);
        }
        self.history.push(self.state);
        // As in Stockfish, a null move leaves the fifty-move counter alone.
        self.state.ep = None;
        self.state.side = self.state.side.other();
        // A null move breaks any repetition cycle; positions on either side of it must not match.
        self.state.reversible_start = self.history.len();
        self.state.key = key;
        self.state.plies_from_null = 0;
        self.state.repetition = 0;
        self.state.captured = None;
        self.refresh_checks();
        debug_assert_eq!(self.keys_from_scratch(), self.current_keys());
    }

    /// Static exchange evaluation: whether the capture sequence on the destination square
    /// nets at least `threshold` for the side to move, using the swap-list shortcut with
    /// x-ray attackers revealed as occupancy shrinks. Pins are ignored, as is usual.
    pub fn see_ge(&self, mv: Move, threshold: i32) -> bool {
        // Stockfish's piece values, the scale its pruning thresholds assume.
        const SEE_VALUES: [i32; 6] = [208, 781, 825, 1276, 2538, 20_000];
        if mv.is_castle() {
            return threshold <= 0;
        }
        let from = mv.from();
        let to = mv.to();
        let en_passant = mv.flag() == MoveFlag::EnPassant as u8;
        let victim = if en_passant {
            SEE_VALUES[PieceType::Pawn.index()]
        } else {
            self.piece_at(to)
                .map_or(0, |piece| SEE_VALUES[piece.kind.index()])
        };
        let promotion_gain = mv.promotion().map_or(0, |kind| {
            SEE_VALUES[kind.index()] - SEE_VALUES[PieceType::Pawn.index()]
        });
        let mut swap = victim + promotion_gain - threshold;
        if swap < 0 {
            return false;
        }
        let Some(mover) = self.piece_at(from) else {
            return false;
        };
        swap = SEE_VALUES[mv.promotion().unwrap_or(mover.kind).index()] - swap;
        if swap <= 0 {
            return true;
        }
        let mut occupancy = (self.occupied() ^ from.bit()) | to.bit();
        if en_passant {
            occupancy = occupancy & !Square::new(to.file(), from.rank()).bit();
        }
        let diagonal = self.state.pieces[PieceType::Bishop.index()]
            | self.state.pieces[PieceType::Queen.index()];
        let straight = self.state.pieces[PieceType::Rook.index()]
            | self.state.pieces[PieceType::Queen.index()];
        let mut attackers = (self.attackers(to, Color::White, occupancy)
            | self.attackers(to, Color::Black, occupancy))
            & occupancy;
        let mut side = mover.color;
        let mut result = true;
        loop {
            side = side.other();
            attackers = attackers & occupancy;
            let side_attackers = attackers & self.state.colors[side.index()];
            if side_attackers.0 == 0 {
                break;
            }
            result = !result;
            let kind = [
                PieceType::Pawn,
                PieceType::Knight,
                PieceType::Bishop,
                PieceType::Rook,
                PieceType::Queen,
                PieceType::King,
            ]
            .into_iter()
            .find(|kind| (side_attackers & self.state.pieces[kind.index()]).0 != 0)
            .expect("an attacker has a piece type");
            if kind == PieceType::King {
                let defended = (attackers & self.state.colors[side.other().index()]).0 != 0;
                return if defended { !result } else { result };
            }
            swap = SEE_VALUES[kind.index()] - swap;
            if swap < i32::from(result) {
                break;
            }
            let used = side_attackers & self.state.pieces[kind.index()];
            occupancy = occupancy ^ Bitboard(used.0 & used.0.wrapping_neg());
            if matches!(kind, PieceType::Pawn | PieceType::Bishop | PieceType::Queen) {
                attackers = attackers | (bishop_attacks(to, occupancy) & diagonal);
            }
            if matches!(kind, PieceType::Rook | PieceType::Queen) {
                attackers = attackers | (rook_attacks(to, occupancy) & straight);
            }
        }
        result
    }

    pub fn format_move(&self, mv: Move, chess960: bool) -> String {
        let destination = if mv.is_castle() && !chess960 {
            Square::new(
                if mv.flag() == 2 { 6 } else { 2 },
                self.state.side.back_rank(),
            )
        } else {
            mv.to()
        };
        let mut text = format!("{}{}", mv.from(), destination);
        if let Some(kind) = mv.promotion() {
            text.push(
                Piece {
                    color: Color::Black,
                    kind,
                }
                .fen(),
            );
        }
        text
    }

    pub fn parse_move(&self, text: &str, chess960: bool) -> Option<Move> {
        self.legal_moves()
            .iter()
            .find(|&mv| self.format_move(mv, chess960) == text)
    }

    fn refresh_keys(&mut self) {
        let (key, pawn_key, non_pawn_keys, minor_key) = self.keys_from_scratch();
        self.state.key = key;
        self.state.pawn_key = pawn_key;
        self.state.non_pawn_keys = non_pawn_keys;
        self.state.minor_key = minor_key;
    }

    fn keys_from_scratch(&self) -> (u64, u64, [u64; 2], u64) {
        let mut key = 0;
        let mut pawn = 0;
        let mut non_pawn = [0; 2];
        let mut minor = 0;
        for index in 0..64 {
            if let Some(piece) = self.state.mailbox[index] {
                let part = piece_hash(piece, Square(index as u8));
                key ^= part;
                add_to_keys(piece, part, &mut pawn, &mut non_pawn, &mut minor);
            }
        }
        if self.state.side == Color::Black {
            key ^= hash_word(0x1000);
        }
        key ^= castling_hash(self.state.castling);
        if let Some(ep) = self.hashable_ep() {
            key ^= hash_word(0x2000 + ep.file() as u64);
        }
        (key, pawn, non_pawn, minor)
    }

    fn hashable_ep(&self) -> Option<Square> {
        let ep = self.state.ep?;
        let side = self.state.side;
        let pawns = self.state.pieces[PieceType::Pawn.index()] & self.state.colors[side.index()];
        for from in pawn_attacks(side.other(), ep) & pawns {
            let captured = Square::new(ep.file(), from.rank());
            let after =
                Bitboard((self.occupied().0 & !from.bit().0 & !captured.bit().0) | ep.bit().0);
            if !self.is_attacked(self.king(side), side.other(), after, Some(captured)) {
                return Some(ep);
            }
        }
        None
    }

    fn update_keys_from_move(
        &mut self,
        mv: Move,
        piece: Piece,
        old_rights: CastlingRights,
        old_ep: Option<Square>,
    ) {
        let previous = self.history.last().expect("make saved previous state");
        let mut key = previous.key
            ^ hash_word(0x1000)
            ^ castling_hash(old_rights)
            ^ castling_hash(self.state.castling);
        if old_ep.is_some() {
            let prior = Self {
                state: *previous,
                history: Vec::new(),
            };
            if let Some(ep) = prior.hashable_ep() {
                key ^= hash_word(0x2000 + ep.file() as u64);
            }
        }
        if let Some(ep) = self.hashable_ep() {
            key ^= hash_word(0x2000 + ep.file() as u64);
        }
        let mut pawn = previous.pawn_key;
        let mut non_pawn = previous.non_pawn_keys;
        let mut minor = previous.minor_key;
        let mut toggle = |at: Square, moved: Piece| {
            let part = piece_hash(moved, at);
            key ^= part;
            add_to_keys(moved, part, &mut pawn, &mut non_pawn, &mut minor);
        };
        toggle(mv.from(), piece);
        if mv.is_castle() {
            let king_to = Square::new(if mv.flag() == 2 { 6 } else { 2 }, piece.color.back_rank());
            let rook_to = Square::new(if mv.flag() == 2 { 5 } else { 3 }, piece.color.back_rank());
            toggle(king_to, piece);
            let rook = Piece {
                color: piece.color,
                kind: PieceType::Rook,
            };
            toggle(mv.to(), rook);
            toggle(rook_to, rook);
        } else {
            let placed = Piece {
                color: piece.color,
                kind: mv.promotion().unwrap_or(piece.kind),
            };
            toggle(mv.to(), placed);
            let captured_square = if mv.flag() == MoveFlag::EnPassant as u8 {
                Square::new(mv.to().file(), mv.from().rank())
            } else {
                mv.to()
            };
            if let Some(captured) = previous.mailbox[captured_square.0 as usize] {
                toggle(captured_square, captured);
            }
        }
        self.state.key = key;
        self.state.pawn_key = pawn;
        self.state.non_pawn_keys = non_pawn;
        self.state.minor_key = minor;
    }
}

/// Adds or removes a piece's hash in the structure keys: pawns in the pawn key, every
/// other piece in its side's non-pawn key, knights and bishops also in the minor key.
fn add_to_keys(piece: Piece, part: u64, pawn: &mut u64, non_pawn: &mut [u64; 2], minor: &mut u64) {
    if piece.kind == PieceType::Pawn {
        *pawn ^= part;
    } else {
        non_pawn[piece.color.index()] ^= part;
        if matches!(piece.kind, PieceType::Knight | PieceType::Bishop) {
            *minor ^= part;
        }
    }
}

/// The key differences of every reversible move of one piece between two squares, with
/// the side to move toggled, in two hash tables with cuckoo displacement.
struct Cuckoo {
    keys: Box<[u64; 8192]>,
    moves: Box<[(Square, Square); 8192]>,
}

impl Cuckoo {
    fn first(key: u64) -> usize {
        (key & 0x1fff) as usize
    }

    fn second(key: u64) -> usize {
        ((key >> 16) & 0x1fff) as usize
    }

    fn find(&self, key: u64) -> Option<(Square, Square)> {
        [Self::first(key), Self::second(key)]
            .into_iter()
            .find(|&slot| self.keys[slot] == key)
            .map(|slot| self.moves[slot])
    }
}

fn cuckoo() -> &'static Cuckoo {
    static TABLES: OnceLock<Cuckoo> = OnceLock::new();
    TABLES.get_or_init(|| {
        let mut keys = Box::new([0_u64; 8192]);
        let mut moves = Box::new([(Square(0), Square(0)); 8192]);
        for color in [Color::White, Color::Black] {
            for kind in [
                PieceType::Knight,
                PieceType::Bishop,
                PieceType::Rook,
                PieceType::Queen,
                PieceType::King,
            ] {
                let piece = Piece { color, kind };
                for a in 0..64_u8 {
                    for b in a + 1..64 {
                        let (from, to) = (Square(a), Square(b));
                        let attacks = match kind {
                            PieceType::Knight => knight_attacks(from),
                            PieceType::Bishop => bishop_attacks(from, Bitboard::EMPTY),
                            PieceType::Rook => rook_attacks(from, Bitboard::EMPTY),
                            PieceType::Queen => {
                                bishop_attacks(from, Bitboard::EMPTY)
                                    | rook_attacks(from, Bitboard::EMPTY)
                            }
                            _ => king_attacks(from),
                        };
                        if !attacks.contains(to) {
                            continue;
                        }
                        let mut key =
                            piece_hash(piece, from) ^ piece_hash(piece, to) ^ hash_word(0x1000);
                        let mut entry = (from, to);
                        let mut slot = Cuckoo::first(key);
                        loop {
                            std::mem::swap(&mut keys[slot], &mut key);
                            std::mem::swap(&mut moves[slot], &mut entry);
                            if key == 0 {
                                break;
                            }
                            slot = if slot == Cuckoo::first(key) {
                                Cuckoo::second(key)
                            } else {
                                Cuckoo::first(key)
                            };
                        }
                    }
                }
            }
        }
        Cuckoo { keys, moves }
    })
}

fn place(state: &mut State, square: Square, piece: Piece) {
    state.mailbox[square.0 as usize] = Some(piece);
    state.pieces[piece.kind.index()] = state.pieces[piece.kind.index()] | square.bit();
    state.colors[piece.color.index()] = state.colors[piece.color.index()] | square.bit();
}

fn remove(state: &mut State, square: Square) -> Option<Piece> {
    let piece = state.mailbox[square.0 as usize].take()?;
    state.pieces[piece.kind.index()] = state.pieces[piece.kind.index()] ^ square.bit();
    state.colors[piece.color.index()] = state.colors[piece.color.index()] ^ square.bit();
    Some(piece)
}

fn king_square(state: &State, color: Color) -> Square {
    let kings = state.pieces[PieceType::King.index()] & state.colors[color.index()];
    Square(kings.0.trailing_zeros() as u8)
}

fn hash_word(value: u64) -> u64 {
    let mut value = value.wrapping_add(0x6a09_e667_f3bc_c909);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn piece_hash(piece: Piece, square: Square) -> u64 {
    hash_word((piece.color.index() * 6 * 64 + piece.kind.index() * 64 + square.0 as usize) as u64)
}

fn castling_hash(rights: CastlingRights) -> u64 {
    let mut hash = 0;
    for (index, rook) in rights.0.iter().enumerate() {
        if let Some(rook) = rook {
            hash ^= hash_word(0x3000 + (index * 8 + rook.file() as usize) as u64);
        }
    }
    hash
}

pub fn perft(position: &mut Position, depth: u8) -> u64 {
    if depth == 0 {
        return 1;
    }
    let moves = position.legal_moves();
    if depth == 1 {
        return moves.len() as u64;
    }
    let mut nodes = 0;
    for mv in moves.iter() {
        position.make(mv);
        nodes += perft(position, depth - 1);
        position.unmake();
    }
    nodes
}
