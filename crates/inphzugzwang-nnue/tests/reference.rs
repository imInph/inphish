use inphzugzwang_core::Position;
use inphzugzwang_nnue::{network, AccumulatorStack, Network};

const REFERENCE: &str = include_str!("../../../tests/nnue/reference.txt");

#[test]
fn matches_stockfish_19_exactly() {
    let network = network();
    let mut count = 0;
    for line in REFERENCE.lines().filter(|line| !line.starts_with('#')) {
        let fields: Vec<&str> = line.split('|').collect();
        let [fen, psqt, positional, evaluation] = fields[..] else {
            panic!("{line}");
        };
        let position = Position::from_fen(fen).expect(fen);
        let output = network.evaluate_position(&position);
        assert_eq!(output.psqt, psqt.parse::<i32>().unwrap(), "{fen}");
        assert_eq!(
            output.positional,
            positional.parse::<i32>().unwrap(),
            "{fen}"
        );
        if evaluation != "-" {
            assert_eq!(
                output.evaluation(&position),
                evaluation.parse::<i32>().unwrap(),
                "{fen}"
            );
        }
        count += 1;
    }
    assert!(count > 2000);
}

#[test]
fn rejects_damaged_files() {
    let bytes = include_bytes!("../net/nn-1a298aa575a0.nnue");
    assert!(Network::parse(&bytes[..bytes.len() - 1]).is_err());
    let mut longer = bytes.to_vec();
    longer.push(0);
    assert!(Network::parse(&longer).is_err());
    let mut version = bytes.to_vec();
    version[0] ^= 1;
    assert!(Network::parse(&version).is_err());
}

/// Walks every line to a fixed depth with the accumulator stack, as the search makes and
/// takes back moves, and checks the accumulators against ones computed from scratch.
/// Positions are checked at the leaves and at every third inner node, so that updates run
/// over several moves at once, forwards and after king moves backwards.
fn walk(
    position: &mut Position,
    stack: &mut AccumulatorStack,
    depth: u8,
    counter: &mut usize,
    checked: &mut usize,
) {
    let network = network();
    for mv in position.legal_moves().iter() {
        position.make_recorded(mv, stack.push());
        *counter += 1;
        if depth == 1 || (*counter).is_multiple_of(3) {
            let fresh = network.fresh(position);
            let current = stack.current(position);
            assert!(
                current.values == fresh.values && current.psqt == fresh.psqt,
                "{} after {}",
                position.fen(),
                position.format_move(mv, true)
            );
            *checked += 1;
        }
        if depth > 1 {
            walk(position, stack, depth - 1, counter, checked);
        }
        position.unmake();
        stack.pop();
    }
}

#[test]
fn incremental_updates_match_fresh() {
    let suites = [
        include_str!("../../../tests/perft/standard.txt"),
        include_str!("../../../tests/perft/chess960.txt"),
    ];
    let mut checked = 0;
    let mut counter = 0;
    let mut stack = AccumulatorStack::new();
    for fen in suites
        .iter()
        .flat_map(|suite| suite.lines())
        .step_by(2)
        .filter_map(|line| line.splitn(4, '|').nth(3))
    {
        let mut position = Position::from_fen(fen).expect(fen);
        stack.reset(&position);
        walk(&mut position, &mut stack, 3, &mut counter, &mut checked);
    }
    assert!(checked > 100_000, "{checked}");
}
