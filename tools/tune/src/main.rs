use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs;
use std::process::ExitCode;

use inphzugzwang_core::{Move, PieceType, Position, START_FEN};
use inphzugzwang_eval::{trace, traced_score, weights, Trace, KING_ZONE, MAX_PHASE, PARAMS, PST};

// Opening-book plies say nothing about the evaluation, and neighbouring plies of one game
// are nearly the same sample, so positions are taken sparsely after the opening.
const SKIP_PLIES: usize = 16;
const PLY_STRIDE: usize = 3;
const VALIDATION_EVERY: usize = 10;

struct Sample {
    trace: Trace,
    result: f64,
}

struct Options {
    output: String,
    epochs: usize,
    rate: f64,
    prior: f64,
    pgns: Vec<String>,
}

fn main() -> ExitCode {
    let options = match parse_args(std::env::args().skip(1).collect()) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            eprintln!("usage: inphzugzwang-tune [--epochs N] [--rate R] [--prior L] OUTPUT_WEIGHTS_RS PGN...");
            return ExitCode::from(2);
        }
    };
    let mut seen = HashSet::new();
    let mut samples = Vec::new();
    let mut games = 0;
    for path in &options.pgns {
        let Ok(text) = fs::read_to_string(path) else {
            eprintln!("cannot read {path}");
            return ExitCode::from(2);
        };
        for game in split_games(&text) {
            if collect_game(&game, &mut seen, &mut samples) {
                games += 1;
            }
        }
    }
    let (training, validation): (Vec<_>, Vec<_>) = samples
        .into_iter()
        .enumerate()
        .partition(|(index, _)| index % VALIDATION_EVERY != 0);
    let training: Vec<Sample> = training.into_iter().map(|(_, sample)| sample).collect();
    let validation: Vec<Sample> = validation.into_iter().map(|(_, sample)| sample).collect();
    eprintln!(
        "{games} games, {} training and {} validation positions",
        training.len(),
        validation.len()
    );
    if training.is_empty() {
        eprintln!("no usable positions");
        return ExitCode::from(1);
    }

    let mut params: Vec<(f64, f64)> = weights()
        .iter()
        .map(|&(mg, eg)| (f64::from(mg), f64::from(eg)))
        .collect();
    let k = fit_scale(&training, &params);
    eprintln!(
        "K {k:.3}; initial loss training {:.6} validation {:.6}",
        loss(&training, &params, k),
        loss(&validation, &params, k)
    );
    adam(&training, &mut params, k, &options);
    eprintln!(
        "final loss training {:.6} validation {:.6}",
        loss(&training, &params, k),
        loss(&validation, &params, k)
    );
    if let Err(error) = fs::write(&options.output, render(&params)) {
        eprintln!("cannot write {}: {error}", options.output);
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

fn parse_args(args: Vec<String>) -> Result<Options, String> {
    let mut epochs = 2000;
    let mut rate = 1.0;
    let mut prior = 1e-7;
    let mut rest = Vec::new();
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--epochs" => {
                epochs = iter
                    .next()
                    .and_then(|value| value.parse().ok())
                    .ok_or("--epochs needs a number")?;
            }
            "--prior" => {
                prior = iter
                    .next()
                    .and_then(|value| value.parse().ok())
                    .ok_or("--prior needs a number")?;
            }
            "--rate" => {
                rate = iter
                    .next()
                    .and_then(|value| value.parse().ok())
                    .ok_or("--rate needs a number")?;
            }
            _ => rest.push(arg),
        }
    }
    if rest.len() < 2 {
        return Err("need an output path and at least one PGN".to_owned());
    }
    let output = rest.remove(0);
    Ok(Options {
        output,
        epochs,
        rate,
        prior,
        pgns: rest,
    })
}

fn split_games(text: &str) -> Vec<String> {
    let mut games = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        if line.starts_with("[Event ") && !current.trim().is_empty() {
            games.push(std::mem::take(&mut current));
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.trim().is_empty() {
        games.push(current);
    }
    games
}

fn header<'a>(game: &'a str, name: &str) -> Option<&'a str> {
    let prefix = format!("[{name} \"");
    game.lines()
        .find_map(|line| line.strip_prefix(prefix.as_str()))
        .and_then(|rest| rest.strip_suffix("\"]"))
}

fn movetext(game: &str) -> String {
    let mut text = String::new();
    let mut depth = 0;
    for line in game.lines().filter(|line| !line.starts_with('[')) {
        for character in line.chars() {
            match character {
                '{' => depth += 1,
                '}' => depth = (depth - 1).max(0),
                _ if depth == 0 => text.push(character),
                _ => {}
            }
        }
        text.push(' ');
    }
    text
}

