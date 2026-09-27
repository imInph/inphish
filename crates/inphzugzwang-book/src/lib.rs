//! Polyglot opening books: the Polyglot key of a position, and the moves a book stores
//! for it. A book is a sequence of 16-byte big-endian entries sorted by key, each a key,
//! a move, a weight and four bytes of learning data that are ignored here.

mod random;

use std::borrow::Cow;
use std::sync::OnceLock;

use inphzugzwang_core::{pawn_attacks, Color, Move, PieceType, Position, Square};

use random::RANDOM;

const ENTRY: usize = 16;
const CASTLE: usize = 768;
const EN_PASSANT: usize = 772;
const TURN: usize = 780;

/// The Polyglot key of a standard chess position.
pub fn key(position: &Position) -> u64 {
    const KINDS: [PieceType; 6] = [
        PieceType::Pawn,
        PieceType::Knight,
        PieceType::Bishop,
        PieceType::Rook,
        PieceType::Queen,
        PieceType::King,
    ];
    let mut key = 0;
    for color in [Color::White, Color::Black] {
        for kind in KINDS {
            // Black's pawn is kind 0 and White's 1, and so on up to the kings.
            let kind_index = 2 * kind.index() + usize::from(color == Color::White);
            for square in position.pieces(color, kind) {
                key ^= RANDOM[64 * kind_index + square.index()];
            }
        }
    }
    let rights = position.castling_rights();
    let flags = [
        rights.get(Color::White, true),
        rights.get(Color::White, false),
        rights.get(Color::Black, true),
        rights.get(Color::Black, false),
    ];
    for (index, right) in flags.into_iter().enumerate() {
        if right.is_some() {
            key ^= RANDOM[CASTLE + index];
        }
    }
    // The file counts when a pawn of the side to move stands next to the pawn that just
    // moved two squares, even if capturing it would be illegal, as Polyglot defines it.
    let side = position.side_to_move();
    if let Some(target) = position.en_passant() {
        let pawns = position.pieces(side, PieceType::Pawn);
        if (pawn_attacks(side.other(), target) & pawns).count() > 0 {
            key ^= RANDOM[EN_PASSANT + usize::from(target.file())];
        }
    }
    if side == Color::White {
        key ^= RANDOM[TURN];
    }
    key
}

/// inphish's own book, bundled in the binary.
const BUILT_IN: &[u8] = include_bytes!("../book/inphish.bin");

/// A Polyglot book held in memory.
pub struct Book {
    bytes: Cow<'static, [u8]>,
}

/// The bundled book, checked on first use.
pub fn built_in() -> &'static Book {
    static BOOK: OnceLock<Book> = OnceLock::new();
    BOOK.get_or_init(|| Book::new(BUILT_IN).expect("bundled book is valid"))
}

impl Book {
    /// Takes the contents of a book file, which must be whole entries sorted by key.
    pub fn new(bytes: impl Into<Cow<'static, [u8]>>) -> Result<Self, &'static str> {
        let bytes = bytes.into();
        if !bytes.len().is_multiple_of(ENTRY) {
            return Err("book size is not a whole number of entries");
        }
        let book = Self { bytes };
        let sorted = (1..book.len()).all(|index| book.key_at(index - 1) <= book.key_at(index));
        if !sorted {
            return Err("book entries are not sorted by key");
        }
        Ok(book)
    }

    pub fn len(&self) -> usize {
        self.bytes.len() / ENTRY
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    fn entry(&self, index: usize) -> &[u8; ENTRY] {
        self.bytes[index * ENTRY..(index + 1) * ENTRY]
            .try_into()
            .expect("entry width")
    }

    fn key_at(&self, index: usize) -> u64 {
        u64::from_be_bytes(self.entry(index)[..8].try_into().expect("key width"))
    }

    /// The legal moves the book gives for `position`, with their weights, in book order.
    /// Entries whose move is not legal here, as after a key collision, are skipped.
    pub fn moves(&self, position: &Position) -> Vec<(Move, u16)> {
        let key = key(position);
        let first = partition(self.len(), |index| self.key_at(index) < key);
        let legal = position.legal_moves();
        (first..self.len())
            .take_while(|&index| self.key_at(index) == key)
            .filter_map(|index| {
                let entry = self.entry(index);
                let raw = u16::from_be_bytes([entry[8], entry[9]]);
                let weight = u16::from_be_bytes([entry[10], entry[11]]);
                let mv = legal.iter().find(|&mv| matches(mv, raw))?;
                Some((mv, weight))
            })
            .collect()
    }

    /// A book move for `position`: the heaviest with `best`, otherwise one drawn with
    /// probability proportional to its weight using `random`. Moves of weight zero are
    /// never chosen.
    pub fn choose(&self, position: &Position, best: bool, random: u64) -> Option<Move> {
        let moves: Vec<(Move, u16)> = self
            .moves(position)
            .into_iter()
            .filter(|&(_, weight)| weight > 0)
            .collect();
        if best {
            return moves
                .iter()
                .max_by_key(|&&(_, weight)| weight)
                .map(|&(mv, _)| mv);
        }
        let total: u64 = moves.iter().map(|&(_, weight)| u64::from(weight)).sum();
        if total == 0 {
            return None;
        }
        let mut pick = random % total;
        for (mv, weight) in moves {
            if pick < u64::from(weight) {
                return Some(mv);
            }
            pick -= u64::from(weight);
        }
        None
    }
}

/// The first index in `0..len` for which `before` is false, given that it is true for a
/// prefix and false afterwards.
fn partition(len: usize, before: impl Fn(usize) -> bool) -> usize {
    let (mut low, mut high) = (0, len);
    while low < high {
        let middle = low + (high - low) / 2;
        if before(middle) {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    low
}

/// Whether a Polyglot move, which gives castling as the king taking its own rook, is
/// `mv`. The move stores destination and origin squares in six bits each and the
/// promotion piece, knight to queen, in the next three.
fn matches(mv: Move, raw: u16) -> bool {
    let to = Square::new((raw & 7) as u8, ((raw >> 3) & 7) as u8);
    let from = Square::new(((raw >> 6) & 7) as u8, ((raw >> 9) & 7) as u8);
    let promotion = match (raw >> 12) & 7 {
        0 => None,
        1 => Some(PieceType::Knight),
        2 => Some(PieceType::Bishop),
        3 => Some(PieceType::Rook),
        4 => Some(PieceType::Queen),
        _ => return false,
    };
    mv.from() == from && mv.to() == to && mv.promotion() == promotion
}
