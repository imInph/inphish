use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

struct Engine {
    child: Child,
    lines: Receiver<String>,
}

impl Engine {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_inphish"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        Self { child, lines: rx }
    }

    fn send(&mut self, command: &str) {
        writeln!(self.child.stdin.as_mut().unwrap(), "{command}").unwrap();
        self.child.stdin.as_mut().unwrap().flush().unwrap();
    }

    fn until(&self, prefix: &str) -> String {
        for _ in 0..100 {
            let line = self.lines.recv_timeout(Duration::from_secs(2)).unwrap();
            if line.starts_with(prefix) {
                return line;
            }
        }
        panic!("expected {prefix}");
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn uci_smoke() {
    let mut engine = Engine::spawn();
    assert!(engine
        .lines
        .recv_timeout(Duration::from_millis(100))
        .is_err());
    engine.send("uci");
    assert_eq!(
        engine.until("id name"),
        concat!("id name inphish ", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(
        engine.until("option name Hash"),
        "option name Hash type spin default 16 min 1 max 1024"
    );
    assert_eq!(
        engine.until("option name Clear Hash"),
        "option name Clear Hash type button"
    );
    assert_eq!(engine.until("uciok"), "uciok");
    engine.send("setoption name Hash value 1");
    engine.send("setoption name Clear Hash");
    engine.send("isready");
    assert_eq!(engine.until("readyok"), "readyok");
    engine.send("position startpos moves e2e4 e7e5");
    engine.send("go depth 3");
    let best = engine.until("bestmove ");
    assert_ne!(best, "bestmove 0000");
    engine.send("go infinite");
    engine.send("isready");
    assert_eq!(engine.until("readyok"), "readyok");
    engine.send("stop");
    assert!(engine.until("bestmove ").starts_with("bestmove "));
    engine.send("go wtime 30 btime 30");
    assert!(engine.until("bestmove ").starts_with("bestmove "));
    engine.send("position fen 7k/6Q1/5K2/8/8/8/8/8 b - - 0 1");
    engine.send("go depth 2");
    assert_eq!(engine.until("bestmove "), "bestmove 0000");
    engine.send("quit");
    assert!(engine.child.wait().unwrap().success());
}

#[test]
fn uci_edge_cases() {
    let mut engine = Engine::spawn();
    engine.send("go depth 2");
    let opening = engine.until("bestmove ");
    assert!(matches!(
        &opening[9..10],
        "a" | "b" | "c" | "d" | "e" | "f" | "g" | "h"
    ));
    assert!(matches!(&opening[10..11], "1" | "2"), "{opening}");

    engine.send("position startpos moves e2e4 e2e4 d7d5");
    engine.send("go depth 2");
    let reply = engine.until("bestmove ");
    assert!(matches!(&reply[10..11], "7" | "8"), "{reply}");

    engine.send("position startpos");
    engine.send("go nodes 500");
    assert_ne!(engine.until("bestmove "), "bestmove 0000");
    engine.send("go movetime 50");
    assert_ne!(engine.until("bestmove "), "bestmove 0000");

    engine.send("go ponder wtime 1000 btime 1000");
    let deadline = std::time::Instant::now() + Duration::from_millis(300);
    while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
        if let Ok(line) = engine.lines.recv_timeout(left) {
            assert!(!line.starts_with("bestmove"), "ponder ended early: {line}");
        }
    }
    engine.send("ponderhit");
    assert_ne!(engine.until("bestmove "), "bestmove 0000");

    engine.send("go infinite");
    engine.send("quit");
    assert!(engine.until("bestmove ").starts_with("bestmove "));
    assert!(engine.child.wait().unwrap().success());
}

#[test]
fn uci_chess960_castling() {
    let mut engine = Engine::spawn();
    engine.send("uci");
    assert_eq!(
        engine.until("option name UCI_Chess960"),
        "option name UCI_Chess960 type check default false"
    );
    engine.until("uciok");

    let open = "rnbqk2r/pppppppp/8/8/8/8/PPPPPPPP/RNBQK2R w";
    engine.send(&format!("position fen {open} KQkq - 0 1"));
    engine.send("go depth 1 searchmoves e1g1");
    assert_eq!(engine.until("bestmove "), "bestmove e1g1");

    engine.send("setoption name UCI_Chess960 value true");
    for rights in ["KQkq", "HAha"] {
        engine.send(&format!("position fen {open} {rights} - 0 1"));
        engine.send("go depth 1 searchmoves e1h1");
        assert_eq!(engine.until("bestmove "), "bestmove e1h1");
        engine.send("go depth 1 searchmoves e1g1");
        assert_eq!(engine.until("bestmove "), "bestmove 0000");
    }

    // The king starts beside its rook, so the castle is only expressible as king takes rook.
    let adjacent = "rk5r/pppppppp/8/8/8/8/PPPPPPPP/RK5R w AHah - 0 1";
    engine.send(&format!("position fen {adjacent} moves b1a1 b8h8"));
    engine.send("go depth 1 searchmoves c1b1");
    assert_eq!(engine.until("bestmove "), "bestmove c1b1");
    engine.send(&format!("position fen {adjacent}"));
    engine.send("go depth 1 searchmoves b1a1");
    assert_eq!(engine.until("bestmove "), "bestmove b1a1");

    engine.send("setoption name UCI_Chess960 value false");
    engine.send(&format!("position fen {open} KQkq - 0 1"));
    engine.send("go depth 1 searchmoves e1g1");
    assert_eq!(engine.until("bestmove "), "bestmove e1g1");
    engine.send("quit");
    assert!(engine.child.wait().unwrap().success());
}

#[test]
fn uci_multipv_lines() {
    let mut engine = Engine::spawn();
    engine.send("setoption name MultiPV value 3");
    engine.send("position startpos moves e2e4");
    engine.send("go depth 5");
    let mut last = Vec::new();
    loop {
        let line = engine.lines.recv_timeout(Duration::from_secs(5)).unwrap();
        if line.starts_with("bestmove ") {
            let first = last
                .first()
                .map(|pv: &String| pv.split(' ').next().unwrap());
            assert_eq!(first, Some(&line[9..13]), "{line}");
            break;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let at = |key: &str| fields[fields.iter().position(|&word| word == key).unwrap() + 1];
        let index: usize = at("multipv").parse().unwrap();
        if index == 1 {
            last.clear();
        }
        assert_eq!(index, last.len() + 1, "{line}");
        let pv = fields[fields.iter().position(|&word| word == "pv").unwrap() + 1..].join(" ");
        last.push(pv);
    }
    assert_eq!(last.len(), 3);
    let mut heads: Vec<_> = last
        .iter()
        .map(|pv| pv.split(' ').next().unwrap())
        .collect();
    heads.sort_unstable();
    heads.dedup();
    assert_eq!(heads.len(), 3, "{last:?}");

    engine.send("setoption name MultiPV value 50");
    engine.send("position fen 7k/8/8/8/8/8/8/K7 w - - 0 1");
    engine.send("go depth 2");
    let mut most = 0;
    loop {
        let line = engine.lines.recv_timeout(Duration::from_secs(5)).unwrap();
        if line.starts_with("bestmove ") {
            break;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let index = fields.iter().position(|&word| word == "multipv").unwrap();
        most = most.max(fields[index + 1].parse::<usize>().unwrap());
    }
    assert_eq!(most, 3);
    engine.send("quit");
    assert!(engine.child.wait().unwrap().success());
}

#[test]
fn uci_show_wdl() {
    let mut engine = Engine::spawn();
    engine.send("go depth 2");
    assert!(!engine.until("info depth 2").contains(" wdl "));
    engine.until("bestmove ");
    engine.send("setoption name UCI_ShowWDL value true");
    engine.send("position fen k7/8/1QK5/8/8/8/8/8 w - - 0 1");
    engine.send("go depth 3");
    let info = engine.until("info depth 1");
    assert!(info.contains("score mate 1 wdl 1000 0 0 "), "{info}");
    engine.until("bestmove ");
    engine.send("position startpos");
    engine.send("go depth 2");
    let info = engine.until("info depth 2");
    let fields: Vec<&str> = info.split_whitespace().collect();
    let at = fields.iter().position(|&word| word == "wdl").unwrap();
    let sum: u32 = fields[at + 1..at + 4]
        .iter()
        .map(|value| value.parse::<u32>().unwrap())
        .sum();
    assert_eq!(sum, 1000, "{info}");
    engine.send("quit");
    assert!(engine.child.wait().unwrap().success());
}

#[test]
fn uci_threads() {
    let mut engine = Engine::spawn();
    engine.send("uci");
    assert_eq!(
        engine.until("option name Threads"),
        "option name Threads type spin default 1 min 1 max 256"
    );
    engine.until("uciok");
    engine.send("setoption name Threads value 4");
    engine.send("position startpos moves e2e4 c7c5");
    engine.send("go depth 6");
    assert_ne!(engine.until("bestmove "), "bestmove 0000");
    engine.send("go nodes 20000");
    let mut nodes = 0;
    loop {
        let line = engine.lines.recv_timeout(Duration::from_secs(5)).unwrap();
        if line.starts_with("bestmove ") {
            assert_ne!(line, "bestmove 0000");
            break;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        if let Some(at) = fields.iter().position(|&word| word == "nodes") {
            nodes = fields[at + 1].parse::<u64>().unwrap();
        }
    }
    // Helpers report nodes in batches of 1,024 and keep searching until the main thread
    // stops them, so on a busy machine the total can pass the limit by several batches.
    // The bound only catches a limit that is ignored outright.
    assert!((20_000..40_000).contains(&nodes), "{nodes}");
    engine.send("go infinite");
    engine.send("isready");
    assert_eq!(engine.until("readyok"), "readyok");
    engine.send("stop");
    assert_ne!(engine.until("bestmove "), "bestmove 0000");
    engine.send("position fen 7k/6Q1/5K2/8/8/8/8/8 b - - 0 1");
    engine.send("go depth 4");
    assert_eq!(engine.until("bestmove "), "bestmove 0000");
    engine.send("quit");
    assert!(engine.child.wait().unwrap().success());
}

#[test]
fn uci_limit_strength() {
    let mut engine = Engine::spawn();
    engine.send("uci");
    assert_eq!(
        engine.until("option name UCI_LimitStrength"),
        "option name UCI_LimitStrength type check default false"
    );
    assert_eq!(
        engine.until("option name UCI_Elo"),
        "option name UCI_Elo type spin default 2600 min 1320 max 2600"
    );
    engine.until("uciok");
    engine.send("setoption name UCI_LimitStrength value true");
    engine.send("setoption name UCI_Elo value 1");
    engine.send("position startpos moves e2e4");
    engine.send("go wtime 10000 btime 10000");
    let mut deepest_line = 0;
    loop {
        let line = engine.lines.recv_timeout(Duration::from_secs(5)).unwrap();
        if line.starts_with("bestmove ") {
            assert_ne!(line, "bestmove 0000");
            break;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let at = fields.iter().position(|&word| word == "multipv").unwrap();
        deepest_line = deepest_line.max(fields[at + 1].parse::<usize>().unwrap());
        let nodes = fields.iter().position(|&word| word == "nodes").unwrap();
        assert!(fields[nodes + 1].parse::<u64>().unwrap() <= 2_100, "{line}");
    }
    assert_eq!(deepest_line, 1);
    engine.send("quit");
    assert!(engine.child.wait().unwrap().success());
}
