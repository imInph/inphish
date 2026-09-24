use inphzugzwang_core::{
    bishop_attacks, king_attacks, knight_attacks, pawn_attacks, rook_attacks, Bitboard, Color,
    PieceType, Position, Square,
};

/// Rough exchange values used by move ordering and pruning margins, not by the evaluation.
pub const VALUES: [i32; 6] = [100, 320, 330, 500, 900, 0];

const MG_VALUES: [i32; 6] = [82, 337, 365, 477, 1025, 0];
const EG_VALUES: [i32; 6] = [94, 281, 297, 512, 936, 0];
const PHASE_WEIGHTS: [i32; 6] = [0, 1, 1, 2, 4, 0];
const MAX_PHASE: i32 = 24;

// Piece-square tables from Ronald Friederich's PeSTO, published on the Chess Programming
// Wiki. They are laid out from a8 to h1 as printed there, so a white piece on square s
// reads index s ^ 56 and a black piece reads index s.
#[rustfmt::skip]
const MG_TABLES: [[i32; 64]; 6] = [
    [
          0,   0,   0,   0,   0,   0,   0,   0,
         98, 134,  61,  95,  68, 126,  34, -11,
         -6,   7,  26,  31,  65,  56,  25, -20,
        -14,  13,   6,  21,  23,  12,  17, -23,
        -27,  -2,  -5,  12,  17,   6,  10, -25,
        -26,  -4,  -4, -10,   3,   3,  33, -12,
        -35,  -1, -20, -23, -15,  24,  38, -22,
          0,   0,   0,   0,   0,   0,   0,   0,
    ],
    [
        -167, -89, -34, -49,  61, -97, -15, -107,
         -73, -41,  72,  36,  23,  62,   7,  -17,
         -47,  60,  37,  65,  84, 129,  73,   44,
          -9,  17,  19,  53,  37,  69,  18,   22,
         -13,   4,  16,  13,  28,  19,  21,   -8,
         -23,  -9,  12,  10,  19,  17,  25,  -16,
         -29, -53, -12,  -3,  -1,  18, -14,  -19,
        -105, -21, -58, -33, -17, -28, -19,  -23,
    ],
    [
        -29,   4, -82, -37, -25, -42,   7,  -8,
        -26,  16, -18, -13,  30,  59,  18, -47,
        -16,  37,  43,  40,  35,  50,  37,  -2,
         -4,   5,  19,  50,  37,  37,   7,  -2,
         -6,  13,  13,  26,  34,  12,  10,   4,
          0,  15,  15,  15,  14,  27,  18,  10,
          4,  15,  16,   0,   7,  21,  33,   1,
        -33,  -3, -14, -21, -13, -12, -39, -21,
    ],
    [
         32,  42,  32,  51,  63,   9,  31,  43,
         27,  32,  58,  62,  80,  67,  26,  44,
         -5,  19,  26,  36,  17,  45,  61,  16,
        -24, -11,   7,  26,  24,  35,  -8, -20,
        -36, -26, -12,  -1,   9,  -7,   6, -23,
        -45, -25, -16, -17,   3,   0,  -5, -33,
        -44, -16, -20,  -9,  -1,  11,  -6, -71,
        -19, -13,   1,  17,  16,   7, -37, -26,
    ],
    [
        -28,   0,  29,  12,  59,  44,  43,  45,
        -24, -39,  -5,   1, -16,  57,  28,  54,
        -13, -17,   7,   8,  29,  56,  47,  57,
        -27, -27, -16, -16,  -1,  17,  -2,   1,
         -9, -26,  -9, -10,  -2,  -4,   3,  -3,
        -14,   2, -11,  -2,  -5,   2,  14,   5,
        -35,  -8,  11,   2,   8,  15,  -3,   1,
         -1, -18,  -9,  10, -15, -25, -31, -50,
    ],
    [
        -65,  23,  16, -15, -56, -34,   2,  13,
         29,  -1, -20,  -7,  -8,  -4, -38, -29,
         -9,  24,   2, -16, -20,   6,  22, -22,
        -17, -20, -12, -27, -30, -25, -14, -36,
        -49,  -1, -27, -39, -46, -44, -33, -51,
        -14, -14, -22, -46, -44, -30, -15, -27,
          1,   7,  -8, -64, -43, -16,   9,   8,
        -15,  36,  12, -54,   8, -28,  24,  14,
    ],
];

