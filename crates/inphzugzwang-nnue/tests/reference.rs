use inphzugzwang_core::Position;
use inphzugzwang_nnue::{network, Network};

const REFERENCE: &str = include_str!("../../../tests/nnue/reference.txt");

#[test]
fn matches_stockfish_13_exactly() {
    let network = network();
    let mut count = 0;
    for line in REFERENCE.lines().filter(|line| !line.starts_with('#')) {
        let (fen, expected) = line.split_once('|').expect(line);
        let position = Position::from_fen(fen).expect(fen);
        assert_eq!(
            network.evaluate_position(&position),
            expected.parse::<i32>().unwrap(),
            "{fen}"
        );
        count += 1;
    }
    assert!(count > 2000);
}

#[test]
fn rejects_damaged_files() {
    let bytes = include_bytes!("../net/nn-62ef826d1a6d.nnue");
    assert!(Network::parse(&bytes[..bytes.len() - 1]).is_err());
    let mut longer = bytes.to_vec();
    longer.push(0);
    assert!(Network::parse(&longer).is_err());
    let mut version = bytes.to_vec();
    version[0] ^= 1;
    assert!(Network::parse(&version).is_err());
}
