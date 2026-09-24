use inphzugzwang_core::{
    bishop_attacks, king_attacks, knight_attacks, pawn_attacks, rook_attacks, Bitboard, Color,
    PieceType, Position, Square,
};

mod weights;

use weights::WEIGHTS;

/// Rough exchange values used by move ordering and pruning margins, not by the evaluation.
pub const VALUES: [i32; 6] = [100, 320, 330, 500, 900, 0];

const PHASE_WEIGHTS: [i32; 6] = [0, 1, 1, 2, 4, 0];
pub const MAX_PHASE: i32 = 24;
pub const TEMPO: i32 = 10;

// Weight indices. Piece-square entries start from Ronald Friederich's PeSTO tables, published
// on the Chess Programming Wiki and laid out from a8 to h1, so a white piece on square s
// reads entry s ^ 56 and a black piece reads entry s.
const MATERIAL: usize = 0;
const PST: usize = MATERIAL + 6;
// Indexed by relative rank; the piece-square tables already reward advancement, so these
// carry only the part that depends on no enemy pawn being able to stop the pawn.
const PASSED: usize = PST + 6 * 64;
const DOUBLED: usize = PASSED + 8;
const ISOLATED: usize = DOUBLED + 1;
const BISHOP_PAIR: usize = ISOLATED + 1;
const ROOK_OPEN_FILE: usize = BISHOP_PAIR + 1;
const ROOK_SEMI_OPEN_FILE: usize = ROOK_OPEN_FILE + 1;
const MOBILITY: usize = ROOK_SEMI_OPEN_FILE + 1;
pub const KING_ZONE: usize = MOBILITY + 4;
const SHIELD_PAWN: usize = KING_ZONE + 6;
const THREAT_BY_PAWN: usize = SHIELD_PAWN + 1;
const THREAT_BY_MINOR: usize = THREAT_BY_PAWN + 1;
const THREAT_BY_ROOK: usize = THREAT_BY_MINOR + 1;
const HANGING: usize = THREAT_BY_ROOK + 1;
const KNIGHT_OUTPOST: usize = HANGING + 1;
const ROOK_SEVENTH: usize = KNIGHT_OUTPOST + 1;
const PASSED_KING_DISTANCE: usize = ROOK_SEVENTH + 1;
pub const PARAMS: usize = PASSED_KING_DISTANCE + 1;

// Mobility counts reachable squares beyond a typical number, so an average piece is neutral.
const TYPICAL_MOBILITY: [i32; 4] = [4, 6, 6, 12];

const FILE_A: u64 = 0x0101_0101_0101_0101;
const PIECES: [PieceType; 6] = [
    PieceType::Pawn,
    PieceType::Knight,
    PieceType::Bishop,
    PieceType::Rook,
    PieceType::Queen,
    PieceType::King,
];

/// Receives each evaluation feature for one side. The engine sums weights directly; the
/// tuner records the features so it can refit the weights.
trait Sink {
    fn add(&mut self, index: usize, count: i32);
    fn king_attack(&mut self, by_kind: [i32; 6], attackers: i32);
}

#[derive(Clone, Copy, Default)]
struct Score {
    mg: i32,
    eg: i32,
}

impl Sink for Score {
    fn add(&mut self, index: usize, count: i32) {
        self.mg += WEIGHTS[index].0 * count;
        self.eg += WEIGHTS[index].1 * count;
    }

    fn king_attack(&mut self, by_kind: [i32; 6], attackers: i32) {
        let (mg, eg) = king_attack_weight(by_kind, |index| WEIGHTS[index]);
        self.mg += mg * attackers / 4;
        self.eg += eg * attackers / 4;
    }
}

fn king_attack_weight(by_kind: [i32; 6], weight: impl Fn(usize) -> (i32, i32)) -> (i32, i32) {
    by_kind
        .iter()
        .enumerate()
        .fold((0, 0), |(mg, eg), (kind, &count)| {
            let (w_mg, w_eg) = weight(KING_ZONE + kind);
            (mg + w_mg * count, eg + w_eg * count)
        })
}

#[derive(Default)]
struct Attacks {
    pawns: u64,
    minors: u64,
    rooks: u64,
    all: u64,
}