#[rustfmt::skip]
const EG_TABLES: [[i32; 64]; 6] = [
    [
          0,   0,   0,   0,   0,   0,   0,   0,
        178, 173, 158, 134, 147, 132, 165, 187,
         94, 100,  85,  67,  56,  53,  82,  84,
         32,  24,  13,   5,  -2,   4,  17,  17,
         13,   9,  -3,  -7,  -7,  -8,   3,  -1,
          4,   7,  -6,   1,   0,  -5,  -1,  -8,
         13,   8,   8,  10,  13,   0,   2,  -7,
          0,   0,   0,   0,   0,   0,   0,   0,
    ],
    [
        -58, -38, -13, -28, -31, -27, -63, -99,
        -25,  -8, -25,  -2,  -9, -25, -24, -52,
        -24, -20,  10,   9,  -1,  -9, -19, -41,
        -17,   3,  22,  22,  22,  11,   8, -18,
        -18,  -6,  16,  25,  16,  17,   4, -18,
        -23,  -3,  -1,  15,  10,  -3, -20, -22,
        -42, -20, -10,  -5,  -2, -20, -23, -44,
        -29, -51, -23, -15, -22, -18, -50, -64,
    ],
    [
        -14, -21, -11,  -8,  -7,  -9, -17, -24,
         -8,  -4,   7, -12,  -3, -13,  -4, -14,
          2,  -8,   0,  -1,  -2,   6,   0,   4,
         -3,   9,  12,   9,  14,  10,   3,   2,
         -6,   3,  13,  19,   7,  10,  -3,  -9,
        -12,  -3,   8,  10,  13,   3,  -7, -15,
        -14, -18,  -7,  -1,   4,  -9, -15, -27,
        -23,  -9, -23,  -5,  -9, -16,  -5, -17,
    ],
    [
         13,  10,  18,  15,  12,  12,   8,   5,
         11,  13,  13,  11,  -3,   3,   8,   3,
          7,   7,   7,   5,   4,  -3,  -5,  -3,
          4,   3,  13,   1,   2,   1,  -1,   2,
          3,   5,   8,   4,  -5,  -6,  -8, -11,
         -4,   0,  -5,  -1,  -7, -12,  -8, -16,
         -6,  -6,   0,   2,  -9,  -9, -11,  -3,
         -9,   2,   3,  -1,  -5, -13,   4, -20,
    ],
    [
         -9,  22,  22,  27,  27,  19,  10,  20,
        -17,  20,  32,  41,  58,  25,  30,   0,
        -20,   6,   9,  49,  47,  35,  19,   9,
          3,  22,  24,  45,  57,  40,  57,  36,
        -18,  28,  19,  47,  31,  34,  39,  23,
        -16, -27,  15,   6,   9,  17,  10,   5,
        -22, -23, -30, -16, -16, -23, -36, -32,
        -33, -28, -22, -43,  -5, -32, -20, -41,
    ],
    [
        -74, -35, -18, -18, -11,  15,   4, -17,
        -12,  17,  14,  17,  17,  38,  23,  11,
         10,  17,  23,  15,  20,  45,  44,  13,
         -8,  22,  24,  27,  26,  33,  26,   3,
        -18,  -4,  21,  24,  27,  23,   9, -11,
        -19,  -3,  11,  21,  23,  16,   7,  -9,
        -27, -11,   4,  13,  14,   4,  -5, -17,
        -53, -34, -21, -11, -28, -14, -24, -43,
    ],
];

// Indexed by relative rank. The PeSTO pawn tables already reward advancement in general,
// so these only add the part that depends on no enemy pawn being able to stop the pawn.
const PASSED_MG: [i32; 8] = [0, 0, 5, 10, 20, 35, 55, 0];
const PASSED_EG: [i32; 8] = [0, 10, 15, 25, 45, 75, 120, 0];
const DOUBLED: (i32, i32) = (-10, -20);
const ISOLATED: (i32, i32) = (-10, -12);
const BISHOP_PAIR: (i32, i32) = (25, 50);
const ROOK_OPEN_FILE: (i32, i32) = (25, 10);
const ROOK_SEMI_OPEN_FILE: (i32, i32) = (12, 6);
// Per reachable square beyond a typical count, so an average piece contributes about zero.
const MOBILITY: [(i32, i32, i32); 4] = [(4, 4, 4), (5, 5, 6), (2, 4, 6), (1, 2, 12)];
const KING_ZONE_ATTACK: [i32; 6] = [0, 20, 20, 40, 80, 0];
const SHIELD_PAWN: i32 = 12;
const TEMPO: i32 = 10;

const FILE_A: u64 = 0x0101_0101_0101_0101;

