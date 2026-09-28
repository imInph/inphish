//! Inference for Stockfish 19's network `nn-1a298aa575a0`, bundled in the binary. Three
//! feature sets feed one accumulator per perspective: HalfKAv2_hm piece squares, threats
//! of one piece on another, and pairs of pawns on the same or neighbouring files. The
//! arithmetic follows Stockfish 19 exactly, so for any position the value equals what
//! Stockfish 19 computes with the same network.

use std::mem::MaybeUninit;
use std::sync::OnceLock;

use inphzugzwang_core::{
    between, bishop_attacks, king_attacks, knight_attacks, pawn_attacks, rook_attacks, Bitboard,
    Color, Move, MoveFlag, Piece, PieceType, Position, Square,
};

/// Width of one perspective's accumulator.
pub const HALF: usize = 1024;
/// Piece-square buckets of the accumulator and layer stacks, chosen by piece count.
const BUCKETS: usize = 8;
/// Features per king bucket: ten piece kinds and the kings, by 64 squares.
const PIECE_SQUARES: usize = 11 * 64;
const PSQ_INPUTS: usize = 32 * PIECE_SQUARES;
const THREAT_INPUTS: usize = 59_808;
/// Unordered pairs of 96 pawn identities: two colours by the 48 squares of ranks 2 to 7.
const PAIR_INPUTS: usize = 96 * 95 / 2;
/// Threat and pawn-pair features share one weight table, pairs after threats.
const EXTRA_INPUTS: usize = THREAT_INPUTS + PAIR_INPUTS;
const FC0_OUTPUTS: usize = 32;
const FC1_INPUTS: usize = 2 * FC0_OUTPUTS;
const FC1_OUTPUTS: usize = 32;
const FC2_INPUTS: usize = FC1_INPUTS + 2 * FC1_OUTPUTS;
const VERSION: u32 = 0x6A44_8AFA;
const WEIGHT_SCALE_BITS: u32 = 6;
const OUTPUT_SCALE: i32 = 16;
/// Upper bound of simultaneously active threat or pawn-pair features, as in Stockfish.
const MAX_EXTRA: usize = 256;
/// Capacity for the threats or pawn pairs around the squares one move changes; a move
/// touching more refreshes the accumulator instead.
const TOUCHING: usize = 48;
const PIECE_KINDS: [PieceType; 6] = [
    PieceType::Pawn,
    PieceType::Knight,
    PieceType::Bishop,
    PieceType::Rook,
    PieceType::Queen,
    PieceType::King,
];

const NET: &[u8] = include_bytes!("../net/nn-1a298aa575a0.nnue");

/// One of the eight layer stacks after the feature transformer.
struct Stack {
    fc0_bias: [i32; FC0_OUTPUTS],
    /// Stored by groups of four inputs: for each group, each output's four weights.
    fc0_weights: Box<[i8]>,
    fc1_bias: [i32; FC1_OUTPUTS],
    fc1_weights: Box<[i8]>,
    fc2_bias: i32,
    fc2_weights: [i8; FC2_INPUTS],
}

pub struct Network {
    feature_bias: Box<[i16]>,
    feature_weights: Box<[i16]>,
    psqt_weights: Box<[i32]>,
    extra_weights: Box<[i8]>,
    extra_psqt_weights: Box<[i32]>,
    stacks: Box<[Stack]>,
}

/// Feature-transformer sums for both perspectives, indexed by colour.
#[derive(Clone)]
pub struct Accumulator {
    pub values: [[i16; HALF]; 2],
    pub psqt: [[i32; BUCKETS]; 2],
}

impl Default for Accumulator {
    fn default() -> Self {
        Self {
            values: [[0; HALF]; 2],
            psqt: [[0; BUCKETS]; 2],
        }
    }
}

/// The two parts of the network's output in Stockfish's internal units, for the side to
/// move.
pub struct Output {
    pub psqt: i32,
    pub positional: i32,
}

impl Output {
    /// Stockfish 19's `Eval::evaluate` with no optimism: the output shrunk where its two
    /// parts disagree, scaled up with the material left and damped by the fifty-move
    /// counter, kept below the tablebase range.
    pub fn evaluation(&self, position: &Position) -> i32 {
        let nnue = self.psqt + self.positional;
        let complexity = (self.psqt - self.positional).abs();
        let nnue = nnue - (i64::from(nnue) * i64::from(complexity) / 18_236) as i32;
        let pawns = (position.pieces(Color::White, PieceType::Pawn)
            | position.pieces(Color::Black, PieceType::Pawn))
        .count() as i32;
        let material = 534 * pawns + non_pawn_material(position);
        let value = nnue + (i64::from(nnue) * i64::from(material) / 91_000) as i32;
        let value = value - value * i32::from(position.halfmove_clock()) / 199;
        value.clamp(-31_506, 31_506)
    }
}

/// Knights, bishops, rooks and queens of both sides at Stockfish's middlegame values.
pub fn non_pawn_material(position: &Position) -> i32 {
    const VALUES: [(PieceType, i32); 4] = [
        (PieceType::Knight, 781),
        (PieceType::Bishop, 825),
        (PieceType::Rook, 1276),
        (PieceType::Queen, 2538),
    ];
    let mut total = 0;
    for color in [Color::White, Color::Black] {
        for (kind, value) in VALUES {
            total += position.pieces(color, kind).count() as i32 * value;
        }
    }
    total
}

/// A short list of threat or pawn-pair features in a perspective-free encoding. Items
/// beyond its capacity are dropped and mark it as overflowed.
#[derive(Clone)]
struct List<const N: usize> {
    len: usize,
    overflowed: bool,
    items: [MaybeUninit<u32>; N],
}

impl<const N: usize> List<N> {
    fn new() -> Self {
        Self {
            len: 0,
            overflowed: false,
            items: [const { MaybeUninit::uninit() }; N],
        }
    }

    fn clear(&mut self) {
        self.len = 0;
        self.overflowed = false;
    }

    fn push(&mut self, item: u32) {
        if self.len == N {
            self.overflowed = true;
            return;
        }
        self.items[self.len].write(item);
        self.len += 1;
    }

    fn as_slice(&self) -> &[u32] {
        // SAFETY: the first `len` items were written by `push`.
        unsafe { std::slice::from_raw_parts(self.items.as_ptr().cast(), self.len) }
    }

    fn sorted(mut self) -> Self {
        // SAFETY: as in `as_slice`.
        unsafe { std::slice::from_raw_parts_mut(self.items.as_mut_ptr().cast::<u32>(), self.len) }
            .sort_unstable();
        self
    }
}

/// Features a move removes and adds, read from the position before the move. A king
/// move changes every feature of its own perspective, which is then refreshed instead.
/// Threats and pawn pairs are compared on the squares the move changes: those the move
/// empties or fills, and so every threat from, onto or through them.
pub struct Delta {
    removed: [(Piece, Square); 2],
    removed_len: usize,
    added: [(Piece, Square); 2],
    added_len: usize,
    king_moved: Option<Color>,
    changed: Bitboard,
    threats: List<TOUCHING>,
    pairs: List<TOUCHING>,
}

impl Delta {
    fn remove(&mut self, piece: Piece, square: Square) {
        self.removed[self.removed_len] = (piece, square);
        self.removed_len += 1;
        self.changed = self.changed | square.bit();
    }

    fn add(&mut self, piece: Piece, square: Square) {
        self.added[self.added_len] = (piece, square);
        self.added_len += 1;
        self.changed = self.changed | square.bit();
    }
}

/// Feature changes of one move for both perspectives, prepared by `Network::prepare`.
#[derive(Clone)]
pub struct Update {
    gone: [[u32; 2]; 2],
    gone_len: usize,
    new: [[u32; 2]; 2],
    new_len: usize,
    extra_gone: [List<{ 2 * TOUCHING }>; 2],
    extra_new: [List<{ 2 * TOUCHING }>; 2],
}

