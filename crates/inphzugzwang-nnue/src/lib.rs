//! Inference for Stockfish 15.1's HalfKAv2_hm network `nn-ad9b42354671`, bundled in the
//! binary. The arithmetic follows Stockfish 15.1 exactly, so for any position the value
//! equals what Stockfish 15.1 computes with the same network.

use std::sync::OnceLock;

use inphzugzwang_core::{Bitboard, Color, Move, MoveFlag, Piece, PieceType, Position, Square};

/// Width of one perspective's accumulator.
pub const HALF: usize = 1024;
/// Piece-square buckets of the accumulator and layer stacks, chosen by piece count.
const BUCKETS: usize = 8;
/// Features per king bucket: ten piece kinds and the kings, by 64 squares.
const PIECE_SQUARES: usize = 11 * 64;
const INPUTS: usize = 32 * PIECE_SQUARES;
const FC0_OUTPUTS: usize = 16;
/// The first layer's squared and clipped outputs, 30 of them, padded to 32.
const FC1_INPUTS: usize = 32;
const FC1_OUTPUTS: usize = 32;
const VERSION: u32 = 0x7AF3_2F20;
const WEIGHT_SCALE_BITS: u32 = 6;
const OUTPUT_SCALE: i32 = 16;
const PIECE_KINDS: [PieceType; 6] = [
    PieceType::Pawn,
    PieceType::Knight,
    PieceType::Bishop,
    PieceType::Rook,
    PieceType::Queen,
    PieceType::King,
];

const NET: &[u8] = include_bytes!("../net/nn-ad9b42354671.nnue");

/// One of the eight layer stacks after the feature transformer.
struct Stack {
    fc0_bias: [i32; FC0_OUTPUTS],
    fc0_weights: Box<[i8]>,
    fc1_bias: [i32; FC1_OUTPUTS],
    fc1_weights: Box<[i8]>,
    fc2_bias: i32,
    fc2_weights: [i8; FC1_OUTPUTS],
}

pub struct Network {
    feature_bias: Box<[i16]>,
    feature_weights: Box<[i16]>,
    psqt_weights: Box<[i32]>,
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

/// The two parts of the network's output, in Stockfish's internal units times 16.
pub struct Output {
    pub psqt: i32,
    pub positional: i32,
}

impl Output {
    /// Stockfish's `NNUE::evaluate(pos, false)`.
    pub fn raw(&self) -> i32 {
        (self.psqt + self.positional) / OUTPUT_SCALE
    }

