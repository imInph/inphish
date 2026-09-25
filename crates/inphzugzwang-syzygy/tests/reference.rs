//! Compares probes with Stockfish 13 on `tests/syzygy/reference.txt`. The tables are not in
//! the repository; set `SYZYGY_PATH` to a directory with the 3-5 piece WDL and DTZ files.

use inphzugzwang_core::Position;
use inphzugzwang_syzygy::{ProbeState, Tablebases};

const REFERENCE: &str = include_str!("../../../tests/syzygy/reference.txt");

#[test]
#[ignore = "needs SYZYGY_PATH with the 3-5 piece tables"]
fn matches_stockfish_13() {
    let path = std::env::var("SYZYGY_PATH").expect("SYZYGY_PATH is set");
    let tables = Tablebases::open(&path);
    assert_eq!(tables.len(), 145);
    assert_eq!(tables.max_pieces(), 5);
    let mut compared = 0;
    for line in REFERENCE.lines().filter(|line| !line.starts_with('#')) {
        let fields: Vec<&str> = line.split('|').collect();
        let mut position = Position::from_fen(fields[0]).expect(fields[0]);
        let before = position.fen();
        let (wdl, wdl_state) = tables.probe_wdl(&mut position);
        let (dtz, dtz_state) = tables.probe_dtz(&mut position);
        assert_eq!(position.fen(), before);
        // Stockfish 13 fails a few probes it should answer; those are only run for crashes.
        if fields[2] == "0" {
            continue;
        }
        assert_ne!(wdl_state, ProbeState::Fail, "{line}");
        assert_ne!(dtz_state, ProbeState::Fail, "{line}");
        assert_eq!(wdl.to_string(), fields[1], "wdl {line}");
        assert_eq!(dtz.to_string(), fields[3], "dtz {line}");
        compared += 1;
    }
    assert!(compared > 3900, "{compared}");
}
