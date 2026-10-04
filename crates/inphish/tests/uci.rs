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
            let line = self.lines.recv_timeout(Duration::from_secs(10)).unwrap();
            if line.starts_with(prefix) {
                return line;
            }
        }
        panic!("expected {prefix}");
    }

    fn no_bestmove(&self, duration: Duration) {
        let deadline = std::time::Instant::now() + duration;
        while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
            if let Ok(line) = self.lines.recv_timeout(left) {
                assert!(!line.starts_with("bestmove"), "search ended early: {line}");
            }
        }
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
    engine.no_bestmove(Duration::from_millis(300));
    engine.send("ponderhit");
    assert_ne!(engine.until("bestmove "), "bestmove 0000");

    engine.send("go infinite");
    engine.send("quit");
    assert!(engine.until("bestmove ").starts_with("bestmove "));
    assert!(engine.child.wait().unwrap().success());
}

#[test]
fn uci_ponder_lifecycle() {
    let mut engine = Engine::spawn();
    engine.send("uci");
    assert_eq!(
        engine.until("option name Ponder"),
        "option name Ponder type check default false"
    );
    engine.until("uciok");
    engine.send("go depth 4");
    assert!(!engine.until("bestmove ").contains(" ponder "));
    engine.send("setoption name Ponder value true");
    engine.send("setoption name Clear Hash");
    engine.send("go depth 4");
    let best = engine.until("bestmove ");
    let words: Vec<_> = best.split_whitespace().collect();
    assert_eq!(words.len(), 4, "{best}");
    assert_eq!(words[2], "ponder");
    let mut position = inphzugzwang_core::Position::startpos();
    for word in [words[1], words[3]] {
        let mv = position
            .parse_move(word, false)
            .expect("legal predicted line");
        position.make(mv);
    }
    engine.send(&format!(
        "position startpos moves {} {}",
        words[1], words[3]
    ));
    // Even a completed depth-limited search must wait for the prediction to be confirmed.
    engine.send("go ponder depth 2 wtime 1000 btime 1000");
    engine.until("info depth 2");
    engine.no_bestmove(Duration::from_millis(100));
    engine.send("ponderhit");
    assert_ne!(engine.until("bestmove "), "bestmove 0000");
    // A missed prediction is stopped, then the GUI supplies the actual position.
    engine.send("go ponder wtime 1000 btime 1000");
    engine.until("info depth 1");
    engine.send("stop");
    engine.until("bestmove ");
    engine.send("position startpos moves d2d4");
    engine.send("go depth 2");
    assert_ne!(engine.until("bestmove "), "bestmove 0000");
    // Terminal positions obey the same command lifecycle.
    engine.send("position fen 7k/6Q1/5K2/8/8/8/8/8 b - - 0 1");
    engine.send("go ponder");
    engine.until("info depth 0");
    engine.no_bestmove(Duration::from_millis(100));
    engine.send("ponderhit");
    assert_eq!(engine.until("bestmove "), "bestmove 0000");
    engine.send("go infinite");
    engine.until("info depth 0");
    engine.no_bestmove(Duration::from_millis(100));
    engine.send("quit");
    assert_eq!(engine.until("bestmove "), "bestmove 0000");
    assert!(engine.child.wait().unwrap().success());
}

