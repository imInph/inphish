use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use inphzugzwang_core::{Color, Position};
use inphzugzwang_search::{
    search_with_table, uci_score, Control, Info, Limits, TranspositionTable,
};

use crate::bench;

enum Event {
    Input(String),
    Eof,
    Info(u64, Info),
    Done(u64, inphzugzwang_search::Result),
}

struct Active {
    id: u64,
    control: Control,
    handle: JoinHandle<()>,
    position: Position,
    chess960: bool,
}

struct Engine {
    position: Position,
    active: Option<Active>,
    next_id: u64,
    pending: Option<Limits>,
    quitting: bool,
    overhead: u64,
    hash_mb: u32,
    hash: Arc<TranspositionTable>,
    chess960: bool,
    debug: bool,
}

pub fn run() -> io::Result<()> {
    let (tx, rx) = mpsc::channel();
    let input_tx = tx.clone();
    let _input = thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            match line {
                Ok(line) => {
                    if input_tx.send(Event::Input(line)).is_err() {
                        return;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = input_tx.send(Event::Eof);
    });
    let mut engine = Engine {
        position: Position::startpos(),
        active: None,
        next_id: 0,
        pending: None,
        quitting: false,
        overhead: 20,
        hash_mb: 16,
        hash: Arc::new(TranspositionTable::new(16).expect("default hash allocation failed")),
        chess960: false,
        debug: false,
    };
    let mut output = io::stdout().lock();
    while let Ok(event) = rx.recv() {
        match event {
            Event::Input(line) => engine.command(&line, &tx, &mut output)?,
            Event::Eof => {
                engine.quitting = true;
                if let Some(active) = &engine.active {
                    active.control.stop.store(true, Ordering::Relaxed);
                }
            }
            Event::Info(id, info) => {
                if let Some(active) = &engine.active {
                    if active.id == id {
                        write_line(
                            &mut output,
                            &format_info(&active.position, active.chess960, &info),
                        )?;
                    }
                }
            }
            Event::Done(id, result) => {
                if engine.active.as_ref().is_some_and(|active| active.id == id) {
                    let active = engine.active.take().expect("matching search");
                    let _ = active.handle.join();
                    let best = result.best.map_or_else(
                        || "0000".to_owned(),
                        |mv| active.position.format_move(mv, active.chess960),
                    );
                    let mut line = format!("bestmove {best}");
                    if result.info.pv.len() > 1 {
                        let mut after = active.position.clone();
                        after.make(result.info.pv[0]);
                        line.push_str(&format!(
                            " ponder {}",
                            after.format_move(result.info.pv[1], active.chess960)
                        ));
                    }
                    write_line(&mut output, &line)?;
                    if !engine.quitting {
                        if let Some(mut limits) = engine.pending.take() {
                            limits.started = Some(Instant::now());
                            engine.start(limits, &tx, &mut output)?;
                        }
                    }
                }
            }
        }
        if engine.quitting && engine.active.is_none() {
            break;
        }
    }
    Ok(())
}

impl Engine {
    fn command(&mut self, line: &str, tx: &Sender<Event>, out: &mut impl Write) -> io::Result<()> {
        let words: Vec<_> = line.split_whitespace().collect();
        let Some(command) = words.first().copied() else {
            return Ok(());
        };
        match command {
            "uci" => {
                write_line(out, concat!("id name inphish ", env!("CARGO_PKG_VERSION")))?;
                write_line(out, "id author Kaevo")?;
                write_line(
                    out,
                    "option name Move Overhead type spin default 20 min 0 max 5000",
                )?;
                write_line(out, "option name Hash type spin default 16 min 1 max 1024")?;
                write_line(out, "option name Clear Hash type button")?;
                write_line(out, "option name UCI_Chess960 type check default false")?;
                write_line(out, "uciok")?;
            }
            "isready" => write_line(out, "readyok")?,
            "ucinewgame" => {
                self.position = Position::startpos();
                self.stop();
                self.clear_hash();
            }
            "position" => {
                if let Some(position) = parse_position(&words[1..], self.chess960) {
                    self.position = position;
                }
            }
            "go" => {
                let started = Instant::now();
                let mut limits =
                    parse_go(&words[1..], &self.position, self.overhead, self.chess960);
                limits.started = Some(started);
                if self.active.is_some() {
                    self.stop();
                    self.pending = Some(limits);
                } else {
                    self.start(limits, tx, out)?;
                }
            }
            "stop" => self.stop(),
            "ponderhit" => {
                if let Some(active) = &self.active {
                    active.control.ponderhit.store(true, Ordering::Relaxed);
                }
            }
            "quit" => {
                self.pending = None;
                self.quitting = true;
                self.stop();
            }
            "setoption" => self.setoption(&words[1..]),
            "debug" => self.debug = words.get(1) == Some(&"on"),
            "d" if self.debug => eprintln!("{}", self.position.fen()),
            "bench" => {
                let depth = words
                    .get(1)
                    .and_then(|word| word.parse::<u8>().ok())
                    .unwrap_or(4);
                if let Ok((nodes, nps)) = bench::measure(depth.max(1)) {
                    eprintln!("{nodes} nodes {nps} nps");
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn start(
        &mut self,
        limits: Limits,
        tx: &Sender<Event>,
        out: &mut impl Write,
    ) -> io::Result<()> {
        if limits.immediate {
            let legal = self.position.legal_moves();
            let best = legal
                .iter()
                .find(|mv| !limits.searchmoves_only || limits.searchmoves.contains(mv));
            let label = best.map_or_else(
                || "0000".to_owned(),
                |mv| self.position.format_move(mv, self.chess960),
            );
            let score = if best.is_none() && self.position.checkers().0 != 0 {
                "mate 0".to_owned()
            } else {
                "cp 0".to_owned()
            };
            let suffix = if best.is_some() {
                format!(" {label}")
            } else {
                String::new()
            };
            write_line(out, &format!("info depth 0 seldepth 0 multipv 1 score {score} nodes 0 nps 0 hashfull 0 tbhits 0 time 0 pv{suffix}"))?;
            write_line(out, &format!("bestmove {label}"))?;
            return Ok(());
        }
        self.next_id += 1;
        let id = self.next_id;
        let position = self.position.clone();
        let active_position = self.position.clone();
        let control = Control {
            stop: Arc::new(AtomicBool::new(false)),
            ponderhit: Arc::new(AtomicBool::new(false)),
        };
        let worker_control = Control {
            stop: control.stop.clone(),
            ponderhit: control.ponderhit.clone(),
        };
        let worker_tx = tx.clone();
        let hash = self.hash.clone();
        let handle = thread::Builder::new()
            .name("inphish-search".to_owned())
            .stack_size(16 * 1024 * 1024)
            .spawn(move || {
                let result = search_with_table(position, limits, &worker_control, &hash, |info| {
                    let _ = worker_tx.send(Event::Info(id, info));
                });
                let _ = worker_tx.send(Event::Done(id, result));
            })
            .expect("search thread could not start");
        self.active = Some(Active {
            id,
            control,
            handle,
            position: active_position,
            chess960: self.chess960,
        });
        Ok(())
    }

    fn stop(&self) {
        if let Some(active) = &self.active {
            active.control.stop.store(true, Ordering::Relaxed);
        }
    }

    fn setoption(&mut self, words: &[&str]) {
        if !words
            .first()
            .is_some_and(|word| word.eq_ignore_ascii_case("name"))
        {
            return;
        }
        let value_at = words
            .iter()
            .position(|word| word.eq_ignore_ascii_case("value"));
        let name = words[1..value_at.unwrap_or(words.len())]
            .join(" ")
            .to_ascii_lowercase();
        let value = value_at.map_or("", |index| words.get(index + 1).copied().unwrap_or(""));
        if name == "move overhead" {
            if let Ok(ms) = value.parse::<u64>() {
                self.overhead = ms.min(5000);
            }
        } else if name == "hash" {
            if let Ok(megabytes) = value.parse::<u32>() {
                let megabytes = megabytes.clamp(1, 1024);
                if let Some(table) = TranspositionTable::new(megabytes) {
                    self.hash_mb = megabytes;
                    self.hash = Arc::new(table);
                }
            }
        } else if name == "clear hash" {
            self.clear_hash();
        } else if name == "uci_chess960" {
            if let Ok(enabled) = value.to_ascii_lowercase().parse::<bool>() {
                self.chess960 = enabled;
            }
        }
    }

    fn clear_hash(&mut self) {
        if let Some(table) = TranspositionTable::new(self.hash_mb) {
            self.hash = Arc::new(table);
        }
    }
}

fn parse_position(words: &[&str], chess960: bool) -> Option<Position> {
    let mut index = 0;
    let mut position = match words.first().copied()? {
        "startpos" => {
            index += 1;
            Position::startpos()
        }
        "fen" => {
            if words.len() < 7 {
                return None;
            }
            index += 7;
            Position::from_fen(&words[1..7].join(" ")).ok()?
        }
        _ => return None,
    };
    if words.get(index) == Some(&"moves") {
        for text in &words[index + 1..] {
            let Some(mv) = position.parse_move(text, chess960) else {
                break;
            };
            position.make(mv);
        }
    }
    Some(position)
}

fn parse_go(words: &[&str], position: &Position, overhead: u64, chess960: bool) -> Limits {
    let mut limits = Limits::default();
    let mut wtime = None;
    let mut btime = None;
    let mut winc = 0;
    let mut binc = 0;
    let mut movestogo = 30;
    let mut movetime = None;
    let mut index = 0;
    while index < words.len() {
        let key = words[index];
        if key == "searchmoves" {
            limits.searchmoves_only = true;
            index += 1;
            while index < words.len() && !is_go_key(words[index]) {
                if let Some(mv) = position.parse_move(words[index], chess960) {
                    if !limits.searchmoves.contains(&mv) {
                        limits.searchmoves.push(mv);
                    }
                }
                index += 1;
            }
            continue;
        }
        if key == "infinite" {
            limits.infinite = true;
            index += 1;
            continue;
        }
        if key == "ponder" {
            limits.ponder = true;
            index += 1;
            continue;
        }
        let value = words
            .get(index + 1)
            .and_then(|word| word.parse::<u64>().ok());
        match key {
            "wtime" => wtime = value,
            "btime" => btime = value,
            "winc" => winc = value.unwrap_or(0),
            "binc" => binc = value.unwrap_or(0),
            "movestogo" => movestogo = value.unwrap_or(30).clamp(1, 100),
            "depth" => limits.depth = value.map(|n| n.clamp(1, 127) as u8),
            "nodes" => limits.nodes = value.map(|n| n.max(1)),
            "mate" => limits.depth = value.map(|n| n.saturating_mul(2).clamp(1, 127) as u8),
            "movetime" => movetime = value,
            _ => {}
        }
        index += if value.is_some() { 2 } else { 1 };
    }
    if !limits.infinite {
        if let Some(ms) = movetime {
            let budget = ms.saturating_sub(overhead).max(1);
            limits.immediate = !limits.ponder && ms <= 30;
            limits.soft = Some(Duration::from_millis(budget.saturating_mul(3) / 4));
            limits.hard = Some(Duration::from_millis(budget));
        } else {
            let clock = if position.side_to_move() == Color::White {
                wtime
            } else {
                btime
            };
            let increment = if position.side_to_move() == Color::White {
                winc
            } else {
                binc
            };
            if let Some(remaining) = clock {
                // Spawning the search thread costs microseconds, so only a clock already inside the
                // overhead margin skips the search; anything more still buys a few plies.
                limits.immediate = !limits.ponder && remaining <= overhead.saturating_add(30);
                let safe = remaining.saturating_sub(overhead).max(1);
                let target = (safe / movestogo).saturating_add(increment.saturating_mul(3) / 4);
                let soft = target.clamp(1, (safe.saturating_mul(2) / 5).max(1));
                let hard = target
                    .saturating_mul(3)
                    .clamp(soft, (safe.saturating_mul(3) / 4).max(soft));
                limits.soft = Some(Duration::from_millis(soft));
                limits.hard = Some(Duration::from_millis(hard));
            }
        }
    }
    limits
}

fn is_go_key(word: &str) -> bool {
    matches!(
        word,
        "wtime"
            | "btime"
            | "winc"
            | "binc"
            | "movestogo"
            | "depth"
            | "nodes"
            | "mate"
            | "movetime"
            | "infinite"
            | "ponder"
    )
}

fn format_info(position: &Position, chess960: bool, info: &Info) -> String {
    let millis = info.elapsed.as_millis() as u64;
    let nps = info.nodes.saturating_mul(1000) / millis.max(1);
    let mut line = format!(
        "info depth {} seldepth {} multipv 1 score {} nodes {} nps {} hashfull {} tbhits 0 time {} pv",
        info.depth, info.seldepth, uci_score(info.score), info.nodes, nps, info.hashfull, millis
    );
    let mut after = position.clone();
    for &mv in &info.pv {
        line.push(' ');
        line.push_str(&after.format_move(mv, chess960));
        after.make(mv);
    }
    line
}

fn write_line(out: &mut impl Write, line: &str) -> io::Result<()> {
    writeln!(out, "{line}")?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_stops_on_illegal_move() {
        let pos = parse_position(
            &["startpos", "moves", "e2e4", "e7e5", "e2e4", "g1f3"],
            false,
        )
        .unwrap();
        let expected = parse_position(&["startpos", "moves", "e2e4", "e7e5"], false).unwrap();
        assert_eq!(pos.fen(), expected.fen());
    }

    #[test]
    fn go_parses_clock_and_searchmoves() {
        let pos = Position::startpos();
        let limits = parse_go(
            &[
                "wtime",
                "60000",
                "btime",
                "60000",
                "winc",
                "100",
                "searchmoves",
                "e2e4",
                "d2d4",
            ],
            &pos,
            20,
            false,
        );
        assert!(limits.hard.is_some());
        assert_eq!(limits.searchmoves.len(), 2);
    }

    #[test]
    fn malformed_commands_do_not_panic() {
        let position = Position::startpos();
        let tokens = [
            "name",
            "value",
            "fen",
            "moves",
            "startpos",
            "go",
            "depth",
            "mate",
            "wtime",
            "searchmoves",
            "99999999999999999999",
            "-1",
            "",
            "e2e4",
            "x",
            "0",
        ];
        let mut state = 0x6543_9876_u64;
        for _ in 0..10_000 {
            let len = (state % 20) as usize;
            let mut words = Vec::with_capacity(len);
            for _ in 0..len {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                words.push(tokens[(state as usize) % tokens.len()]);
            }
            let _ = parse_position(&words, false);
            let _ = parse_go(&words, &position, 20, false);
        }
    }

    #[test]
    fn tiny_clocks_and_large_values_are_bounded() {
        let position = Position::startpos();
        assert!(parse_go(&["wtime", "50"], &position, 20, false).immediate);
        assert!(!parse_go(&["wtime", "51"], &position, 20, false).immediate);
        for words in [
            &["wtime", "0", "winc", "0"][..],
            &[
                "wtime",
                "18446744073709551615",
                "winc",
                "18446744073709551615",
            ][..],
            &["mate", "18446744073709551615"][..],
        ] {
            let _ = parse_go(words, &position, 20, false);
        }
    }
}
