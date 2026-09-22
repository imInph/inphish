use inphzugzwang_core::{Color, PieceType, Position, Square};

pub const VALUES: [i32; 6] = [100, 320, 330, 500, 900, 0];

pub fn evaluate(position: &Position) -> i32 {
    let mut white = 0;
    let mut black = 0;
    let mut phase = 0;
    for rank in 0..8 {
        for file in 0..8 {
            if let Some(piece) = position.piece_at(Square::new(file, rank)) {
                phase += match piece.kind {
                    PieceType::Knight | PieceType::Bishop => 1,
                    PieceType::Rook => 2,
                    PieceType::Queen => 4,
                    _ => 0,
                };
            }
        }
    }
    for rank in 0..8 {
        for file in 0..8 {
            let Some(piece) = position.piece_at(Square::new(file, rank)) else {
                continue;
            };
            let relative_rank = if piece.color == Color::White {
                rank as i32
            } else {
                7 - rank as i32
            };
            let center = 6
                - (file as i32 - 3).abs().min((file as i32 - 4).abs())
                - (relative_rank - 3).abs().min((relative_rank - 4).abs());
            let pst = match piece.kind {
                PieceType::Pawn => relative_rank * 8 + center * 2,
                PieceType::Knight => center * 9,
                PieceType::Bishop => center * 5,
                PieceType::Rook => relative_rank * 2 + center,
                PieceType::Queen => center * 2,
                PieceType::King => {
                    let middlegame = -(center * 7);
                    let endgame = center * 8;
                    (middlegame * phase.min(24) + endgame * (24 - phase.min(24))) / 24
                }
            };
            let score = VALUES[piece.kind.index()] + pst;
            if piece.color == Color::White {
                white += score;
            } else {
                black += score;
            }
        }
    }
    let score = white - black;
    if position.side_to_move() == Color::White {
        score
    } else {
        -score
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_is_balanced() {
        assert_eq!(evaluate(&Position::startpos()), 0);
    }

    #[test]
    fn material_changes_sign_with_turn() {
        let white = Position::from_fen("4k3/8/8/8/8/8/3Q4/4K3 w - - 0 1").unwrap();
        let black = Position::from_fen("4k3/8/8/8/8/8/3Q4/4K3 b - - 0 1").unwrap();
        assert!(evaluate(&white) > 800);
        assert_eq!(evaluate(&white), -evaluate(&black));
    }
}
