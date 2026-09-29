//! The search: a port of Stockfish 19's (GPL-3.0, https://github.com/official-stockfish/
//! Stockfish) principal variation search, quiescence search, move ordering, histories,
//! iterative deepening with aspiration windows, time management and Lazy SMP thread
//! voting, on this engine's board, network, tablebases and interface. Scores are in
//! Stockfish's internal units, 208 to a pawn, and converted to centipawns only where
//! they are reported.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use inphzugzwang_core::{Bitboard, Color, Move, MoveList, PieceType, Position};
use inphzugzwang_nnue::AccumulatorStack;
use inphzugzwang_syzygy::{probeable, ProbeState, Tablebases, WDL_DRAW};

mod history;
mod picker;
mod tt;

use history::{
    continuation_table, piece_square, Correction, Histories, CORRECTION_LIMIT, LOW_PLY,
    NO_PIECE_SQUARE, SENTINEL,
};
use picker::{
    is_capture, is_capture_stage, moved_square, target, victim, victim_slot, victim_value, Lists,
    Picker, PIECE_VALUES,
};
pub use tt::TranspositionTable;
use tt::{Bound, Previous, Record};

const MAX_PLY: usize = 246;
const MAX_MOVES: usize = 256;
const VALUE_MATE: i32 = 32_000;
const VALUE_INFINITE: i32 = 32_001;
/// Marks a missing score or evaluation; outside every real score.
const VALUE_NONE: i32 = 32_002;
const MATE_IN_MAX_PLY: i32 = VALUE_MATE - MAX_PLY as i32;
/// Score of a tablebase win at the root, just below the mate range.
const VALUE_TB: i32 = MATE_IN_MAX_PLY - 1;
/// Lowest tablebase win score; evaluations stay below it.
const TB_WIN_IN_MAX_PLY: i32 = VALUE_TB - MAX_PLY as i32;
const DEPTH_QS: i32 = 0;
const DEPTH_UNSEARCHED: i32 = -2;
pub(crate) const DEPTH_NONE: i32 = -3;
/// Stockfish 19's divisors of history in the reduced depth of quiet-move pruning.
const LMR_DIVISOR: [i32; 16] = [
    3637, 2787, 2761, 2939, 3171, 3347, 3147, 2762, 2772, 3106, 3107, 3060, 3112, 2991, 3090, 3542,
];
/// Searched moves remembered per node for history maluses.
const SEARCHED_CAPACITY: usize = 32;

fn is_valid(value: i32) -> bool {
    value != VALUE_NONE
}

fn is_win(value: i32) -> bool {
    value >= TB_WIN_IN_MAX_PLY
}

fn is_loss(value: i32) -> bool {
    value <= -TB_WIN_IN_MAX_PLY
}

pub(crate) fn is_decisive(value: i32) -> bool {
    is_win(value) || is_loss(value)
}

fn mate_in(ply: usize) -> i32 {
    VALUE_MATE - ply as i32
}

fn mated_in(ply: usize) -> i32 {
    -VALUE_MATE + ply as i32
}

/// A score for reporting: centipawns at 208 internal units to a pawn, with mate and
/// tablebase scores passed through.
fn to_centipawns(value: i32) -> i32 {
    if is_decisive(value) {
        value
    } else {
        value * 100 / 208
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Clock {
    /// Milliseconds left on the clock of the side to move.
    pub time: u64,
    pub increment: u64,
    pub movestogo: Option<u64>,
    pub overhead: u64,
}

#[derive(Clone, Default)]
pub struct Limits {
    pub depth: Option<u8>,
    pub nodes: Option<u64>,
    /// Time the search aims to use, when not set from `clock`.
    pub soft: Option<Duration>,
    /// Time the search never exceeds, when not set from `clock`.
    pub hard: Option<Duration>,
    /// The clock, from which time management sets the soft and hard limits.
    pub clock: Option<Clock>,
    pub infinite: bool,
    pub ponder: bool,
    pub searchmoves: Vec<Move>,
    pub searchmoves_only: bool,
    /// Number of principal variations to report; 0 and 1 both mean one.
    pub multipv: usize,
    /// Search threads including the main one; 0 and 1 both mean one.
    pub threads: usize,
    /// Playing strength to imitate, as an Elo on the scale of `UCI_Elo`.
    pub strength: Option<u16>,
    /// How much worse than even a draw counts for the side to move at the root, in
    /// internal units; zero scores draws as even, as Stockfish does.
    pub contempt: i32,
    pub tablebases: Option<Arc<Tablebases>>,
    pub immediate: bool,
    pub started: Option<Instant>,
}

#[derive(Clone)]
pub struct Info {
    pub depth: u8,
    pub seldepth: usize,
    pub score: i32,
    pub nodes: u64,
    pub elapsed: Duration,
    pub hashfull: u16,
    pub pv: Vec<Move>,
    pub multipv: usize,
    pub tbhits: u64,
}

pub struct Result {
    pub best: Option<Move>,
    pub info: Info,
}

pub struct Control {
    pub stop: Arc<AtomicBool>,
    pub ponderhit: Arc<AtomicBool>,
}

/// Statistics a search thread learns and keeps for later moves of the same game.
pub(crate) struct Memory {
    histories: Histories,
    /// Move lists for the pickers, three per ply: the node's own, that of a search of
    /// the same node without its table move, and the quiescence search's.
    lists: Box<[Lists]>,
    accumulators: AccumulatorStack,
}

impl Memory {
    fn new() -> Self {
        Self {
            histories: Histories::new(),
            lists: (0..LIST_SLOTS * (MAX_PLY + 2))
                .map(|_| [MoveList::new(), MoveList::new()])
                .collect(),
            accumulators: AccumulatorStack::new(),
        }
    }
}

/// Picker list slots per ply.
const LIST_SLOTS: usize = 3;

/// Stockfish's time allocation: an optimum the search aims for, which it scales by how
/// settled the best move is, and a maximum it never exceeds, in milliseconds. Below one
/// second fewer moves are assumed to remain. `time_adjust` is fixed on the first move of
/// a game.
fn allocate(clock: &Clock, ply: u32, time_adjust: &mut Option<f64>) -> (u64, u64) {
    let time = clock.time.max(1) as f64;
    let (increment, overhead) = (clock.increment as f64, clock.overhead as f64);
    let mut horizon = clock.movestogo.map_or(50.0, |moves| moves.min(50) as f64);
    if time < 1000.0 && clock.movestogo.is_none() {
        horizon = (time * 0.05).floor();
    }
    let left = (time + increment * (horizon - 1.0) - overhead * (2.0 + horizon)).max(1.0);
    let ply = f64::from(ply);
    let (optimum_scale, maximum_scale) = if clock.movestogo.is_none() {
        let adjust = *time_adjust.get_or_insert(0.3272 * left.log10() - 0.4141);
        let log_time = (time / 1000.0).log10();
        let optimum_constant = (0.002_986_9 + 0.000_335_54 * log_time).min(0.004_905);
        let maximum_constant = (3.3744 + 3.0608 * log_time).max(3.1441);
        (
            (0.012_112 + (ply + 3.227_13).powf(0.468_66) * optimum_constant)
                .min(0.194_04 * time / left)
                * adjust,
            (maximum_constant + ply / 12.352).min(6.873),
        )
    } else {
        (
            ((0.88 + ply / 116.4) / horizon).min(0.88 * time / left),
            1.3 + 0.11 * horizon,
        )
    };
    let optimum = (optimum_scale * left).max(1.0);
    let maximum = optimum.max((0.8097 * time - overhead).min(maximum_scale * optimum));
    // Always a margin short of the flag.
    let ceiling = (time - overhead - 5.0).max(1.0);
    (optimum.min(ceiling) as u64, maximum.min(ceiling) as u64)
}

fn interpolate(x: f64, x0: f64, x1: f64, y0: f64, y1: f64) -> f64 {
    y0 + (x - x0) * (y1 - y0) / (x1 - x0)
}

#[derive(Clone)]
struct RootMove {
    pv: Vec<Move>,
    previous_pv: Vec<Move>,
    score: i32,
    previous_score: i32,
    average_score: i32,
    mean_squared_score: i64,
    uci_score: i32,
    effort: u64,
    sel_depth: usize,
    inexact_lower: bool,
    inexact_upper: bool,
    previous_exact: bool,
    tb_rank: i32,
}

impl RootMove {
    fn new(mv: Move) -> Self {
        Self {
            pv: vec![mv],
            previous_pv: Vec::new(),
            score: -VALUE_INFINITE,
            previous_score: -VALUE_INFINITE,
            average_score: -VALUE_INFINITE,
            mean_squared_score: -i64::from(VALUE_INFINITE) * i64::from(VALUE_INFINITE),
            uci_score: -VALUE_INFINITE,
            effort: 0,
            sel_depth: 0,
            inexact_lower: false,
            inexact_upper: false,
            previous_exact: false,
            tb_rank: 0,
        }
    }

    fn is_inexact(&self) -> bool {
        self.inexact_lower || self.inexact_upper
    }

    fn is_exact_loss(&self) -> bool {
        self.score != -VALUE_INFINITE && is_loss(self.score) && !self.is_inexact()
    }

    fn unset_inexact(&mut self) {
        self.inexact_lower = false;
        self.inexact_upper = false;
    }
}

/// Stockfish's root move order: by score, then by the previous iteration's score.
fn stable_sort(moves: &mut [RootMove]) {
    moves.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(b.previous_score.cmp(&a.previous_score))
    });
}

/// Per-ply search state, read up to six plies back.
#[derive(Clone, Copy)]
struct Frame {
    /// Move played from this ply, null when none or a null move.
    mv: Move,
    null: bool,
    /// Continuation table of replies to `mv`.
    continuation: usize,
    /// Piece and destination of `mv`, `NO_PIECE_SQUARE` for none.
    correction_row: usize,
    excluded: Move,
    static_eval: i32,
    stat_score: i32,
    move_count: i32,
    in_check: bool,
    tt_pv: bool,
    tt_hit: bool,
    follow_pv: bool,
    cutoff_count: i32,
    reduction: i32,
}

impl Default for Frame {
    fn default() -> Self {
        Self {
            mv: Move::NULL,
            null: false,
            continuation: SENTINEL,
            correction_row: NO_PIECE_SQUARE,
            excluded: Move::NULL,
            static_eval: VALUE_NONE,
            stat_score: 0,
            move_count: 0,
            in_check: false,
            tt_pv: false,
            tt_hit: false,
            follow_pv: false,
            cutoff_count: 0,
            reduction: 0,
        }
    }
}

/// Frames before the root, so that looking seven plies back never leaves the stack.
const FRAME_OFFSET: usize = 8;

/// State the threads of one search share.
struct Shared {
    stop: AtomicBool,
    /// Nodes of all threads, added in batches; node limits apply to this total.
    nodes: AtomicU64,
    best_move_changes: AtomicU64,
    increase_depth: AtomicBool,
}

