//! Inference for Stockfish 13's HalfKP 256x2-32-32 network `nn-62ef826d1a6d`, bundled in
//! the binary. The arithmetic follows Stockfish 13 exactly, so for any position the value
//! equals what Stockfish 13 computes with the same network.

use std::sync::OnceLock;

use inphzugzwang_core::{Color, Move, MoveFlag, Piece, PieceType, Position, Square};

/// Width of one perspective's accumulator.
pub const HALF: usize = 256;
/// Piece-square features per king square: one unused slot, then ten piece kinds by 64.
const PIECE_SQUARES: usize = 641;
const INPUTS: usize = 64 * PIECE_SQUARES;
const HIDDEN: usize = 32;
const VERSION: u32 = 0x7AF3_2F16;
const WEIGHT_SCALE_BITS: u32 = 6;
const OUTPUT_SCALE: i32 = 16;

const NET: &[u8] = include_bytes!("../net/nn-62ef826d1a6d.nnue");

pub struct Network {
    feature_bias: Box<[i16]>,
    feature_weights: Box<[i16]>,
    hidden1_bias: [i32; HIDDEN],
    hidden1_weights: Box<[i8]>,
    hidden2_bias: [i32; HIDDEN],
    hidden2_weights: Box<[i8]>,
    output_bias: i32,
    output_weights: [i8; HIDDEN],
}

/// Feature-transformer sums for both perspectives, indexed by colour.
#[derive(Clone)]
pub struct Accumulator {
    pub values: [[i16; HALF]; 2],
}

impl Default for Accumulator {
    fn default() -> Self {
        Self {
            values: [[0; HALF]; 2],
        }
    }
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
        if piece.kind != PieceType::King {
            self.removed[self.removed_len] = (piece, square);
            self.removed_len += 1;
        }
    }

    fn add(&mut self, piece: Piece, square: Square) {
        if piece.kind != PieceType::King {
            self.added[self.added_len] = (piece, square);
            self.added_len += 1;
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
    };
    if mv.is_castle() {
        let kingside = mv.flag() == 2;
        let rook = Piece {
            color: side,
            kind: PieceType::Rook,
        };
        delta.remove(rook, mv.to());
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

    fn i32s<const N: usize>(&mut self) -> Result<[i32; N], &'static str> {
        let bytes = self.take(N * 4)?;
        Ok(std::array::from_fn(|index| {
            i32::from_le_bytes(
                bytes[index * 4..index * 4 + 4]
                    .try_into()
                    .expect("four bytes"),
            )
        }))
    }

    fn i8s(&mut self, count: usize) -> Result<Box<[i8]>, &'static str> {
        Ok(self.take(count)?.iter().map(|&byte| byte as i8).collect())
    }
}