#[test]
fn uci_evalfile() {
    let mut engine = Engine::spawn();
    engine.send("uci");
    assert_eq!(
        engine.until("option name EvalFile"),
        "option name EvalFile type string default <empty>"
    );
    engine.until("uciok");
    engine.send("go depth 1");
    let baseline = engine.until("info depth 1");
    engine.until("bestmove ");
    let score = |line: &str| {
        let words: Vec<_> = line.split_whitespace().collect();
        let index = words.iter().position(|&word| word == "cp").unwrap();
        words[index + 1].parse::<i32>().unwrap()
    };
    let path = std::env::temp_dir().join(format!("inphish network {}.nnue", std::process::id()));
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../inphzugzwang-nnue/net/nn-252f33942263.nnue");
    let mut bytes = std::fs::read(source).unwrap();
    // Change the final layer stack's output bias, used in positions with 29-32 pieces.
    let bias = bytes.len() - 132;
    let value = i32::from_le_bytes(bytes[bias..bias + 4].try_into().unwrap()) + 1600;
    bytes[bias..bias + 4].copy_from_slice(&value.to_le_bytes());
    std::fs::write(&path, &bytes).unwrap();
    engine.send("go infinite");
    engine.until("info depth 1");
    engine.send(&format!("setoption name EvalFile value {}", path.display()));
    assert!(engine
        .until("info string loaded SFNNv17 network")
        .ends_with(&path.display().to_string()));
    engine.send("isready");
    engine.until("readyok");
    // Loading new weights does not terminate a search using the previous network.
    engine.no_bestmove(Duration::from_millis(50));
    engine.send("stop");
    engine.until("bestmove ");
    engine.send("go depth 1");
    let external = engine.until("info depth 1");
    engine.until("bestmove ");
    assert_ne!(score(&external), score(&baseline));
    // A failed replacement leaves the external network selected.
    bytes[4] ^= 1;
    std::fs::write(&path, &bytes).unwrap();
    engine.send(&format!("setoption name EvalFile value {}", path.display()));
    assert!(engine
        .until("info string network not loaded")
        .contains("unsupported network architecture"));
    engine.send("setoption name Clear Hash");
    engine.send("go depth 1");
    assert_eq!(score(&engine.until("info depth 1")), score(&external));
    engine.until("bestmove ");
    engine.send("setoption name EvalFile value /nonexistent/network.nnue");
    assert!(engine
        .until("info string network not loaded")
        .contains("keeping previous network"));
    engine.send("setoption name EvalFile value <empty>");
    engine.until("info string using bundled SFNNv17 network");
    engine.send("go depth 1");
    assert_eq!(score(&engine.until("info depth 1")), score(&baseline));
    engine.until("bestmove ");
    engine.send("quit");
    assert!(engine.child.wait().unwrap().success());
    std::fs::remove_file(path).unwrap();
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
    // Every thread adds its nodes to a shared total in batches of 1,024 and stops once the
    // total reaches the limit, so it can pass the limit by at most about a batch per thread,
    // however the threads are scheduled.
    assert!((20_000..20_000 + 5 * 1024).contains(&nodes), "{nodes}");
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
        "option name UCI_Elo type spin default 3000 min 1320 max 3000"
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

#[test]
fn uci_syzygy_path() {
    let mut engine = Engine::spawn();
    engine.send("uci");
    assert_eq!(
        engine.until("option name SyzygyPath"),
        "option name SyzygyPath type string default <empty>"
    );
    engine.until("uciok");
    engine.send("setoption name SyzygyPath value /nonexistent/tables with space");
    assert_eq!(
        engine.until("info string"),
        "info string found 0 tablebases"
    );
    engine.send("position fen 8/8/8/4k3/8/8/2KQ4/8 w - - 0 1");
    engine.send("go depth 4");
    assert!(engine.until("info depth 4").contains(" tbhits 0 "));
    engine.until("bestmove ");
    if let Ok(path) = std::env::var("SYZYGY_PATH") {
        engine.send(&format!("setoption name SyzygyPath value {path}"));
        assert_eq!(
            engine.until("info string"),
            "info string found 145 tablebases"
        );
        engine.send("go depth 6");
        let info = engine.until("info depth 6");
        assert!(info.contains(" score cp 29871 "), "{info}");
        engine.until("bestmove ");
        // Six pieces: the search reaches the tables through captures.
        engine.send("position fen 8/8/3k4/8/2r5/2K2R2/5P2/7q w - - 0 1");
        engine.send("go depth 8");
        let info = engine.until("info depth 8");
        let at = info.find(" tbhits ").unwrap() + 8;
        let hits: u64 = info[at..].split(' ').next().unwrap().parse().unwrap();
        assert!(hits > 0, "{info}");
        engine.until("bestmove ");
    }
    engine.send("quit");
    assert!(engine.child.wait().unwrap().success());
}

#[test]
fn uci_own_book() {
    let mut engine = Engine::spawn();
    engine.send("uci");
    assert_eq!(
        engine.until("option name OwnBook"),
        "option name OwnBook type check default false"
    );
    assert_eq!(
        engine.until("option name BookFile"),
        "option name BookFile type string default <empty>"
    );
    assert_eq!(
        engine.until("option name Book Depth"),
        "option name Book Depth type spin default 40 min 1 max 200"
    );
    assert_eq!(
        engine.until("option name Book Best Move"),
        "option name Book Best Move type check default false"
    );
    engine.until("uciok");
    // Off by default: the engine searches.
    engine.send("position startpos");
    engine.send("go depth 1");
    engine.until("info depth 1");
    engine.until("bestmove ");
    // The bundled book answers at once.
    engine.send("setoption name OwnBook value true");
    engine.send("setoption name Book Best Move value true");
    engine.send("go wtime 60000 btime 60000");
    let book = engine.until("info string book move ");
    let best = engine.until("bestmove ");
    assert_eq!(
        book["info string book move ".len()..],
        best["bestmove ".len()..]
    );
    // Analysis and positions past the book depth search instead.
    engine.send("go depth 1 infinite");
    engine.until("info depth 1");
    engine.send("stop");
    engine.until("bestmove ");
    engine.send("setoption name Book Depth value 1");
    engine.send("position startpos moves e2e4");
    engine.send("go depth 1");
    engine.until("info depth 1");
    engine.until("bestmove ");
    // A book file of one entry: 1. Nf3 from the start.
    let path = std::env::temp_dir().join(format!("inphish-book-{}.bin", std::process::id()));
    let mut entry = 0x463b_9618_1691_fc9c_u64.to_be_bytes().to_vec();
    entry.extend(((6_u16 << 6) | 21).to_be_bytes());
    entry.extend([0, 1, 0, 0, 0, 0]);
    std::fs::write(&path, entry).unwrap();
    engine.send(&format!("setoption name BookFile value {}", path.display()));
    assert_eq!(
        engine.until("info string"),
        "info string loaded book with 1 entries"
    );
    engine.send("position startpos");
    engine.send("go wtime 60000 btime 60000");
    assert_eq!(engine.until("bestmove "), "bestmove g1f3");
    std::fs::remove_file(&path).unwrap();
    engine.send("setoption name BookFile value /nonexistent/book.bin");
    assert!(engine
        .until("info string")
        .starts_with("info string book not loaded"));
    engine.send("quit");
    assert!(engine.child.wait().unwrap().success());
}
