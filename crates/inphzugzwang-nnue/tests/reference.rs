use inphzugzwang_core::Position;
use inphzugzwang_nnue::{delta, network, non_pawn_material, Accumulator, Network, RefreshCache};

const REFERENCE: &str = include_str!("../../../tests/nnue/reference.txt");

#[test]
fn matches_stockfish_15_1_exactly() {
    let network = network();
    let mut count = 0;
    for line in REFERENCE.lines().filter(|line| !line.starts_with('#')) {
        let mut fields = line.split('|');
        let (fen, raw, adjusted) = (
            fields.next().expect(line),
            fields.next().expect(line),
            fields.next().expect(line),
        );
        let position = Position::from_fen(fen).expect(fen);
        let output = network.evaluate_position(&position);
        assert_eq!(output.raw(), raw.parse::<i32>().unwrap(), "{fen}");
        assert_eq!(
            output.adjusted(non_pawn_material(&position)),
            adjusted.parse::<i32>().unwrap(),
            "{fen}"
        );
        count += 1;
    }
    assert!(count > 2000);
}

#[test]
fn rejects_damaged_files() {
    let bytes = include_bytes!("../net/nn-ad9b42354671.nnue");
    assert!(Network::parse(&bytes[..bytes.len() - 1]).is_err());
    let mut longer = bytes.to_vec();
    longer.push(0);
    assert!(Network::parse(&longer).is_err());
    let mut version = bytes.to_vec();
    version[0] ^= 1;
    assert!(Network::parse(&version).is_err());
}

/// Walks every line to a fixed depth, deriving each accumulator from its parent's, and
/// checks it against one computed from scratch. King moves go through the refresh cache,
/// which the walk shares across all positions as the search does.
fn walk(
    position: &mut Position,
    parent: &Accumulator,
    depth: u8,
    cache: &mut RefreshCache,
    checked: &mut usize,
) {
    let network = network();
    for mv in position.legal_moves().iter() {
        let delta = delta(position, mv);
        position.make(mv);
        let mut child = Accumulator::default();
        network.apply(parent, &mut child, &delta, position, cache);
        let fresh = network.fresh(position);
        assert!(
            child.values == fresh.values && child.psqt == fresh.psqt,
            "{} after {}",
            position.fen(),
            position.format_move(mv, true)
        );
        *checked += 1;
        if depth > 1 {
            walk(position, &child, depth - 1, cache, checked);
        }
        position.unmake();
    }
}

#[test]
fn incremental_updates_match_fresh() {
    let suites = [
        include_str!("../../../tests/perft/standard.txt"),
        include_str!("../../../tests/perft/chess960.txt"),
    ];
    let mut checked = 0;
    let mut cache = RefreshCache::new();
    for fen in suites
        .iter()
        .flat_map(|suite| suite.lines())
        .step_by(2)
        .filter_map(|line| line.splitn(4, '|').nth(3))
    {
        let mut position = Position::from_fen(fen).expect(fen);
        let root = network().fresh(&position);
        walk(&mut position, &root, 3, &mut cache, &mut checked);
    }
    assert!(checked > 100_000, "{checked}");
}
