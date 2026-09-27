//! Builds inphish's Polyglot opening book from PGN games, such as the Lichess database.
//!
//! `collect RUNS [MIN_ELO]` reads PGN on standard input and writes the moves of the games that pass
//! the filters, counted by position and move, to sorted run files in the `RUNS`
//! directory. `merge RUNS TOTALS` merges the runs into one file of totals. `sizes TOTALS`
//! prints how many book entries each minimum game count would keep, and `write TOTALS
//! BOOK MIN_GAMES` writes the book.
//!
//! Counts are wins, draws and losses for the side making the move. A move is kept when
//! it was played in at least `MIN_GAMES` games and in at least 5% of the games that
//! reached the position, and, unless it scores best there, when it scored at least 40%.
//! Its weight is two points per win and one per draw, scaled to fit 16 bits.

use std::collections::BinaryHeap;
use std::env;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use inphzugzwang_book::key;
use inphzugzwang_core::{Move, PieceType, Position, Square};

/// Default lowest rating of both players.
const MIN_ELO: u32 = 2200;
const MIN_BASE_SECONDS: u32 = 180;
const MAX_PLY: usize = 40;
const MIN_SHARE: f64 = 0.05;
const MIN_SCORE: f64 = 0.40;
/// Moves held in memory before a sorted run is written.
const RUN_MOVES: usize = 40_000_000;

/// One position and move with the results of the games that played it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Total {
    key: u64,
    mv: u16,
    wins: u32,
    draws: u32,
    losses: u32,
}

const TOTAL_BYTES: usize = 22;

impl Total {
    fn games(&self) -> u32 {
        self.wins + self.draws + self.losses
    }

    fn write(&self, out: &mut impl Write) -> io::Result<()> {
        out.write_all(&self.key.to_le_bytes())?;
        out.write_all(&self.mv.to_le_bytes())?;
        out.write_all(&self.wins.to_le_bytes())?;
        out.write_all(&self.draws.to_le_bytes())?;
        out.write_all(&self.losses.to_le_bytes())
    }

    fn read(input: &mut impl Read) -> io::Result<Option<Self>> {
        let mut bytes = [0; TOTAL_BYTES];
        match input.read_exact(&mut bytes) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(error) => return Err(error),
        }
        let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        Ok(Some(Self {
            key: u64::from_le_bytes(bytes[..8].try_into().unwrap()),
            mv: u16::from_le_bytes([bytes[8], bytes[9]]),
            wins: u32_at(10),
            draws: u32_at(14),
            losses: u32_at(18),
        }))
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["collect", runs] => collect(Path::new(runs), MIN_ELO),
        ["collect", runs, min_elo] => match min_elo.parse() {
            Ok(min_elo) => collect(Path::new(runs), min_elo),
            Err(_) => Err(io::Error::other("MIN_ELO must be a number")),
        },
        ["merge", runs, totals] => merge(Path::new(runs), Path::new(totals)),
        ["sizes", totals] => sizes(Path::new(totals)),
        ["write", totals, book, min_games] => match min_games.parse() {
            Ok(min_games) => write_book(Path::new(totals), Path::new(book), min_games),
            Err(_) => Err(io::Error::other("MIN_GAMES must be a number")),
        },
        _ => Err(io::Error::other(
            "usage: book-builder collect RUNS [MIN_ELO] | merge RUNS TOTALS | sizes TOTALS | write TOTALS BOOK MIN_GAMES",
        )),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

/// The headers that decide whether a game is used.
#[derive(Default)]
struct Headers {
    white_elo: u32,
    black_elo: u32,
    base_seconds: u32,
    normal_end: bool,
    /// Points for White: 2 for a win, 1 for a draw, 0 for a loss.
    result: Option<u8>,
}

impl Headers {
    fn read(&mut self, line: &[u8]) {
        let Some((name, value)) = header(line) else {
            return;
        };
        match name {
            b"WhiteElo" => self.white_elo = number(value),
            b"BlackElo" => self.black_elo = number(value),
            b"TimeControl" => {
                self.base_seconds = number(value.split(|&c| c == b'+').next().unwrap_or(b""))
            }
            b"Termination" => self.normal_end = value == b"Normal" || value == b"Time forfeit",
            b"Result" => {
                self.result = match value {
                    b"1-0" => Some(2),
                    b"1/2-1/2" => Some(1),
                    b"0-1" => Some(0),
                    _ => None,
                }
            }
            _ => {}
        }
    }