impl Update {
    /// Makes this an update that changes nothing, as for a null move.
    pub fn clear(&mut self) {
        self.gone_len = 0;
        self.new_len = 0;
        for list in self.extra_gone.iter_mut().chain(&mut self.extra_new) {
            list.clear();
        }
    }

    /// An update that changes nothing.
    pub fn none() -> Self {
        Self {
            gone: [[0; 2]; 2],
            gone_len: 0,
            new: [[0; 2]; 2],
            new_len: 0,
            extra_gone: [List::new(), List::new()],
            extra_new: [List::new(), List::new()],
        }
    }
}

pub fn delta(position: &Position, mv: Move) -> Delta {
    let side = position.side_to_move();
    let piece = position
        .piece_at(mv.from())
        .expect("legal move has a mover");
    let placeholder = (piece, mv.from());
    let mut delta = Delta {
        removed: [placeholder; 2],
        removed_len: 0,
        added: [placeholder; 2],
        added_len: 0,
        king_moved: (piece.kind == PieceType::King).then_some(side),
        changed: Bitboard::EMPTY,
        threats: List::new(),
        pairs: List::new(),
    };
    if mv.is_castle() {
        let kingside = mv.flag() == 2;
        let rook = Piece {
            color: side,
            kind: PieceType::Rook,
        };
        delta.remove(piece, mv.from());
        delta.remove(rook, mv.to());
        delta.add(
            piece,
            Square::new(if kingside { 6 } else { 2 }, side.back_rank()),
        );
        delta.add(
            rook,
            Square::new(if kingside { 5 } else { 3 }, side.back_rank()),
        );
    } else {
        delta.remove(piece, mv.from());
        if mv.flag() == MoveFlag::EnPassant as u8 {
            let victim = Piece {
                color: side.other(),
                kind: PieceType::Pawn,
            };
            delta.remove(victim, Square::new(mv.to().file(), mv.from().rank()));
        } else if let Some(victim) = position.piece_at(mv.to()) {
            delta.remove(victim, mv.to());
        }
        let placed = Piece {
            color: side,
            kind: mv.promotion().unwrap_or(piece.kind),
        };
        delta.add(placed, mv.to());
    }
    delta.threats = touching_threats(position, delta.changed);
    delta.pairs = touching_pairs(position, delta.changed);
    delta
}

/// Stockfish's piece numbering: 1 to 6 for White's pawn to king, 9 to 14 for Black's.
fn code(piece: Piece) -> usize {
    8 * piece.color.index() + piece.kind.index() + 1
}

fn square(index: usize) -> Square {
    Square::new((index % 8) as u8, (index / 8) as u8)
}

/// Squares a piece of `code` attacks from `from` on an empty board.
fn pseudo_attacks(code: usize, from: Square) -> Bitboard {
    let color = if code < 8 { Color::White } else { Color::Black };
    match code & 7 {
        1 => pawn_attacks(color, from),
        2 => knight_attacks(from),
        3 => bishop_attacks(from, Bitboard::EMPTY),
        4 => rook_attacks(from, Bitboard::EMPTY),
        5 => bishop_attacks(from, Bitboard::EMPTY) | rook_attacks(from, Bitboard::EMPTY),
        6 => king_attacks(from),
        _ => Bitboard::EMPTY,
    }
}

/// Threat index tables built as Stockfish 19 builds them at compile time.
struct ThreatTables {
    /// [attacker][attacked][from < to]: the attacker-and-target base, or `THREAT_INPUTS`
    /// for pairs that are not features.
    base: [[[u32; 2]; 16]; 16],
    /// [attacker][from]: attacks of that piece from all lower squares.
    offsets: [[u32; 64]; 16],
    /// [attacker][from][to]: attacks from `from` on squares below `to`.
    below: Box<[[[u8; 64]; 64]; 16]>,
}

fn threat_tables() -> &'static ThreatTables {
    static TABLES: OnceLock<ThreatTables> = OnceLock::new();
    TABLES.get_or_init(|| {
        const PIECES: [usize; 12] = [1, 2, 3, 4, 5, 6, 9, 10, 11, 12, 13, 14];
        // Kinds a threat from each kind may target, pawns to queens, and the number of
        // them over both colours.
        const MAP: [[i32; 6]; 6] = [
            [-1, 0, -1, 1, -1, -1],
            [0, 1, 2, 3, 4, -1],
            [0, 1, 2, 3, -1, -1],
            [0, 1, 2, 3, -1, -1],
            [0, 1, 2, 3, 4, -1],
            [-1, -1, -1, -1, -1, -1],
        ];
        const TARGETS: [i64; 7] = [0, 4, 10, 8, 8, 10, 0];
        let mut offsets = [[0_u32; 64]; 16];
        let mut helpers = [(0_i64, 0_i64); 16];
        let mut cumulative = 0;
        for piece in PIECES {
            let mut per_piece = 0;
            for (from, offset) in offsets[piece].iter_mut().enumerate() {
                *offset = per_piece as u32;
                if piece & 7 != 1 || (8..56).contains(&from) {
                    per_piece += i64::from(pseudo_attacks(piece, square(from)).count());
                }
            }
            helpers[piece] = (per_piece, cumulative);
            cumulative += TARGETS[piece & 7] * per_piece;
        }
        let mut base = [[[THREAT_INPUTS as u32; 2]; 16]; 16];
        for attacker in PIECES {
            for attacked in PIECES {
                let (attacker_kind, attacked_kind) = (attacker & 7, attacked & 7);
                let map = MAP[attacker_kind - 1][attacked_kind - 1];
                if map < 0 {
                    continue;
                }
                let other_colour = attacker ^ attacked == 8;
                let semi_excluded =
                    attacker_kind == attacked_kind && (other_colour || attacker_kind != 1);
                let (per_piece, cumulative) = helpers[attacker];
                let feature = cumulative
                    + ((attacked >> 3) as i64 * (TARGETS[attacker_kind] / 2) + i64::from(map))
                        * per_piece;
                base[attacker][attacked][0] = feature as u32;
                if !semi_excluded {
                    base[attacker][attacked][1] = feature as u32;
                }
            }
        }
        let mut below = Box::new([[[0_u8; 64]; 64]; 16]);
        for piece in PIECES {
            for from in 0..64 {
                let attacks = pseudo_attacks(piece, square(from)).0;
                for to in 0..64 {
                    below[piece][from][to] = (attacks & ((1_u64 << to) - 1)).count_ones() as u8;
                }
            }
        }
        ThreatTables {
            base,
            offsets,
            below,
        }
    })
}

/// A threat as attacker code, attacker square, target square and target code.
fn encode_threat(attacker: usize, from: Square, to: Square, attacked: usize) -> u32 {
    ((from.index() as u32) << 16)
        | ((to.index() as u32) << 8)
        | ((attacker as u32) << 4)
        | attacked as u32
}

/// The threat and pawn-pair features mirror the board so that the perspective's king
/// stands on files a to d, the opposite of the piece-square features, and rotate it for
/// Black.
fn extra_orientation(perspective: Color, king: Square) -> usize {
    (if king.file() < 4 { 0 } else { 7 }) ^ (56 * perspective.index())
}

/// Feature indices of encoded threats and pawn pairs for one perspective.
struct Indexer {
    tables: &'static ThreatTables,
    orientation: usize,
    perspective: usize,
}

impl Indexer {
    fn new(perspective: Color, king: Square) -> Self {
        Self {
            tables: threat_tables(),
            orientation: extra_orientation(perspective, king),
            perspective: perspective.index(),
        }
    }

