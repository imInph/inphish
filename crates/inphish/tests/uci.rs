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
    assert_eq!(engine.until("id name"), "id name inphish 0.1.0");
    assert_eq!(engine.until("uciok"), "uciok");
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