struct Worker<'a> {
    position: Position,
    limits: &'a Limits,
    control: &'a Control,
    shared: &'a Shared,
    tt: &'a TranspositionTable,
    thread: usize,
    threads: usize,
    started: Instant,
    timed_started: Option<Instant>,
    optimum: Option<f64>,
    maximum: Option<f64>,
    calls: i32,
    nodes: u64,
    tbhits: u64,
    sel_depth: usize,
    memory: Memory,
    stack: Box<[Frame]>,
    reductions: Box<[i32]>,
    root_moves: Vec<RootMove>,
    pv_index: usize,
    pv_last: usize,
    root_depth: i32,
    completed_depth: i32,
    root_delta: i32,
    nmp_min_ply: usize,
    optimism: [i32; 2],
    /// The value of a draw for each side to move: less than even for the root side by
    /// the contempt, more for the other.
    draw: [i32; 2],
    last_iteration_pv: Vec<Move>,
    pv: Box<[[Move; MAX_PLY + 2]]>,
    pv_len: Box<[usize]>,
    tablebases: Option<Arc<Tablebases>>,
}

/// What a finished thread reports for choosing the move.
struct Finished {
    root_moves: Vec<RootMove>,
    completed_depth: i32,
}

pub fn search(
    position: Position,
    limits: Limits,
    control: &Control,
    on_info: impl FnMut(Info),
) -> Result {
    let tt = TranspositionTable::new(16).expect("default hash allocation failed");
    search_with_table(position, limits, control, &tt, on_info)
}

/// Lazy SMP: helper threads run their own iterative deepening on the same position and
/// share only the transposition table; the main thread manages time and reports, and
/// the move comes from the thread that wins Stockfish's vote.
pub fn search_with_table(
    position: Position,
    limits: Limits,
    control: &Control,
    tt: &TranspositionTable,
    mut on_info: impl FnMut(Info),
) -> Result {
    tt.next_generation();
    let mut limits = limits;
    let mut previous = tt.previous();
    if let Some(clock) = limits.clock {
        let (optimum, maximum) = allocate(&clock, position.game_ply(), &mut previous.time_adjust);
        limits.soft = Some(Duration::from_millis(optimum));
        limits.hard = Some(Duration::from_millis(maximum));
        tt.set_previous(previous);
    }
    if let Some(elo) = limits.strength {
        let cap = strength_nodes(elo);
        limits.nodes = Some(limits.nodes.map_or(cap, |nodes| nodes.min(cap)));
    }
    // With the root in the tablebases, only the moves that keep the best result under the
    // fifty-move rule are searched, and the search no longer probes, as in Stockfish.
    let root_tb = limits
        .tablebases
        .as_deref()
        .and_then(|tables| rank_root(tables, &position, &limits));
    if let Some((moves, _)) = &root_tb {
        limits.searchmoves = moves.clone();
        limits.searchmoves_only = true;
        limits.tablebases = None;
    }
    let root_tb_score = root_tb.map(|(_, score)| score);
    let shared = Shared {
        stop: AtomicBool::new(false),
        nodes: AtomicU64::new(0),
        best_move_changes: AtomicU64::new(0),
        increase_depth: AtomicBool::new(true),
    };
    let threads = limits.threads.max(1);
    let seed = position.key()
        ^ std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos() as u64);
    let (limits, shared) = (&limits, &shared);
    std::thread::scope(|scope| {
        let helpers: Vec<_> = (1..threads)
            .map(|thread| {
                let position = position.clone();
                std::thread::Builder::new()
                    .name("inphish-helper".to_owned())
                    .stack_size(64 * 1024 * 1024)
                    .spawn_scoped(scope, move || {
                        let mut worker =
                            Worker::new(position, limits, control, shared, tt, thread, threads);
                        worker.iterative_deepening(&mut |_| {});
                        let finished = Finished {
                            root_moves: std::mem::take(&mut worker.root_moves),
                            completed_depth: worker.completed_depth,
                        };
                        tt.keep_memory(worker.memory);
                        finished
                    })
                    .expect("helper thread could not start")
            })
            .collect();
        let mut worker = Worker::new(position.clone(), limits, control, shared, tt, 0, threads);
        let reported = limits.multipv.max(1);
        let mut report = |info: Info| on_info(with_tb_score(&info, root_tb_score));
        if worker.root_moves.is_empty() {
            shared.stop.store(true, Ordering::Relaxed);
            let score = if position.checkers().0 != 0 {
                -VALUE_MATE
            } else {
                0
            };
            let info = Info {
                depth: 0,
                seldepth: 0,
                score,
                nodes: 0,
                elapsed: Duration::ZERO,
                hashfull: 0,
                pv: Vec::new(),
                multipv: 1,
                tbhits: 0,
            };
            report(info.clone());
            for helper in helpers {
                let _ = helper.join();
            }
            tt.keep_memory(worker.memory);
            return Result { best: None, info };
        }
        worker.iterative_deepening(&mut report);
        // Wait for "stop" or "ponderhit" when pondering or searching infinitely.
        while !worker.control.stop.load(Ordering::Relaxed)
            && (limits.infinite
                || (limits.ponder && !worker.control.ponderhit.load(Ordering::Relaxed)))
        {
            std::thread::sleep(Duration::from_millis(1));
        }
        shared.stop.store(true, Ordering::Relaxed);
        let mut finished = vec![Finished {
            root_moves: worker.root_moves.clone(),
            completed_depth: worker.completed_depth,
        }];
        for helper in helpers {
            if let Ok(done) = helper.join() {
                finished.push(done);
            }
        }
        let best_thread = if threads > 1 && limits.depth.is_none() && limits.strength.is_none() {
            best_thread(&finished)
        } else {
            0
        };
        let best_moves = &finished[best_thread].root_moves;
        if let Some(best) = best_moves.first() {
            let average = best.average_score;
            tt.set_previous(Previous {
                score: Some((best.score, average)),
                ..tt.previous()
            });
        }
        let lines: Vec<Info> = best_moves
            .iter()
            .take(worker.line_count())
            .enumerate()
            .map(|(index, root)| {
                worker.info(root, finished[best_thread].completed_depth, index + 1)
            })
            .collect();
        for line in lines.iter().take(reported) {
            report(line.clone());
        }
        tt.keep_memory(std::mem::replace(&mut worker.memory, Memory::empty()));
        let chosen = match limits.strength {
            Some(elo) if lines.len() > 1 => weakened_choice(&lines, elo, seed),
            _ => 0,
        };
        let info = with_tb_score(&lines[chosen], root_tb_score);
        Result {
            best: info.pv.first().copied(),
            info,
        }
    })
}

/// Stockfish's thread vote: each thread votes for its best move with the score above the
/// worst thread's plus a constant; decisive results prefer the shortest mate.
fn best_thread(finished: &[Finished]) -> usize {
    let min_score = finished
        .iter()
        .map(|done| done.root_moves[0].score)
        .min()
        .unwrap_or(0);
    let vote = |mv: Move| -> i64 {
        finished
            .iter()
            .filter(|done| done.root_moves[0].pv[0] == mv)
            .map(|done| i64::from(done.root_moves[0].score - min_score + 14))
            .sum()
    };
    let decisive = |root: &RootMove| {
        root.score != -VALUE_INFINITE && is_decisive(root.score) && !root.is_inexact()
    };
    let mut best = 0;
    for (index, done) in finished.iter().enumerate() {
        if done.completed_depth == 0 {
            continue;
        }
        let current = &finished[best].root_moves[0];
        let candidate = &done.root_moves[0];
        let (best_vote, candidate_vote) = (vote(current.pv[0]), vote(candidate.pv[0]));
        if decisive(current) {
            if decisive(candidate) && candidate.score.abs() > current.score.abs() {
                best = index;
            }
        } else if decisive(candidate)
            || (!is_loss(candidate.score)
                && (candidate_vote > best_vote
                    || (candidate_vote == best_vote && candidate.pv.len() > current.pv.len())))
        {
            best = index;
        }
    }
    best
}

/// The line to report: with the root in the tablebases its score is the tablebase result
/// unless the search found a mate, while the search itself keeps using its own scores.
fn with_tb_score(info: &Info, tb_score: Option<i32>) -> Info {
    let mut shown = info.clone();
    if let Some(score) = tb_score.filter(|_| info.score.abs() < MATE_IN_MAX_PLY) {
        shown.score = score;
    }
    shown
}

/// Ranks the root moves by DTZ the way Stockfish's `root_probe` does and returns those of
/// the best rank with the score to report, or nothing if the root is not covered.
fn rank_root(
    tables: &Tablebases,
    position: &Position,
    limits: &Limits,
) -> Option<(Vec<Move>, i32)> {
    if !probeable(position, tables.max_pieces()) {
        return None;
    }
    let mut position = position.clone();
    let rule50 = i32::from(position.halfmove_clock());
    let repeated = position.has_repeated();
    let mut ranked = Vec::new();
    for mv in position.legal_moves().iter() {
        if limits.searchmoves_only && !limits.searchmoves.contains(&mv) {
            continue;
        }
        position.make(mv);
        let mut dtz = if position.halfmove_clock() == 0 {
            let (wdl, state) = tables.probe_wdl(&mut position);
            (state != ProbeState::Fail).then(|| inphzugzwang_syzygy::dtz_before_zeroing(-wdl))
        } else {
            let (dtz, state) = tables.probe_dtz(&mut position);
            (state != ProbeState::Fail).then_some(-dtz + (-dtz).signum())
        };
        if dtz == Some(2) && position.checkers().0 != 0 && position.legal_moves().is_empty() {
            dtz = Some(1);
        }
        position.unmake();
        let dtz = dtz?;
        let rank = if dtz > 0 {
            if dtz + rule50 <= 99 && !repeated {
                1000
            } else {
                1000 - (dtz + rule50)
            }
        } else if dtz < 0 {
            if -dtz * 2 + rule50 < 100 {
                -1000
            } else {
                -1000 + (-dtz + rule50)
            }
        } else {
            0
        };
        ranked.push((mv, rank));
    }
    let best = ranked.iter().map(|&(_, rank)| rank).max()?;
    // Certain results score as won or lost; results the fifty-move rule endangers get a
    // small score that grows as the counter leaves more room.
    let score = if best >= 900 {
        VALUE_TB - 1
    } else if best > 0 {
        (best - 800).max(3) / 2
    } else if best == 0 {
        WDL_DRAW
    } else if best > -900 {
        (best + 800).min(-3) / 2
    } else {
        -VALUE_TB + 1
    };
    let moves = ranked
        .into_iter()
        .filter(|&(_, rank)| rank == best)
        .map(|(mv, _)| mv)
        .collect();
    Some((moves, score))
}

/// Lines searched when strength is limited, so that a weaker move can be chosen.
const STRENGTH_LINES: usize = 4;

/// Node budgets for limited strength as base-2 logarithms at calibration points, from
/// matches against Stockfish 19 at the same `UCI_Elo`; settings between two points
/// interpolate linearly.
const STRENGTH_CURVE: [(u16, f64); 6] = [
    (1320, 6.7),
    (1600, 8.2),
    (2200, 10.15),
    (2600, 11.0),
    (2800, 12.6),
    (3000, 13.1),
];