fn file_mask(file: u8) -> u64 {
    FILE_A << file
}

fn adjacent_files(file: u8) -> u64 {
    let mut mask = 0;
    if file > 0 {
        mask |= file_mask(file - 1);
    }
    if file < 7 {
        mask |= file_mask(file + 1);
    }
    mask
}

/// Squares strictly in front of `square` from `color`'s point of view on the given files.
fn forward_span(color: Color, square: Square, files: u64) -> u64 {
    let rank = square.rank();
    let ahead = match color {
        Color::White if rank < 7 => u64::MAX << (8 * (rank + 1)),
        Color::Black if rank > 0 => u64::MAX >> (8 * (8 - rank)),
        _ => 0,
    };
    ahead & files
}

fn relative_rank(color: Color, square: Square) -> u8 {
    match color {
        Color::White => square.rank(),
        Color::Black => 7 - square.rank(),
    }
}

fn distance(a: Square, b: Square) -> i32 {
    i32::from(a.file().abs_diff(b.file()).max(a.rank().abs_diff(b.rank())))
}

fn pawn_attack_span(color: Color, pawns: Bitboard) -> u64 {
    pawns
        .into_iter()
        .fold(0, |acc, square| acc | pawn_attacks(color, square).0)
}

fn evaluate_side(position: &Position, color: Color, sink: &mut impl Sink) -> Attacks {
    let enemy = color.other();
    let occupied = position.occupied();
    let own_pieces = position.side_pieces(color);
    let own_pawns = position.pieces(color, PieceType::Pawn);
    let enemy_pawns = position.pieces(enemy, PieceType::Pawn);
    let enemy_pawn_attacks = pawn_attack_span(enemy, enemy_pawns);
    let own_king = position.king(color);
    let enemy_king = position.king(enemy);
    let enemy_king_zone = king_attacks(enemy_king).0 | enemy_king.bit().0;
    let mut attacks = Attacks {
        pawns: pawn_attack_span(color, own_pawns),
        ..Attacks::default()
    };
    attacks.all = attacks.pawns | king_attacks(own_king).0;
    let mut king_attackers_by_kind = [0; 6];
    let mut king_attackers = 0;

    for kind in PIECES {
        for square in position.pieces(color, kind) {
            let table_index = match color {
                Color::White => square.index() ^ 56,
                Color::Black => square.index(),
            };
            sink.add(MATERIAL + kind.index(), 1);
            sink.add(PST + kind.index() * 64 + table_index, 1);
            let reach = match kind {
                PieceType::Knight => knight_attacks(square),
                PieceType::Bishop => bishop_attacks(square, occupied),
                PieceType::Rook => rook_attacks(square, occupied),
                PieceType::Queen => {
                    bishop_attacks(square, occupied) | rook_attacks(square, occupied)
                }
                PieceType::Pawn | PieceType::King => continue,
            };
            attacks.all |= reach.0;
            match kind {
                PieceType::Knight | PieceType::Bishop => attacks.minors |= reach.0,
                PieceType::Rook => attacks.rooks |= reach.0,
                _ => {}
            }
            let reachable = (reach.0 & !own_pieces.0 & !enemy_pawn_attacks).count_ones() as i32;
            sink.add(
                MOBILITY + kind.index() - 1,
                reachable - TYPICAL_MOBILITY[kind.index() - 1],
            );
            if reach.0 & enemy_king_zone != 0 {
                king_attackers += 1;
                king_attackers_by_kind[kind.index()] += 1;
            }
            match kind {
                PieceType::Rook => {
                    let file = file_mask(square.file());
                    if file & own_pawns.0 == 0 {
                        sink.add(
                            if file & enemy_pawns.0 == 0 {
                                ROOK_OPEN_FILE
                            } else {
                                ROOK_SEMI_OPEN_FILE
                            },
                            1,
                        );
                    }
                    if relative_rank(color, square) == 6 {
                        sink.add(ROOK_SEVENTH, 1);
                    }
                }
                PieceType::Knight => {
                    let rank = relative_rank(color, square);
                    let safe = forward_span(color, square, adjacent_files(square.file()))
                        & enemy_pawns.0
                        == 0;
                    if (3..=5).contains(&rank) && safe && attacks.pawns & square.bit().0 != 0 {
                        sink.add(KNIGHT_OUTPOST, 1);
                    }
                }
                _ => {}
            }
        }
    }

    // A single attacker rarely threatens mate, so king pressure only counts once two
    // pieces are aimed at the zone, and grows with the number of attackers.
    if king_attackers >= 2 {
        sink.king_attack(king_attackers_by_kind, king_attackers);
    }

    for square in own_pawns {
        let file = square.file();
        if own_pawns.0 & file_mask(file) & !square.bit().0 != 0
            && forward_span(color, square, file_mask(file)) & own_pawns.0 != 0
        {
            sink.add(DOUBLED, 1);
        }
        if own_pawns.0 & adjacent_files(file) == 0 {
            sink.add(ISOLATED, 1);
        }
        let stoppers = forward_span(color, square, file_mask(file) | adjacent_files(file));
        if stoppers & enemy_pawns.0 == 0 {
            let rank = relative_rank(color, square);
            sink.add(PASSED + rank as usize, 1);
            if rank < 7 {
                let stop = Square::new(
                    file,
                    match color {
                        Color::White => square.rank() + 1,
                        Color::Black => square.rank() - 1,
                    },
                );
                sink.add(
                    PASSED_KING_DISTANCE,
                    distance(enemy_king, stop) - distance(own_king, stop),
                );
            }
        }
    }

    if position.pieces(color, PieceType::Bishop).count() >= 2 {
        sink.add(BISHOP_PAIR, 1);
    }

    let shield_files = file_mask(own_king.file()) | adjacent_files(own_king.file());
    let near_ranks = (1..=2)
        .filter_map(|step| match color {
            Color::White => own_king.rank().checked_add(step).filter(|&rank| rank < 8),
            Color::Black => own_king.rank().checked_sub(step),
        })
        .fold(0, |mask, rank| mask | (0xFF_u64 << (8 * rank)));
    let shield = (shield_files & near_ranks & own_pawns.0)
        .count_ones()
        .min(3) as i32;
    sink.add(SHIELD_PAWN, shield);
    attacks
}