    /// The threat's feature, or `None` when the network has no feature for it.
    fn threat(&self, threat: u32) -> Option<u32> {
        let from = ((threat >> 16) & 63) as usize ^ self.orientation;
        let to = ((threat >> 8) & 63) as usize ^ self.orientation;
        let swap = 8 * self.perspective;
        let attacker = ((threat >> 4) & 15) as usize ^ swap;
        let attacked = (threat & 15) as usize ^ swap;
        let index = self.tables.base[attacker][attacked][usize::from(from < to)]
            + self.tables.offsets[attacker][from]
            + u32::from(self.tables.below[attacker][from][to]);
        (index < THREAT_INPUTS as u32).then_some(index)
    }

    fn pair(&self, pair: u32) -> u32 {
        let id = |square: u32, colour: u32| {
            48 * (colour as usize ^ self.perspective) + (square as usize ^ self.orientation) - 8
        };
        let a = id((pair >> 16) & 63, (pair >> 1) & 1);
        let b = id((pair >> 8) & 63, pair & 1);
        let (high, low) = (a.max(b), a.min(b));
        (THREAT_INPUTS + high * (high - 1) / 2 + low) as u32
    }

    /// Features of the listed threats and pawn pairs.
    fn features<const N: usize>(&self, threats: &[u32], pairs: &[u32], out: &mut List<N>) {
        for &threat in threats {
            if let Some(feature) = self.threat(threat) {
                out.push(feature);
            }
        }
        for &pair in pairs {
            out.push(self.pair(pair));
        }
    }
}

/// Pieces of each kind of both colours.
fn kinds(position: &Position) -> [Bitboard; 6] {
    PIECE_KINDS
        .map(|kind| position.pieces(Color::White, kind) | position.pieces(Color::Black, kind))
}

/// Adds every threat of the piece on `from`: pawns threaten knights and rooks, knights
/// and queens every piece but kings, bishops and rooks every piece but queens and kings.
/// With `through` set, only threats from, onto or through its squares are kept.
fn push_threats<const N: usize>(
    position: &Position,
    kinds: &[Bitboard; 6],
    from: Square,
    through: Option<Bitboard>,
    list: &mut List<N>,
) {
    let Some(piece) = position.piece_at(from) else {
        return;
    };
    let occupied = position.occupied();
    let [pawns, knights, bishops, rooks, queens, _] = *kinds;
    let (attacks, targets) = match piece.kind {
        PieceType::Pawn => (pawn_attacks(piece.color, from), knights | rooks),
        PieceType::Knight => (
            knight_attacks(from),
            pawns | knights | bishops | rooks | queens,
        ),
        PieceType::Bishop => (
            bishop_attacks(from, occupied),
            pawns | knights | bishops | rooks,
        ),
        PieceType::Rook => (
            rook_attacks(from, occupied),
            pawns | knights | bishops | rooks,
        ),
        PieceType::Queen => (
            bishop_attacks(from, occupied) | rook_attacks(from, occupied),
            pawns | knights | bishops | rooks | queens,
        ),
        PieceType::King => return,
    };
    for to in attacks & targets {
        if let Some(changed) = through {
            let touches = changed.contains(from)
                || changed.contains(to)
                || (between(from, to) & changed) != Bitboard::EMPTY;
            if !touches {
                continue;
            }
        }
        let attacked = position.piece_at(to).expect("target is occupied");
        list.push(encode_threat(code(piece), from, to, code(attacked)));
    }
}

/// Every active threat.
fn all_threats(position: &Position) -> List<MAX_EXTRA> {
    let kinds = kinds(position);
    let mut list = List::new();
    for from in position.occupied() {
        push_threats(position, &kinds, from, None, &mut list);
    }
    list
}

/// Active threats from, onto or through the `changed` squares, sorted since attackers
/// and their targets are visited in square order. A threat whose
/// squares and the squares between them are all unchanged is the same before and after a
/// move, so comparing these lists before and after gives exactly the threats it changes.
fn touching_threats(position: &Position, changed: Bitboard) -> List<TOUCHING> {
    let kinds = kinds(position);
    let [_, knights, bishops, rooks, queens, _] = kinds;
    let occupied = position.occupied();
    let mut attackers = occupied & changed;
    for square in changed {
        attackers = attackers
            | (pawn_attacks(Color::Black, square) & position.pieces(Color::White, PieceType::Pawn))
            | (pawn_attacks(Color::White, square) & position.pieces(Color::Black, PieceType::Pawn))
            | (knight_attacks(square) & knights)
            | (bishop_attacks(square, occupied) & (bishops | queens))
            | (rook_attacks(square, occupied) & (rooks | queens));
    }
    let mut list = List::new();
    for from in attackers {
        push_threats(position, &kinds, from, Some(changed), &mut list);
    }
    debug_assert!(list.as_slice().is_sorted());
    list
}

/// Squares of ranks 2 to 7 on the file of `square` and its neighbours, other than itself.
fn pair_band(square: Square) -> Bitboard {
    const FILE_A: u64 = 0x0101_0101_0101_0101;
    const INNER_RANKS: u64 = 0x00FF_FFFF_FFFF_FF00;
    let file = FILE_A << square.file();
    let files = file | ((file << 1) & !FILE_A) | ((file >> 1) & !(FILE_A << 7));
    Bitboard(files & INNER_RANKS & !square.bit().0)
}

/// A pawn pair as its lower square and colour and its higher square and colour.
fn encode_pair(position: &Position, a: Square, b: Square) -> u32 {
    let (low, high) = if a.index() < b.index() {
        (a, b)
    } else {
        (b, a)
    };
    let colour = |square: Square| position.piece_at(square).expect("pawn").color.index() as u32;
    ((low.index() as u32) << 16) | ((high.index() as u32) << 8) | (colour(low) << 1) | colour(high)
}

fn all_pairs(position: &Position) -> List<MAX_EXTRA> {
    let pawns = position.pieces(Color::White, PieceType::Pawn)
        | position.pieces(Color::Black, PieceType::Pawn);
    let mut list = List::new();
    for a in pawns {
        for b in pair_band(a) & pawns {
            if a.index() < b.index() {
                list.push(encode_pair(position, a, b));
            }
        }
    }
    list
}

/// Pawn pairs with a pawn on a `changed` square, sorted.
fn touching_pairs(position: &Position, changed: Bitboard) -> List<TOUCHING> {
    let pawns = position.pieces(Color::White, PieceType::Pawn)
        | position.pieces(Color::Black, PieceType::Pawn);
    let mut list = List::new();
    for a in pawns & changed {
        for b in pair_band(a) & pawns {
            if !changed.contains(b) || a.index() < b.index() {
                list.push(encode_pair(position, a, b));
            }
        }
    }
    list.sorted()
}

/// Items of sorted `before` missing from sorted `after`, and the reverse.
fn difference(before: &[u32], after: &[u32]) -> (List<TOUCHING>, List<TOUCHING>) {
    let (mut gone, mut new) = (List::new(), List::new());
    let (mut i, mut j) = (0, 0);
    while i < before.len() || j < after.len() {
        match (before.get(i), after.get(j)) {
            (Some(&x), Some(&y)) if x == y => {
                i += 1;
                j += 1;
            }
            (Some(&x), Some(&y)) if x < y => {
                gone.push(x);
                i += 1;
            }
            (Some(&x), None) => {
                gone.push(x);
                i += 1;
            }
            (_, Some(&y)) => {
                new.push(y);
                j += 1;
            }
            (None, None) => unreachable!(),
        }
    }
    (gone, new)
}

/// Accumulators last computed for each perspective and king square, with the pieces
/// they were computed for. A king move refreshes the piece-square part from the entry
/// for its new square by adding and removing only the pieces that differ, instead of
/// summing every piece; the threat and pawn-pair parts are then summed afresh.
pub struct RefreshCache {
    entries: Box<[CacheEntry]>,
}