fn strength_nodes(elo: u16) -> u64 {
    let elo = elo.clamp(STRENGTH_MIN, STRENGTH_MAX);
    let segment = STRENGTH_CURVE
        .windows(2)
        .find(|pair| elo <= pair[1].0)
        .expect("the curve spans the setting range");
    let ((low, low_nodes), (high, high_nodes)) = (segment[0], segment[1]);
    let share = f64::from(elo - low) / f64::from(high - low);
    (low_nodes + share * (high_nodes - low_nodes)).exp2() as u64
}

pub const STRENGTH_MIN: u16 = 1320;
pub const STRENGTH_MAX: u16 = 3000;
/// Setting from which moves are no longer weakened and only the node budget limits play.
const STRENGTH_UNWEAKENED: u16 = 2600;

/// Chooses a line in the manner of Stockfish's skill level: each line's score gets a push
/// that grows with its distance from the best line and with a random share of the spread
/// of scores, both scaled by the weakness, and the line with the highest total is played.
fn weakened_choice(lines: &[Info], elo: u16, seed: u64) -> usize {
    let weakness = i32::from(STRENGTH_UNWEAKENED.saturating_sub(elo.max(STRENGTH_MIN)) / 14).max(1);
    let top = lines[0].score;
    let delta = (top - lines[lines.len() - 1].score).clamp(0, 100);
    let mut state = seed | 1;
    let mut chosen = 0;
    let mut chosen_total = i32::MIN;
    for (index, line) in lines.iter().enumerate() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let random = (state % weakness as u64) as i32;
        let push = (weakness * (top - line.score).min(1000) + delta * random) / 128;
        if line.score + push > chosen_total {
            chosen_total = line.score + push;
            chosen = index;
        }
    }
    chosen
}

/// The key under which a position is stored in the table: from a fifty-move counter of
/// 14 on it changes every eight plies, as Stockfish's `adjust_key50` does, so that results
/// do not carry over between the same position at very different distances from the rule.
fn table_key(position: &Position) -> u64 {
    adjusted_key(position.key(), position.halfmove_clock())
}

fn adjusted_key(key: u64, rule50: u16) -> u64 {
    let rule50 = u64::from(rule50);
    if rule50 < 14 {
        return key;
    }
    let bucket = ((rule50 - 14) / 8)
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    key ^ bucket
}

/// A mate or tablebase score is stored relative to the node rather than the root.
fn value_to_tt(value: i32, ply: usize) -> i32 {
    if is_win(value) {
        value + ply as i32
    } else if is_loss(value) {
        value - ply as i32
    } else {
        value
    }
}

/// The inverse of `value_to_tt`. A mate or tablebase score that the fifty-move counter
/// may no longer allow comes back as the highest score short of them, as in Stockfish.
fn value_from_tt(value: i32, ply: usize, rule50: i32) -> i32 {
    if !is_valid(value) {
        return VALUE_NONE;
    }
    if is_win(value) {
        if (value >= MATE_IN_MAX_PLY && VALUE_MATE - value > 100 - rule50)
            || VALUE_TB - value > 100 - rule50
        {
            return TB_WIN_IN_MAX_PLY - 1;
        }
        return value - ply as i32;
    }
    if is_loss(value) {
        if (value <= -MATE_IN_MAX_PLY && VALUE_MATE + value > 100 - rule50)
            || VALUE_TB + value > 100 - rule50
        {
            return -TB_WIN_IN_MAX_PLY + 1;
        }
        return value + ply as i32;
    }
    value
}

/// Knights, bishops, rooks and queens of one side at Stockfish's piece values.
fn non_pawn_material(position: &Position, color: Color) -> i32 {
    [
        PieceType::Knight,
        PieceType::Bishop,
        PieceType::Rook,
        PieceType::Queen,
    ]
    .into_iter()
    .map(|kind| position.pieces(color, kind).count() as i32 * PIECE_VALUES[kind.index()])
    .sum()
}

impl Memory {
    /// A placeholder left behind when a thread's statistics are handed back.
    fn empty() -> Self {
        Self {
            histories: Histories::empty(),
            lists: Box::default(),
            accumulators: AccumulatorStack::empty(),
        }
    }
}

impl<'a> Worker<'a> {
    fn new(
        position: Position,
        limits: &'a Limits,
        control: &'a Control,
        shared: &'a Shared,
        tt: &'a TranspositionTable,
        thread: usize,
        threads: usize,
    ) -> Self {
        let started = limits.started.unwrap_or_else(Instant::now);
        let mut memory = tt.take_memory();
        memory.accumulators.reset(&position);
        let reductions = (0..MAX_MOVES)
            .map(|index| {
                if index == 0 {
                    0
                } else {
                    (2872.0 / 128.0 * (index as f64).ln()) as i32
                }
            })
            .collect();
        let root_moves = position
            .legal_moves()
            .iter()
            .filter(|mv| !limits.searchmoves_only || limits.searchmoves.contains(mv))
            .map(RootMove::new)
            .collect();
        let timed = limits.soft.is_some() || limits.hard.is_some();
        let mut draw = [limits.contempt; 2];
        draw[position.side_to_move().index()] = -limits.contempt;
        Worker {
            draw,
            timed_started: (!limits.ponder).then_some(started),
            optimum: limits
                .soft
                .filter(|_| timed)
                .map(|soft| soft.as_secs_f64() * 1000.0),
            maximum: limits.hard.map(|hard| hard.as_secs_f64() * 1000.0),
            position,
            limits,
            control,
            shared,
            tt,
            thread,
            threads,
            started,
            calls: 0,
            nodes: 0,
            tbhits: 0,
            sel_depth: 0,
            memory,
            stack: vec![Frame::default(); MAX_PLY + FRAME_OFFSET + 4].into_boxed_slice(),
            reductions,
            root_moves,
            pv_index: 0,
            pv_last: 0,
            root_depth: 0,
            completed_depth: 0,
            root_delta: 1,
            nmp_min_ply: 0,
            optimism: [0; 2],
            last_iteration_pv: Vec::new(),
            pv: vec![[Move::NULL; MAX_PLY + 2]; MAX_PLY + 2].into_boxed_slice(),
            pv_len: vec![0; MAX_PLY + 2].into_boxed_slice(),
            tablebases: limits.tablebases.clone(),
        }
    }

    fn is_main(&self) -> bool {
        self.thread == 0
    }

    fn at(&self, ply: usize, back: usize) -> &Frame {
        &self.stack[ply + FRAME_OFFSET - back]
    }

    fn at_mut(&mut self, ply: usize, back: usize) -> &mut Frame {
        &mut self.stack[ply + FRAME_OFFSET - back]
    }

    fn stopped(&self) -> bool {
        self.shared.stop.load(Ordering::Relaxed)
    }

    fn stop(&self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }

    /// Nodes of all threads: every thread adds its count to the shared counter in batches
    /// of 1,024, so this is exact for one thread and at most a batch per thread short.
    fn total_nodes(&self) -> u64 {
        self.shared.nodes.load(Ordering::Relaxed) + (self.nodes & 1023)
    }

    fn refresh_ponder(&mut self) {
        if self.timed_started.is_none() && self.control.ponderhit.load(Ordering::Relaxed) {
            self.timed_started = Some(Instant::now());
        }
    }

    fn pondering(&self) -> bool {
        self.timed_started.is_none()
    }

    /// Milliseconds since the clock started running for this search.
    fn elapsed(&self) -> f64 {
        self.timed_started
            .map_or(0.0, |started| started.elapsed().as_secs_f64() * 1000.0)
    }

    /// The main thread's periodic check of the stop command, node limit and time limit.
    fn check_time(&mut self) {
        self.calls -= 1;
        if self.calls > 0 {
            return;
        }
        self.calls = self
            .limits
            .nodes
            .map_or(512, |nodes| (nodes / 1024).clamp(1, 512) as i32);
        if self.control.stop.load(Ordering::Relaxed) {
            self.stop();
            return;
        }
        self.refresh_ponder();
        if self
            .limits
            .nodes
            .is_some_and(|limit| self.total_nodes() >= limit)
        {
            self.stop();
            return;
        }
        if !self.pondering() && !self.limits.infinite {
            if let Some(maximum) = self.maximum {
                if self.elapsed() >= maximum {
                    self.stop();
                }
            }
        }
    }

    fn line_count(&self) -> usize {
        let requested = self.limits.multipv.max(1);
        let lines = if self.limits.strength.is_some() {
            requested.max(STRENGTH_LINES)
        } else {
            requested
        };
        lines.min(self.root_moves.len())
    }

    fn info(&self, root: &RootMove, depth: i32, multipv: usize) -> Info {
        let score = if root.score == -VALUE_INFINITE {
            root.previous_score
        } else {
            root.uci_score
        };
        Info {
            depth: depth.clamp(1, 255) as u8,
            seldepth: root.sel_depth.max(1),
            score: to_centipawns(if score == -VALUE_INFINITE { 0 } else { score }),
            nodes: self.total_nodes(),
            elapsed: self.started.elapsed(),
            hashfull: self.tt.hashfull(),
            pv: root.pv.clone(),
            multipv,
            tbhits: self.tbhits,
        }
    }

    /// Makes `mv` from `ply`, recording what it changes for the accumulators, and records
    /// it in the ply's frame for the histories of the replies.
    fn do_move(&mut self, mv: Move, ply: usize) {
        let in_check = self.at(ply, 0).in_check;
        let capture = is_capture_stage(mv);
        let square = moved_square(&self.position, mv);
        let frame = self.at_mut(ply, 0);
        frame.mv = mv;
        frame.null = false;
        frame.continuation = continuation_table(in_check, capture, square);
        frame.correction_row = square;
        self.nodes += 1;
        if self.nodes & 1023 == 0 {
            // Every thread checks a node limit as it adds a batch, so the total stops close
            // to the limit even while the main thread, which checks more often, is waiting
            // for a processor.
            let total = self.shared.nodes.fetch_add(1024, Ordering::Relaxed) + 1024;
            if self.limits.nodes.is_some_and(|limit| total >= limit) {
                self.stop();
            }
        }
        // As in Stockfish, the entry is loaded while the move is made, from a key that
        // is exact for all but the rare moves of more than one piece.
        let (key, rule50) = self.position.prefetch_key(mv);
        self.tt.prefetch(adjusted_key(key, rule50));
        let dirty = self.memory.accumulators.push();
        self.position.make_recorded(mv, dirty);
        let [white, black] = self.position.non_pawn_keys();
        self.memory.histories.prefetch_corrections(
            [
                self.position.pawn_key(),
                self.position.minor_key(),
                white,
                black,
            ],
            [
                self.at(ply, 1).correction_row,
                self.at(ply, 3).correction_row,
            ],
            square,
        );
    }

    fn undo_move(&mut self) {
        self.position.unmake();
        self.memory.accumulators.pop();
    }

    fn do_null_move(&mut self, ply: usize) {
        let frame = self.at_mut(ply, 0);
        frame.mv = Move::NULL;
        frame.null = true;
        frame.continuation = SENTINEL;
        frame.correction_row = NO_PIECE_SQUARE;
        self.position.make_null();
    }

    /// Stockfish 19's evaluation for the side to move, with its optimism.
    fn evaluate(&mut self) -> i32 {
        let position = &self.position;
        let optimism = self.optimism[position.side_to_move().index()];
        self.memory
            .accumulators
            .evaluate(position)
            .evaluation_with_optimism(position, optimism)
    }