impl Network {
    /// Reads the Stockfish 13 file format: a version, a hash and a description, then the
    /// feature transformer and the three affine layers, each behind its own hash.
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
        reader.u32()?;
        let hidden1_bias = reader.i32s::<HIDDEN>()?;
        let hidden1_weights = reader.i8s(HIDDEN * 2 * HALF)?;
        let hidden2_bias = reader.i32s::<HIDDEN>()?;
        let hidden2_weights = reader.i8s(HIDDEN * HIDDEN)?;
        let [output_bias] = reader.i32s::<1>()?;
        let output_weights = reader.i8s(HIDDEN)?;
        if !reader.bytes.is_empty() {
            return Err("network file has trailing bytes");
        }
        Ok(Self {
            feature_bias,
            feature_weights,
            hidden1_bias,
            hidden1_weights,
            hidden2_bias,
            hidden2_weights,
            output_bias,
            output_weights: output_weights[..].try_into().expect("32 weights"),
        })
    }

    /// Recomputes one perspective's accumulator from the board.
    pub fn refresh(&self, position: &Position, perspective: Color, values: &mut [i16; HALF]) {
        values.copy_from_slice(&self.feature_bias);
        let king = position.king(perspective);
        for color in [Color::White, Color::Black] {
            for kind in [
                PieceType::Pawn,
                PieceType::Knight,
                PieceType::Bishop,
                PieceType::Rook,
                PieceType::Queen,
            ] {
                for square in position.pieces(color, kind) {
                    let feature = feature(perspective, king, Piece { color, kind }, square);
                    self.add(values, feature);
                }
            }
        }
    }

    pub fn add(&self, values: &mut [i16; HALF], feature: usize) {
        let row = &self.feature_weights[feature * HALF..(feature + 1) * HALF];
        for (value, &weight) in values.iter_mut().zip(row) {
            *value = value.wrapping_add(weight);
        }
    }

    pub fn sub(&self, values: &mut [i16; HALF], feature: usize) {
        let row = &self.feature_weights[feature * HALF..(feature + 1) * HALF];
        for (value, &weight) in values.iter_mut().zip(row) {
            *value = value.wrapping_sub(weight);
        }
    }

    /// Derives the accumulator after a move from the one before it; `after` is the
    /// position once the move is made.
    pub fn apply(
        &self,
        parent: &Accumulator,
        child: &mut Accumulator,
        delta: &Delta,
        after: &Position,
    ) {
        for perspective in [Color::White, Color::Black] {
            let values = &mut child.values[perspective.index()];
            if delta.king_moved == Some(perspective) {
                self.refresh(after, perspective, values);
                continue;
            }
            let king = after.king(perspective);
            let row = |&(piece, square): &(Piece, Square)| {
                let feature = feature(perspective, king, piece, square);
                &self.feature_weights[feature * HALF..(feature + 1) * HALF]
            };
            let parent = &parent.values[perspective.index()];
            let removed = &delta.removed[..delta.removed_len];
            let added = &delta.added[..delta.added_len];
            // One pass over the accumulator for the common shapes: a quiet move removes
            // and adds one feature, a capture removes two and adds one.
            match (removed, added) {
                ([gone], [new]) => {
                    let (gone, new) = (row(gone), row(new));
                    for index in 0..HALF {
                        values[index] = parent[index]
                            .wrapping_sub(gone[index])
                            .wrapping_add(new[index]);
                    }
                }
                ([first, second], [new]) => {
                    let (first, second, new) = (row(first), row(second), row(new));
                    for index in 0..HALF {
                        values[index] = parent[index]
                            .wrapping_sub(first[index])
                            .wrapping_sub(second[index])
                            .wrapping_add(new[index]);
                    }
                }
                _ => {
                    *values = *parent;
                    for gone in removed {
                        for (value, &weight) in values.iter_mut().zip(row(gone)) {
                            *value = value.wrapping_sub(weight);
                        }
                    }
                    for new in added {
                        for (value, &weight) in values.iter_mut().zip(row(new)) {
                            *value = value.wrapping_add(weight);
                        }
                    }
                }
            }
        }
    }

    pub fn fresh(&self, position: &Position) -> Accumulator {
        let mut accumulator = Accumulator::default();
        for perspective in [Color::White, Color::Black] {
            self.refresh(
                position,
                perspective,
                &mut accumulator.values[perspective.index()],
            );
        }
        accumulator
    }

    /// Network output for the side to move, in Stockfish 13's internal units.
    pub fn evaluate(&self, accumulator: &Accumulator, side: Color) -> i32 {
        let mut input = [0_u8; 2 * HALF];
        for (half, perspective) in [side, side.other()].into_iter().enumerate() {
            for (slot, &value) in input[half * HALF..(half + 1) * HALF]
                .iter_mut()
                .zip(&accumulator.values[perspective.index()])
            {
                *slot = value.clamp(0, 127) as u8;
            }
        }
        let hidden1 = affine_relu(&input, &self.hidden1_weights, &self.hidden1_bias);
        let hidden2 = affine_relu(&hidden1, &self.hidden2_weights, &self.hidden2_bias);
        let output = self.output_bias
            + hidden2
                .iter()
                .zip(&self.output_weights)
                .map(|(&input, &weight)| i32::from(input) * i32::from(weight))
                .sum::<i32>();
        output / OUTPUT_SCALE
    }

    /// Evaluates a position from scratch.
    pub fn evaluate_position(&self, position: &Position) -> i32 {
        self.evaluate(&self.fresh(position), position.side_to_move())
    }
}

/// An affine layer followed by Stockfish's clipped ReLU, which rescales by 2^-6.
fn affine_relu(input: &[u8], weights: &[i8], bias: &[i32; HIDDEN]) -> [u8; HIDDEN] {
    std::array::from_fn(|row| {
        let weights = &weights[row * input.len()..(row + 1) * input.len()];
        let sum = bias[row] + dot(input, weights);
        (sum >> WEIGHT_SCALE_BITS).clamp(0, 127) as u8
    })
}