#[derive(Clone)]
struct CacheEntry {
    values: [i16; HALF],
    psqt: [i32; BUCKETS],
    pieces: [[Bitboard; 6]; 2],
}

impl RefreshCache {
    pub fn new() -> Self {
        let empty = CacheEntry {
            values: network().feature_bias[..].try_into().expect("bias width"),
            psqt: [0; BUCKETS],
            pieces: [[Bitboard::EMPTY; 6]; 2],
        };
        Self {
            entries: vec![empty; 2 * 64].into_boxed_slice(),
        }
    }
}

impl Default for RefreshCache {
    fn default() -> Self {
        Self::new()
    }
}

/// The bundled network, parsed on first use.
pub fn network() -> &'static Network {
    static NETWORK: OnceLock<Network> = OnceLock::new();
    NETWORK.get_or_init(|| Network::parse(NET).expect("bundled network is valid"))
}

struct Reader<'a> {
    bytes: &'a [u8],
}

impl Reader<'_> {
    fn take(&mut self, count: usize) -> Result<&[u8], &'static str> {
        if self.bytes.len() < count {
            return Err("network file is truncated");
        }
        let (head, tail) = self.bytes.split_at(count);
        self.bytes = tail;
        Ok(head)
    }

    fn u32(&mut self) -> Result<u32, &'static str> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("four bytes"),
        ))
    }

    fn i32s(&mut self, count: usize) -> Result<Box<[i32]>, &'static str> {
        Ok(self
            .take(count * 4)?
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&quad| i32::from_le_bytes(quad))
            .collect())
    }

    fn i32_array<const N: usize>(&mut self) -> Result<[i32; N], &'static str> {
        Ok(self.i32s(N)?[..].try_into().expect("requested length"))
    }

    fn i8s(&mut self, count: usize) -> Result<Box<[i8]>, &'static str> {
        Ok(self.take(count)?.iter().map(|&byte| byte as i8).collect())
    }

    /// A block of `count` signed LEB128 values behind Stockfish's magic string and byte
    /// length, each narrowed to `T` as Stockfish stores it.
    fn leb128<T>(&mut self, count: usize, narrow: fn(i32) -> T) -> Result<Box<[T]>, &'static str> {
        if self.take(17)? != b"COMPRESSED_LEB128" {
            return Err("network block is not LEB128 compressed");
        }
        let length = self.u32()? as usize;
        let bytes = self.take(length)?;
        let mut values = Vec::with_capacity(count);
        let (mut result, mut shift) = (0_u32, 0_u32);
        for &byte in bytes {
            result |= u32::from(byte & 0x7f) << (shift % 32);
            shift += 7;
            if byte & 0x80 == 0 {
                let value = if shift >= 32 || byte & 0x40 == 0 {
                    result
                } else {
                    result | !((1_u32 << shift) - 1)
                };
                values.push(narrow(value as i32));
                (result, shift) = (0, 0);
            }
        }
        if values.len() != count || shift != 0 {
            return Err("network block has the wrong length");
        }
        Ok(values.into_boxed_slice())
    }
}