    fn value_draw(&self) -> i32 {
        self.draw_score() - 1 + (self.nodes & 2) as i32
    }

    /// A draw for the side to move, with the contempt.
    fn draw_score(&self) -> i32 {
        self.draw[self.position.side_to_move().index()]
    }

    fn reduction(&self, improving: bool, depth: i32, move_count: i32, delta: i32) -> i32 {
        let scale = self.reductions[depth.clamp(0, MAX_MOVES as i32 - 1) as usize]
            * self.reductions[move_count.clamp(0, MAX_MOVES as i32 - 1) as usize];
        scale - delta * 577 / self.root_delta + i32::from(!improving) * scale * 197 / 512 + 982
    }

    /// The piece standing on the destination of the move that led to `ply`, with that
    /// square, as indexed in the history tables.
    fn previous_square(&self, ply: usize) -> Option<usize> {
        let previous = self.at(ply, 1).mv;
        if previous == Move::NULL {
            return None;
        }
        let piece = self.position.piece_at(previous.to())?;
        Some(piece_square(piece, previous.to().index()))
    }

    fn correction_value(&self, ply: usize) -> i32 {
        let side = self.position.side_to_move().index();
        let histories = &self.memory.histories;
        let position = &self.position;
        let [white, black] = position.non_pawn_keys();
        let pawn = histories.correction(Correction::Pawn, position.pawn_key(), side);
        let minor = histories.correction(Correction::Minor, position.minor_key(), side);
        let white = histories.correction(Correction::WhiteNonPawn, white, side);
        let black = histories.correction(Correction::BlackNonPawn, black, side);
        let continuation = match self.previous_square(ply) {
            Some(square) => {
                8761 * (histories.continuation_correction(self.at(ply, 2).correction_row, square)
                    + histories.continuation_correction(self.at(ply, 4).correction_row, square))
            }
            None if self.at(ply, 1).mv != Move::NULL => 0,
            None => 64_049,
        };
        15_341 * pawn + 10_569 * minor + 12_906 * (white + black) + continuation
    }

    fn update_correction_history(&mut self, ply: usize, bonus: i32) {
        let side = self.position.side_to_move().index();
        let [white, black] = self.position.non_pawn_keys();
        let (pawn_key, minor_key) = (self.position.pawn_key(), self.position.minor_key());
        let previous = self.previous_square(ply);
        let rows = (
            self.at(ply, 2).correction_row,
            self.at(ply, 4).correction_row,
        );
        let histories = &mut self.memory.histories;
        histories.update_correction(Correction::Pawn, pawn_key, side, bonus);
        histories.update_correction(Correction::Minor, minor_key, side, bonus * 150 / 128);
        histories.update_correction(Correction::WhiteNonPawn, white, side, bonus * 186 / 128);
        histories.update_correction(Correction::BlackNonPawn, black, side, bonus * 186 / 128);
        if let Some(square) = previous {
            histories.update_continuation_correction(rows.0, square, bonus * 130 / 128);
            histories.update_continuation_correction(rows.1, square, bonus * 70 / 128);
        }
    }

    /// Continuation histories of the move pairs the move of `square` at `ply` forms with
    /// the moves one to six plies earlier, weighted by distance and boosted when several
    /// of them already agree.
    fn update_continuation_histories(&mut self, ply: usize, square: usize, bonus: i32) {
        const WEIGHTS: [(usize, i32); 6] =
            [(1, 520), (2, 390), (3, 145), (4, 251), (5, 66), (6, 209)];
        const MULTIPLIERS: [i32; 7] = [94, 103, 110, 106, 119, 126, 121];
        let in_check = self.at(ply, 0).in_check;
        let mut positive = 0;
        for (back, weight) in WEIGHTS {
            if in_check && back > 2 {
                break;
            }
            let frame = *self.at(ply, back);
            if frame.mv == Move::NULL {
                continue;
            }
            let histories = &mut self.memory.histories;
            if histories.continuation(frame.continuation, square) > 0 {
                positive += 1;
            }
            let multiplier = MULTIPLIERS[positive];
            histories.update_continuation(
                frame.continuation,
                square,
                bonus * weight * multiplier / 65_536 + 73 * i32::from(back < 2),
            );
        }
    }

    fn update_quiet_histories(&mut self, ply: usize, mv: Move, bonus: i32) {
        let side = self.position.side_to_move().index();
        let square = moved_square(&self.position, mv);
        let pawn_key = self.position.pawn_key();
        let histories = &mut self.memory.histories;
        histories.update_main(side, mv, bonus);
        if ply < LOW_PLY {
            histories.update_low_ply(ply, mv, bonus * 712 / 1024);
        }
        self.update_continuation_histories(ply, square, bonus * 750 / 1024);
        let pawn_bonus = bonus * if bonus > -4 { 1104 } else { 459 } / 1024;
        self.memory
            .histories
            .update_pawn(pawn_key, square, pawn_bonus);
    }

    #[allow(clippy::too_many_arguments)]
    fn update_all_stats(
        &mut self,
        ply: usize,
        best_move: Move,
        quiets: &[Move],
        captures: &[Move],
        depth: i32,
        tt_move: Move,
        pv_node: bool,
    ) {
        let mut bonus = (133 * depth - 81).min(1487)
            + 364 * i32::from(best_move == tt_move)
            + self.at(ply, 1).stat_score / 28;
        let malus = (968 * depth - 235).min(2244);
        if !pv_node {
            bonus += (i64::from(bonus) * (quiets.len() + captures.len()) as i64 / 256) as i32;
        }
        if is_capture_stage(best_move) {
            let square = moved_square(&self.position, best_move);
            let taken = victim_slot(target(&self.position, best_move));
            self.memory
                .histories
                .update_capture(square, taken, bonus * 1427 / 1024);
        } else {
            self.update_quiet_histories(ply, best_move, bonus * 899 / 1024);
            let mut malus = malus * 1159 / 1024;
            for &mv in quiets {
                malus = malus * 921 / 1024;
                self.update_quiet_histories(ply, mv, -malus);
            }
        }
        // An early quiet move of the previous ply, not the table move, that was refuted
        // loses credit.
        let previous = *self.at(ply, 1);
        if previous.move_count == 1 + i32::from(previous.tt_hit)
            && self.position.captured_piece().is_none()
        {
            if let Some(square) = self.previous_square(ply) {
                self.update_continuation_histories(ply - 1, square, -malus * 713 / 1024);
            }
        }
        for &mv in captures {
            let square = moved_square(&self.position, mv);
            let taken = victim_slot(target(&self.position, mv));
            self.memory
                .histories
                .update_capture(square, taken, -malus * 1489 / 1024);
        }
    }

    /// Probes the WDL tables just after a capture or pawn move, when the position has few
    /// enough pieces and no castling rights, as in Stockfish.
    fn probe_tablebases(&mut self, ply: usize) -> Option<(i32, Bound)> {
        let tables = self.tablebases.clone()?;
        if self.position.halfmove_clock() != 0 || !probeable(&self.position, tables.max_pieces()) {
            return None;
        }
        let (wdl, state) = tables.probe_wdl(&mut self.position);
        if state == ProbeState::Fail {
            return None;
        }
        self.tbhits += 1;
        let tb_value = VALUE_TB - ply as i32;
        Some(if wdl < -1 {
            (-tb_value, Bound::Upper)
        } else if wdl > 1 {
            (tb_value, Bound::Lower)
        } else {
            (self.draw_score() + 2 * wdl, Bound::Exact)
        })
    }

    /// A move that repeats the piece shuffling of the last four plies late in a quiet
    /// phase, which singular extensions leave alone.
    fn is_shuffling(&self, mv: Move, ply: usize) -> bool {
        if is_capture_stage(mv)
            || self.position.halfmove_clock() < 10
            || self.position.plies_from_null() < 6
            || ply < 20
        {
            return false;
        }
        let (two, four) = (self.at(ply, 2).mv, self.at(ply, 4).mv);
        mv.from() == two.to() && two.from() == four.to()
    }

    fn update_pv(&mut self, ply: usize, mv: Move) {
        self.pv[ply][0] = mv;
        let child_len = self.pv_len[ply + 1].min(MAX_PLY - ply);
        for index in 0..child_len {
            self.pv[ply][index + 1] = self.pv[ply + 1][index];
        }
        self.pv_len[ply] = child_len + 1;
    }

