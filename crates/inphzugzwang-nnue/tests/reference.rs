use inphzugzwang_core::Position;
use inphzugzwang_nnue::{network, AccumulatorStack, Network};

const REFERENCE: &str = include_str!("../../../tests/nnue/reference.txt");

#[test]
fn matches_stockfish_dev_exactly() {
    let network = network();
    check_reference(network);
}

fn check_reference(network: &Network) {
    let mut count = 0;
    for line in REFERENCE.lines().filter(|line| !line.starts_with('#')) {
        let fields: Vec<&str> = line.split('|').collect();
        let [fen, positional, evaluation] = fields[..] else {
            panic!("{line}");
        };
        let position = Position::from_fen(fen).expect(fen);
        let output = network.evaluate_position(&position);
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
fn external_network_matches_reference() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("net/nn-252f33942263.nnue");
    let network = Network::load(path).unwrap();
    check_reference(&network);
}

#[test]
fn rejects_damaged_files() {
    let bytes = include_bytes!("../net/nn-252f33942263.nnue");
    assert!(Network::parse(&bytes[..bytes.len() - 1]).is_err());
    let mut longer = bytes.to_vec();
    longer.push(0);
    assert!(Network::parse(&longer).is_err());
    let mut version = bytes.to_vec();
    version[0] ^= 1;
    assert!(Network::parse(&version).is_err());
    version[0] ^= 1;
    let word = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
    let transformer = 12 + word(8);
    let bias = transformer + 4;
    let psq = bias + 21 + word(bias + 17) + (59_808 + 4560) * 1024;
    let stack = psq + 21 + word(psq + 17);
    for offset in [4, transformer, stack] {
        version[offset] ^= 1;
        assert!(Network::parse(&version).is_err(), "hash at {offset}");
        version[offset] ^= 1;
    }
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
                current.values == fresh.values,
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
