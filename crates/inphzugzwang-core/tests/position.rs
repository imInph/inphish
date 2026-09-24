use inphzugzwang_core::{Color, PieceType, Position, Square, START_FEN};

#[test]
fn tactical_generation_matches_legal_move_filter() {
    for fen in [
        START_FEN,
        "k7/8/8/3pP3/4K3/8/8/8 w - d6 0 1",
        "k6r/6P1/8/8/8/8/8/7K w - - 0 1",
        "k3r3/8/8/8/1b6/8/8/4K1N1 w - - 0 1",
        "k7/8/8/8/8/8/8/6KR w H - 0 1",
    ] {
        let position = Position::from_fen(fen).unwrap();
        let in_check = position.checkers().0 != 0;
        let mut expected: Vec<_> = position
            .legal_moves()
            .iter()
            .filter(|mv| in_check || mv.flag() & 4 != 0 || mv.promotion() == Some(PieceType::Queen))
            .map(|mv| mv.raw())
            .collect();
        let mut actual: Vec<_> = position
            .tactical_moves()
            .iter()
            .map(|mv| mv.raw())
            .collect();
        expected.sort_unstable();
        actual.sort_unstable();
        assert_eq!(actual, expected, "{fen}");
    }
}

#[test]
fn fen_round_trips_standard_and_chess960() {
    for fen in [
        START_FEN,
        "bqnb1rkr/pp3ppp/3ppn2/2p5/5P2/P2P4/NPP1P1PP/BQ1BNRKR w HFhf - 2 9",
        "k7/8/8/8/8/8/8/6KR w H - 0 1",
        "k7/8/8/8/8/8/8/4KR2 w F - 0 1",
        "7k/8/8/8/8/8/8/R2K1R2 w KQ - 0 1",
    ] {
        let position = Position::from_fen(fen).expect(fen);
        let again = Position::from_fen(&position.fen()).expect(fen);
        assert_eq!(position.fen(), again.fen());
        assert_eq!(position.key(), again.key());
    }
}

#[test]
fn malformed_fens_return_errors() {
    for fen in [
        "",
        "8/8/8/8/8/8/8/8 w - - 0 1",
        "k7/8/8/8/8/8/8/K6P w - - 0 1",
        "k7/8/8/8/8/8/8/K7 w K - 0 1",
        "k7/8/8/8/8/8/8/K7 w - e3 0 1",
        "k7/8/8/8/8/8/8/K7 w - - 0 0",
    ] {
        assert!(Position::from_fen(fen).is_err(), "{fen}");
    }
}

#[test]
fn random_play_restores_full_state_and_hashes() {
    let mut position = Position::startpos();
    let mut seed = 0x947e_982f_6bc1_4d3a_u64;
    let mut snapshots = Vec::new();
    for _ in 0..160 {
        let moves = position.legal_moves();
        if moves.is_empty() {
            break;
        }
        let in_check = position.checkers().0 != 0;
        let mut expected: Vec<_> = moves
            .iter()
            .filter(|mv| in_check || mv.flag() & 4 != 0 || mv.promotion() == Some(PieceType::Queen))
            .map(|mv| mv.raw())
            .collect();
        let mut actual: Vec<_> = position
            .tactical_moves()
            .iter()
            .map(|mv| mv.raw())
            .collect();
        expected.sort_unstable();
        actual.sort_unstable();
        assert_eq!(actual, expected, "{}", position.fen());
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let mv = moves
            .iter()
            .nth(seed as usize % moves.len())
            .expect("legal move");
        assert!(position.is_pseudo_legal(mv));
        snapshots.push((
            position.fen(),
            position.key(),
            position.pawn_key(),
            position.non_pawn_keys(),
        ));
        let gives_check = position.gives_check(mv);
        position.make(mv);
        assert_eq!(gives_check, position.checkers().0 != 0);
        let reloaded = Position::from_fen(&position.fen()).expect("legal game FEN");
        assert_eq!(position.key(), reloaded.key());
        assert_eq!(position.pawn_key(), reloaded.pawn_key());
        assert_eq!(position.non_pawn_keys(), reloaded.non_pawn_keys());
    }
    for snapshot in snapshots.into_iter().rev() {
        position.unmake();
        assert_eq!(
            (
                position.fen(),
                position.key(),
                position.pawn_key(),
                position.non_pawn_keys()
            ),
            snapshot
        );
    }
    assert_eq!(position.fen(), START_FEN);
}

