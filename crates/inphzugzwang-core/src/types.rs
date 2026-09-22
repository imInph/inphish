use std::fmt;
use std::ops::{BitAnd, BitOr, BitXor, Not};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Bitboard(pub u64);

impl Bitboard {
    pub const EMPTY: Self = Self(0);

    pub const fn contains(self, square: Square) -> bool {
        self.0 & (1_u64 << square.0) != 0
    }

    pub fn pop(&mut self) -> Option<Square> {
        if self.0 == 0 {
            None
        } else {
            let square = Square(self.0.trailing_zeros() as u8);
            self.0 &= self.0 - 1;
            Some(square)
        }
    }

    pub const fn count(self) -> u32 {
        self.0.count_ones()
    }
}

impl Iterator for Bitboard {
    type Item = Square;

    fn next(&mut self) -> Option<Self::Item> {
        self.pop()
    }
}

impl BitAnd for Bitboard {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl BitOr for Bitboard {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitXor for Bitboard {
    type Output = Self;
    fn bitxor(self, rhs: Self) -> Self {
        Self(self.0 ^ rhs.0)
    }
}

impl Not for Bitboard {
    type Output = Self;
    fn not(self) -> Self {
        Self(!self.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct Square(pub(crate) u8);

impl Square {
    pub const fn new(file: u8, rank: u8) -> Self {
        assert!(
            file < 8 && rank < 8,
            "square coordinates must be on the board"
        );
        Self(rank * 8 + file)
    }

    pub const fn index(self) -> usize {
        self.0 as usize
    }

    pub const fn file(self) -> u8 {
        self.0 & 7
    }

    pub const fn rank(self) -> u8 {
        self.0 >> 3
    }

    pub const fn bit(self) -> Bitboard {
        Bitboard(1_u64 << self.0)
    }

    pub fn parse(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != 2
            || !(b'a'..=b'h').contains(&bytes[0])
            || !(b'1'..=b'8').contains(&bytes[1])
        {
            return None;
        }
        Some(Self::new(bytes[0] - b'a', bytes[1] - b'1'))
    }
}

impl fmt::Display for Square {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", char::from(b'a' + self.file()), self.rank() + 1)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Color {
    White,
    Black,
}

impl Color {
    pub const fn index(self) -> usize {
        self as usize
    }

    pub const fn other(self) -> Self {
        match self {
            Self::White => Self::Black,
            Self::Black => Self::White,
        }
    }

    pub const fn back_rank(self) -> u8 {
        match self {
            Self::White => 0,
            Self::Black => 7,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PieceType {
    Pawn,
    Knight,
    Bishop,
    Rook,
    Queen,
    King,
}

impl PieceType {
    pub const fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Piece {
    pub color: Color,
    pub kind: PieceType,
}

impl Piece {
    pub const fn fen(self) -> char {
        let letter = match self.kind {
            PieceType::Pawn => 'p',
            PieceType::Knight => 'n',
            PieceType::Bishop => 'b',
            PieceType::Rook => 'r',
            PieceType::Queen => 'q',
            PieceType::King => 'k',
        };
        if matches!(self.color, Color::White) {
            letter.to_ascii_uppercase()
        } else {
            letter
        }
    }

    pub fn from_fen(letter: char) -> Option<Self> {
        let kind = match letter.to_ascii_lowercase() {
            'p' => PieceType::Pawn,
            'n' => PieceType::Knight,
            'b' => PieceType::Bishop,
            'r' => PieceType::Rook,
            'q' => PieceType::Queen,
            'k' => PieceType::King,
            _ => return None,
        };
        Some(Self {
            color: if letter.is_ascii_uppercase() {
                Color::White
            } else {
                Color::Black
            },
            kind,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CastlingRights(pub(crate) [Option<Square>; 4]);

impl CastlingRights {
    pub const NONE: Self = Self([None; 4]);

    pub const fn get(self, color: Color, kingside: bool) -> Option<Square> {
        self.0[color.index() * 2 + if kingside { 0 } else { 1 }]
    }

    pub fn set(&mut self, color: Color, kingside: bool, rook: Option<Square>) {
        self.0[color.index() * 2 + if kingside { 0 } else { 1 }] = rook;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum MoveFlag {
    Quiet = 0,
    DoublePush = 1,
    KingCastle = 2,
    QueenCastle = 3,
    Capture = 4,
    EnPassant = 5,
    KnightPromotion = 8,
    BishopPromotion = 9,
    RookPromotion = 10,
    QueenPromotion = 11,
    KnightCapturePromotion = 12,
    BishopCapturePromotion = 13,
    RookCapturePromotion = 14,
    QueenCapturePromotion = 15,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Move(pub(crate) u16);

impl Move {
    pub const NULL: Self = Self(0);

    pub const fn new(from: Square, to: Square, flag: MoveFlag) -> Self {
        Self(from.0 as u16 | ((to.0 as u16) << 6) | ((flag as u16) << 12))
    }

    pub const fn from(self) -> Square {
        Square((self.0 & 63) as u8)
    }

    pub const fn to(self) -> Square {
        Square(((self.0 >> 6) & 63) as u8)
    }

    pub const fn flag(self) -> u8 {
        (self.0 >> 12) as u8
    }

    pub const fn is_castle(self) -> bool {
        self.flag() == 2 || self.flag() == 3
    }

    pub const fn promotion(self) -> Option<PieceType> {
        match self.flag() & 11 {
            8 => Some(PieceType::Knight),
            9 => Some(PieceType::Bishop),
            10 => Some(PieceType::Rook),
            11 => Some(PieceType::Queen),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ScoredMove {
    pub mv: Move,
    pub score: i32,
}

pub struct MoveList {
    moves: [ScoredMove; 256],
    len: usize,
}

impl Default for MoveList {
    fn default() -> Self {
        Self::new()
    }
}

impl MoveList {
    pub const fn new() -> Self {
        Self {
            moves: [ScoredMove {
                mv: Move::NULL,
                score: 0,
            }; 256],
            len: 0,
        }
    }

    pub fn push(&mut self, mv: Move) {
        assert!(self.len < self.moves.len(), "legal move list exceeds 256");
        self.moves[self.len] = ScoredMove { mv, score: 0 };
        self.len += 1;
    }

    pub fn iter(&self) -> impl Iterator<Item = Move> + '_ {
        self.moves[..self.len].iter().map(|entry| entry.mv)
    }

    pub fn score(&self, index: usize) -> Option<i32> {
        self.moves
            .get(index)
            .filter(|_| index < self.len)
            .map(|entry| entry.score)
    }

    pub fn set_score(&mut self, index: usize, score: i32) -> bool {
        if index >= self.len {
            return false;
        }
        self.moves[index].score = score;
        true
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}