    /// Iterative deepening with aspiration windows, and for the main thread time
    /// management, after Stockfish 19's `iterative_deepening`.
    #[allow(clippy::too_many_lines)]
    fn iterative_deepening(&mut self, report: &mut impl FnMut(Info)) {
        if self.root_moves.is_empty() {
            return;
        }
        let main = self.is_main();
        let us = self.position.side_to_move().index();
        let previous = self.tt.previous();
        // With no earlier search in the game the falling-eval term takes its maximum, as
        // Stockfish's does on the first move.
        let (previous_score, previous_average) = previous.score.unwrap_or((0, VALUE_INFINITE));
        let mut iteration_scores = [previous_score; 4];
        let mut iteration_index = 0;
        let mut last_best_pv: Vec<Move> = Vec::new();
        let mut last_best_depth = 0;
        let mut last_best_score = -VALUE_INFINITE;
        let mut time_reduction = 1.0;
        let mut total_changes = 0.0;
        let mut search_again = 0;
        let mut best_value = -VALUE_INFINITE;
        let multi_pv = self.line_count();
        self.memory.histories.start_search();
        while (self.root_depth + 1) < MAX_PLY as i32 && !self.stopped() {
            if main
                && self
                    .limits
                    .depth
                    .is_some_and(|depth| self.root_depth >= i32::from(depth))
            {
                break;
            }
            self.root_depth += 1;
            if main {
                total_changes /= 2.0;
            }
            for (index, root) in self.root_moves.iter_mut().enumerate() {
                root.previous_score = root.score;
                root.previous_pv = root.pv.clone();
                root.previous_exact = index < multi_pv;
            }
            let mut pv_first = 0;
            self.pv_last = 0;
            if !self.shared.increase_depth.load(Ordering::Relaxed) {
                search_again += 1;
            }
            self.pv_index = 0;
            while self.pv_index < multi_pv {
                if self.pv_index == self.pv_last {
                    pv_first = self.pv_last;
                    self.pv_last += 1;
                    while self.pv_last < self.root_moves.len()
                        && self.root_moves[self.pv_last].tb_rank
                            == self.root_moves[pv_first].tb_rank
                    {
                        self.pv_last += 1;
                    }
                }
                let root = &self.root_moves[self.pv_index];
                self.last_iteration_pv = root.previous_pv.clone();
                self.sel_depth = 0;
                let mut delta = 5
                    + (self.thread % 8) as i32
                    + (root.mean_squared_score.abs() / 10_193).min(i64::from(VALUE_INFINITE))
                        as i32;
                let average = root.average_score;
                let mut alpha = (average - delta).max(-VALUE_INFINITE);
                let mut beta = (average + delta).min(VALUE_INFINITE);
                let optimism = 114 * average / (average.abs() + 85);
                self.optimism[us] = optimism;
                self.optimism[1 - us] = -optimism;
                let mut failed_high = 0;
                loop {
                    let adjusted =
                        (self.root_depth - failed_high - 3 * (search_again + 1) / 4).max(1);
                    self.root_delta = (beta - alpha).max(1);
                    best_value = self.search::<true, true>(adjusted, alpha, beta, 0, false);
                    stable_sort(&mut self.root_moves[self.pv_index..self.pv_last]);
                    if self.stopped() {
                        break;
                    }
                    if best_value <= alpha {
                        beta = alpha;
                        alpha = (best_value - delta).max(-VALUE_INFINITE);
                        failed_high = 0;
                    } else if best_value >= beta {
                        alpha = (beta - delta).max(alpha);
                        beta = (best_value + delta).min(VALUE_INFINITE);
                        failed_high += 1;
                    } else {
                        break;
                    }
                    delta += 47 * delta / 128;
                }
                // A line cut short by the stop keeps its last exact result rather than an
                // unfinished loss.
                if self.stopped() && self.pv_index > 0 {
                    let index = self.pv_index;
                    let above = self.root_moves[index - 1].score;
                    let root = &mut self.root_moves[index];
                    if (is_loss(above) && root.score < above) || root.is_exact_loss() {
                        if root.previous_score != -VALUE_INFINITE
                            && root.previous_exact
                            && root.previous_score <= above
                        {
                            root.score = root.previous_score;
                            root.uci_score = root.previous_score;
                            root.previous_score = -VALUE_INFINITE;
                            root.pv = root.previous_pv.clone();
                            root.unset_inexact();
                        } else {
                            if is_loss(above) {
                                root.score = above;
                                root.uci_score = above;
                                root.previous_score = -VALUE_INFINITE;
                                root.pv.truncate(1);
                                root.inexact_upper = true;
                            } else {
                                root.inexact_upper = false;
                            }
                            root.inexact_lower = !root.inexact_upper;
                        }
                    }
                }
                stable_sort(&mut self.root_moves[pv_first..=self.pv_index]);
                if self.stopped() {
                    break;
                }
                self.pv_index += 1;
            }
            let forgotten_mate = last_best_score != -VALUE_INFINITE
                && last_best_score.abs() >= MATE_IN_MAX_PLY
                && (self.root_moves[0].score.abs() < last_best_score.abs()
                    || self.root_moves[0].is_inexact());
            if !self.stopped() {
                self.completed_depth = self.root_depth;
                if last_best_pv.first() != self.root_moves[0].pv.first() {
                    last_best_depth = self.root_depth;
                }
                if !forgotten_mate {
                    last_best_pv = self.root_moves[0].pv.clone();
                    last_best_score = self.root_moves[0].score;
                }
            }
            let aborted_loss =
                self.stopped() && self.pv_index == 0 && self.root_moves[0].is_exact_loss();
            if aborted_loss || (self.root_moves[0].score != -VALUE_INFINITE && forgotten_mate) {
                if let Some(&first) = last_best_pv.first() {
                    if let Some(position) =
                        self.root_moves.iter().position(|root| root.pv[0] == first)
                    {
                        self.root_moves[..=position].rotate_right(1);
                        let root = &mut self.root_moves[0];
                        root.score = last_best_score;
                        root.uci_score = last_best_score;
                        root.pv = last_best_pv.clone();
                        root.unset_inexact();
                    }
                } else if aborted_loss {
                    self.root_moves[0].inexact_lower = true;
                }
            }
            if main && !self.stopped() {
                for index in 0..multi_pv.min(self.limits.multipv.max(1)) {
                    let info = self.info(&self.root_moves[index], self.completed_depth, index + 1);
                    report(info);
                }
            }
            if !main {
                continue;
            }
            total_changes += self.shared.best_move_changes.swap(0, Ordering::Relaxed) as f64;
            let decided = self.root_moves[multi_pv - 1].score >= mate_in(3)
                || self.root_moves[0].score == mated_in(2);
            self.refresh_ponder();
            if let (Some(optimum), false) = (self.optimum, self.stopped() || self.limits.infinite) {
                let effort = (self.root_moves[0].effort * 100_000 / self.nodes.max(1)) as f64;
                let falling = ((11.48
                    + 2.30 * f64::from(previous_average - best_value)
                    + 1.1 * f64::from(iteration_scores[iteration_index] - best_value))
                    / 100.0)
                    .clamp(0.576, 1.728);
                time_reduction = interpolate(
                    f64::from(self.root_depth - last_best_depth),
                    4.96,
                    18.79,
                    0.639,
                    1.712,
                )
                .clamp(0.629, 1.544);
                let reduction = (1.468 + previous.time_reduction) / (2.284 * time_reduction);
                let instability = 1.077 + 2.229 * total_changes / self.threads as f64;
                let high_effort =
                    interpolate(effort, 75_800.0, 104_510.0, 0.969, 0.714).clamp(0.693, 0.838);
                let mut total = optimum * falling * reduction * instability * high_effort;
                if self.root_moves.len() == 1 {
                    total = total.min(500.0);
                }
                let limit = self.maximum.map_or(total, |maximum| total.min(maximum));
                let elapsed = self.elapsed();
                if !self.pondering() && (elapsed > limit || decided) {
                    self.stop();
                } else {
                    self.shared.increase_depth.store(
                        self.pondering() || elapsed <= total * 0.5,
                        Ordering::Relaxed,
                    );
                }
            }
            iteration_scores[iteration_index] = best_value;
            iteration_index = (iteration_index + 1) & 3;
        }
        if main {
            self.tt.set_previous(Previous {
                time_reduction,
                ..self.tt.previous()
            });
        }
    }

