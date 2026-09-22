use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use inphzugzwang_core::Position;
use inphzugzwang_search::{search, Control, Limits};

const STANDARD: &str = include_str!("../../../tests/perft/standard.txt");
const CHESS960: &str = include_str!("../../../tests/perft/chess960.txt");
const STARTS: &str = include_str!("../../../tests/perft/chess960_starts.txt");

pub fn run(depth: u8) -> Result<(), String> {
    let (nodes, nps) = measure(depth)?;
    println!("{nodes} nodes {nps} nps");
    Ok(())
}

pub fn measure(depth: u8) -> Result<(u64, u64), String> {
    let started = Instant::now();
    let mut nodes = 0;
    let control = Control {
        stop: Arc::new(AtomicBool::new(false)),
        ponderhit: Arc::new(AtomicBool::new(false)),
    };
    for fen in STANDARD
        .lines()
        .step_by(2)
        .chain(CHESS960.lines().step_by(2))
        .chain(STARTS.lines().take(33))
        .filter_map(|line| line.splitn(4, '|').nth(3))
    {
        let position = Position::from_fen(fen).map_err(|error| error.to_string())?;
        let result = search(
            position,
            Limits {
                depth: Some(depth),
                ..Limits::default()
            },
            &control,
            |_| {},
        );
        nodes += result.info.nodes;
    }
    let elapsed = started.elapsed().as_millis().max(1) as u64;
    Ok((nodes, nodes.saturating_mul(1000) / elapsed))
}