fn evaluate_threats(
    position: &Position,
    color: Color,
    own: &Attacks,
    enemy: &Attacks,
    sink: &mut impl Sink,
) {
    let them = color.other();
    let kind = |kind: PieceType| position.pieces(them, kind).0;
    let minors = kind(PieceType::Knight) | kind(PieceType::Bishop);
    let majors = kind(PieceType::Rook) | kind(PieceType::Queen);
    let pieces = minors | majors;
    sink.add(THREAT_BY_PAWN, (own.pawns & pieces).count_ones() as i32);
    sink.add(THREAT_BY_MINOR, (own.minors & majors).count_ones() as i32);
    sink.add(
        THREAT_BY_ROOK,
        (own.rooks & kind(PieceType::Queen)).count_ones() as i32,
    );
    sink.add(HANGING, (own.all & pieces & !enemy.all).count_ones() as i32);
}

fn phase(position: &Position) -> i32 {
    let mut phase = 0;
    for color in [Color::White, Color::Black] {
        for kind in [
            PieceType::Knight,
            PieceType::Bishop,
            PieceType::Rook,
            PieceType::Queen,
        ] {
            phase += PHASE_WEIGHTS[kind.index()] * position.pieces(color, kind).count() as i32;
        }
    }
    phase.min(MAX_PHASE)
}

fn non_pawn_material(position: &Position, color: Color) -> i32 {
    [
        PieceType::Knight,
        PieceType::Bishop,
        PieceType::Rook,
        PieceType::Queen,
    ]
    .into_iter()
    .map(|kind| VALUES[kind.index()] * position.pieces(color, kind).count() as i32)
    .sum()
}

/// Scale in sixteenths applied to a winning endgame score: without pawns, an advantage of
/// at most a minor piece cannot usually be converted.
fn endgame_scale(position: &Position, strong: Color) -> i32 {
    if position.pieces(strong, PieceType::Pawn).0 != 0 {
        return 16;
    }
    let margin = non_pawn_material(position, strong) - non_pawn_material(position, strong.other());
    if margin <= VALUES[PieceType::Bishop.index()] {
        2
    } else {
        16
    }
}