    /// Principal variation search after Stockfish 19's `search`, whose pruning, extension
    /// and reduction rules and constants it follows.
    #[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
    fn search<const PV: bool, const ROOT: bool>(
        &mut self,
        mut depth: i32,
        mut alpha: i32,
        mut beta: i32,
        ply: usize,
        cut_node: bool,
    ) -> i32 {
        let all_node = !(PV || cut_node);
        let seek_mate = self.root_depth >= 16 && self.root_moves[self.pv_index].score.abs() >= 2000;
        if depth <= 0 {
            return self.quiescence::<PV>(alpha, beta, ply);
        }
        depth = depth.min(MAX_PLY as i32 - 1);
        self.pv_len[ply] = 0;
        if !ROOT && alpha < 0 && self.position.upcoming_repetition(ply) {
            alpha = self.value_draw();
            if alpha >= beta {
                return alpha;
            }
        }
        let in_check = self.position.checkers().0 != 0;
        let prior_capture = self.position.captured_piece().is_some();
        let side = self.position.side_to_move();
        let us = side.index();
        {
            let previous = *self.at(ply, 1);
            let follow_pv = ROOT
                || (previous.follow_pv
                    && ply >= 1
                    && self.last_iteration_pv.get(ply - 1) == Some(&previous.mv));
            let frame = self.at_mut(ply, 0);
            frame.in_check = in_check;
            frame.move_count = 0;
            frame.follow_pv = follow_pv;
        }
        let mut best = -VALUE_INFINITE;
        let mut max_value = VALUE_INFINITE;
        if self.is_main() {
            self.check_time();
        }
        if PV && self.sel_depth < ply + 1 {
            self.sel_depth = ply + 1;
        }
        if !ROOT {
            if self.stopped() || self.position.is_draw(ply) || ply >= MAX_PLY {
                return if ply >= MAX_PLY && !in_check {
                    self.evaluate()
                } else {
                    self.value_draw()
                };
            }
            alpha = alpha.max(mated_in(ply));
            beta = beta.min(mate_in(ply + 1));
            if alpha >= beta {
                return alpha;
            }
        }
        let previous_move = self.at(ply, 1).mv;
        let previous_square = self.previous_square(ply);
        let mut best_move = Move::NULL;
        let prior_reduction = self.at(ply, 1).reduction;
        self.at_mut(ply, 1).reduction = 0;
        self.at_mut(ply, 0).stat_score = 0;
        self.at_mut(ply + 2, 0).cutoff_count = 0;
        let correction = self.correction_value(ply);
        // Transposition table lookup. A stored move that is not legal here marks a key
        // collision, and the entry is ignored.
        let excluded = self.at(ply, 0).excluded;
        let key = table_key(&self.position);
        let rule50 = i32::from(self.position.halfmove_clock());
        let hit = self
            .tt
            .probe(key)
            .filter(|record| record.mv == Move::NULL || self.position.is_legal_move(record.mv));
        self.at_mut(ply, 0).tt_hit = hit.is_some();
        let tt_move = if ROOT {
            self.root_moves[self.pv_index].pv[0]
        } else {
            hit.map_or(Move::NULL, |record| record.mv)
        };
        let tt_value = hit.map_or(VALUE_NONE, |record| {
            value_from_tt(record.score, ply, rule50)
        });
        let tt_depth = hit.map_or(DEPTH_NONE, |record| record.depth);
        let tt_bound = hit.map_or(Bound::None, |record| record.bound);
        if excluded == Move::NULL {
            self.at_mut(ply, 0).tt_pv = PV || hit.is_some_and(|record| record.pv);
        }
        let tt_pv = self.at(ply, 0).tt_pv;
        let tt_capture = tt_move != Move::NULL && is_capture_stage(tt_move);
        // Static evaluation.
        let mut unadjusted = VALUE_NONE;
        let mut eval;
        if in_check {
            let earlier = self.at(ply, 2).static_eval;
            self.at_mut(ply, 0).static_eval = earlier;
            eval = earlier;
        } else if excluded != Move::NULL {
            eval = self.at(ply, 0).static_eval;
            unadjusted = eval;
        } else if let Some(record) = hit {
            unadjusted = if is_valid(record.eval) {
                record.eval
            } else {
                self.evaluate()
            };
            eval = corrected(unadjusted, correction);
            self.at_mut(ply, 0).static_eval = eval;
            if is_valid(tt_value) && tt_bound.covers(tt_value > eval) {
                eval = tt_value;
            }
        } else {
            unadjusted = self.evaluate();
            eval = corrected(unadjusted, correction);
            self.at_mut(ply, 0).static_eval = eval;
            self.tt.store(
                key,
                Record {
                    mv: Move::NULL,
                    score: VALUE_NONE,
                    eval: unadjusted,
                    depth: DEPTH_UNSEARCHED,
                    bound: Bound::None,
                    pv: tt_pv,
                },
            );
        }
        let static_eval = self.at(ply, 0).static_eval;
        let mut improving = static_eval > self.at(ply, 2).static_eval;
        let opponent_worsening = static_eval > -self.at(ply, 1).static_eval;
        // Hindsight: adjust the depth by how the evaluation turned out after a reduction.
        if prior_reduction >= 3 && !opponent_worsening {
            depth += 1;
        }
        if prior_reduction >= 2 && depth >= 2 && static_eval + self.at(ply, 1).static_eval > 166 {
            depth -= 1;
        }
        // Table cutoff at non-PV nodes.
        let tt_deep = tt_depth > depth - i32::from(tt_value <= beta);
        if !PV
            && excluded == Move::NULL
            && tt_deep
            && is_valid(tt_value)
            && tt_bound.covers(tt_value >= beta)
            && (cut_node == (tt_value >= beta) || depth > 4)
        {
            if tt_move != Move::NULL && tt_value >= beta {
                if !tt_capture {
                    self.update_quiet_histories(ply, tt_move, (112 * depth).min(695));
                }
                if self.at(ply, 1).move_count < 5 && !prior_capture {
                    if let Some(square) = previous_square {
                        self.update_continuation_histories(ply - 1, square, -2210);
                    }
                }
            }
            if rule50 < 96 {
                if depth >= 7 && tt_move != Move::NULL && !is_decisive(tt_value) {
                    // The cutoff stands if the position after the table move agrees.
                    self.position.make(tt_move);
                    let next = self.tt.probe(table_key(&self.position));
                    self.position.unmake();
                    match next.filter(|record| is_valid(record.score)) {
                        None => return tt_value,
                        Some(record) if (tt_value >= beta) == (-record.score >= beta) => {
                            return tt_value
                        }
                        _ => {}
                    }
                } else {
                    return tt_value;
                }
            }
        } else if !PV
            && excluded == Move::NULL
            && tt_deep
            && is_valid(tt_value)
            && tt_bound != Bound::Exact
            && tt_bound.covers(tt_value < beta)
            && depth > 5
        {
            // Only the bound kept this entry from cutting; it is worth less now.
            self.tt.penalize(key, 1);
        }
        // Tablebases.
        if !ROOT && excluded == Move::NULL {
            if let Some((value, bound)) = self.probe_tablebases(ply) {
                if bound == Bound::Exact
                    || (bound == Bound::Lower && value >= beta)
                    || (bound == Bound::Upper && value <= alpha)
                {
                    self.tt.store(
                        key,
                        Record {
                            mv: Move::NULL,
                            score: value_to_tt(value, ply),
                            eval: VALUE_NONE,
                            depth: (depth + 6).min(MAX_PLY as i32 - 1),
                            bound,
                            pv: tt_pv,
                        },
                    );
                    return value;
                }
                if PV {
                    if bound == Bound::Lower {
                        best = value;
                        alpha = alpha.max(best);
                    } else {
                        max_value = value;
                    }
                }
            }
        }
        if !in_check {
            // The evaluation's swing after the previous quiet move says how good it was.
            let previous = *self.at(ply, 1);
            if previous.mv != Move::NULL && !previous.in_check && !prior_capture {
                let swing = (-(previous.static_eval + static_eval)).clamp(-189, 194) + 60;
                self.memory
                    .histories
                    .update_main(1 - us, previous.mv, swing * 11);
                if let Some(square) = previous_square {
                    let piece = self.position.piece_at(previous.mv.to());
                    if hit.is_none()
                        && piece.is_some_and(|piece| piece.kind != PieceType::Pawn)
                        && previous.mv.promotion().is_none()
                    {
                        let pawn_key = self.position.pawn_key();
                        self.memory
                            .histories
                            .update_pawn(pawn_key, square, swing * 13);
                    }
                }
            }
            // Razoring.
            if !PV && eval < alpha - 482 * depth * depth {
                return self.quiescence::<false>(alpha, beta, ply);
            }
            // Reverse futility pruning.
            if !tt_pv
                && depth < if seek_mate { 6 } else { 19 }
                && eval >= beta
                && (tt_move == Move::NULL || tt_capture)
                && !is_loss(beta)
                && !is_win(eval)
            {
                let multiplier = (45 + depth * 4).min(85) - 20 * i32::from(!self.at(ply, 0).tt_hit);
                let margin = multiplier * depth
                    - (2789 * i32::from(improving) + 335 * i32::from(opponent_worsening))
                        * multiplier
                        / 1024
                    + correction.abs() / 198_435;
                if eval - margin >= beta {
                    return (661 * beta + 363 * eval) / 1024;
                }
            }
            // Null move search with verification.
            if cut_node
                && static_eval >= beta - 13 * depth - 47 * i32::from(improving) + 365
                && excluded == Move::NULL
                && non_pawn_material(&self.position, side) != 0
                && ply >= self.nmp_min_ply
                && beta >= -2000
            {
                let reduction = 7 + depth / 3 + ((static_eval - beta) / 256).max(0);
                self.do_null_move(ply);
                let null_value = -self.search::<false, false>(
                    depth - reduction,
                    -beta,
                    -beta + 1,
                    ply + 1,
                    false,
                );
                self.position.unmake();
                self.at_mut(ply, 0).null = false;
                if self.stopped() {
                    return 0;
                }
                if null_value >= beta && !is_win(null_value) {
                    if self.nmp_min_ply != 0 || depth < 16 {
                        return null_value;
                    }
                    self.nmp_min_ply = ply + (3 * (depth - reduction) / 4).max(0) as usize;
                    let value =
                        self.search::<false, false>(depth - reduction, beta - 1, beta, ply, false);
                    self.nmp_min_ply = 0;
                    if value >= beta {
                        return null_value;
                    }
                }
            }
            improving |= static_eval >= beta;
            // Internal iterative reduction.
            if !self.at(ply, 0).follow_pv && !all_node && depth >= 6 && tt_move == Move::NULL {
                depth -= 1;
            }
            // ProbCut.
            let probcut_beta = beta + 241 - 64 * i32::from(improving);
            if depth >= 3 && !is_decisive(beta) && !(is_valid(tt_value) && tt_value < probcut_beta)
            {
                let mut picker = Picker::probcut(tt_move, probcut_beta - static_eval);
                let probcut_depth = depth - if improving { 5 } else { 3 };
                let slot = LIST_SLOTS * ply + usize::from(excluded != Move::NULL);
                while let Some(mv) = picker.next(
                    &self.position,
                    &self.memory.histories,
                    &mut self.memory.lists[slot],
                ) {
                    if mv == excluded {
                        continue;
                    }
                    self.do_move(mv, ply);
                    let mut value =
                        -self.quiescence::<false>(-probcut_beta, -probcut_beta + 1, ply + 1);
                    if value >= probcut_beta && probcut_depth > 0 {
                        value = -self.search::<false, false>(
                            probcut_depth,
                            -probcut_beta,
                            -probcut_beta + 1,
                            ply + 1,
                            !cut_node,
                        );
                    }
                    self.undo_move();
                    if self.stopped() {
                        return 0;
                    }
                    if value >= probcut_beta {
                        self.tt.store(
                            key,
                            Record {
                                mv,
                                score: value_to_tt(value, ply),
                                eval: unadjusted,
                                depth: probcut_depth + 1,
                                bound: Bound::Lower,
                                pv: tt_pv,
                            },
                        );
                        if !is_decisive(value) {
                            return value - (probcut_beta - beta);
                        }
                    }
                }
            }
        }
        // A small ProbCut: a stored lower bound far above beta.
        let probcut_beta = beta + 428;
        if !ROOT
            && tt_bound.covers(true)
            && tt_depth >= depth - 4
            && tt_value >= probcut_beta
            && !is_decisive(beta)
            && is_valid(tt_value)
            && !is_decisive(tt_value)
        {
            return probcut_beta;
        }
        let continuations = [1, 2, 3, 4, 5, 6].map(|back| self.at(ply, back).continuation);
        let mut picker = Picker::main(&self.position, tt_move, depth, ply, continuations);
        let mut value = best;
        let mut move_count = 0;
        let mut quiets = [Move::NULL; SEARCHED_CAPACITY];
        let mut quiet_count = 0;
        let mut captures = [Move::NULL; SEARCHED_CAPACITY];
        let mut capture_count = 0;
        let own_material = non_pawn_material(&self.position, side);
        let slot = LIST_SLOTS * ply + usize::from(excluded != Move::NULL);
        while let Some(mv) = picker.next(
            &self.position,
            &self.memory.histories,
            &mut self.memory.lists[slot],
        ) {
            if mv == excluded {
                continue;
            }
            if ROOT
                && !self.root_moves[self.pv_index..self.pv_last]
                    .iter()
                    .any(|root| root.pv[0] == mv)
            {
                continue;
            }
            move_count += 1;
            self.at_mut(ply, 0).move_count = move_count;
            let mut extension = 0;
            let capture = is_capture_stage(mv);
            let mover = self
                .position
                .piece_at(mv.from())
                .expect("a move has a mover");
            let square = moved_square(&self.position, mv);
            let gives_check = self.position.gives_check(mv);
            let taken = victim(&self.position, mv);
            let on_target = target(&self.position, mv);
            let mut new_depth = depth - 1;
            let delta = beta - alpha;
            let mut r = self.reduction(improving, depth, move_count, delta);
            if tt_pv {
                r += 929;
            }
            // Pruning at shallow depth.
            if !ROOT && own_material != 0 && !is_loss(best) {
                if move_count >= (3 + depth * depth) / (2 - i32::from(improving)) {
                    picker.skip_quiet_moves();
                }
                let mut lmr_depth = new_depth - r / 1024;
                if capture || gives_check {
                    let capture_history = self
                        .memory
                        .histories
                        .capture(square, victim_slot(on_target));
                    if !gives_check && lmr_depth < 8 {
                        let futility = static_eval
                            + 234
                            + 247 * lmr_depth
                            + victim_value(on_target)
                            + 134 * capture_history / 1024;
                        if futility <= alpha {
                            continue;
                        }
                    }
                    let margin = 177 * depth + capture_history * 34 / 1024;
                    if (alpha >= 0 || own_material != PIECE_VALUES[mover.kind.index()])
                        && !self.position.see_ge(mv, -margin)
                    {
                        continue;
                    }
                } else if !self.at(ply, 0).follow_pv || !PV {
                    let index = (depth.min(LMR_DIVISOR.len() as i32) - 1) as usize;
                    let histories = &self.memory.histories;
                    let mut history = histories.continuation(continuations[0], square)
                        + histories.continuation(continuations[1], square)
                        + histories.pawn(self.position.pawn_key(), square);
                    if history < -4136 * depth {
                        continue;
                    }
                    history += 69 * histories.main(us, mv) / 32;
                    lmr_depth += history / LMR_DIVISOR[index];
                    let futility =
                        static_eval + 119 * lmr_depth + 90 * i32::from(static_eval > alpha) + 164;
                    if !in_check && lmr_depth < 12 && futility <= alpha {
                        if best <= futility && !is_decisive(best) && !is_win(futility) {
                            best = futility;
                        }
                        continue;
                    }
                    let lmr_depth = lmr_depth.max(0);
                    if !self.position.see_ge(mv, -23 * lmr_depth * lmr_depth) {
                        continue;
                    }
                }
            }
            // Singular extension, multi-cut and negative extensions.
            if !ROOT
                && mv == tt_move
                && excluded == Move::NULL
                && depth >= 6 + i32::from(tt_pv)
                && is_valid(tt_value)
                && !is_decisive(tt_value)
                && tt_bound.covers(true)
                && tt_depth >= depth - 3
                && !self.is_shuffling(mv, ply)
                && !seek_mate
            {
                let singular_beta = tt_value - (59 + 66 * i32::from(tt_pv && !PV)) * depth / 63;
                let singular_depth = new_depth / 2;
                self.at_mut(ply, 0).excluded = mv;
                value = self.search::<false, false>(
                    singular_depth,
                    singular_beta - 1,
                    singular_beta,
                    ply,
                    cut_node,
                );
                self.at_mut(ply, 0).excluded = Move::NULL;
                if self.stopped() {
                    return 0;
                }
                if value < singular_beta {
                    let correction_adjust = correction.abs() / 198_368;
                    let beyond_root = i32::from(ply as i32 > self.root_depth);
                    let double_margin = -2 + 204 * i32::from(PV)
                        - 152 * i32::from(!tt_capture)
                        - correction_adjust
                        - 1175 * self.memory.histories.tt_move() / 114_178
                        - beyond_root * 38;
                    let triple_margin = 70 + 279 * i32::from(PV) - 188 * i32::from(!tt_capture)
                        + 81 * i32::from(tt_pv)
                        - correction_adjust
                        - beyond_root * 43;
                    extension = 1
                        + i32::from(value < singular_beta - double_margin)
                        + i32::from(value < singular_beta - triple_margin);
                    depth += 1;
                } else if value >= beta && !is_decisive(value) {
                    self.memory.histories.update_tt_move(-421 - 110 * depth);
                    if !in_check && value > static_eval {
                        let bonus = ((value - static_eval) * singular_depth * 177 / 1024)
                            .clamp(-CORRECTION_LIMIT / 4, CORRECTION_LIMIT / 4);
                        self.update_correction_history(ply, bonus);
                    }
                    return value;
                } else if tt_value >= beta || cut_node {
                    extension = -3;
                }
            }
            let node_count = if ROOT { self.nodes } else { 0 };
            let (main_history, continuation_one, continuation_two) = {
                let histories = &self.memory.histories;
                (
                    histories.main(us, mv),
                    histories.continuation(continuations[0], square),
                    histories.continuation(continuations[1], square),
                )
            };
            let capture_history = self.memory.histories.capture(square, victim_slot(taken));
            self.do_move(mv, ply);
            new_depth += extension;
            // Late move reductions, in 1024ths of a ply.
            if tt_pv {
                r -= 3023
                    + i32::from(PV) * 1004
                    + i32::from(tt_value > alpha) * 885
                    + i32::from(tt_depth >= depth) * (816 + i32::from(cut_node) * 940);
            }
            r += 697;
            r -= move_count * 65;
            r -= correction.abs() / 26_310;
            if cut_node {
                r += 4026 + 933 * i32::from(tt_move == Move::NULL);
            }
            if tt_capture {
                r += 1079;
            }
            let child_cutoffs = self.at(ply + 1, 0).cutoff_count;
            if child_cutoffs > 1 {
                r += 264 + 1095 * i32::from(child_cutoffs > 2) + 1138 * i32::from(all_node);
            } else if mv == tt_move {
                r -= 2179;
            }
            let stat_score = if capture {
                873 * victim_value(taken) / 128 + capture_history
            } else {
                (2252 * main_history + 1126 * continuation_one + 1093 * continuation_two) / 1024
            };
            self.at_mut(ply, 0).stat_score = stat_score;
            r -= stat_score * 439 / 4096;
            if !capture && !is_decisive(alpha) {
                r += 3 * (alpha - eval).clamp(-64, 96);
            }
            if all_node {
                r += r * 276 / (256 * depth + 268);
            }
            if depth >= 2 && move_count > 1 {
                let reduced = (new_depth - r / 1024).min(new_depth + 2).max(1) + i32::from(PV);
                self.at_mut(ply, 0).reduction = new_depth - reduced;
                value = -self.search::<false, false>(reduced, -(alpha + 1), -alpha, ply + 1, true);
                self.at_mut(ply, 0).reduction = 0;
                if value > alpha && !self.stopped() {
                    let deeper = reduced < new_depth && value > best + 53;
                    let shallower = value < best + 8;
                    new_depth += i32::from(deeper) - i32::from(shallower);
                    if new_depth > reduced {
                        value = -self.search::<false, false>(
                            new_depth,
                            -(alpha + 1),
                            -alpha,
                            ply + 1,
                            !cut_node,
                        );
                    }
                    self.update_continuation_histories(ply, square, 1334);
                }
            } else if !PV || move_count > 1 {
                if tt_move == Move::NULL {
                    r += 1127;
                }
                let reduced =
                    new_depth - i32::from(r > 5234) - i32::from(r > 5487 && new_depth > 2);
                value =
                    -self.search::<false, false>(reduced, -(alpha + 1), -alpha, ply + 1, !cut_node);
            }
            if PV && (move_count == 1 || value > alpha) && !self.stopped() {
                if mv == tt_move
                    && ((is_valid(tt_value) && is_decisive(tt_value) && tt_depth > 0)
                        || tt_depth > 1)
                {
                    new_depth = new_depth.max(1);
                }
                value = -self.search::<true, false>(new_depth, -beta, -alpha, ply + 1, false);
            }
            self.undo_move();
            if self.stopped() {
                return 0;
            }
            if ROOT {
                let spent = self.nodes - node_count;
                let sel_depth = self.sel_depth;
                let child_pv: Vec<Move> = self.pv[1][..self.pv_len[1]].to_vec();
                let first_line = self.pv_index == 0;
                let root = self
                    .root_moves
                    .iter_mut()
                    .find(|root| root.pv[0] == mv)
                    .expect("a searched root move is listed");
                root.effort += spent;
                let earlier = (root.effort - spent).max(1);
                let weight = (32 * spent * 2 / (spent * 2 + 3 * earlier)).clamp(12, 24);
                let squared_weight = weight.min(16) as i64;
                let (weight, value64) = (weight as i64, i64::from(value));
                let squared = value64 * value64.abs();
                // Stockfish computes these averages in unsigned arithmetic, which rounds
                // negative results down rather than toward zero.
                root.average_score = if root.average_score == -VALUE_INFINITE {
                    value
                } else {
                    ((value64 * weight + i64::from(root.average_score) * (32 - weight))
                        .div_euclid(32)) as i32
                };
                root.mean_squared_score = if root.mean_squared_score
                    == -i64::from(VALUE_INFINITE) * i64::from(VALUE_INFINITE)
                {
                    squared
                } else {
                    (squared * squared_weight + root.mean_squared_score * (32 - squared_weight))
                        .div_euclid(32)
                };
                if move_count == 1 || value > alpha {
                    root.score = value;
                    root.uci_score = value;
                    root.sel_depth = sel_depth;
                    root.unset_inexact();
                    if value >= beta {
                        root.inexact_lower = true;
                        root.uci_score = beta;
                    } else if value <= alpha {
                        root.inexact_upper = true;
                        root.uci_score = alpha;
                    }
                    root.pv.truncate(1);
                    root.pv.extend(child_pv);
                    if move_count > 1 && first_line {
                        self.shared
                            .best_move_changes
                            .fetch_add(1, Ordering::Relaxed);
                    }
                } else {
                    root.score = -VALUE_INFINITE;
                }
            }
            // An alternative as good as the best is sometimes promoted near the leaves.
            let promote = i32::from(
                value == best
                    && ply + 2 >= self.root_depth as usize
                    && self.nodes & 14 == 0
                    && !is_win(value.abs() + 1),
            );
            if value + promote > best {
                best = value;
                if value + promote > alpha {
                    best_move = mv;
                    if PV && !ROOT {
                        self.update_pv(ply, mv);
                    }
                    if value >= beta {
                        self.at_mut(ply, 0).cutoff_count += i32::from(extension < 2 || PV);
                        break;
                    }
                    if depth > 3 && depth < 12 && !is_decisive(value) {
                        depth -= 3;
                    }
                    alpha = value;
                }
            }
            if mv != best_move && move_count as usize <= SEARCHED_CAPACITY {
                if capture && capture_count < SEARCHED_CAPACITY {
                    captures[capture_count] = mv;
                    capture_count += 1;
                } else if !capture && quiet_count < SEARCHED_CAPACITY {
                    quiets[quiet_count] = mv;
                    quiet_count += 1;
                }
            }
        }
        if best >= beta && !is_decisive(best) && !is_decisive(alpha) {
            best = (best * depth + beta) / (depth + 1);
        }
        if move_count == 0 {
            best = if excluded != Move::NULL {
                alpha
            } else if in_check {
                mated_in(ply)
            } else {
                self.draw_score()
            };
        } else if best_move != Move::NULL {
            self.update_all_stats(
                ply,
                best_move,
                &quiets[..quiet_count],
                &captures[..capture_count],
                depth,
                tt_move,
                PV,
            );
            if !PV {
                let bonus = if best_move == tt_move { 918 } else { -747 };
                self.memory.histories.update_tt_move(bonus);
            }
        } else if let Some(square) = previous_square {
            if !prior_capture {
                // The previous quiet move refuted everything here, so it gets credit.
                let previous = *self.at(ply, 1);
                let scale = (-241 - previous.stat_score / 98
                    + (59 * depth).min(420)
                    + 186 * i32::from(previous.move_count > 9)
                    + 142 * i32::from(!in_check && best <= static_eval - 106)
                    + 159 * i32::from(!previous.in_check && best <= -previous.static_eval - 68))
                .max(0);
                let scaled = (150 * depth - 85).min(1337) * scale;
                self.update_continuation_histories(ply - 1, square, scaled * 263 / 16_384);
                self.memory
                    .histories
                    .update_main(1 - us, previous_move, scaled * 215 / 32_768);
                let piece = self.position.piece_at(previous_move.to());
                if piece.is_some_and(|piece| piece.kind != PieceType::Pawn)
                    && previous_move.promotion().is_none()
                {
                    let pawn_key = self.position.pawn_key();
                    self.memory
                        .histories
                        .update_pawn(pawn_key, square, scaled * 324 / 8192);
                }
            } else if let Some(captured) = self.position.captured_piece() {
                self.memory
                    .histories
                    .update_capture(square, captured.kind.index() + 1, 892);
            }
        }
        if PV {
            best = best.min(max_value);
        }
        if best <= alpha {
            let frame_pv = self.at(ply, 0).tt_pv || self.at(ply, 1).tt_pv;
            self.at_mut(ply, 0).tt_pv = frame_pv;
        }
        if excluded == Move::NULL && !(ROOT && self.pv_index > 0) {
            let bound = if best >= beta {
                Bound::Lower
            } else if PV && best_move != Move::NULL {
                Bound::Exact
            } else {
                Bound::Upper
            };
            self.tt.store(
                key,
                Record {
                    mv: best_move,
                    score: value_to_tt(best, ply),
                    eval: unadjusted,
                    depth: if move_count != 0 {
                        depth
                    } else {
                        (depth + 6).min(MAX_PLY as i32 - 1)
                    },
                    bound,
                    pv: self.at(ply, 0).tt_pv,
                },
            );
        }
        // The search result corrects the static evaluation when it lies on the side the
        // bound allows: above it with a best move, below it without one.
        if !in_check
            && !(best_move != Move::NULL && is_capture(best_move))
            && (best > static_eval) == (best_move != Move::NULL)
        {
            let scale = if best_move == Move::NULL { 18 } else { 12 };
            let bonus = ((best - static_eval) * depth * scale / 128)
                .clamp(-CORRECTION_LIMIT / 4, CORRECTION_LIMIT / 4);
            self.update_correction_history(ply, 1061 * bonus / 1024);
        }
        best
    }

