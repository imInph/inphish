use inphzugzwang_core::{Position, START_FEN};

#[test]
fn mutated_valid_fens_never_panic() {
    let seeds = [
        START_FEN.as_bytes(),
        b"bqnb1rkr/pp3ppp/3ppn2/2p5/5P2/P2P4/NPP1P1PP/BQ1BNRKR w HFhf - 2 9",
        b"k7/8/8/4KPpr/8/8/8/8 w - g6 0 1",
    ];
    let alphabet = b"0123456789abcdefghKQkqABCDEFGH/ -\t\n";
    let mut random = 0x17a4_b92f_8e61_3d05_u64;
    for index in 0..100_000 {
        let seed = seeds[index % seeds.len()];
        let mut bytes = seed.to_vec();
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        let at = random as usize % (bytes.len() + 1);
        let symbol = alphabet[(random >> 32) as usize % alphabet.len()];
        match random % 3 {
            0 if at < bytes.len() => bytes[at] = symbol,
            1 if at < bytes.len() => {
                bytes.remove(at);
            }
            _ => bytes.insert(at, symbol),
        }
        let text = std::str::from_utf8(&bytes).expect("ASCII mutation");
        if let Ok(position) = Position::from_fen(text) {
            let reloaded = Position::from_fen(&position.fen()).expect("emitted FEN");
            assert_eq!(position.key(), reloaded.key());
        }
    }
}