pub fn evaluate(position: &Position) -> i32 {
    let mut white = Score::default();
    let mut black = Score::default();
    let white_attacks = evaluate_side(position, Color::White, &mut white);
    let black_attacks = evaluate_side(position, Color::Black, &mut black);
    evaluate_threats(
        position,
        Color::White,
        &white_attacks,
        &black_attacks,
        &mut white,
    );
    evaluate_threats(
        position,
        Color::Black,
        &black_attacks,
        &white_attacks,
        &mut black,
    );
    let mg = white.mg - black.mg;
    let mut eg = white.eg - black.eg;
    let strong = if eg >= 0 { Color::White } else { Color::Black };
    eg = eg * endgame_scale(position, strong) / 16;
    let phase = phase(position);
    let score = (mg * phase + eg * (MAX_PHASE - phase)) / MAX_PHASE;
    let relative = match position.side_to_move() {
        Color::White => score,
        Color::Black => -score,
    };
    relative + TEMPO
}

/// Evaluation features of one position for the tuner. `terms` holds white-minus-black
/// feature counts; king attacks are kept per side because their weight is scaled by the
/// attacker count. The score is `tapered(mg, eg * scale[strong] / 16) + tempo`, with the
/// strong side chosen by the sign of the endgame sum.
pub struct Trace {
    pub terms: Vec<(u16, i16)>,
    pub king_attacks: [([i32; 6], i32); 2],
    pub phase: i32,
    pub scale: [i32; 2],
    pub white_to_move: bool,
}

struct TraceSink {
    counts: Vec<i32>,
    sign: i32,
    king_attack: ([i32; 6], i32),
}

impl Sink for TraceSink {
    fn add(&mut self, index: usize, count: i32) {
        self.counts[index] += self.sign * count;
    }

    fn king_attack(&mut self, by_kind: [i32; 6], attackers: i32) {
        self.king_attack = (by_kind, attackers);
    }
}

pub fn trace(position: &Position) -> Trace {
    let mut sink = TraceSink {
        counts: vec![0; PARAMS],
        sign: 1,
        king_attack: ([0; 6], 0),
    };
    let white_attacks = evaluate_side(position, Color::White, &mut sink);
    let white_king = std::mem::take(&mut sink.king_attack);
    sink.sign = -1;
    let black_attacks = evaluate_side(position, Color::Black, &mut sink);
    let black_king = std::mem::take(&mut sink.king_attack);
    evaluate_threats(
        position,
        Color::Black,
        &black_attacks,
        &white_attacks,
        &mut sink,
    );
    sink.sign = 1;
    evaluate_threats(
        position,
        Color::White,
        &white_attacks,
        &black_attacks,
        &mut sink,
    );
    Trace {
        terms: sink
            .counts
            .iter()
            .enumerate()
            .filter(|(_, &count)| count != 0)
            .map(|(index, &count)| (index as u16, count as i16))
            .collect(),
        king_attacks: [white_king, black_king],
        phase: phase(position),
        scale: [
            endgame_scale(position, Color::White),
            endgame_scale(position, Color::Black),
        ],
        white_to_move: position.side_to_move() == Color::White,
    }
}

/// White-perspective evaluation of a trace under arbitrary weights, matching `evaluate` for
/// the built-in weights up to integer rounding.
pub fn traced_score(trace: &Trace, weights: &[(f64, f64)]) -> f64 {
    let (mut mg, mut eg) = trace
        .terms
        .iter()
        .fold((0.0, 0.0), |(mg, eg), &(index, count)| {
            let (w_mg, w_eg) = weights[index as usize];
            (mg + w_mg * f64::from(count), eg + w_eg * f64::from(count))
        });
    for (side, &(by_kind, attackers)) in trace.king_attacks.iter().enumerate() {
        let sign = if side == 0 { 1.0 } else { -1.0 };
        for (kind, &count) in by_kind.iter().enumerate() {
            let (w_mg, w_eg) = weights[KING_ZONE + kind];
            let factor = sign * f64::from(count * attackers) / 4.0;
            mg += w_mg * factor;
            eg += w_eg * factor;
        }
    }
    let strong = if eg >= 0.0 { 0 } else { 1 };
    eg *= f64::from(trace.scale[strong]) / 16.0;
    let phase = f64::from(trace.phase);
    let max = f64::from(MAX_PHASE);
    let tempo = if trace.white_to_move {
        f64::from(TEMPO)
    } else {
        -f64::from(TEMPO)
    };
    (mg * phase + eg * (max - phase)) / max + tempo
}

