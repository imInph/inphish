#![no_main]

use inphzugzwang_core::{Position, START_FEN};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        check(text);
    }
    let mut candidate = START_FEN.as_bytes().to_vec();
    for mutation in data.chunks_exact(2).take(8) {
        let index = mutation[0] as usize % candidate.len();
        candidate[index] = mutation[1];
    }
    if let Ok(text) = std::str::from_utf8(&candidate) {
        check(text);
    }
});

fn check(text: &str) {
    if let Ok(position) = Position::from_fen(text) {
        let round_trip = Position::from_fen(&position.fen()).expect("emitted FEN must parse");
        assert_eq!(position.key(), round_trip.key());
    }
}