    /// Quiescence search after Stockfish 19's `qsearch`: captures and queen promotions,
    /// or evasions in check, until the position is quiet; stand pat when not in check.
    #[allow(clippy::too_many_lines)]
    fn quiescence<const PV: bool>(&mut self, mut alpha: i32, beta: i32, ply: usize) -> i32 {
        self.pv_len[ply] = 0;
        if alpha < 0 && self.position.upcoming_repetition(ply) {
            alpha = self.value_draw();
            if alpha >= beta {
                return alpha;
            }
        }
        let in_check = self.position.checkers().0 != 0;
        self.at_mut(ply, 0).in_check = in_check;
        if PV && self.sel_depth < ply + 1 {
            self.sel_depth = ply + 1;
        }
        if self.position.is_draw(ply) || ply >= MAX_PLY {
            return if ply >= MAX_PLY && !in_check {
                self.evaluate()
            } else {
                self.draw_score()
            };
        }
        let key = table_key(&self.position);
        let rule50 = i32::from(self.position.halfmove_clock());
        let hit = self
            .tt
            .probe(key)
            .filter(|record| record.mv == Move::NULL || self.position.is_legal_move(record.mv));
        self.at_mut(ply, 0).tt_hit = hit.is_some();
        let tt_move = hit.map_or(Move::NULL, |record| record.mv);
        let tt_value = hit.map_or(VALUE_NONE, |record| {
            value_from_tt(record.score, ply, rule50)
        });
        let tt_bound = hit.map_or(Bound::None, |record| record.bound);
        let pv_hit = hit.is_some_and(|record| record.pv);
        if !PV
            && hit.is_some_and(|record| record.depth >= DEPTH_QS)
            && is_valid(tt_value)
            && tt_bound.covers(tt_value >= beta)
        {
            return tt_value;
        }
        let mut unadjusted = VALUE_NONE;
        let mut best;
        let futility_base;
        if in_check {
            best = -VALUE_INFINITE;
            futility_base = -VALUE_INFINITE;
        } else {
            let correction = self.correction_value(ply);
            unadjusted = match hit {
                Some(record) if is_valid(record.eval) => record.eval,
                _ => self.evaluate(),
            };
            let static_eval = corrected(unadjusted, correction);
            self.at_mut(ply, 0).static_eval = static_eval;
            best = static_eval;
            if hit.is_some()
                && is_valid(tt_value)
                && !is_decisive(tt_value)
                && tt_bound.covers(tt_value > best)
            {
                best = tt_value;
            }
            if best >= beta {
                if !is_decisive(best) {
                    best = (441 * best + 583 * beta) / 1024;
                }
                if hit.is_none() {
                    self.tt.store(
                        key,
                        Record {
                            mv: Move::NULL,
                            score: VALUE_NONE,
                            eval: unadjusted,
                            depth: DEPTH_UNSEARCHED,
                            bound: Bound::Lower,
                            pv: false,
                        },
                    );
                }
                return best;
            }
            if best > alpha {
                alpha = best;
            }
            futility_base = static_eval + 306;
        }
        let previous = self.at(ply, 1).mv;
        let previous_to = (previous != Move::NULL).then(|| previous.to());
        let continuations = [1, 2, 3, 4, 5, 6].map(|back| self.at(ply, back).continuation);
        let mut picker = Picker::main(&self.position, tt_move, DEPTH_QS, ply, continuations);
        let mut best_move = Move::NULL;
        let mut move_count = 0;
        let slot = LIST_SLOTS * ply + 2;
        while let Some(mv) = picker.next(
            &self.position,
            &self.memory.histories,
            &mut self.memory.lists[slot],
        ) {
            let gives_check = self.position.gives_check(mv);
            let capture = is_capture_stage(mv);
            move_count += 1;
            if !is_loss(best) {
                if !gives_check
                    && Some(mv.to()) != previous_to
                    && !is_loss(futility_base)
                    && mv.promotion().is_none()
                {
                    if move_count > 2 {
                        continue;
                    }
                    let on_target = self
                        .position
                        .piece_at(mv.to())
                        .map_or(0, |piece| PIECE_VALUES[piece.kind.index()]);
                    let futility = futility_base + on_target;
                    if futility <= alpha {
                        best = best.max(futility);
                        continue;
                    }
                    if !self.position.see_ge(mv, alpha - futility_base) {
                        best = best.max(alpha.min(futility_base));
                        continue;
                    }
                }
                if !capture {
                    continue;
                }
                if !self.position.see_ge(mv, -74) {
                    continue;
                }
            }
            self.do_move(mv, ply);
            let value = -self.quiescence::<PV>(-beta, -alpha, ply + 1);
            self.undo_move();
            if value > best {
                best = value;
                if value > alpha {
                    best_move = mv;
                    if PV {
                        self.update_pv(ply, mv);
                    }
                    if value < beta {
                        alpha = value;
                    } else {
                        break;
                    }
                }
            }
        }
        if move_count == 0 {
            if in_check {
                return mated_in(ply);
            }
            // Stalemate is checked for only where it is plausible: no pawn can push, the
            // side has no pieces, and a piece was just captured.
            let side = self.position.side_to_move();
            let pawns = self.position.pieces(side, PieceType::Pawn);
            let pushes = if side == Color::White {
                Bitboard(pawns.0 << 8)
            } else {
                Bitboard(pawns.0 >> 8)
            };
            if (pushes & !self.position.occupied()).0 == 0
                && non_pawn_material(&self.position, side) == 0
                && self
                    .position
                    .captured_piece()
                    .is_some_and(|piece| piece.kind != PieceType::Pawn)
                && self.position.legal_moves().is_empty()
            {
                best = 0;
            }
        }
        if !is_decisive(best) && best > beta {
            best = (462 * best + 562 * beta) / 1024;
        }
        self.tt.store(
            key,
            Record {
                mv: best_move,
                score: value_to_tt(best, ply),
                eval: unadjusted,
                depth: DEPTH_QS,
                bound: if best >= beta {
                    Bound::Lower
                } else {
                    Bound::Upper
                },
                pv: pv_hit,
            },
        );
        best
    }
}