pub fn weights() -> &'static [(i32, i32)] {
    &WEIGHTS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mirrored(fen: &str) -> String {
        let mut fields = fen.split_whitespace();
        let board = fields.next().unwrap();
        let side = fields.next().unwrap();
        let flipped: Vec<String> = board
            .split('/')
            .rev()
            .map(|rank| {
                rank.chars()
                    .map(|c| {
                        if c.is_ascii_uppercase() {
                            c.to_ascii_lowercase()
                        } else {
                            c.to_ascii_uppercase()
                        }
                    })
                    .collect()
            })
            .collect();
        let side = if side == "w" { "b" } else { "w" };
        format!("{} {} - - 0 1", flipped.join("/"), side)
    }

    #[test]
    fn start_is_balanced() {
        assert_eq!(evaluate(&Position::startpos()), TEMPO);
    }

    #[test]
    fn material_changes_sign_with_turn() {
        let white = Position::from_fen("4k3/8/8/8/8/8/3Q4/4K3 w - - 0 1").unwrap();
        let black = Position::from_fen("4k3/8/8/8/8/8/3Q4/4K3 b - - 0 1").unwrap();
        assert!(evaluate(&white) > 800);
        assert_eq!(evaluate(&white) - TEMPO, -(evaluate(&black) - TEMPO));
    }

    #[test]
    fn colour_mirror_is_symmetric() {
        for fen in [
            "r1bqkb1r/pppp1ppp/2n2n2/4p3/2B1P3/5N2/PPPP1PPP/RNBQK2R w KQkq - 4 4",
            "8/5pk1/6p1/3P4/1r6/6P1/5PK1/3R4 w - - 0 40",
            "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10",
            "8/8/4k3/8/2N5/8/4K3/8 w - - 0 1",
        ] {
            let original = Position::from_fen(fen).unwrap();
            let flipped = Position::from_fen(&mirrored(fen)).unwrap();
            assert_eq!(evaluate(&original), evaluate(&flipped), "{fen}");
        }
    }

    #[test]
    fn lone_minor_is_scaled_toward_a_draw() {
        let knight = Position::from_fen("8/8/4k3/8/2N5/8/4K3/8 w - - 0 1").unwrap();
        assert!(evaluate(&knight) < 100);
    }

    #[test]
    fn passed_pawn_outweighs_blocked_pawn() {
        let passed = Position::from_fen("4k3/p7/8/3P4/8/8/1P6/4K3 w - - 0 1").unwrap();
        let stopped = Position::from_fen("4k3/2p5/8/3P4/8/8/1P6/4K3 w - - 0 1").unwrap();
        assert!(evaluate(&passed) > evaluate(&stopped) + 15);
    }

    #[test]
    fn trace_reproduces_the_evaluation() {
        let weights: Vec<(f64, f64)> = weights()
            .iter()
            .map(|&(mg, eg)| (f64::from(mg), f64::from(eg)))
            .collect();
        for fen in [
            "r1bqkb1r/pppp1ppp/2n2n2/4p3/2B1P3/5N2/PPPP1PPP/RNBQK2R w KQkq - 4 4",
            "8/5pk1/6p1/3P4/1r6/6P1/5PK1/3R4 b - - 0 40",
            "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10",
            "2kr3r/ppp2ppp/2n5/2b1q3/4P1b1/2NB4/PPPQ1PPP/R4RK1 b - - 0 12",
            "8/8/4k3/8/2N5/8/4K3/8 w - - 0 1",
        ] {
            let position = Position::from_fen(fen).unwrap();
            let relative = evaluate(&position);
            let white = if position.side_to_move() == Color::White {
                relative
            } else {
                -relative
            };
            let traced = traced_score(&trace(&position), &weights);
            assert!(
                (traced - f64::from(white)).abs() <= 2.0,
                "{fen}: {traced} vs {white}"
            );
        }
    }
}
