use inphzugzwang_book::{key, Book};
use inphzugzwang_core::Position;

/// The test keys published with the Polyglot book format.
#[test]
fn matches_published_keys() {
    let cases = [
        (
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
            0x463b_9618_1691_fc9c,
        ),
        (
            "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1",
            0x823c_9b50_fd11_4196,
        ),
        (
            "rnbqkbnr/ppp1pppp/8/3p4/4P3/8/PPPP1PPP/RNBQKBNR w KQkq d6 0 2",
            0x0756_b944_61c5_0fb0,
        ),
        (
            "rnbqkbnr/ppp1pppp/8/3pP3/8/8/PPPP1PPP/RNBQKBNR b KQkq - 0 2",
            0x662f_afb9_65db_29d4,
        ),
        (
            "rnbqkbnr/ppp1p1pp/8/3pPp2/8/8/PPPP1PPP/RNBQKBNR w KQkq f6 0 3",
            0x22a4_8b5a_8e47_ff78,
        ),
        (
            "rnbqkbnr/ppp1p1pp/8/3pPp2/8/8/PPPPKPPP/RNBQ1BNR b kq - 0 3",
            0x652a_607c_a3f2_42c1,
        ),
        (
            "rnbq1bnr/ppp1pkpp/8/3pPp2/8/8/PPPPKPPP/RNBQ1BNR w - - 0 4",
            0x00fd_d303_c946_bdd9,
        ),
        (
            "rnbqkbnr/p1pppppp/8/8/PpP4P/8/1P1PPPP1/RNBQKBNR b KQkq c3 0 3",
            0x3c81_23ea_7b06_7637,
        ),
        (
            "rnbqkbnr/p1pppppp/8/8/P6P/R1p5/1P1PPPP1/1NBQKBNR b Kkq - 0 4",
            0x5c3f_9b82_9b27_9560,
        ),
    ];
    for (fen, expected) in cases {
        assert_eq!(key(&Position::from_fen(fen).expect(fen)), expected, "{fen}");
    }
}

/// Polyglot hashes the en-passant file whenever a pawn of the side to move stands next
/// to the pawn that moved two squares, even when that pawn is pinned and cannot capture.
#[test]
fn hashes_en_passant_for_a_pinned_pawn() {
    // After ...e7e5 the f5 pawn cannot take on e6: the rook on a5 would then check h5.
    let with = Position::from_fen("8/8/8/r3pP1K/8/8/8/4k3 w - e6 0 2").unwrap();
    let without = Position::from_fen("8/8/8/r3pP1K/8/8/8/4k3 w - - 0 2").unwrap();
    assert!(!with
        .legal_moves()
        .iter()
        .any(|mv| with.format_move(mv, false) == "f5e6"));
    // RandomEnPassant starts at offset 772; the e file is its fifth entry.
    let e_file = 0xcf31_45de_0add_4289;
    assert_eq!(key(&with), key(&without) ^ e_file);
}

fn entry(key: u64, raw: u16, weight: u16) -> Vec<u8> {
    let mut bytes = key.to_be_bytes().to_vec();
    bytes.extend(raw.to_be_bytes());
    bytes.extend(weight.to_be_bytes());
    bytes.extend([0; 4]);
    bytes
}

#[test]
fn finds_weighted_moves_and_castling() {
    let start = Position::startpos();
    let castle =
        Position::from_fen("r1bqkbnr/pppp1ppp/2n5/1B2p3/4P3/5N2/PPPP1PPP/RNBQK2R w KQkq - 4 4")
            .unwrap();
    // e2e4 and d2d4 from the start, the illegal a1a8 as after a key collision, and
    // castling given as e1h1.
    let e2e4 = (1 << 9) | (4 << 6) | (3 << 3) | 4;
    let d2d4 = (1 << 9) | (3 << 6) | (3 << 3) | 3;
    let a1a8 = 7 << 3;
    let e1h1 = (4 << 6) | 7;
    let mut rows = [
        (key(&start), e2e4, 30),
        (key(&start), d2d4, 10),
        (key(&start), a1a8, 50),
        (key(&castle), e1h1, 1),
    ];
    rows.sort_by_key(|&(key, _, _)| key);
    let book = Book::new(rows.iter().flat_map(|&(k, m, w)| entry(k, m, w)).collect()).unwrap();
    let moves: Vec<String> = book
        .moves(&start)
        .iter()
        .map(|&(mv, weight)| format!("{}:{weight}", start.format_move(mv, false)))
        .collect();
    assert_eq!(moves, ["e2e4:30", "d2d4:10"]);
    let best = book.choose(&start, true, 0).unwrap();
    assert_eq!(start.format_move(best, false), "e2e4");
    assert_eq!(
        start.format_move(book.choose(&start, false, 35).unwrap(), false),
        "d2d4"
    );
    assert_eq!(
        start.format_move(book.choose(&start, false, 29).unwrap(), false),
        "e2e4"
    );
    let castling = book.choose(&castle, true, 0).unwrap();
    assert!(castling.is_castle());
    assert!(book
        .moves(&Position::from_fen("8/8/8/8/8/8/8/K6k w - - 0 1").unwrap())
        .is_empty());
}

#[test]
fn rejects_malformed_books() {
    assert!(Book::new(vec![0; 15]).is_err());
    let mut unsorted = entry(2, 0, 1);
    unsorted.extend(entry(1, 0, 1));
    assert!(Book::new(unsorted).is_err());
}