fn collect_game(game: &str, seen: &mut HashSet<u64>, samples: &mut Vec<Sample>) -> bool {
    let result = match header(game, "Result") {
        Some("1-0") => 1.0,
        Some("0-1") => 0.0,
        Some("1/2-1/2") => 0.5,
        _ => return false,
    };
    // A game lost on time or by a crash says nothing about who stood better.
    if !matches!(
        header(game, "Termination"),
        None | Some("normal") | Some("adjudication")
    ) {
        return false;
    }
    let fen = header(game, "FEN").unwrap_or(START_FEN);
    let fen = if fen.split_whitespace().count() == 4 {
        format!("{fen} 0 1")
    } else {
        fen.to_owned()
    };
    let Ok(mut position) = Position::from_fen(&fen) else {
        return false;
    };
    for (ply, token) in movetext(game)
        .split_whitespace()
        .filter(|token| is_move_token(token))
        .enumerate()
    {
        let Some(mv) = find_san(&position, token) else {
            return ply > 0;
        };
        position.make(mv);
        if ply + 1 >= SKIP_PLIES
            && (ply + 1) % PLY_STRIDE == 0
            && is_quiet(&position)
            && seen.insert(position.key())
        {
            samples.push(Sample {
                trace: trace(&position),
                result,
            });
        }
    }
    true
}

fn is_move_token(token: &str) -> bool {
    !(token.ends_with('.')
        || token.starts_with('$')
        || matches!(token, "1-0" | "0-1" | "1/2-1/2" | "*"))
}

fn is_quiet(position: &Position) -> bool {
    position.checkers().0 == 0
        && position
            .tactical_moves()
            .iter()
            .all(|mv| !position.see_ge(mv, 1))
}

fn find_san(position: &Position, token: &str) -> Option<Move> {
    let wanted = token.trim_end_matches(['+', '#', '!', '?']);
    let wanted = wanted.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.');
    position
        .legal_moves()
        .iter()
        .find(|&mv| san(position, mv) == wanted)
}

/// Standard algebraic notation without check marks, enough to match fastchess output.
fn san(position: &Position, mv: Move) -> String {
    if mv.is_castle() {
        return if mv.flag() == 2 { "O-O" } else { "O-O-O" }.to_owned();
    }
    let piece = position
        .piece_at(mv.from())
        .expect("legal move has a piece");
    let capture = mv.flag() & 4 != 0;
    let mut text = String::new();
    if piece.kind == PieceType::Pawn {
        if capture {
            text.push((b'a' + mv.from().file()) as char);
        }
    } else {
        text.push(piece.fen().to_ascii_uppercase());
        let rivals: Vec<Move> = position
            .legal_moves()
            .iter()
            .filter(|&other| {
                other != mv
                    && other.to() == mv.to()
                    && !other.is_castle()
                    && position.piece_at(other.from()) == Some(piece)
            })
            .collect();
        if !rivals.is_empty() {
            let file_unique = rivals
                .iter()
                .all(|other| other.from().file() != mv.from().file());
            let rank_unique = rivals
                .iter()
                .all(|other| other.from().rank() != mv.from().rank());
            let square = mv.from().to_string();
            if file_unique {
                text.push_str(&square[..1]);
            } else if rank_unique {
                text.push_str(&square[1..]);
            } else {
                text.push_str(&square);
            }
        }
    }
    if capture {
        text.push('x');
    }
    text.push_str(&mv.to().to_string());
    if let Some(kind) = mv.promotion() {
        text.push('=');
        text.push(match kind {
            PieceType::Knight => 'N',
            PieceType::Bishop => 'B',
            PieceType::Rook => 'R',
            _ => 'Q',
        });
    }
    text
}

fn sigmoid(score: f64, k: f64) -> f64 {
    1.0 / (1.0 + (-k * score / 400.0).exp())
}

fn loss(samples: &[Sample], params: &[(f64, f64)], k: f64) -> f64 {
    samples
        .iter()
        .map(|sample| {
            let error = sample.result - sigmoid(traced_score(&sample.trace, params), k);
            error * error
        })
        .sum::<f64>()
        / samples.len().max(1) as f64
}

fn fit_scale(samples: &[Sample], params: &[(f64, f64)]) -> f64 {
    let (mut low, mut high) = (0.1_f64, 10.0_f64);
    for _ in 0..60 {
        let a = low + (high - low) / 3.0;
        let b = high - (high - low) / 3.0;
        if loss(samples, params, a) < loss(samples, params, b) {
            high = b;
        } else {
            low = a;
        }
    }
    (low + high) / 2.0
}