    /// Stockfish's `NNUE::evaluate(pos, true)`, which weights the positional part more
    /// as material comes off.
    pub fn adjusted(&self, non_pawn_material: i32) -> i32 {
        let delta = 24 - non_pawn_material / 9560;
        ((1024 - delta) * self.psqt + (1024 + delta) * self.positional) / (1024 * OUTPUT_SCALE)
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

/// Features a move removes and adds, read from the position before the move. A king
/// move changes every feature of its own perspective, which is then refreshed instead.
pub struct Delta {
    removed: [(Piece, Square); 2],
    removed_len: usize,
    added: [(Piece, Square); 2],
    added_len: usize,
    king_moved: Option<Color>,
}

impl Delta {
    fn remove(&mut self, piece: Piece, square: Square) {
        self.removed[self.removed_len] = (piece, square);
        self.removed_len += 1;
    }

    fn add(&mut self, piece: Piece, square: Square) {
        self.added[self.added_len] = (piece, square);
        self.added_len += 1;
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
        return delta;
    }
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
    delta
}

/// Accumulators last computed for each perspective and king square, with the pieces
/// they were computed for. A king move refreshes from the entry for its new square by
/// adding and removing only the pieces that differ, instead of summing every piece.
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

    fn i16s(&mut self, count: usize) -> Result<Box<[i16]>, &'static str> {
        Ok(self
            .take(count * 2)?
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&pair| i16::from_le_bytes(pair))
            .collect())
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
}

impl Network {
    /// Reads the Stockfish 15.1 file format: a version, a hash and a description, then
    /// the feature transformer and eight layer stacks, each behind its own hash.
    pub fn parse(bytes: &[u8]) -> Result<Self, &'static str> {
        let mut reader = Reader { bytes };
        if reader.u32()? != VERSION {
            return Err("unsupported network version");
        }
        reader.u32()?;
        let description = reader.u32()? as usize;
        reader.take(description)?;
        reader.u32()?;
        let feature_bias = reader.i16s(HALF)?;
        let feature_weights = reader.i16s(INPUTS * HALF)?;
        let psqt_weights = reader.i32s(INPUTS * BUCKETS)?;
        let mut stacks = Vec::with_capacity(BUCKETS);
        for _ in 0..BUCKETS {
            reader.u32()?;
            let fc0_bias = reader.i32_array::<FC0_OUTPUTS>()?;
            let fc0_weights = reader.i8s(FC0_OUTPUTS * HALF)?;
            let fc1_bias = reader.i32_array::<FC1_OUTPUTS>()?;
            let fc1_weights = reader.i8s(FC1_OUTPUTS * FC1_INPUTS)?;
            let [fc2_bias] = reader.i32_array::<1>()?;
            let fc2_weights = reader.i8s(FC1_OUTPUTS)?;
            stacks.push(Stack {
                fc0_bias,
                fc0_weights,
                fc1_bias,
                fc1_weights,
                fc2_bias,
                fc2_weights: fc2_weights[..].try_into().expect("32 weights"),
            });
        }
        if !reader.bytes.is_empty() {
            return Err("network file has trailing bytes");
        }
        Ok(Self {
            feature_bias,
            feature_weights,
            psqt_weights,
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

    /// Adds (`sign` 1) or removes (`sign` -1) one feature in place.
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
        for perspective in [Color::White, Color::Black] {
            let side = perspective.index();
            let (values, psqt) = (&mut child.values[side], &mut child.psqt[side]);
            if delta.king_moved == Some(perspective) {
                self.refresh_cached(after, perspective, values, psqt, cache);
                continue;
            }
            let king = after.king(perspective);
            let index =
                |&(piece, square): &(Piece, Square)| feature(perspective, king, piece, square);
            let gone = [
                index(&delta.removed[0]),
                index(&delta.removed[delta.removed_len - 1]),
            ];
            let new = [
                index(&delta.added[0]),
                index(&delta.added[delta.added_len - 1]),
            ];
            let parent_values = &parent.values[side];
            // One pass over the accumulator for each shape: a quiet move removes and adds
            // one feature, a capture removes two, and castling seen by the other side
            // moves two pieces.
            match (delta.removed_len, delta.added_len) {
                (1, 1) => combine(
                    values,
                    parent_values,
                    [self.row(gone[0])],
                    [self.row(new[0])],
                ),
                (2, 1) => combine(
                    values,
                    parent_values,
                    [self.row(gone[0]), self.row(gone[1])],
                    [self.row(new[0])],
                ),
                _ => combine(
                    values,
                    parent_values,
                    [self.row(gone[0]), self.row(gone[1])],
                    [self.row(new[0]), self.row(new[1])],
                ),
            }
            *psqt = parent.psqt[side];
            for &feature in &gone[..delta.removed_len] {
                for (value, &weight) in psqt.iter_mut().zip(self.psqt_row(feature)) {
                    *value -= weight;
                }
            }
            for &feature in &new[..delta.added_len] {
                for (value, &weight) in psqt.iter_mut().zip(self.psqt_row(feature)) {
                    *value += weight;
                }
            }
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
        // Each perspective's two halves are clipped and multiplied pairwise.
        let mut input = [0_u8; HALF];
        for (half, perspective) in [us, them].into_iter().enumerate() {
            let (low, high) = accumulator.values[perspective].split_at(HALF / 2);
            for ((slot, &low), &high) in input[half * HALF / 2..(half + 1) * HALF / 2]
                .iter_mut()
                .zip(low)
                .zip(high)
            {
                let (low, high) = (i32::from(low.clamp(0, 127)), i32::from(high.clamp(0, 127)));
                *slot = (low * high / 128) as u8;
            }
        }
        let stack = &self.stacks[bucket];
        let fc0 = products::<FC0_OUTPUTS>(&input, &stack.fc0_weights);
        let fc0: [i32; FC0_OUTPUTS] = std::array::from_fn(|row| stack.fc0_bias[row] + fc0[row]);
        // The first fifteen outputs enter the next layer both squared and clipped; the
        // sixteenth goes straight to the output.
        let mut hidden = [0_u8; FC1_INPUTS];
        for index in 0..FC0_OUTPUTS - 1 {
            let value = i64::from(fc0[index]);
            hidden[index] = (((value * value) >> (2 * WEIGHT_SCALE_BITS)) / 128).min(127) as u8;
            hidden[FC0_OUTPUTS - 1 + index] = (fc0[index] >> WEIGHT_SCALE_BITS).clamp(0, 127) as u8;
        }
        let fc1 = products::<FC1_OUTPUTS>(&hidden, &stack.fc1_weights);
        let fc1: [u8; FC1_OUTPUTS] = std::array::from_fn(|row| {
            ((stack.fc1_bias[row] + fc1[row]) >> WEIGHT_SCALE_BITS).clamp(0, 127) as u8
        });
        let fc2 = stack.fc2_bias
            + fc1
                .iter()
                .zip(&stack.fc2_weights)
                .map(|(&input, &weight)| i32::from(input) * i32::from(weight))
                .sum::<i32>();
        // The pass-through output is scaled so that 127 << 6, its 1.0, becomes 600 * 16.
        let forward = i64::from(fc0[FC0_OUTPUTS - 1]) * (600 * i64::from(OUTPUT_SCALE))
            / (127 << WEIGHT_SCALE_BITS);
        Output {
            psqt,
            positional: fc2 + forward as i32,
        }
    }

    /// Evaluates a position from scratch.
    pub fn evaluate_position(&self, position: &Position) -> Output {
        self.evaluate(&self.fresh(position), position)
    }
}

/// Writes `parent` less the `removed` rows plus the `added` rows into `values`, in one
/// pass. On x86-64 the same loop is compiled a second time for AVX2 and chosen at run
/// time, since the release builds target the baseline instruction set.
fn combine<const R: usize, const A: usize>(
    values: &mut [i16; HALF],
    parent: &[i16; HALF],
    removed: [&[i16; HALF]; R],
    added: [&[i16; HALF]; A],
) {
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") {
            // SAFETY: AVX2 was just detected.
            return unsafe { simd::combine_avx2(values, parent, removed, added) };
        }
    }
    combine_plain(values, parent, removed, added);
}

#[inline(always)]
fn combine_plain<const R: usize, const A: usize>(
    values: &mut [i16; HALF],
    parent: &[i16; HALF],
    removed: [&[i16; HALF]; R],
    added: [&[i16; HALF]; A],
) {
    for index in 0..HALF {
        let mut value = parent[index];
        for row in removed {
            value = value.wrapping_sub(row[index]);
        }
        for row in added {
            value = value.wrapping_add(row[index]);
        }
        values[index] = value;
    }
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
    #[cfg(target_arch = "x86_64")]
    use super::HALF;

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    pub unsafe fn combine_avx2<const R: usize, const A: usize>(
        values: &mut [i16; HALF],
        parent: &[i16; HALF],
        removed: [&[i16; HALF]; R],
        added: [&[i16; HALF]; A],
    ) {
        super::combine_plain(values, parent, removed, added);
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