#[test]
fn en_passant_hash_requires_a_legal_capture() {
    let pinned = Position::from_fen("k7/8/8/4KPpr/8/8/8/8 w - g6 0 1").unwrap();
    let without = Position::from_fen("k7/8/8/4KPpr/8/8/8/8 w - - 0 1").unwrap();
    assert_eq!(pinned.key(), without.key());
    assert!(pinned.parse_move("f5g6", false).is_none());
    let legal = Position::from_fen("k7/8/8/5Pp1/8/8/8/4K3 w - g6 0 1").unwrap();
    let no_ep = Position::from_fen("k7/8/8/5Pp1/8/8/8/4K3 w - - 0 1").unwrap();
    assert_ne!(legal.key(), no_ep.key());
    assert!(legal.parse_move("f5g6", false).is_some());
}

#[test]
fn repetition_and_fifty_move_rules() {
    let mut position = Position::startpos();
    for move_text in [
        "g1f3", "g8f6", "f3g1", "f6g8", "g1f3", "g8f6", "f3g1", "f6g8",
    ] {
        let mv = position.parse_move(move_text, false).expect(move_text);
        position.make(mv);
    }
    assert!(position.is_threefold());
    let mate = Position::from_fen("k7/1Q6/2K5/8/8/8/8/8 b - - 100 1").unwrap();
    assert!(mate.is_checkmate());
    assert!(!mate.is_fifty_move_draw());
    let non_mate = Position::from_fen("k7/8/2K5/8/8/8/8/8 b - - 100 1").unwrap();
    assert!(non_mate.is_fifty_move_draw());
}

#[test]
fn insufficient_material_cases() {
    for fen in [
        "k7/8/8/8/8/8/8/K7 w - - 0 1",
        "k7/8/8/8/8/8/8/KN6 w - - 0 1",
        "k7/8/8/8/8/8/8/KB6 w - - 0 1",
        "k4b2/8/8/8/8/8/8/K1B5 w - - 0 1",
    ] {
        assert!(
            Position::from_fen(fen).unwrap().is_insufficient_material(),
            "{fen}"
        );
    }
    assert!(!Position::from_fen("k7/8/8/8/8/8/8/KBN5 w - - 0 1")
        .unwrap()
        .is_insufficient_material());
}

#[test]
fn stationary_chess960_castles_use_rook_destinations() {
    for (fen, move_text) in [
        ("k7/8/8/8/8/8/8/6KR w H - 0 1", "g1h1"),
        ("k7/8/8/8/8/8/8/4KR2 w F - 0 1", "e1f1"),
    ] {
        let mut position = Position::from_fen(fen).unwrap();
        let mv = position.parse_move(move_text, true).expect(move_text);
        let key = position.key();
        position.make(mv);
        assert_eq!(
            position
                .piece_at(Square::parse("g1").unwrap())
                .unwrap()
                .color,
            Color::White
        );
        position.unmake();
        assert_eq!(position.key(), key);
        assert_eq!(position.fen(), fen);
    }
}

#[test]
fn castling_paths_and_check_evasions() {
    let overlapping = Position::from_fen("k7/8/8/8/8/8/8/4K1R1 w G - 0 1").unwrap();
    assert!(overlapping.parse_move("e1g1", true).is_some());
    let attacked = Position::from_fen("k4r2/8/8/8/8/8/8/4K2R w H - 0 1").unwrap();
    assert!(attacked.parse_move("e1h1", true).is_none());
    let en_passant = Position::from_fen("k7/8/8/3pP3/4K3/8/8/8 w - d6 0 1").unwrap();
    assert!(en_passant.parse_move("e5d6", false).is_some());
    let double_check = Position::from_fen("k3r3/8/8/8/1b6/8/8/4K1N1 w - - 0 1").unwrap();
    assert_eq!(double_check.checkers().count(), 2);
    assert!(double_check
        .legal_moves()
        .iter()
        .all(|mv| mv.from() == Square::parse("e1").unwrap()));
    let pinned = Position::from_fen("k3r3/8/8/8/8/8/4R3/4K3 w - - 0 1").unwrap();
    assert!(pinned.parse_move("e2e3", false).is_some());
    assert!(pinned.parse_move("e2f2", false).is_none());
}