#[derive(Clone, Copy, Default)]
struct Score {
    mg: i32,
    eg: i32,
}

impl Score {
    fn add(&mut self, (mg, eg): (i32, i32)) {
        self.mg += mg;
        self.eg += eg;
    }
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

fn pawn_attack_span(color: Color, pawns: Bitboard) -> u64 {
    pawns
        .into_iter()
        .fold(0, |acc, square| acc | pawn_attacks(color, square).0)
}

fn evaluate_side(position: &Position, color: Color, score: &mut Score) {
    let enemy = color.other();
    let occupied = position.occupied();
    let own_pieces = position.side_pieces(color);
    let own_pawns = position.pieces(color, PieceType::Pawn);
    let enemy_pawns = position.pieces(enemy, PieceType::Pawn);
    let enemy_pawn_attacks = pawn_attack_span(enemy, enemy_pawns);
    let enemy_king = position.king(enemy);
    let enemy_king_zone = king_attacks(enemy_king).0 | enemy_king.bit().0;
    let mut king_attack_weight = 0;
    let mut king_attackers = 0;

    for kind in [
        PieceType::Pawn,
        PieceType::Knight,
        PieceType::Bishop,
        PieceType::Rook,
        PieceType::Queen,
        PieceType::King,
    ] {
        for square in position.pieces(color, kind) {
            let table_index = match color {
                Color::White => square.index() ^ 56,
                Color::Black => square.index(),
            };
            score.add((
                MG_VALUES[kind.index()] + MG_TABLES[kind.index()][table_index],
                EG_VALUES[kind.index()] + EG_TABLES[kind.index()][table_index],
            ));
            let attacks = match kind {
                PieceType::Knight => knight_attacks(square),
                PieceType::Bishop => bishop_attacks(square, occupied),
                PieceType::Rook => rook_attacks(square, occupied),
                PieceType::Queen => {
                    bishop_attacks(square, occupied) | rook_attacks(square, occupied)
                }
                PieceType::Pawn | PieceType::King => continue,
            };
            let (mg, eg, typical) = MOBILITY[kind.index() - 1];
            let reachable =
                (attacks.0 & !own_pieces.0 & !enemy_pawn_attacks).count_ones() as i32 - typical;
            score.add((mg * reachable, eg * reachable));
            if attacks.0 & enemy_king_zone != 0 {
                king_attackers += 1;
                king_attack_weight += KING_ZONE_ATTACK[kind.index()];
            }
            if kind == PieceType::Rook {
                let file = file_mask(square.file());
                if file & own_pawns.0 == 0 {
                    score.add(if file & enemy_pawns.0 == 0 {
                        ROOK_OPEN_FILE
                    } else {
                        ROOK_SEMI_OPEN_FILE
                    });
                }
            }
        }
    }

    // A single attacker rarely threatens mate, so king pressure only counts once two
    // pieces are aimed at the zone, and grows with the number of attackers.
    if king_attackers >= 2 {
        score.add((king_attack_weight * king_attackers / 4, 0));
    }

    for square in own_pawns {
        let file = square.file();
        if own_pawns.0 & file_mask(file) & !square.bit().0 != 0
            && forward_span(color, square, file_mask(file)) & own_pawns.0 != 0
        {
            score.add(DOUBLED);
        }
        if own_pawns.0 & adjacent_files(file) == 0 {
            score.add(ISOLATED);
        }
        let stoppers = forward_span(color, square, file_mask(file) | adjacent_files(file));
        if stoppers & enemy_pawns.0 == 0 {
            let relative_rank = match color {
                Color::White => square.rank(),
                Color::Black => 7 - square.rank(),
            } as usize;
            score.add((PASSED_MG[relative_rank], PASSED_EG[relative_rank]));
        }
    }

    if position.pieces(color, PieceType::Bishop).count() >= 2 {
        score.add(BISHOP_PAIR);
    }

    let king = position.king(color);
    let shield_files = file_mask(king.file()) | adjacent_files(king.file());
    let near_ranks = (1..=2)
        .filter_map(|step| match color {
            Color::White => king.rank().checked_add(step).filter(|&rank| rank < 8),
            Color::Black => king.rank().checked_sub(step),
        })
        .fold(0, |mask, rank| mask | (0xFF_u64 << (8 * rank)));
    let shield = (shield_files & near_ranks & own_pawns.0)
        .count_ones()
        .min(3) as i32;
    score.add((SHIELD_PAWN * shield, 0));
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
    evaluate_side(position, Color::White, &mut white);
    evaluate_side(position, Color::Black, &mut black);
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
}