impl Network {
    /// Reads the Stockfish 19 file format: a version, a hash and a description, then the
    /// feature transformer and eight layer stacks, each behind its own hash.
    pub fn parse(bytes: &[u8]) -> Result<Self, &'static str> {
        let mut reader = Reader { bytes };
        if reader.u32()? != VERSION {
            return Err("unsupported network version");
        }
        reader.u32()?;
        let description = reader.u32()? as usize;
        reader.take(description)?;
        reader.u32()?;
        let feature_bias = reader.leb128(HALF, |value| value as i16)?;
        let threat_weights = reader.i8s(THREAT_INPUTS * HALF)?;
        let threat_psqt = reader.leb128(THREAT_INPUTS * BUCKETS, |value| value)?;
        let pair_weights = reader.i8s(PAIR_INPUTS * HALF)?;
        let pair_psqt = reader.leb128(PAIR_INPUTS * BUCKETS, |value| value)?;
        let feature_weights = reader.leb128(PSQ_INPUTS * HALF, |value| value as i16)?;
        let psqt_weights = reader.leb128(PSQ_INPUTS * BUCKETS, |value| value)?;
        let mut stacks = Vec::with_capacity(BUCKETS);
        for _ in 0..BUCKETS {
            reader.u32()?;
            let fc0_bias = reader.i32_array::<FC0_OUTPUTS>()?;
            let rows = reader.i8s(FC0_OUTPUTS * HALF)?;
            let fc0_weights = (0..FC0_OUTPUTS * HALF)
                .map(|index| {
                    let (group, rest) = (index / (4 * FC0_OUTPUTS), index % (4 * FC0_OUTPUTS));
                    rows[(rest / 4) * HALF + group * 4 + rest % 4]
                })
                .collect();
            let fc1_bias = reader.i32_array::<FC1_OUTPUTS>()?;
            let fc1_weights = reader.i8s(FC1_OUTPUTS * FC1_INPUTS)?;
            let [fc2_bias] = reader.i32_array::<1>()?;
            let fc2_weights = reader.i8s(FC2_INPUTS)?;
            stacks.push(Stack {
                fc0_bias,
                fc0_weights,
                fc1_bias,
                fc1_weights,
                fc2_bias,
                fc2_weights: fc2_weights[..].try_into().expect("128 weights"),
            });
        }
        if !reader.bytes.is_empty() {
            return Err("network file has trailing bytes");
        }
        let extra_weights = [threat_weights, pair_weights].concat().into_boxed_slice();
        let extra_psqt_weights = [threat_psqt, pair_psqt].concat().into_boxed_slice();
        debug_assert_eq!(extra_weights.len(), EXTRA_INPUTS * HALF);
        Ok(Self {
            feature_bias,
            feature_weights,
            psqt_weights,
            extra_weights,
            extra_psqt_weights,
            stacks: stacks.into_boxed_slice(),
        })
    }

    fn row(&self, feature: usize) -> &[i16; HALF] {
        self.feature_weights[feature * HALF..(feature + 1) * HALF]
            .try_into()
            .expect("row width")
    }

    fn psqt_row(&self, feature: usize) -> &[i32; BUCKETS] {
        self.psqt_weights[feature * BUCKETS..(feature + 1) * BUCKETS]
            .try_into()
            .expect("bucket count")
    }

    fn extra_psqt_row(&self, feature: usize) -> &[i32; BUCKETS] {
        self.extra_psqt_weights[feature * BUCKETS..(feature + 1) * BUCKETS]
            .try_into()
            .expect("bucket count")
    }

    /// Adds (`sign` 1) or removes (`sign` -1) one piece-square feature in place.
    fn toggle(
        &self,
        values: &mut [i16; HALF],
        psqt: &mut [i32; BUCKETS],
        feature: usize,
        sign: i16,
    ) {
        for (value, &weight) in values.iter_mut().zip(self.row(feature)) {
            *value = value.wrapping_add(sign.wrapping_mul(weight));
        }
        for (value, &weight) in psqt.iter_mut().zip(self.psqt_row(feature)) {
            *value += i32::from(sign) * weight;
        }
    }

    /// Adds every threat and pawn-pair feature of `position` for one perspective.
    fn add_extras(
        &self,
        position: &Position,
        perspective: Color,
        values: &mut [i16; HALF],
        psqt: &mut [i32; BUCKETS],
    ) {
        let mut features = List::<{ 2 * MAX_EXTRA }>::new();
        Indexer::new(perspective, position.king(perspective)).features(
            all_threats(position).as_slice(),
            all_pairs(position).as_slice(),
            &mut features,
        );
        for &feature in features.as_slice() {
            for (value, &weight) in psqt.iter_mut().zip(self.extra_psqt_row(feature as usize)) {
                *value += weight;
            }
        }
        let parent = *values;
        update_values(values, &parent, self, &[], &[], &[], features.as_slice());
    }

    /// Recomputes one perspective's accumulator from the board.
    pub fn refresh(
        &self,
        position: &Position,
        perspective: Color,
        values: &mut [i16; HALF],
        psqt: &mut [i32; BUCKETS],
    ) {
        values.copy_from_slice(&self.feature_bias);
        *psqt = [0; BUCKETS];
        let king = position.king(perspective);
        for color in [Color::White, Color::Black] {
            for kind in PIECE_KINDS {
                for square in position.pieces(color, kind) {
                    let feature = feature(perspective, king, Piece { color, kind }, square);
                    self.toggle(values, psqt, feature, 1);
                }
            }
        }
        self.add_extras(position, perspective, values, psqt);
    }

    /// Refreshes one perspective through the cache entry for its king square.
    fn refresh_cached(
        &self,
        position: &Position,
        perspective: Color,
        values: &mut [i16; HALF],
        psqt: &mut [i32; BUCKETS],
        cache: &mut RefreshCache,
    ) {
        let king = position.king(perspective);
        let entry = &mut cache.entries[perspective.index() * 64 + king.index()];
        for color in [Color::White, Color::Black] {
            for (index, kind) in PIECE_KINDS.into_iter().enumerate() {
                let now = position.pieces(color, kind);
                let before = entry.pieces[color.index()][index];
                let piece = Piece { color, kind };
                for square in before & !now {
                    let feature = feature(perspective, king, piece, square);
                    self.toggle(&mut entry.values, &mut entry.psqt, feature, -1);
                }
                for square in now & !before {
                    let feature = feature(perspective, king, piece, square);
                    self.toggle(&mut entry.values, &mut entry.psqt, feature, 1);
                }
                entry.pieces[color.index()][index] = now;
            }
        }
        *values = entry.values;
        *psqt = entry.psqt;
        self.add_extras(position, perspective, values, psqt);
    }

    /// Derives the accumulator after a move from the one before it; `after` is the
    /// position once the move is made.
    pub fn apply(
        &self,
        parent: &Accumulator,
        child: &mut Accumulator,
        delta: &Delta,
        after: &Position,
        cache: &mut RefreshCache,
    ) {
        let threats = touching_threats(after, delta.changed);
        let pairs = touching_pairs(after, delta.changed);
        let overflowed = delta.threats.overflowed
            || delta.pairs.overflowed
            || threats.overflowed
            || pairs.overflowed;
        let (threats_gone, threats_new) = difference(delta.threats.as_slice(), threats.as_slice());
        let (pairs_gone, pairs_new) = difference(delta.pairs.as_slice(), pairs.as_slice());
        for perspective in [Color::White, Color::Black] {
            let side = perspective.index();
            let (values, psqt) = (&mut child.values[side], &mut child.psqt[side]);
            if overflowed || delta.king_moved == Some(perspective) {
                self.refresh_cached(after, perspective, values, psqt, cache);
                continue;
            }
            let king = after.king(perspective);
            let index = |&(piece, square): &(Piece, Square)| {
                feature(perspective, king, piece, square) as u32
            };
            let gone = [index(&delta.removed[0]), index(&delta.removed[1])];
            let new = [index(&delta.added[0]), index(&delta.added[1])];
            let gone = &gone[..delta.removed_len];
            let new = &new[..delta.added_len];
            let indexer = Indexer::new(perspective, king);
            let mut extra_gone = List::<{ 2 * TOUCHING }>::new();
            let mut extra_new = List::<{ 2 * TOUCHING }>::new();
            indexer.features(
                threats_gone.as_slice(),
                pairs_gone.as_slice(),
                &mut extra_gone,
            );
            indexer.features(threats_new.as_slice(), pairs_new.as_slice(), &mut extra_new);
            *psqt = parent.psqt[side];
            for (rows, sign) in [(gone, -1), (new, 1)] {
                for &feature in rows {
                    for (value, &weight) in psqt.iter_mut().zip(self.psqt_row(feature as usize)) {
                        *value += sign * weight;
                    }
                }
            }
            for (rows, sign) in [(&extra_gone, -1), (&extra_new, 1)] {
                for &feature in rows.as_slice() {
                    let weights = self.extra_psqt_row(feature as usize);
                    for (value, &weight) in psqt.iter_mut().zip(weights) {
                        *value += sign * weight;
                    }
                }
            }
            update_values(
                values,
                &parent.values[side],
                self,
                gone,
                new,
                extra_gone.as_slice(),
                extra_new.as_slice(),
            );
        }
    }

    /// The feature changes of the move `delta` describes, for both perspectives, so that
    /// the accumulator can be updated later and only if an evaluation needs it. Returns
    /// false, leaving `update` unspecified, when the move needs a refresh instead: a king move, or more changed
    /// threats than an incremental update holds.
    pub fn prepare(&self, delta: &Delta, after: &Position, update: &mut Update) -> bool {
        if delta.king_moved.is_some() || delta.threats.overflowed || delta.pairs.overflowed {
            return false;
        }
        let threats = touching_threats(after, delta.changed);
        let pairs = touching_pairs(after, delta.changed);
        if threats.overflowed || pairs.overflowed {
            return false;
        }
        let (threats_gone, threats_new) = difference(delta.threats.as_slice(), threats.as_slice());
        let (pairs_gone, pairs_new) = difference(delta.pairs.as_slice(), pairs.as_slice());
        update.clear();
        update.gone_len = delta.removed_len;
        update.new_len = delta.added_len;
        for perspective in [Color::White, Color::Black] {
            let side = perspective.index();
            let king = after.king(perspective);
            for (slot, &(piece, square)) in delta.removed[..delta.removed_len].iter().enumerate() {
                update.gone[side][slot] = feature(perspective, king, piece, square) as u32;
            }
            for (slot, &(piece, square)) in delta.added[..delta.added_len].iter().enumerate() {
                update.new[side][slot] = feature(perspective, king, piece, square) as u32;
            }
            let indexer = Indexer::new(perspective, king);
            indexer.features(
                threats_gone.as_slice(),
                pairs_gone.as_slice(),
                &mut update.extra_gone[side],
            );
            indexer.features(
                threats_new.as_slice(),
                pairs_new.as_slice(),
                &mut update.extra_new[side],
            );
        }
        true
    }

    /// Derives a child's accumulator from its parent's by a prepared update.
    pub fn apply_update(&self, parent: &Accumulator, child: &mut Accumulator, update: &Update) {
        for side in 0..2 {
            let gone = &update.gone[side][..update.gone_len];
            let new = &update.new[side][..update.new_len];
            let psqt = &mut child.psqt[side];
            *psqt = parent.psqt[side];
            for (rows, sign) in [(gone, -1), (new, 1)] {
                for &feature in rows {
                    for (value, &weight) in psqt.iter_mut().zip(self.psqt_row(feature as usize)) {
                        *value += sign * weight;
                    }
                }
            }
            for (rows, sign) in [(&update.extra_gone[side], -1), (&update.extra_new[side], 1)] {
                for &feature in rows.as_slice() {
                    let weights = self.extra_psqt_row(feature as usize);
                    for (value, &weight) in psqt.iter_mut().zip(weights) {
                        *value += sign * weight;
                    }
                }
            }
            update_values(
                &mut child.values[side],
                &parent.values[side],
                self,
                gone,
                new,
                update.extra_gone[side].as_slice(),
                update.extra_new[side].as_slice(),
            );
        }
    }

    pub fn fresh(&self, position: &Position) -> Accumulator {
        let mut accumulator = Accumulator::default();
        for perspective in [Color::White, Color::Black] {
            let side = perspective.index();
            self.refresh(
                position,
                perspective,
                &mut accumulator.values[side],
                &mut accumulator.psqt[side],
            );
        }
        accumulator
    }

    /// Network output for the side to move of `position`, whose accumulator is given.
    pub fn evaluate(&self, accumulator: &Accumulator, position: &Position) -> Output {
        let side = position.side_to_move();
        let bucket = (position.occupied().count() as usize - 1) / 4;
        let (us, them) = (side.index(), side.other().index());
        let psqt = (accumulator.psqt[us][bucket] - accumulator.psqt[them][bucket]) / 2;
        // Each perspective's two halves are clipped to 0 to 255 and multiplied pairwise.
        let mut input = [0_u8; HALF];
        for (half, perspective) in [us, them].into_iter().enumerate() {
            let (low, high) = accumulator.values[perspective].split_at(HALF / 2);
            pairwise(
                low.try_into().expect("half width"),
                high.try_into().expect("half width"),
                (&mut input[half * HALF / 2..(half + 1) * HALF / 2])
                    .try_into()
                    .expect("half width"),
            );
        }
        let stack = &self.stacks[bucket];
        let fc0 = first_layer(&input, &stack.fc0_weights);
        let fc0: [i32; FC0_OUTPUTS] = std::array::from_fn(|row| stack.fc0_bias[row] + fc0[row]);
        // Every layer's outputs go on both squared and clipped, and the last layer reads
        // all of them.
        let mut hidden = [0_u8; FC2_INPUTS];
        activate(&fc0, &mut hidden[..FC1_INPUTS], WEIGHT_SCALE_BITS + 1);
        let fc1 = products::<FC1_OUTPUTS>(&hidden[..FC1_INPUTS], &stack.fc1_weights);
        let fc1: [i32; FC1_OUTPUTS] = std::array::from_fn(|row| stack.fc1_bias[row] + fc1[row]);
        activate(&fc1, &mut hidden[FC1_INPUTS..], WEIGHT_SCALE_BITS);
        let fc2 = stack.fc2_bias
            + hidden
                .iter()
                .zip(&stack.fc2_weights)
                .map(|(&input, &weight)| i32::from(input) * i32::from(weight))
                .sum::<i32>();
        let forward = fc2 + fc0[FC0_OUTPUTS - 2] - fc0[FC0_OUTPUTS - 1];
        // 1.0 in the output is 128 << 6 twice over, and becomes 600 * 16.
        let positional = i64::from(forward) * (600 * i64::from(OUTPUT_SCALE))
            / (128 * (1 << WEIGHT_SCALE_BITS) * 2);
        Output {
            psqt: psqt / OUTPUT_SCALE,
            positional: positional as i32 / OUTPUT_SCALE,
        }
    }

    /// Evaluates a position from scratch.
    pub fn evaluate_position(&self, position: &Position) -> Output {
        self.evaluate(&self.fresh(position), position)
    }
}

