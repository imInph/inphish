//! Inference for Stockfish 13's HalfKP 256x2-32-32 network `nn-62ef826d1a6d`, bundled in
//! the binary. The arithmetic follows Stockfish 13 exactly, so for any position the value
//! equals what Stockfish 13 computes with the same network.

use std::sync::OnceLock;

use inphzugzwang_core::{Color, Piece, PieceType, Position, Square};

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
        let mut accumulator = Accumulator::default();
        for perspective in [Color::White, Color::Black] {
            self.refresh(
                position,
                perspective,
                &mut accumulator.values[perspective.index()],
            );
        }
        self.evaluate(&accumulator, position.side_to_move())
    }
}

/// An affine layer followed by Stockfish's clipped ReLU, which rescales by 2^-6.
fn affine_relu(input: &[u8], weights: &[i8], bias: &[i32; HIDDEN]) -> [u8; HIDDEN] {
    std::array::from_fn(|row| {
        let weights = &weights[row * input.len()..(row + 1) * input.len()];
        let sum = bias[row]
            + input
                .iter()
                .zip(weights)
                .map(|(&input, &weight)| i32::from(input) * i32::from(weight))
                .sum::<i32>();
        (sum >> WEIGHT_SCALE_BITS).clamp(0, 127) as u8
    })
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
