use std::fmt;

use crate::attacks::{between, line};
use crate::{bishop_attacks, king_attacks, knight_attacks, pawn_attacks, rook_attacks};
use crate::{Bitboard, CastlingRights, Color, Move, MoveFlag, MoveList, Piece, PieceType, Square};

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
    checkers: Bitboard,
    pinned: Bitboard,
    reversible_start: usize,
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
            checkers: Bitboard::EMPTY,
            pinned: Bitboard::EMPTY,
            reversible_start: 0,
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

    pub fn non_pawn_keys(&self) -> [u64; 2] {
        self.state.non_pawn_keys
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
        let side = self.state.side;
        let enemy = side.other();
        let own = self.state.colors[side.index()];
        let theirs = self.state.colors[enemy.index()];
        let occupancy = own | theirs;
        let king = self.king(side);
        for to in king_attacks(king) & !own {
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
            return moves;
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
                        self.push_pawn_move(&mut moves, from, to, false);
                    }
                    let start_rank = if side == Color::White { 1 } else { 6 };
                    if from.rank() == start_rank {
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
            for to in pawn_attacks(side, from) & theirs & check_mask & pinned_line {
                self.push_pawn_move(&mut moves, from, to, true);
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
                for to in attacks & !own & check_mask & allowed {
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
        if self.state.checkers.0 == 0 {
            self.add_castles(&mut moves);
        }
        moves
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
        debug_assert!(self.is_legal(mv));
        assert!(
            self.history.len() < MAX_HISTORY,
            "position history exhausted"
        );
        self.history.push(self.state);
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
        if mv.is_castle() {
            let kingside = mv.flag() == 2;
            let king_to = Square::new(if kingside { 6 } else { 2 }, side.back_rank());
            let rook_to = Square::new(if kingside { 5 } else { 3 }, side.back_rank());
            remove(&mut self.state, from);
            remove(&mut self.state, to);
            place(&mut self.state, king_to, piece);
            place(
                &mut self.state,
                rook_to,
                Piece {
                    color: side,
                    kind: PieceType::Rook,
                },
            );
        } else {
            remove(&mut self.state, from);
            let captured = if mv.flag() == MoveFlag::EnPassant as u8 {
                let captured_square = Square::new(to.file(), from.rank());
                remove(&mut self.state, captured_square)
            } else {
                remove(&mut self.state, to)
            };
            if captured.is_some() {
                self.state.halfmove = 0;
            }
            let placed = Piece {
                color: side,
                kind: mv.promotion().unwrap_or(piece.kind),
            };
            place(&mut self.state, to, placed);
            if mv.flag() == MoveFlag::DoublePush as u8 {
                self.state.ep = Some(Square::new(from.file(), (from.rank() + to.rank()) / 2));
            }
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
        debug_assert_eq!(
            self.keys_from_scratch(),
            (
                self.state.key,
                self.state.pawn_key,
                self.state.non_pawn_keys
            )
        );
    }

    pub fn unmake(&mut self) {
        self.state = self.history.pop().expect("unmake requires a prior move");
        debug_assert_eq!(
            self.keys_from_scratch(),
            (
                self.state.key,
                self.state.pawn_key,
                self.state.non_pawn_keys
            )
        );
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
        let (key, pawn_key, non_pawn_keys) = self.keys_from_scratch();
        self.state.key = key;
        self.state.pawn_key = pawn_key;
        self.state.non_pawn_keys = non_pawn_keys;
    }

    fn keys_from_scratch(&self) -> (u64, u64, [u64; 2]) {
        let mut key = 0;
        let mut pawn = 0;
        let mut non_pawn = [0; 2];
        for index in 0..64 {
            if let Some(piece) = self.state.mailbox[index] {
                let part = piece_hash(piece, Square(index as u8));
                key ^= part;
                if piece.kind == PieceType::Pawn {
                    pawn ^= part;
                } else if piece.kind != PieceType::King {
                    non_pawn[piece.color.index()] ^= part;
                }
            }
        }
        if self.state.side == Color::Black {
            key ^= hash_word(0x1000);
        }
        key ^= castling_hash(self.state.castling);
        if let Some(ep) = self.hashable_ep() {
            key ^= hash_word(0x2000 + ep.file() as u64);
        }
        (key, pawn, non_pawn)
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
        let mut toggle = |at: Square, moved: Piece| {
            let part = piece_hash(moved, at);
            key ^= part;
            if moved.kind == PieceType::Pawn {
                pawn ^= part;
            } else if moved.kind != PieceType::King {
                non_pawn[moved.color.index()] ^= part;
            }
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
    }
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