/// The static evaluation with Stockfish 19's correction, kept below the tablebase range.
fn corrected(value: i32, correction: i32) -> i32 {
    (value + correction / 131_072).clamp(-TB_WIN_IN_MAX_PLY + 1, TB_WIN_IN_MAX_PLY - 1)
}

/// Win, draw and loss chances in permille for the side to move, from a centipawn score.
/// The logistic model `win = 1 / (1 + exp((a - score) / b))`, with loss mirrored, was
/// fitted by maximum likelihood to inphish self-play evaluations at 1+0.01, so it is only
/// an estimate.
pub fn wdl(score: i32) -> (u16, u16, u16) {
    const A: f64 = 714.0;
    const B: f64 = 290.0;
    if is_win(score) {
        return (1000, 0, 0);
    }
    if is_loss(score) {
        return (0, 0, 1000);
    }
    let score = f64::from(score);
    let win = (1000.0 / (1.0 + ((A - score) / B).exp())).round() as u16;
    let loss = (1000.0 / (1.0 + ((A + score) / B).exp())).round() as u16;
    (win, 1000 - win - loss, loss)
}

/// A reported score as UCI prints it: mates in moves, anything else in centipawns.
pub fn uci_score(score: i32) -> String {
    if score.abs() >= MATE_IN_MAX_PLY {
        let plies = VALUE_MATE - score.abs();
        let moves = (plies + 1) / 2;
        format!("mate {}", if score < 0 { -moves } else { moves })
    } else {
        format!("cp {score}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn control() -> Control {
        Control {
            stop: Arc::new(AtomicBool::new(false)),
            ponderhit: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn finds_mate_in_one() {
        let position = Position::from_fen("k7/8/1QK5/8/8/8/8/8 w - - 0 1").unwrap();
        let result = search(
            position,
            Limits {
                depth: Some(3),
                ..Limits::default()
            },
            &control(),
            |_| {},
        );
        assert_eq!(uci_score(result.info.score), "mate 1");
    }

    #[test]
    fn wdl_is_complete_and_monotonic() {
        let mut previous = (0, 0, 1000);
        for score in (-VALUE_MATE..=VALUE_MATE).step_by(7) {
            let (win, draw, loss) = wdl(score);
            assert_eq!(win + draw + loss, 1000, "{score}");
            assert!(win >= previous.0 && loss <= previous.2, "{score}");
            previous = (win, draw, loss);
        }
        assert_eq!(wdl(0).0, wdl(0).2);
        assert_eq!(wdl(VALUE_MATE - 3), (1000, 0, 0));
    }

    #[test]
    fn stopped_search_keeps_legal_fallback() {
        let control = control();
        control.stop.store(true, Ordering::Relaxed);
        let position = Position::startpos();
        let result = search(position, Limits::default(), &control, |_| {});
        assert!(result.best.is_some());
    }

    /// Lines reported where repetitions are available are made of legal moves; an early
    /// return on an upcoming repetition once left a stale line behind.
    #[test]
    fn reported_lines_are_legal_with_repetitions_available() {
        let mut position = Position::startpos();
        for text in [
            "g1f3", "g8f6", "f3g1", "f6g8", "g1f3", "g8f6", "b1c3", "b8c6", "c3b1", "c6b8",
        ] {
            let mv = position.parse_move(text, false).expect(text);
            position.make(mv);
        }
        let quiet_endgame = Position::from_fen("8/5k2/3r4/8/8/3R4/5K2/8 w - - 0 60").unwrap();
        for position in [position, quiet_endgame] {
            let mut lines = Vec::new();
            search(
                position.clone(),
                Limits {
                    depth: Some(16),
                    multipv: 3,
                    ..Limits::default()
                },
                &control(),
                |info| lines.push(info.pv),
            );
            for line in lines {
                let mut after = position.clone();
                for mv in line {
                    assert!(after.is_legal(mv), "{}", after.fen());
                    after.make(mv);
                }
            }
        }
    }

    #[test]
    fn table_values_round_trip_mates_and_tablebase_wins() {
        for value in [
            VALUE_MATE - 7,
            -VALUE_MATE + 9,
            VALUE_TB - 20,
            -VALUE_TB + 5,
            123,
        ] {
            assert_eq!(value_from_tt(value_to_tt(value, 5), 5, 0), value);
        }
        assert_eq!(value_from_tt(VALUE_MATE - 7, 3, 98), TB_WIN_IN_MAX_PLY - 1);
    }

    #[test]
    fn allocation_leaves_a_margin() {
        let mut adjust = None;
        for (time, increment) in [(1000, 10), (60_000, 1000), (300, 10), (20, 0)] {
            let clock = Clock {
                time,
                increment,
                movestogo: None,
                overhead: 10,
            };
            let (optimum, maximum) = allocate(&clock, 20, &mut adjust);
            assert!(optimum >= 1 && optimum <= maximum, "{time}");
            assert!(maximum + 10 <= time.max(16), "{time} {maximum}");
        }
    }
}