/// Writes the squared activations of `values` into the first half of `out` and the
/// clipped ones into the second, both on the scale of `shift` bits.
fn activate(values: &[i32], out: &mut [u8], shift: u32) {
    let (squared, clipped) = out.split_at_mut(values.len());
    for ((&value, squared), clipped) in values.iter().zip(squared).zip(clipped) {
        let value64 = i64::from(value);
        *squared = ((value64 * value64) >> (2 * shift + 7)).min(127) as u8;
        *clipped = (value >> shift).clamp(0, 127) as u8;
    }
}

/// Writes `parent` less the `sub` features plus the `add` features into `values` in one
/// pass, piece-square rows of 16 bits and threat or pawn-pair rows of 8, a tile at a time
/// so the running sums stay in registers. On x86-64 the same loop is compiled a second
/// time for AVX2 and chosen at run time, since the release builds target the baseline
/// instruction set.
fn update_values(
    values: &mut [i16; HALF],
    parent: &[i16; HALF],
    network: &Network,
    sub: &[u32],
    add: &[u32],
    sub_extra: &[u32],
    add_extra: &[u32],
) {
    let rows = Rows {
        weights: &network.feature_weights,
        extra: &network.extra_weights,
        sub,
        add,
        sub_extra,
        add_extra,
    };
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") {
            // SAFETY: AVX2 was just detected.
            return unsafe { simd::update_avx2(values, parent, &rows) };
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: Neon is part of the aarch64 baseline.
        return unsafe { simd::update_neon(values, parent, &rows) };
    }
    #[allow(unreachable_code)]
    update_plain(values, parent, &rows);
}

/// The weight tables and the features an update removes and adds.
struct Rows<'a> {
    weights: &'a [i16],
    extra: &'a [i8],
    sub: &'a [u32],
    add: &'a [u32],
    sub_extra: &'a [u32],
    add_extra: &'a [u32],
}

impl Rows<'_> {
    fn row(&self, feature: u32, start: usize, len: usize) -> &[i16] {
        &self.weights[feature as usize * HALF + start..][..len]
    }

    fn extra_row(&self, feature: u32, start: usize, len: usize) -> &[i8] {
        &self.extra[feature as usize * HALF + start..][..len]
    }
}

#[inline(always)]
fn update_plain(values: &mut [i16; HALF], parent: &[i16; HALF], rows: &Rows) {
    const TILE: usize = 128;
    for start in (0..HALF).step_by(TILE) {
        let range = start..start + TILE;
        let mut tile: [i16; TILE] = parent[range.clone()].try_into().expect("tile width");
        for &feature in rows.sub {
            for (value, &weight) in tile.iter_mut().zip(rows.row(feature, start, TILE)) {
                *value = value.wrapping_sub(weight);
            }
        }
        for &feature in rows.add {
            for (value, &weight) in tile.iter_mut().zip(rows.row(feature, start, TILE)) {
                *value = value.wrapping_add(weight);
            }
        }
        for &feature in rows.sub_extra {
            for (value, &weight) in tile.iter_mut().zip(rows.extra_row(feature, start, TILE)) {
                *value = value.wrapping_sub(i16::from(weight));
            }
        }
        for &feature in rows.add_extra {
            for (value, &weight) in tile.iter_mut().zip(rows.extra_row(feature, start, TILE)) {
                *value = value.wrapping_add(i16::from(weight));
            }
        }
        values[range].copy_from_slice(&tile);
    }
}

/// Clips both halves to 0 to 255 and writes their products scaled down by 512, which
/// stay below 128.
fn pairwise(low: &[i16; HALF / 2], high: &[i16; HALF / 2], out: &mut [u8; HALF / 2]) {
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: Neon is part of the aarch64 baseline.
        unsafe { simd::pairwise_neon(low, high, out) }
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        for ((slot, &low), &high) in out.iter_mut().zip(low).zip(high) {
            // Both factors are at most 255, so their product fits in 16 bits.
            let (low, high) = (low.clamp(0, 255) as u16, high.clamp(0, 255) as u16);
            *slot = ((low * high) >> 9) as u8;
        }
    }
}

/// Room for the groups of four inputs that are not all zero, with slack for the four
/// indices the vector version writes at once.
const GROUP_SLOTS: usize = HALF / 4 + 4;

/// Groups of four inputs that are not all zero, listed without branching since about half
/// of them are zero and a branch on each would mostly mispredict.
fn nonzero_groups(input: &[u8; HALF], groups: &mut [u16; GROUP_SLOTS]) -> usize {
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: Neon is part of the aarch64 baseline.
        unsafe { simd::nonzero_groups_neon(input, groups) }
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        let mut count = 0;
        for (group, chunk) in input.as_chunks::<4>().0.iter().enumerate() {
            groups[count] = group as u16;
            count += usize::from(u32::from_ne_bytes(*chunk) != 0);
        }
        count
    }
}

