#![no_main]

use inphzugzwang_core::Position;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        if let Ok(position) = Position::from_fen(text) {
            let round_trip = Position::from_fen(&position.fen()).expect("emitted FEN must parse");
            assert_eq!(position.key(), round_trip.key());
        }
    }
});