/// Full-batch Adam on the mean squared error between results and the sigmoid of the traced
/// score, plus an L2 pull of `prior` toward the starting weights: the training games are few
/// and noisy, and without it the piece-square tables drift hundreds of centipawns. Material
/// stays fixed because it is collinear with the piece-square entries. The score is linear in every weight once the strong side of the endgame scale is
/// fixed, so each gradient is the feature count times the phase share.
fn adam(samples: &[Sample], params: &mut [(f64, f64)], k: f64, options: &Options) {
    let (epochs, rate, prior) = (options.epochs, options.rate, options.prior);
    let initial = params.to_vec();
    let (beta1, beta2, epsilon) = (0.9, 0.999, 1e-8);
    let mut first = vec![(0.0, 0.0); PARAMS];
    let mut second = vec![(0.0, 0.0); PARAMS];
    let max_phase = f64::from(MAX_PHASE);
    for epoch in 1..=epochs {
        let mut gradient = vec![(0.0, 0.0); PARAMS];
        for sample in samples {
            let trace = &sample.trace;
            let score = traced_score(trace, params);
            let probability = sigmoid(score, k);
            let slope =
                -2.0 * (sample.result - probability) * probability * (1.0 - probability) * k
                    / 400.0;
            let eg_sum: f64 = trace
                .terms
                .iter()
                .map(|&(index, count)| params[index as usize].1 * f64::from(count))
                .sum();
            let strong = if eg_sum >= 0.0 { 0 } else { 1 };
            let mg_share = f64::from(trace.phase) / max_phase;
            let eg_share = (max_phase - f64::from(trace.phase)) / max_phase
                * f64::from(trace.scale[strong])
                / 16.0;
            for &(index, count) in &trace.terms {
                let entry = &mut gradient[index as usize];
                entry.0 += slope * f64::from(count) * mg_share;
                entry.1 += slope * f64::from(count) * eg_share;
            }
            for (side, &(by_kind, attackers)) in trace.king_attacks.iter().enumerate() {
                let sign = if side == 0 { 1.0 } else { -1.0 };
                for (kind, &count) in by_kind.iter().enumerate() {
                    let factor = sign * f64::from(count * attackers) / 4.0;
                    let entry = &mut gradient[KING_ZONE + kind];
                    entry.0 += slope * factor * mg_share;
                    entry.1 += slope * factor * eg_share;
                }
            }
        }
        let scale = 1.0 / samples.len() as f64;
        let correction1 = 1.0 - power(beta1, epoch);
        let correction2 = 1.0 - power(beta2, epoch);
        let step = |grad: f64, m: &mut f64, v: &mut f64, p: &mut f64| {
            *m = beta1 * *m + (1.0 - beta1) * grad;
            *v = beta2 * *v + (1.0 - beta2) * grad * grad;
            *p -= rate * (*m / correction1) / ((*v / correction2).sqrt() + epsilon);
        };
        for index in PST..PARAMS {
            let pull = (
                2.0 * prior * (params[index].0 - initial[index].0),
                2.0 * prior * (params[index].1 - initial[index].1),
            );
            let (m, v, p) = (&mut first[index], &mut second[index], &mut params[index]);
            step(
                gradient[index].0 * scale + pull.0,
                &mut m.0,
                &mut v.0,
                &mut p.0,
            );
            step(
                gradient[index].1 * scale + pull.1,
                &mut m.1,
                &mut v.1,
                &mut p.1,
            );
        }
        if epoch % 200 == 0 {
            eprintln!("epoch {epoch}: loss {:.6}", loss(samples, params, k));
        }
    }
}

fn power(beta: f64, epoch: usize) -> f64 {
    beta.powi(epoch as i32)
}

fn render(params: &[(f64, f64)]) -> String {
    let mut text = String::from(
        "// Evaluation weights as (middlegame, endgame) pairs, indexed by the constants in lib.rs.\n\
         // tools/tune regenerates this file.\n\
         #[rustfmt::skip]\n",
    );
    let _ = writeln!(
        text,
        "pub(crate) const WEIGHTS: [(i32, i32); {}] = [",
        params.len()
    );
    for row in params.chunks(8) {
        text.push_str("   ");
        for &(mg, eg) in row {
            let _ = write!(text, " ({}, {}),", mg.round() as i32, eg.round() as i32);
        }
        text.push('\n');
    }
    text.push_str("];\n");
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn san_round_trips_awkward_moves() {
        let cases = [
            (START_FEN, "Nf3"),
            (START_FEN, "e4"),
            ("r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1", "O-O-O"),
            ("4k3/1P6/8/8/8/8/8/4K3 w - - 0 1", "b8=N"),
            ("4k3/8/8/8/8/8/4K3/R6R w - - 0 1", "Rad1"),
            ("4k3/8/8/3pP3/8/8/8/4K3 w - d6 0 1", "exd6"),
            ("4k3/8/8/N7/8/8/8/N3K3 w - - 0 1", "N1b3"),
        ];
        for (fen, text) in cases {
            let position = Position::from_fen(fen).unwrap();
            let mv = find_san(&position, text).unwrap_or_else(|| panic!("{fen} {text}"));
            assert_eq!(san(&position, mv), text);
        }
    }

    #[test]
    fn parses_a_short_game() {
        let game = "[Event \"x\"]\n[Result \"1-0\"]\n\n1. e4 {+0.20/10 0.1s} e5 2. Nf3 Nc6 3. Bb5 a6 4. Ba4 Nf6 5. O-O Be7 6. Re1 b5 7. Bb3 d6 8. c3 O-O 9. h3 Nb8 10. d4 Nbd7 1-0\n";
        let mut seen = HashSet::new();
        let mut samples = Vec::new();
        assert!(collect_game(game, &mut seen, &mut samples));
        assert!(!samples.is_empty());
        assert!(samples.iter().all(|sample| sample.result == 1.0));
    }
}