/// The first layer's sums for inputs of 0 to 127, visiting only the groups of four
/// inputs that are not all zero, which after the pairwise products are about half.
fn first_layer(input: &[u8; HALF], weights: &[i8]) -> [i32; FC0_OUTPUTS] {
    #[cfg(target_arch = "aarch64")]
    {
        if std::arch::is_aarch64_feature_detected!("dotprod") {
            // SAFETY: the dot-product extension was just detected.
            return unsafe { simd::first_layer_neon(input, weights) };
        }
    }
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") {
            // SAFETY: AVX2 was just detected.
            return unsafe { simd::first_layer_avx2(input, weights) };
        }
    }
    first_layer_scalar(input, weights)
}

fn first_layer_scalar(input: &[u8; HALF], weights: &[i8]) -> [i32; FC0_OUTPUTS] {
    let mut out = [0; FC0_OUTPUTS];
    let mut groups = [0; GROUP_SLOTS];
    let count = nonzero_groups(input, &mut groups);
    for &group in &groups[..count] {
        let group = usize::from(group);
        let chunk = &input.as_chunks::<4>().0[group];
        let rows = &weights[group * 4 * FC0_OUTPUTS..(group + 1) * 4 * FC0_OUTPUTS];
        for (out, row) in out.iter_mut().zip(rows.as_chunks::<4>().0) {
            *out += chunk
                .iter()
                .zip(row)
                .map(|(&input, &weight)| i32::from(input) * i32::from(weight))
                .sum::<i32>();
        }
    }
    out
}

/// Dot products of clipped activations (0 to 127) with each of the `ROWS` rows of
/// signed weights stored one after another. Input lengths are multiples of 32 and row
/// counts multiples of 4, which every layer of this network has.
fn products<const ROWS: usize>(input: &[u8], weights: &[i8]) -> [i32; ROWS] {
    debug_assert!(
        weights.len() == ROWS * input.len()
            && input.len().is_multiple_of(32)
            && ROWS.is_multiple_of(4)
    );
    #[cfg(target_arch = "aarch64")]
    {
        if std::arch::is_aarch64_feature_detected!("dotprod") {
            // SAFETY: the dot-product extension was just detected and the lengths match.
            return unsafe { simd::products_neon(input, weights) };
        }
        products_scalar(input, weights)
    }
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") {
            // SAFETY: AVX2 was just detected and the lengths match.
            return unsafe { simd::products_avx2(input, weights) };
        }
        products_scalar(input, weights)
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    {
        products_scalar(input, weights)
    }
}

fn products_scalar<const ROWS: usize>(input: &[u8], weights: &[i8]) -> [i32; ROWS] {
    std::array::from_fn(|row| {
        input
            .iter()
            .zip(&weights[row * input.len()..(row + 1) * input.len()])
            .map(|(&input, &weight)| i32::from(input) * i32::from(weight))
            .sum()
    })
}

mod simd {
    use super::{Rows, FC0_OUTPUTS, GROUP_SLOTS, HALF};

    #[cfg(target_arch = "aarch64")]
    pub unsafe fn pairwise_neon(
        low: &[i16; HALF / 2],
        high: &[i16; HALF / 2],
        out: &mut [u8; HALF / 2],
    ) {
        use std::arch::aarch64::*;
        let zero = vdupq_n_s16(0);
        let ceiling = vdupq_n_s16(255);
        let clip =
            |values: int16x8_t| vreinterpretq_u16_s16(vminq_s16(vmaxq_s16(values, zero), ceiling));
        for start in (0..HALF / 2).step_by(16) {
            let mut bytes = [vdup_n_u8(0); 2];
            for (index, half) in bytes.iter_mut().enumerate() {
                let offset = start + index * 8;
                let product = vmulq_u16(
                    clip(vld1q_s16(low.as_ptr().add(offset))),
                    clip(vld1q_s16(high.as_ptr().add(offset))),
                );
                *half = vmovn_u16(vshrq_n_u16::<9>(product));
            }
            vst1q_u8(out.as_mut_ptr().add(start), vcombine_u8(bytes[0], bytes[1]));
        }
    }

    /// Four groups at a time: a mask of the nonzero ones selects their offsets from a
    /// table, all four slots are written and the count advances by the mask's weight.
    #[cfg(target_arch = "aarch64")]
    pub unsafe fn nonzero_groups_neon(
        input: &[u8; HALF],
        groups: &mut [u16; GROUP_SLOTS],
    ) -> usize {
        use std::arch::aarch64::*;
        const OFFSETS: [[u16; 4]; 16] = {
            let mut table = [[0; 4]; 16];
            let mut mask = 0;
            while mask < 16 {
                let mut slot = 0;
                let mut bit = 0;
                while bit < 4 {
                    if mask & (1 << bit) != 0 {
                        table[mask][slot] = bit as u16;
                        slot += 1;
                    }
                    bit += 1;
                }
                mask += 1;
            }
            table
        };
        let weights = vld1q_u32([1, 2, 4, 8].as_ptr());
        let mut count = 0;
        for base in (0..HALF / 4).step_by(4) {
            let words = vld1q_u32(input.as_ptr().add(base * 4).cast());
            let mask = vaddvq_u32(vandq_u32(vtstq_u32(words, words), weights)) as usize;
            let offsets = vld1_u16(OFFSETS[mask].as_ptr());
            vst1_u16(
                groups.as_mut_ptr().add(count),
                vadd_u16(offsets, vdup_n_u16(base as u16)),
            );
            count += mask.count_ones() as usize;
        }
        count
    }