/// Dot product of clipped activations (0 to 127) with signed weights. Lengths are
/// multiples of 32, which every layer of this network has.
fn dot(input: &[u8], weights: &[i8]) -> i32 {
    debug_assert!(input.len() == weights.len() && input.len() % 32 == 0);
    #[cfg(target_arch = "aarch64")]
    {
        if std::arch::is_aarch64_feature_detected!("dotprod") {
            // SAFETY: the dot-product extension was just detected and the lengths match.
            return unsafe { simd::dot_neon(input, weights) };
        }
        dot_scalar(input, weights)
    }
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") {
            // SAFETY: AVX2 was just detected and the lengths match.
            return unsafe { simd::dot_avx2(input, weights) };
        }
        dot_scalar(input, weights)
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    {
        dot_scalar(input, weights)
    }
}

fn dot_scalar(input: &[u8], weights: &[i8]) -> i32 {
    input
        .iter()
        .zip(weights)
        .map(|(&input, &weight)| i32::from(input) * i32::from(weight))
        .sum()
}

mod simd {
    #[cfg(target_arch = "aarch64")]
    #[target_feature(enable = "dotprod")]
    pub unsafe fn dot_neon(input: &[u8], weights: &[i8]) -> i32 {
        use std::arch::aarch64::*;
        let mut sum = vdupq_n_s32(0);
        for (input, weights) in input.chunks_exact(16).zip(weights.chunks_exact(16)) {
            // Activations never exceed 127, so they are also valid signed bytes.
            let input = vreinterpretq_s8_u8(vld1q_u8(input.as_ptr()));
            sum = vdotq_s32(sum, input, vld1q_s8(weights.as_ptr()));
        }
        vaddvq_s32(sum)
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    pub unsafe fn dot_avx2(input: &[u8], weights: &[i8]) -> i32 {
        use std::arch::x86_64::*;
        let ones = _mm256_set1_epi16(1);
        let mut sum = _mm256_setzero_si256();
        for (input, weights) in input.chunks_exact(32).zip(weights.chunks_exact(32)) {
            let input = _mm256_loadu_si256(input.as_ptr().cast());
            let weights = _mm256_loadu_si256(weights.as_ptr().cast());
            // Pair sums stay within 2 * 127 * 128, so the saturating multiply-add is exact.
            let pairs = _mm256_maddubs_epi16(input, weights);
            sum = _mm256_add_epi32(sum, _mm256_madd_epi16(pairs, ones));
        }
        let halves = _mm_add_epi32(
            _mm256_castsi256_si128(sum),
            _mm256_extracti128_si256(sum, 1),
        );
        let pairs = _mm_add_epi32(halves, _mm_shuffle_epi32(halves, 0b01_00_11_10));
        let total = _mm_add_epi32(pairs, _mm_shuffle_epi32(pairs, 0b10_11_00_01));
        _mm_cvtsi128_si32(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_dot_matches_scalar() {
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for length in [32, 512] {
            for _ in 0..200 {
                let input: Vec<u8> = (0..length).map(|_| (next() % 128) as u8).collect();
                let weights: Vec<i8> = (0..length).map(|_| next() as i8).collect();
                assert_eq!(dot(&input, &weights), dot_scalar(&input, &weights));
            }
            let input = vec![127_u8; length];
            for extreme in [i8::MIN, i8::MAX] {
                assert_eq!(
                    dot(&input, &vec![extreme; length]),
                    dot_scalar(&input, &vec![extreme; length])
                );
            }
        }
    }
}

/// HalfKP feature index: the perspective's king square and the piece's square, both
/// rotated by 180 degrees for Black, and the piece kind with its colour relative to the
/// perspective. Kings themselves are not features.
pub fn feature(perspective: Color, king: Square, piece: Piece, square: Square) -> usize {
    let flip = if perspective == Color::White { 0 } else { 63 };
    let enemy = usize::from(piece.color != perspective);
    let piece_offset = 1 + (piece.kind.index() * 2 + enemy) * 64;
    (square.index() ^ flip) + piece_offset + PIECE_SQUARES * (king.index() ^ flip)
}