    fn accepted(&self, min_elo: u32) -> bool {
        self.white_elo >= min_elo
            && self.black_elo >= min_elo
            && self.base_seconds >= MIN_BASE_SECONDS
            && self.normal_end
            && self.result.is_some()
    }
}

fn header(line: &[u8]) -> Option<(&[u8], &[u8])> {
    let line = line.strip_prefix(b"[")?;
    let space = line.iter().position(|&c| c == b' ')?;
    let value = line[space + 1..].strip_prefix(b"\"")?;
    let end = value.iter().position(|&c| c == b'"')?;
    Some((&line[..space], &value[..end]))
}

fn number(text: &[u8]) -> u32 {
    std::str::from_utf8(text)
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(0)
}

fn collect(runs: &Path, min_elo: u32) -> io::Result<()> {
    fs::create_dir_all(runs)?;
    let mut input = BufReader::with_capacity(1 << 20, io::stdin().lock());
    let mut line = Vec::new();
    let mut headers = Headers::default();
    let mut moves: Vec<(u64, u16, u8)> = Vec::with_capacity(RUN_MOVES);
    let (mut games, mut used, mut rejected, mut run) = (0_u64, 0_u64, 0_u64, 0);
    loop {
        line.clear();
        if input.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        if line.starts_with(b"[") {
            headers.read(&line);
            continue;
        }
        if line.starts_with(b"1.") {
            games += 1;
            if headers.accepted(min_elo) {
                let result = headers.result.expect("accepted games have a result");
                if play(&line, result, &mut moves) {
                    used += 1;
                } else {
                    rejected += 1;
                }
                if moves.len() >= RUN_MOVES - MAX_PLY {
                    write_run(runs, run, &mut moves)?;
                    run += 1;
                }
            }
            headers = Headers::default();
            if games.is_multiple_of(5_000_000) {
                eprintln!("{games} games read, {used} used");
            }
        }
    }
    if !moves.is_empty() {
        write_run(runs, run, &mut moves)?;
    }
    eprintln!("{games} games read, {used} used, {rejected} with an unreadable move");
    Ok(())
}

/// Replays the opening of a game's movetext, recording each position's key and move
/// with the points of the side to move. Returns false if a move cannot be read, keeping
/// the moves before it.
fn play(movetext: &[u8], white_points: u8, out: &mut Vec<(u64, u16, u8)>) -> bool {
    let mut position = Position::startpos();
    let mut ply = 0;
    let mut tokens = movetext.split(|&c| c == b' ');
    let mut in_comment = false;
    while ply < MAX_PLY {
        let Some(token) = tokens.next() else {
            return true;
        };
        if in_comment {
            in_comment = !token.ends_with(b"}");
            continue;
        }
        match token.first() {
            None => continue,
            Some(b'{') => {
                in_comment = !token.ends_with(b"}");
                continue;
            }
            Some(first) if first.is_ascii_digit() || *first == b'$' || *first == b'*' => continue,
            _ => {}
        }
        let Some(san) = std::str::from_utf8(token).ok() else {
            return false;
        };
        let Some(mv) = parse_san(&position, san.trim_end()) else {
            return false;
        };
        let points = if ply % 2 == 0 {
            white_points
        } else {
            2 - white_points
        };
        out.push((key(&position), polyglot_move(&position, mv), points));
        position.make(mv);
        ply += 1;
    }
    true
}

/// The legal move written in standard algebraic notation as `san`, if exactly one is.
fn parse_san(position: &Position, san: &str) -> Option<Move> {
    let san = san.trim_end_matches(['+', '#', '!', '?']);
    let legal = position.legal_moves();
    if san == "O-O" || san == "O-O-O" {
        let kingside = san == "O-O";
        return legal
            .iter()
            .find(|&mv| mv.is_castle() && (mv.flag() == 2) == kingside);
    }
    let bytes = san.as_bytes();
    let kind = match bytes.first()? {
        b'N' => PieceType::Knight,
        b'B' => PieceType::Bishop,
        b'R' => PieceType::Rook,
        b'Q' => PieceType::Queen,
        b'K' => PieceType::King,
        _ => PieceType::Pawn,
    };
    let body = if kind == PieceType::Pawn {
        bytes
    } else {
        &bytes[1..]
    };
    let (body, promotion) = match body.iter().position(|&c| c == b'=') {
        Some(at) => {
            let promotion = match body.get(at + 1)? {
                b'N' => PieceType::Knight,
                b'B' => PieceType::Bishop,
                b'R' => PieceType::Rook,
                b'Q' => PieceType::Queen,
                _ => return None,
            };
            (&body[..at], Some(promotion))
        }
        None => (body, None),
    };
    if body.len() < 2 {
        return None;
    }
    let (hints, target) = body.split_at(body.len() - 2);
    let to = square(target[0], target[1])?;
    let mut file = None;
    let mut rank = None;
    for &hint in hints {
        match hint {
            b'a'..=b'h' => file = Some(hint - b'a'),
            b'1'..=b'8' => rank = Some(hint - b'1'),
            b'x' => {}
            _ => return None,
        }
    }
    let mut found = None;
    for mv in legal.iter() {
        let from = mv.from();
        let moved = position.piece_at(from)?.kind;
        if mv.is_castle()
            || moved != kind
            || mv.to() != to
            || mv.promotion() != promotion
            || file.is_some_and(|file| from.file() != file)
            || rank.is_some_and(|rank| from.rank() != rank)
        {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(mv);
    }
    found
}

fn square(file: u8, rank: u8) -> Option<Square> {
    (file.is_ascii_lowercase() && file <= b'h' && (b'1'..=b'8').contains(&rank))
        .then(|| Square::new(file - b'a', rank - b'1'))
}

/// A move in Polyglot's encoding, castling as the king taking its rook.
fn polyglot_move(position: &Position, mv: Move) -> u16 {
    debug_assert!(position.piece_at(mv.from()).is_some());
    let promotion = match mv.promotion() {
        None => 0,
        Some(PieceType::Knight) => 1,
        Some(PieceType::Bishop) => 2,
        Some(PieceType::Rook) => 3,
        Some(_) => 4,
    };
    (promotion << 12) | ((mv.from().index() as u16) << 6) | mv.to().index() as u16
}

fn run_path(runs: &Path, run: usize) -> PathBuf {
    runs.join(format!("run-{run:04}.bin"))
}

fn write_run(runs: &Path, run: usize, moves: &mut Vec<(u64, u16, u8)>) -> io::Result<()> {
    moves.sort_unstable_by_key(|&(key, mv, _)| (key, mv));
    let mut out = BufWriter::with_capacity(1 << 20, File::create(run_path(runs, run))?);
    let mut current: Option<Total> = None;
    for &(key, mv, points) in moves.iter() {
        let total = match &mut current {
            Some(total) if total.key == key && total.mv == mv => total,
            _ => {
                if let Some(done) = current.take() {
                    done.write(&mut out)?;
                }
                current.insert(Total {
                    key,
                    mv,
                    wins: 0,
                    draws: 0,
                    losses: 0,
                })
            }
        };
        match points {
            2 => total.wins += 1,
            1 => total.draws += 1,
            _ => total.losses += 1,
        }
    }
    if let Some(done) = current {
        done.write(&mut out)?;
    }
    out.flush()?;
    eprintln!("wrote run {run} from {} moves", moves.len());
    moves.clear();
    Ok(())
}

fn merge(runs: &Path, totals: &Path) -> io::Result<()> {
    let mut paths: Vec<PathBuf> = fs::read_dir(runs)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<_>>()?;
    paths.retain(|path| path.extension().is_some_and(|extension| extension == "bin"));
    paths.sort();
    let mut readers = paths
        .iter()
        .map(|path| File::open(path).map(|file| BufReader::with_capacity(1 << 20, file)))
        .collect::<io::Result<Vec<_>>>()?;
    let mut heap = BinaryHeap::new();
    for (index, reader) in readers.iter_mut().enumerate() {
        if let Some(total) = Total::read(reader)? {
            heap.push(std::cmp::Reverse((total, index)));
        }
    }
    let mut out = BufWriter::with_capacity(1 << 20, File::create(totals)?);
    let mut current: Option<Total> = None;
    let mut written = 0_u64;
    while let Some(std::cmp::Reverse((total, index))) = heap.pop() {
        match &mut current {
            Some(sum) if sum.key == total.key && sum.mv == total.mv => {
                sum.wins += total.wins;
                sum.draws += total.draws;
                sum.losses += total.losses;
            }
            _ => {
                if let Some(done) = current.replace(total) {
                    done.write(&mut out)?;
                    written += 1;
                }
            }
        }
        if let Some(next) = Total::read(&mut readers[index])? {
            heap.push(std::cmp::Reverse((next, index)));
        }
    }
    if let Some(done) = current {
        done.write(&mut out)?;
        written += 1;
    }
    out.flush()?;
    eprintln!(
        "{written} position and move totals from {} runs",
        paths.len()
    );
    Ok(())
}

/// Calls `visit` with the totals of each position in the totals file, in key order.
fn for_each_position(
    totals: &Path,
    mut visit: impl FnMut(&[Total]) -> io::Result<()>,
) -> io::Result<()> {
    let mut input = BufReader::with_capacity(1 << 20, File::open(totals)?);
    let mut group: Vec<Total> = Vec::new();
    while let Some(total) = Total::read(&mut input)? {
        if group.first().is_some_and(|first| first.key != total.key) {
            visit(&group)?;
            group.clear();
        }
        group.push(total);
    }
    if !group.is_empty() {
        visit(&group)?;
    }
    Ok(())
}

/// The moves of one position that the book keeps, with their weights.
fn kept(group: &[Total], min_games: u32) -> Vec<(u16, u16)> {
    let reached: u32 = group.iter().map(Total::games).sum();
    let score =
        |total: &Total| f64::from(2 * total.wins + total.draws) / f64::from(2 * total.games());
    let candidates: Vec<&Total> = group
        .iter()
        .filter(|total| {
            total.mv != 0
                && total.games() >= min_games
                && f64::from(total.games()) >= MIN_SHARE * f64::from(reached)
        })
        .collect();
    let best = candidates
        .iter()
        .map(|total| score(total))
        .fold(f64::MIN, f64::max);
    let chosen: Vec<&Total> = candidates
        .into_iter()
        .filter(|total| score(total) >= MIN_SCORE || score(total) >= best)
        .collect();
    let heaviest = chosen
        .iter()
        .map(|total| 2 * u64::from(total.wins) + u64::from(total.draws))
        .max()
        .unwrap_or(0);
    chosen
        .into_iter()
        .map(|total| {
            let weight = 2 * u64::from(total.wins) + u64::from(total.draws);
            let scaled = if heaviest > u64::from(u16::MAX) {
                (weight * u64::from(u16::MAX) / heaviest).max(1)
            } else {
                weight.max(1)
            };
            (total.mv, scaled as u16)
        })
        .collect()
}

fn sizes(totals: &Path) -> io::Result<()> {
    const COUNTS: [u32; 12] = [1, 2, 3, 4, 5, 6, 8, 10, 12, 15, 20, 30];
    let mut entries = [0_u64; COUNTS.len()];
    for_each_position(totals, |group| {
        for (index, &min_games) in COUNTS.iter().enumerate() {
            entries[index] += kept(group, min_games).len() as u64;
        }
        Ok(())
    })?;
    for (min_games, entries) in COUNTS.iter().zip(entries) {
        println!(
            "min games {min_games:>3}: {entries:>10} entries, {:>6.1} MB",
            entries as f64 * 16.0 / 1e6
        );
    }
    Ok(())
}

fn write_book(totals: &Path, book: &Path, min_games: u32) -> io::Result<()> {
    let mut out = BufWriter::with_capacity(1 << 20, File::create(book)?);
    let mut entries = 0_u64;
    for_each_position(totals, |group| {
        let key = group[0].key;
        let mut moves = kept(group, min_games);
        moves.sort_by_key(|&(_, weight)| std::cmp::Reverse(weight));
        for (mv, weight) in moves {
            out.write_all(&key.to_be_bytes())?;
            out.write_all(&mv.to_be_bytes())?;
            out.write_all(&weight.to_be_bytes())?;
            out.write_all(&[0; 4])?;
            entries += 1;
        }
        Ok(())
    })?;
    out.flush()?;
    eprintln!("{entries} entries, {} bytes", entries * 16);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_lichess_movetext() {
        let movetext = b"1. e4 { [%clk 0:03:00] } 1... c5 { [%clk 0:03:00] } 2. Nf3 d6?! 3. d4 cxd4 4. Nxd4 Nf6 5. Nc3 a6 6. Be2 e5 7. Nb3 Be7 8. O-O O-O 9. a4 b6 10. f4 exf4 11. Bxf4 Nbd7 1-0\n";
        let mut moves = Vec::new();
        assert!(play(movetext, 2, &mut moves));
        assert_eq!(moves.len(), 22);
        assert_eq!(moves[0].0, 0x463b_9618_1691_fc9c);
        // e2e4: from e2 (row 1, file 4) to e4 (row 3, file 4), White scoring the win.
        assert_eq!(moves[0].1, (12 << 6) | 28);
        assert_eq!(moves[0].2, 2);
        assert_eq!(moves[1].2, 0);
        // 8. O-O is the king taking its rook, e1h1.
        assert_eq!(moves[14].1, (4 << 6) | 7);
    }

    #[test]
    fn needs_disambiguation_and_promotions() {
        let position = Position::from_fen("8/P7/2k5/8/8/8/4K3/R6R w - - 0 1").unwrap();
        assert!(parse_san(&position, "a8=Q+").is_some());
        assert!(parse_san(&position, "Rd1").is_none());
        assert!(parse_san(&position, "Rad1").is_some());
        assert!(parse_san(&position, "Rhd1").is_some());
        assert!(parse_san(&position, "Kd3").is_some());
    }

    #[test]
    fn keeps_frequent_and_good_moves() {
        let total = |mv, wins, draws, losses| Total {
            key: 1,
            mv,
            wins,
            draws,
            losses,
        };
        // A popular good move, a popular losing move, a rare move and a move below the
        // share threshold.
        let group = [
            total(1, 50, 30, 20),
            total(2, 5, 5, 40),
            total(3, 1, 0, 0),
            total(4, 3, 0, 1),
        ];
        let kept = kept(&group, 3);
        assert_eq!(kept, [(1, 130)]);
    }
}