    #[cfg(target_arch = "aarch64")]
    #[target_feature(enable = "dotprod")]
    pub unsafe fn first_layer_neon(input: &[u8; HALF], weights: &[i8]) -> [i32; FC0_OUTPUTS] {
        use std::arch::aarch64::*;
        let mut sums = [vdupq_n_s32(0); FC0_OUTPUTS / 4];
        let mut groups = [0; GROUP_SLOTS];
        let count = super::nonzero_groups(input, &mut groups);
        let chunks = input.as_chunks::<4>().0;
        for &group in &groups[..count] {
            let group = usize::from(group);
            let word = u32::from_ne_bytes(chunks[group]);
            // Activations never exceed 127, so they are also valid signed bytes.
            let input = vreinterpretq_s8_u32(vdupq_n_u32(word));
            let rows = weights[group * 4 * FC0_OUTPUTS..].as_ptr();
            for (index, sum) in sums.iter_mut().enumerate() {
                *sum = vdotq_s32(*sum, input, vld1q_s8(rows.add(index * 16)));
            }
        }
        let mut out = [0; FC0_OUTPUTS];
        for (out, sum) in out.as_chunks_mut::<4>().0.iter_mut().zip(sums) {
            vst1q_s32(out.as_mut_ptr(), sum);
        }
        out
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    pub unsafe fn first_layer_avx2(input: &[u8; HALF], weights: &[i8]) -> [i32; FC0_OUTPUTS] {
        use std::arch::x86_64::*;
        let ones = _mm256_set1_epi16(1);
        let mut sums = [_mm256_setzero_si256(); FC0_OUTPUTS / 8];
        let mut groups = [0; GROUP_SLOTS];
        let count = super::nonzero_groups(input, &mut groups);
        let chunks = input.as_chunks::<4>().0;
        for &group in &groups[..count] {
            let group = usize::from(group);
            let word = u32::from_ne_bytes(chunks[group]);
            let input = _mm256_set1_epi32(word as i32);
            let rows = weights[group * 4 * FC0_OUTPUTS..].as_ptr();
            for (index, sum) in sums.iter_mut().enumerate() {
                let weights = _mm256_loadu_si256(rows.add(index * 32).cast());
                // Pair sums stay within 2 * 127 * 128, so the saturating multiply-add is
                // exact.
                let pairs = _mm256_maddubs_epi16(input, weights);
                *sum = _mm256_add_epi32(*sum, _mm256_madd_epi16(pairs, ones));
            }
        }
        let mut out = [0; FC0_OUTPUTS];
        for (out, sum) in out.as_chunks_mut::<8>().0.iter_mut().zip(sums) {
            _mm256_storeu_si256(out.as_mut_ptr().cast(), sum);
        }
        out
    }

    /// Sixteen registers of eight lanes hold a 128-lane tile while every row is applied;
    /// byte rows are widened as they are added.
    #[cfg(target_arch = "aarch64")]
    pub unsafe fn update_neon(values: &mut [i16; HALF], parent: &[i16; HALF], rows: &Rows) {
        use std::arch::aarch64::*;
        const LANES: usize = 128;
        for start in (0..HALF).step_by(LANES) {
            let mut tile: [int16x8_t; LANES / 8] =
                std::array::from_fn(|index| vld1q_s16(parent.as_ptr().add(start + index * 8)));
            for &feature in rows.sub {
                let row = rows.row(feature, start, LANES).as_ptr();
                for (index, lanes) in tile.iter_mut().enumerate() {
                    *lanes = vsubq_s16(*lanes, vld1q_s16(row.add(index * 8)));
                }
            }
            for &feature in rows.add {
                let row = rows.row(feature, start, LANES).as_ptr();
                for (index, lanes) in tile.iter_mut().enumerate() {
                    *lanes = vaddq_s16(*lanes, vld1q_s16(row.add(index * 8)));
                }
            }
            for &feature in rows.sub_extra {
                let row = rows.extra_row(feature, start, LANES).as_ptr();
                for (index, pair) in tile.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                    let bytes = vld1q_s8(row.add(index * 16));
                    pair[0] = vsubw_s8(pair[0], vget_low_s8(bytes));
                    pair[1] = vsubw_high_s8(pair[1], bytes);
                }
            }
            for &feature in rows.add_extra {
                let row = rows.extra_row(feature, start, LANES).as_ptr();
                for (index, pair) in tile.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                    let bytes = vld1q_s8(row.add(index * 16));
                    pair[0] = vaddw_s8(pair[0], vget_low_s8(bytes));
                    pair[1] = vaddw_high_s8(pair[1], bytes);
                }
            }
            for (index, lanes) in tile.into_iter().enumerate() {
                vst1q_s16(values.as_mut_ptr().add(start + index * 8), lanes);
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    pub unsafe fn update_avx2(values: &mut [i16; HALF], parent: &[i16; HALF], rows: &Rows) {
        super::update_plain(values, parent, rows);
    }

    /// Rows go four at a time so that each input load serves four independent sums.
    #[cfg(target_arch = "aarch64")]
    #[target_feature(enable = "dotprod")]
    pub unsafe fn products_neon<const ROWS: usize>(input: &[u8], weights: &[i8]) -> [i32; ROWS] {
        use std::arch::aarch64::*;
        let mut out = [0; ROWS];
        let chunks = input.as_chunks::<16>().0;
        for (group, out) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let rows: [&[i8]; 4] = std::array::from_fn(|index| {
                let row = group * 4 + index;
                &weights[row * input.len()..(row + 1) * input.len()]
            });
            let mut sums = [vdupq_n_s32(0); 4];
            for (index, chunk) in chunks.iter().enumerate() {
                // Activations never exceed 127, so they are also valid signed bytes.
                let input = vreinterpretq_s8_u8(vld1q_u8(chunk.as_ptr()));
                for (sum, row) in sums.iter_mut().zip(rows) {
                    *sum = vdotq_s32(*sum, input, vld1q_s8(row[index * 16..].as_ptr()));
                }
            }
            for (out, sum) in out.iter_mut().zip(sums) {
                *out = vaddvq_s32(sum);
            }
        }
        out
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    pub unsafe fn products_avx2<const ROWS: usize>(input: &[u8], weights: &[i8]) -> [i32; ROWS] {
        use std::arch::x86_64::*;
        let ones = _mm256_set1_epi16(1);
        let mut out = [0; ROWS];
        let chunks = input.as_chunks::<32>().0;
        for (group, out) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let rows: [&[i8]; 4] = std::array::from_fn(|index| {
                let row = group * 4 + index;
                &weights[row * input.len()..(row + 1) * input.len()]
            });
            let mut sums = [_mm256_setzero_si256(); 4];
            for (index, chunk) in chunks.iter().enumerate() {
                let input = _mm256_loadu_si256(chunk.as_ptr().cast());
                for (sum, row) in sums.iter_mut().zip(rows) {
                    let weights = _mm256_loadu_si256(row[index * 32..].as_ptr().cast());
                    // Pair sums stay within 2 * 127 * 128, so the saturating multiply-add
                    // is exact.
                    let pairs = _mm256_maddubs_epi16(input, weights);
                    *sum = _mm256_add_epi32(*sum, _mm256_madd_epi16(pairs, ones));
                }
            }
            for (out, sum) in out.iter_mut().zip(sums) {
                let halves = _mm_add_epi32(
                    _mm256_castsi256_si128(sum),
                    _mm256_extracti128_si256(sum, 1),
                );
                let pairs = _mm_add_epi32(halves, _mm_shuffle_epi32(halves, 0b01_00_11_10));
                let total = _mm_add_epi32(pairs, _mm_shuffle_epi32(pairs, 0b10_11_00_01));
                *out = _mm_cvtsi128_si32(total);
            }
        }
        out
    }
}

/// HalfKAv2_hm feature index. The perspective's king chooses one of 32 buckets by its
/// rank and its distance from the nearer edge file; the board is mirrored so that the
/// king stands on files e to h and rotated for Black. Pieces count as the perspective's
/// own or the enemy's, and both kings share one kind.
pub fn feature(perspective: Color, king: Square, piece: Piece, square: Square) -> usize {
    let (file, rank) = (king.index() % 8, king.index() / 8);
    let (rank_flip, relative_rank) = if perspective == Color::White {
        (0, rank)
    } else {
        (56, 7 - rank)
    };
    let (file_flip, edge_distance) = if file < 4 { (7, file) } else { (0, 7 - file) };
    let bucket = (7 - relative_rank) * 4 + edge_distance;
    let kind = if piece.kind == PieceType::King {
        640
    } else {
        piece.kind.index() * 128 + 64 * usize::from(piece.color != perspective)
    };
    (square.index() ^ rank_flip ^ file_flip) + kind + PIECE_SQUARES * bucket
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_first_layer_matches_dense() {
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..50 {
            // Mostly zero inputs, as after the pairwise products.
            let input: [u8; HALF] = std::array::from_fn(|_| {
                if next() % 3 == 0 {
                    (next() % 128) as u8
                } else {
                    0
                }
            });
            let rows: Vec<i8> = (0..FC0_OUTPUTS * HALF).map(|_| next() as i8).collect();
            let grouped: Vec<i8> = (0..FC0_OUTPUTS * HALF)
                .map(|index| {
                    let (group, rest) = (index / (4 * FC0_OUTPUTS), index % (4 * FC0_OUTPUTS));
                    rows[(rest / 4) * HALF + group * 4 + rest % 4]
                })
                .collect();
            let dense = products_scalar::<FC0_OUTPUTS>(&input, &rows);
            assert_eq!(first_layer(&input, &grouped), dense);
            assert_eq!(first_layer_scalar(&input, &grouped), dense);
        }
    }

    #[test]
    fn vector_products_match_scalar() {
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for length in [32, 1024] {
            for _ in 0..50 {
                let input: Vec<u8> = (0..length).map(|_| (next() % 128) as u8).collect();
                let weights: Vec<i8> = (0..length * 32).map(|_| next() as i8).collect();
                assert_eq!(
                    products::<32>(&input, &weights),
                    products_scalar::<32>(&input, &weights)
                );
            }
            let input = vec![127_u8; length];
            for extreme in [i8::MIN, i8::MAX] {
                let weights = vec![extreme; length * 16];
                assert_eq!(
                    products::<16>(&input, &weights),
                    products_scalar::<16>(&input, &weights)
                );
            }
        }
    }
}
