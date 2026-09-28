use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use inphzugzwang_core::{Move, Position};
use inphzugzwang_nnue::{network, Accumulator, RefreshCache, Update};
use inphzugzwang_syzygy::{probeable, ProbeState, Tablebases, WDL_DRAW};

mod history;
mod picker;
mod tt;

use history::{continuation_table, piece_square, Histories, SENTINEL};
use picker::{is_capture, moved_square, victim, victim_slot, Picker};
pub use tt::TranspositionTable;
use tt::{Bound, Record, DEPTH_NONE};

const MAX_PLY: usize = 128;
const MAX_MOVES: usize = 256;
const MATE: i32 = 30_000;
const INF: i32 = 32_000;
/// Marks a missing score or evaluation; outside every real score.
const VALUE_NONE: i32 = 32_001;
const MATE_BOUND: i32 = MATE - MAX_PLY as i32;
/// Score of a tablebase win, just below the mate range so it is never shown as a mate.
const TB_WIN: i32 = MATE_BOUND - 1;
/// Lowest tablebase win score; evaluations stay below it.
const TB_WIN_IN_MAX_PLY: i32 = MATE_BOUND - MAX_PLY as i32;
/// Stockfish's `VALUE_KNOWN_WIN` in centipawns.
const KNOWN_WIN: i32 = v(10_000);
const CORRECTION_ENTRIES: usize = 16_384;
const CORRECTION_GRAIN: i32 = 256;
const CORRECTION_MAX: i32 = 64 * CORRECTION_GRAIN;

#[derive(Clone, Default)]
pub struct Limits {
    pub depth: Option<u8>,
    pub nodes: Option<u64>,
    pub soft: Option<Duration>,
    pub hard: Option<Duration>,
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

/// Statistics a search thread learns and keeps for later moves of the same game: the
/// move-ordering histories and the evaluation correction.
pub(crate) struct Memory {
    histories: Histories,
    correction: Box<[[i32; CORRECTION_ENTRIES]; 2]>,
}

impl Memory {
    fn new() -> Self {
        Self {
            histories: Histories::new(),
            correction: vec![[0; CORRECTION_ENTRIES]; 2]
                .into_boxed_slice()
                .try_into()
                .expect("two sides"),
        }
    }
}

struct Search<'a> {
    position: Position,
    limits: Limits,
    control: &'a Control,
    tt: &'a TranspositionTable,
    started: Instant,
    timed_started: Option<Instant>,
    nodes: u64,
    /// Nodes of all threads, added in batches; node limits apply to this total.
    shared_nodes: &'a AtomicU64,
    tbhits: u64,
    seldepth: usize,
    aborted: bool,
    memory: Memory,
    stack: Box<[Frame]>,
    /// Late move reduction factors by depth or move number, as in Stockfish.
    reductions: Box<[i32]>,
    root_depth: i32,
    completed_depth: i32,
    /// Width of the root window, which scales reductions.
    root_delta: i32,
    /// Ply below which null moves stay off for `nmp_side` during a verification search.
    nmp_min_ply: usize,
    nmp_side: usize,
    pv: [[Move; MAX_PLY]; MAX_PLY],
    pv_len: [usize; MAX_PLY],
    accumulators: Box<[Accumulator]>,
    /// Whether each ply's accumulator is current; if not, `updates` holds how it
    /// follows from the previous ply's.
    computed: [bool; MAX_PLY + 1],
    updates: Box<[Update]>,
    refresh_cache: RefreshCache,
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
/// share only the transposition table, which they fill with results the main thread
/// reuses. The main thread alone manages time, reports and chooses the move.
pub fn search_with_table(
    position: Position,
    limits: Limits,
    control: &Control,
    tt: &TranspositionTable,
    on_info: impl FnMut(Info),
) -> Result {
    tt.next_generation();
    let mut limits = limits;
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
    let shared_nodes = AtomicU64::new(0);
    let helper_control = Control {
        stop: Arc::new(AtomicBool::new(false)),
        ponderhit: Arc::new(AtomicBool::new(false)),
    };
    let helpers = limits.threads.max(1) - 1;
    std::thread::scope(|scope| {
        for index in 0..helpers {
            let position = position.clone();
            let helper_limits = Limits {
                searchmoves: limits.searchmoves.clone(),
                searchmoves_only: limits.searchmoves_only,
                tablebases: limits.tablebases.clone(),
                nodes: limits.nodes,
                ..Limits::default()
            };
            let (helper_control, shared_nodes) = (&helper_control, &shared_nodes);
            std::thread::Builder::new()
                .name("inphish-helper".to_owned())
                .stack_size(16 * 1024 * 1024)
                .spawn_scoped(scope, move || {
                    let mut worker =
                        Search::new(position, helper_limits, helper_control, tt, shared_nodes);
                    worker.help(1 + (index % 2) as u8);
                    tt.keep_memory(worker.memory);
                })
                .expect("helper thread could not start");
        }
        let result = search_main(
            position,
            limits,
            control,
            tt,
            &shared_nodes,
            root_tb_score,
            on_info,
        );
        helper_control.stop.store(true, Ordering::Relaxed);
        result
    })
}

fn search_main(
    position: Position,
    limits: Limits,
    control: &Control,
    tt: &TranspositionTable,
    shared_nodes: &AtomicU64,
    root_tb_score: Option<i32>,
    on_info: impl FnMut(Info),
) -> Result {
    let seed = position.key()
        ^ std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos() as u64);
    let mut worker = Search::new(position, limits, control, tt, shared_nodes);
    let result = iterate(&mut worker, seed, root_tb_score, on_info);
    tt.keep_memory(worker.memory);
    result
}

/// Iterative deepening on the main thread, which reports, manages time and chooses the
/// move.
fn iterate(
    worker: &mut Search,
    seed: u64,
    root_tb_score: Option<i32>,
    mut on_info: impl FnMut(Info),
) -> Result {
    let candidates = worker.root_moves();
    let fallback = candidates.first().copied();
    let mut completed = Info {
        depth: 0,
        seldepth: 0,
        score: 0,
        nodes: 0,
        elapsed: Duration::ZERO,
        hashfull: 0,
        pv: fallback.into_iter().collect(),
        multipv: 1,
        tbhits: 0,
    };
    if candidates.is_empty() {
        completed.score = if worker.position.checkers().0 != 0 {
            -MATE
        } else {
            0
        };
        on_info(completed.clone());
        return Result {
            best: None,
            info: completed,
        };
    }
    let mut best = fallback;
    let mut stable_best = 0_u8;
    let reported = worker.limits.multipv.clamp(1, candidates.len());
    let line_count = if worker.limits.strength.is_some() {
        reported.max(STRENGTH_LINES).min(candidates.len())
    } else {
        reported
    };
    let mut lines: Vec<Info> = Vec::new();
    let max_depth = worker
        .limits
        .depth
        .unwrap_or((MAX_PLY - 1) as u8)
        .min((MAX_PLY - 1) as u8);
    let mut ordered = candidates.clone();
    for depth in 1..=max_depth {
        if worker.should_stop() {
            break;
        }
        let iteration_start_nodes = worker.nodes;
        let previous = (completed.depth > 0).then_some(completed.score);
        let (best_score, iteration_best, best_move_nodes) =
            worker.aspiration(depth, &mut ordered, previous);
        if worker.aborted || best_score == -INF {
            break;
        }
        let score_drop = completed.depth > 0 && completed.score - best_score > 80;
        stable_best = if completed.depth > 0 && best == iteration_best {
            stable_best.saturating_add(1)
        } else {
            0
        };
        best = iteration_best.or(best);
        worker.completed_depth = i32::from(depth);
        completed = Info {
            depth,
            seldepth: worker.seldepth,
            score: best_score,
            nodes: worker.total_nodes(),
            elapsed: worker.started.elapsed(),
            hashfull: worker.tt.hashfull(),
            pv: worker.pv[0][..worker.pv_len[0]].to_vec(),
            multipv: 1,
            tbhits: worker.tbhits,
        };
        on_info(with_tb_score(&completed, root_tb_score));
        if line_count > 1 {
            worker.search_lines(
                depth,
                &ordered,
                &completed,
                &mut lines,
                line_count,
                &mut on_info,
            );
        }
        if !worker.limits.infinite
            && (!worker.limits.ponder || worker.control.ponderhit.load(Ordering::Relaxed))
            && (candidates.len() == 1 || best_score.abs() >= MATE - depth as i32)
        {
            break;
        }
        let best_fraction =
            best_move_nodes.saturating_mul(100) / (worker.nodes - iteration_start_nodes).max(1);
        let scale =
            (100_i32 + if score_drop { 40 } else { 0 } + if best_fraction > 70 { 20 } else { 0 }
                - if stable_best >= 4 {
                    25
                } else if stable_best >= 2 {
                    15
                } else {
                    0
                })
            .clamp(75, 160) as u32;
        if worker.soft_expired(scale) {
            break;
        }
    }
    while !worker.aborted
        && (worker.limits.infinite
            || (worker.limits.ponder && !worker.control.ponderhit.load(Ordering::Relaxed)))
    {
        if worker.should_stop() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    completed.nodes = worker.total_nodes();
    completed.seldepth = worker.seldepth;
    completed.elapsed = worker.started.elapsed();
    completed.hashfull = worker.tt.hashfull();
    completed.tbhits = worker.tbhits;
    completed = with_tb_score(&completed, root_tb_score);
    on_info(completed.clone());
    for line in lines.iter_mut().skip(1) {
        line.nodes = completed.nodes;
        line.seldepth = completed.seldepth;
        line.elapsed = completed.elapsed;
        line.hashfull = completed.hashfull;
        if line.multipv <= reported {
            on_info(line.clone());
        }
    }
    if let (Some(elo), true) = (worker.limits.strength, lines.len() > 1) {
        let chosen = &lines[weakened_choice(&lines, elo, seed)];
        return Result {
            best: chosen.pv.first().copied().or(best),
            info: chosen.clone(),
        };
    }
    Result {
        best,
        info: completed,
    }
}

/// The line to report: with the root in the tablebases its score is the tablebase result
/// unless the search found a mate, while the search itself keeps using its own scores.
fn with_tb_score(info: &Info, tb_score: Option<i32>) -> Info {
    let mut shown = info.clone();
    if let Some(score) = tb_score.filter(|_| info.score.abs() < MATE_BOUND) {
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
    let repeated = position.is_repetition();
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
        TB_WIN
    } else if best > 0 {
        (best - 800).max(3) / 2
    } else if best == 0 {
        WDL_DRAW
    } else if best > -900 {
        (best + 800).min(-3) / 2
    } else {
        -TB_WIN
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
    (1320, 7.4),
    (1600, 8.4),
    (2200, 10.3),
    (2600, 11.0),
    (2800, 12.8),
    (3000, 14.6),
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

/// Converts a margin in Stockfish's internal units, 208 to a pawn, to centipawns, the
/// unit of this search.
const fn v(internal: i32) -> i32 {
    internal * 100 / 208
}

fn futility_margin(depth: i32, improving: bool) -> i32 {
    v(165 * (depth - i32::from(improving)))
}

fn futility_move_count(improving: bool, depth: i32) -> i32 {
    if improving {
        3 + depth * depth
    } else {
        (3 + depth * depth) / 2
    }
}

fn stat_bonus(depth: i32) -> i32 {
    ((12 * depth + 282) * depth - 349).min(1594)
}

/// Stockfish's endgame piece values, for futility margins.
const ENDGAME: [i32; 6] = [208, 854, 915, 1380, 2682, 0];

/// A mate or tablebase score is stored relative to the node rather than the root.
fn value_to_tt(value: i32, ply: usize) -> i32 {
    if value >= TB_WIN_IN_MAX_PLY {
        value + ply as i32
    } else if value <= -TB_WIN_IN_MAX_PLY {
        value - ply as i32
    } else {
        value
    }
}

/// The inverse of `value_to_tt`. A stored mate that the fifty-move counter may no longer
/// allow comes back as the best tablebase score instead, as in Stockfish.
fn value_from_tt(value: i32, ply: usize, rule50: i32) -> i32 {
    if value == VALUE_NONE {
        VALUE_NONE
    } else if value >= TB_WIN_IN_MAX_PLY {
        if value >= MATE_BOUND && MATE - value > 99 - rule50 {
            MATE_BOUND - 1
        } else {
            value - ply as i32
        }
    } else if value <= -TB_WIN_IN_MAX_PLY {
        if value <= -MATE_BOUND && MATE + value > 99 - rule50 {
            -MATE_BOUND + 1
        } else {
            value + ply as i32
        }
    } else {
        value
    }
}

/// Key of a node searched without one move, kept apart from the node's own results.
fn excluded_key(key: u64, excluded: Move) -> u64 {
    key ^ u64::from(excluded.raw()).wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

/// Per-ply search state, read up to six plies back.
#[derive(Clone, Copy)]
struct Frame {
    /// Move played from this ply, null when none or a null move.
    mv: Move,
    null: bool,
    captured: bool,
    /// Continuation table of replies to `mv`.
    continuation: usize,
    static_eval: i32,
    killers: [Move; 2],
    move_count: i32,
    in_check: bool,
    tt_pv: bool,
    tt_hit: bool,
    excluded: Move,
    stat_score: i32,
    double_extensions: i32,
    cutoff_count: i32,
}

impl Default for Frame {
    fn default() -> Self {
        Self {
            mv: Move::NULL,
            null: false,
            captured: false,
            continuation: SENTINEL,
            static_eval: VALUE_NONE,
            killers: [Move::NULL; 2],
            move_count: 0,
            in_check: false,
            tt_pv: false,
            tt_hit: false,
            excluded: Move::NULL,
            stat_score: 0,
            double_extensions: 0,
            cutoff_count: 0,
        }
    }
}

/// Frames before the root, so that looking six plies back never leaves the stack.
const FRAME_OFFSET: usize = 8;

impl<'a> Search<'a> {
    fn new(
        position: Position,
        limits: Limits,
        control: &'a Control,
        tt: &'a TranspositionTable,
        shared_nodes: &'a AtomicU64,
    ) -> Self {
        let started = limits.started.unwrap_or_else(Instant::now);
        let mut accumulators = vec![Accumulator::default(); MAX_PLY + 1].into_boxed_slice();
        accumulators[0] = network().fresh(&position);
        let threads = limits.threads.max(1) as f64;
        let reductions = (0..MAX_MOVES)
            .map(|index| {
                if index == 0 {
                    0
                } else {
                    ((20.26 + threads.ln() / 2.0) * (index as f64).ln()) as i32
                }
            })
            .collect();
        Search {
            position,
            timed_started: if limits.ponder { None } else { Some(started) },
            limits,
            control,
            tt,
            started,
            nodes: 0,
            shared_nodes,
            tbhits: 0,
            seldepth: 0,
            aborted: false,
            memory: tt.take_memory(),
            stack: vec![Frame::default(); MAX_PLY + FRAME_OFFSET + 4].into_boxed_slice(),
            reductions,
            root_depth: 0,
            completed_depth: 0,
            root_delta: 1,
            nmp_min_ply: 0,
            nmp_side: 0,
            pv: [[Move::NULL; MAX_PLY]; MAX_PLY],
            pv_len: [0; MAX_PLY],
            accumulators,
            computed: [true; MAX_PLY + 1],
            updates: (0..=MAX_PLY).map(|_| Update::none()).collect(),
            refresh_cache: RefreshCache::new(),
        }
    }

    fn at(&self, ply: usize, back: usize) -> &Frame {
        &self.stack[ply + FRAME_OFFSET - back]
    }

    fn at_mut(&mut self, ply: usize, back: usize) -> &mut Frame {
        &mut self.stack[ply + FRAME_OFFSET - back]
    }

    /// Probes the WDL tables just after a capture or pawn move, when the position has few
    /// enough pieces and no castling rights. Returns the score and its bound, a win or
    /// loss under the fifty-move rule counting as a small draw offset, as in Stockfish.
    fn probe_tablebases(&mut self, ply: usize) -> Option<(i32, Bound)> {
        let tables = self.limits.tablebases.clone()?;
        if self.position.halfmove_clock() != 0 || !probeable(&self.position, tables.max_pieces()) {
            return None;
        }
        let (wdl, state) = tables.probe_wdl(&mut self.position);
        if state == ProbeState::Fail {
            return None;
        }
        self.tbhits += 1;
        Some(if wdl < -1 {
            (-TB_WIN + ply as i32, Bound::Upper)
        } else if wdl > 1 {
            (TB_WIN - ply as i32, Bound::Lower)
        } else {
            (2 * wdl, Bound::Exact)
        })
    }

    /// Makes `mv` from `ply`, deriving the next ply's accumulator from this one, and
    /// records it in the ply's frame for the histories of the replies.
    fn make(&mut self, mv: Move, ply: usize) {
        let in_check = self.at(ply, 0).in_check;
        let capture = is_capture(mv);
        let square = moved_square(&self.position, mv);
        let frame = self.at_mut(ply, 0);
        frame.mv = mv;
        frame.null = false;
        frame.captured = capture;
        frame.continuation = continuation_table(in_check, capture, square);
        let delta = inphzugzwang_nnue::delta(&self.position, mv);
        self.position.make(mv);
        // The accumulator is only brought up to date when an evaluation needs it, which
        // many nodes never do; moves that need a refresh are applied at once.
        if network().prepare(&delta, &self.position, &mut self.updates[ply + 1]) {
            self.computed[ply + 1] = false;
        } else {
            self.ensure_accumulator(ply);
            let (parents, children) = self.accumulators.split_at_mut(ply + 1);
            network().apply(
                &parents[ply],
                &mut children[0],
                &delta,
                &self.position,
                &mut self.refresh_cache,
            );
            self.computed[ply + 1] = true;
        }
    }

    /// Brings the accumulator of `ply` up to date from the nearest current one before it.
    fn ensure_accumulator(&mut self, ply: usize) {
        let mut start = ply;
        while !self.computed[start] {
            start -= 1;
        }
        for next in start + 1..=ply {
            let (parents, children) = self.accumulators.split_at_mut(next);
            network().apply_update(&parents[next - 1], &mut children[0], &self.updates[next]);
            self.computed[next] = true;
        }
    }

    fn make_null(&mut self, ply: usize) {
        let frame = self.at_mut(ply, 0);
        frame.mv = Move::NULL;
        frame.null = true;
        frame.captured = false;
        frame.continuation = SENTINEL;
        self.position.make_null();
        self.updates[ply + 1].clear();
        self.computed[ply + 1] = false;
    }

    /// Static evaluation in centipawns for the side to move: Stockfish 19's evaluation
    /// without its optimism term, converted at 208 internal units per pawn, and kept
    /// below the tablebase range.
    /// The static evaluation with the correction history applied.
    fn evaluate(&mut self, ply: usize) -> i32 {
        let raw = self.static_evaluation(ply);
        self.corrected(raw)
    }

    fn static_evaluation(&mut self, ply: usize) -> i32 {
        self.ensure_accumulator(ply);
        let position = &self.position;
        let value = network()
            .evaluate(&self.accumulators[ply], position)
            .evaluation(position);
        (value * 100 / 208).clamp(-TB_WIN_IN_MAX_PLY + 1, TB_WIN_IN_MAX_PLY - 1)
    }

    fn root_moves(&self) -> Vec<Move> {
        self.position
            .legal_moves()
            .iter()
            .filter(|mv| !self.limits.searchmoves_only || self.limits.searchmoves.contains(mv))
            .collect()
    }

    /// Helper iterative deepening, starting at `first_depth` so that helpers are spread
    /// over different iterations, until the main thread stops it.
    fn help(&mut self, first_depth: u8) {
        let mut ordered = self.root_moves();
        let mut previous = None;
        for depth in first_depth..MAX_PLY as u8 {
            if ordered.is_empty() || self.should_stop() {
                return;
            }
            let (score, _, _) = self.aspiration(depth, &mut ordered, previous);
            if self.aborted {
                return;
            }
            previous = Some(score);
        }
    }

    /// Nodes of all threads: every thread adds its count to the shared counter in batches
    /// of 1,024, so this is exact for one thread and at most a batch per thread short.
    fn total_nodes(&self) -> u64 {
        self.shared_nodes.load(Ordering::Relaxed) + (self.nodes & 1023)
    }

    /// Searches the lines after the first at `depth`, each over the root moves not already
    /// heading an earlier line. A line whose search is cut short keeps its previous depth.
    fn search_lines(
        &mut self,
        depth: u8,
        ordered: &[Move],
        first: &Info,
        lines: &mut Vec<Info>,
        count: usize,
        on_info: &mut impl FnMut(Info),
    ) {
        if lines.is_empty() {
            lines.push(first.clone());
        } else {
            lines[0] = first.clone();
        }
        let mut excluded: Vec<Move> = first.pv.first().copied().into_iter().collect();
        for index in 1..count {
            let previous = lines.get(index).and_then(|line| line.pv.first().copied());
            let mut remaining: Vec<Move> = ordered
                .iter()
                .copied()
                .filter(|mv| !excluded.contains(mv))
                .collect();
            if let Some(position) = remaining.iter().position(|&mv| Some(mv) == previous) {
                remaining[..=position].rotate_right(1);
            }
            let (score, mv, _) = self.search_root(i32::from(depth), &mut remaining, -INF, INF);
            let Some(mv) = mv.filter(|_| !self.aborted && score > -INF) else {
                return;
            };
            excluded.push(mv);
            let info = Info {
                depth,
                seldepth: self.seldepth,
                score,
                nodes: self.total_nodes(),
                elapsed: self.started.elapsed(),
                hashfull: self.tt.hashfull(),
                pv: self.pv[0][..self.pv_len[0]].to_vec(),
                multipv: index + 1,
                tbhits: self.tbhits,
            };
            if index < self.limits.multipv.max(1) {
                on_info(info.clone());
            }
            if index < lines.len() {
                lines[index] = info;
            } else {
                lines.push(info);
            }
        }
    }

    /// One iteration at `depth` in a window around the previous score, widened after
    /// each fail as in Stockfish; a fail high retries a ply shallower first.
    fn aspiration(
        &mut self,
        depth: u8,
        ordered: &mut [Move],
        previous: Option<i32>,
    ) -> (i32, Option<Move>, u64) {
        let depth = i32::from(depth);
        let (mut alpha, mut beta, mut delta) = (-INF, INF, 0);
        if let Some(previous) = previous.filter(|&score| depth >= 4 && score.abs() < KNOWN_WIN) {
            let internal = previous * 208 / 100;
            delta = v(10 + internal * internal / 15_620).max(1);
            alpha = (previous - delta).max(-INF);
            beta = (previous + delta).min(INF);
        }
        let mut failed_high = 0;
        loop {
            let adjusted = (depth - failed_high).max(1);
            let (score, mv, nodes) = self.search_root(adjusted, ordered, alpha, beta);
            if self.aborted {
                return (score, mv, nodes);
            }
            if score <= alpha && alpha > -INF {
                beta = (alpha + beta) / 2;
                alpha = (score - delta).max(-INF);
                failed_high = 0;
            } else if score >= beta && beta < INF {
                beta = (score + delta).min(INF);
                failed_high += 1;
            } else {
                return (score, mv, nodes);
            }
            delta += delta / 4 + 1;
        }
    }

    /// Searches the root moves in order. Returns the highest score, the best move and the
    /// nodes spent on it, and reorders `moves` for the next iteration: moves that raised
    /// the window by their scores, then the rest in their previous order.
    fn search_root(
        &mut self,
        depth: i32,
        moves: &mut [Move],
        mut alpha: i32,
        beta: i32,
    ) -> (i32, Option<Move>, u64) {
        self.pv_len[0] = 0;
        self.root_depth = depth;
        self.root_delta = (beta - alpha).max(1);
        let in_check = self.position.checkers().0 != 0;
        if !in_check && self.at(0, 0).static_eval == VALUE_NONE {
            self.at_mut(0, 0).static_eval = self.evaluate(0);
        }
        {
            let frame = self.at_mut(0, 0);
            frame.in_check = in_check;
            frame.tt_pv = true;
            frame.excluded = Move::NULL;
            frame.move_count = 0;
        }
        self.stack[FRAME_OFFSET + 1].tt_pv = false;
        self.stack[FRAME_OFFSET + 1].excluded = Move::NULL;
        self.stack[FRAME_OFFSET + 2].killers = [Move::NULL; 2];
        self.stack[FRAME_OFFSET + 2].cutoff_count = 0;
        let side = self.position.side_to_move().index();
        let mut scores = vec![-INF; moves.len()];
        let mut highest = -INF;
        let mut best = None;
        let mut best_nodes = 0;
        for index in 0..moves.len() {
            if self.should_stop() {
                break;
            }
            let mv = moves[index];
            let move_count = index as i32 + 1;
            self.at_mut(0, 0).move_count = move_count;
            let before = self.nodes;
            let capture = is_capture(mv);
            let square = moved_square(&self.position, mv);
            let stat_score = 2 * self.memory.histories.main(side, mv)
                + self.continuation_score(0, square)
                - 4433;
            self.make(mv, 0);
            let new_depth = depth - 1;
            let mut score;
            if index == 0 {
                score = -self.negamax::<true>(new_depth, -beta, -alpha, 1, false);
            } else {
                let mut reduced = new_depth;
                if depth >= 2 && move_count > 2 && !capture {
                    let mut r = self.reduction(true, depth, move_count, beta - alpha);
                    r -= 2 + 1 + 11 / (3 + depth);
                    self.at_mut(0, 0).stat_score = stat_score;
                    r -= stat_score / (13_628 + 4000 * i32::from(depth > 7 && depth < 19));
                    reduced = (new_depth - r).clamp(1, new_depth + 1);
                }
                score = -self.negamax::<false>(reduced, -alpha - 1, -alpha, 1, true);
                if score > alpha && reduced < new_depth && !self.aborted {
                    score = -self.negamax::<false>(new_depth, -alpha - 1, -alpha, 1, true);
                }
                if score > alpha && !self.aborted {
                    score = -self.negamax::<true>(new_depth, -beta, -alpha, 1, false);
                }
            }
            self.position.unmake();
            if self.aborted {
                break;
            }
            highest = highest.max(score);
            if index == 0 || score > alpha {
                scores[index] = score;
                best = Some(mv);
                best_nodes = self.nodes - before;
                self.pv[0][0] = mv;
                let child_len = self.pv_len[1].min(MAX_PLY - 1);
                for step in 0..child_len {
                    self.pv[0][step + 1] = self.pv[1][step];
                }
                self.pv_len[0] = child_len + 1;
            }
            if score > alpha {
                if score >= beta {
                    break;
                }
                alpha = score;
            }
        }
        if !self.aborted {
            let mut order: Vec<usize> = (0..moves.len()).collect();
            order.sort_by_key(|&index| std::cmp::Reverse(scores[index]));
            let reordered: Vec<Move> = order.into_iter().map(|index| moves[index]).collect();
            moves.copy_from_slice(&reordered);
        }
        (highest, best, best_nodes)
    }

    fn refresh_ponder(&mut self) {
        if self.timed_started.is_none() && self.control.ponderhit.load(Ordering::Relaxed) {
            self.timed_started = Some(Instant::now());
        }
    }

    fn should_stop(&mut self) -> bool {
        if self.aborted || self.control.stop.load(Ordering::Relaxed) {
            self.aborted = true;
            return true;
        }
        self.refresh_ponder();
        if self
            .limits
            .nodes
            .is_some_and(|limit| self.total_nodes() >= limit)
        {
            self.aborted = true;
            return true;
        }
        if let (Some(start), Some(hard)) = (self.timed_started, self.limits.hard) {
            if start.elapsed() >= hard {
                self.aborted = true;
                return true;
            }
        }
        false
    }

    fn soft_expired(&mut self, scale: u32) -> bool {
        self.refresh_ponder();
        if self.limits.infinite || self.timed_started.is_none() {
            return false;
        }
        self.limits.soft.is_some_and(|soft| {
            self.timed_started
                .expect("timed search")
                .elapsed()
                .as_millis()
                >= soft.as_millis().saturating_mul(scale as u128) / 100
        })
    }

    fn visit(&mut self, ply: usize) -> bool {
        self.nodes += 1;
        self.seldepth = self.seldepth.max(ply);
        if self.nodes & 31 == 0 || self.limits.nodes.is_some() {
            if self.nodes & 1023 == 0 {
                self.shared_nodes.fetch_add(1024, Ordering::Relaxed);
            }
            self.should_stop()
        } else {
            self.aborted
        }
    }

    /// Draw by repetition, insufficient material or the fifty-move rule, unless the move
    /// that reached the fiftieth move gave mate.
    fn is_draw(&self) -> bool {
        self.position.is_repetition()
            || self.position.is_insufficient_material()
            || (self.position.halfmove_clock() >= 100
                && (self.position.checkers().0 == 0 || !self.position.legal_moves().is_empty()))
    }

    /// A draw scored one centipawn either side of zero, varying with the node count, so
    /// that the search does not settle into a repetition it could avoid.
    fn value_draw(&self) -> i32 {
        -1 + (self.nodes & 2) as i32
    }

    fn reduction(&self, improving: bool, depth: i32, move_count: i32, delta: i32) -> i32 {
        let product = self.reductions[depth.clamp(0, MAX_MOVES as i32 - 1) as usize]
            * self.reductions[move_count.clamp(0, MAX_MOVES as i32 - 1) as usize];
        (product + 1642 - delta * 1024 / self.root_delta) / 1024
            + i32::from(!improving && product > 916)
    }

    /// Continuation histories of a move to `square` after the moves one, two and four
    /// plies earlier.
    fn continuation_score(&self, ply: usize, square: usize) -> i32 {
        [1, 2, 4]
            .into_iter()
            .map(|back| {
                self.memory
                    .histories
                    .continuation(self.at(ply, back).continuation, square)
            })
            .sum()
    }

    fn continuations(&self, ply: usize) -> [usize; 4] {
        [1, 2, 4, 6].map(|back| self.at(ply, back).continuation)
    }

    /// The piece and square of the move that led to `ply`, if it was a move and a piece
    /// stands on its destination.
    fn previous_square(&self, ply: usize) -> Option<usize> {
        let previous = self.at(ply, 1).mv;
        if previous == Move::NULL {
            return None;
        }
        let piece = self.position.piece_at(previous.to())?;
        Some(piece_square(piece, previous.to().index()))
    }

    fn update_continuations(&mut self, ply: usize, square: usize, bonus: i32) {
        let in_check = self.at(ply, 0).in_check;
        for back in [1, 2, 4, 6] {
            if in_check && back > 2 {
                break;
            }
            let frame = self.at(ply, back);
            if frame.mv != Move::NULL {
                let table = frame.continuation;
                self.memory
                    .histories
                    .update_continuation(table, square, bonus);
            }
        }
    }

    fn update_quiet_stats(&mut self, ply: usize, mv: Move, bonus: i32) {
        let frame = self.at_mut(ply, 0);
        if frame.killers[0] != mv {
            frame.killers[1] = frame.killers[0];
            frame.killers[0] = mv;
        }
        let side = self.position.side_to_move().index();
        self.memory.histories.update_main(side, mv, bonus);
        let square = moved_square(&self.position, mv);
        self.update_continuations(ply, square, bonus);
        if let Some(previous) = self.previous_square(ply) {
            self.memory.histories.set_counter(previous, mv);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn update_all_stats(
        &mut self,
        ply: usize,
        best_move: Move,
        best: i32,
        beta: i32,
        quiets: &[Move],
        captures: &[Move],
        depth: i32,
    ) {
        let side = self.position.side_to_move().index();
        let bonus = stat_bonus(depth + 1);
        if is_capture(best_move) {
            let square = moved_square(&self.position, best_move);
            let victim = victim_slot(victim(&self.position, best_move));
            self.memory.histories.update_capture(square, victim, bonus);
        } else {
            let quiet_bonus = if best > beta + v(137) {
                bonus
            } else {
                stat_bonus(depth)
            };
            self.update_quiet_stats(ply, best_move, quiet_bonus);
            for &mv in quiets {
                self.memory.histories.update_main(side, mv, -quiet_bonus);
                let square = moved_square(&self.position, mv);
                self.update_continuations(ply, square, -quiet_bonus);
            }
        }
        // An early quiet move of the previous ply that was refuted loses some credit.
        let previous = *self.at(ply, 1);
        if (previous.move_count == 1 + i32::from(previous.tt_hit)
            || previous.mv == previous.killers[0])
            && !previous.captured
        {
            if let Some(square) = self.previous_square(ply) {
                self.update_continuations(ply - 1, square, -bonus);
            }
        }
        for &mv in captures {
            let square = moved_square(&self.position, mv);
            let victim = victim_slot(victim(&self.position, mv));
            self.memory.histories.update_capture(square, victim, -bonus);
        }
    }

    /// Principal variation search after Stockfish 15.1 (GPL-3.0,
    /// https://github.com/official-stockfish/Stockfish), whose pruning, extension and
    /// reduction rules and margins it follows, the margins converted to centipawns.
    #[allow(clippy::too_many_lines)]
    fn negamax<const PV: bool>(
        &mut self,
        mut depth: i32,
        mut alpha: i32,
        mut beta: i32,
        ply: usize,
        cut_node: bool,
    ) -> i32 {
        if depth <= 0 {
            return self.quiescence::<PV>(alpha, beta, ply, 0);
        }
        self.pv_len[ply] = 0;
        if self.visit(ply) {
            return 0;
        }
        let in_check = self.position.checkers().0 != 0;
        self.at_mut(ply, 0).in_check = in_check;
        let prior_capture = self.at(ply, 1).captured;
        let side = self.position.side_to_move();
        let us = side.index();
        if self.is_draw() {
            return self.value_draw();
        }
        if ply >= MAX_PLY - 1 {
            return if in_check { 0 } else { self.evaluate(ply) };
        }
        // No line from here can mate faster than a mate at the next ply or be mated
        // sooner than now, so a window outside those bounds is already decided.
        alpha = alpha.max(-MATE + ply as i32);
        beta = beta.min(MATE - ply as i32 - 1);
        if alpha >= beta {
            return alpha;
        }
        let double_extensions = self.at(ply, 1).double_extensions;
        {
            let frame = self.at_mut(ply, 0);
            frame.double_extensions = double_extensions;
            frame.move_count = 0;
        }
        self.stack[ply + FRAME_OFFSET + 1].tt_pv = false;
        self.stack[ply + FRAME_OFFSET + 1].excluded = Move::NULL;
        self.stack[ply + FRAME_OFFSET + 2].killers = [Move::NULL; 2];
        self.stack[ply + FRAME_OFFSET + 2].cutoff_count = 0;
        self.stack[ply + FRAME_OFFSET + 2].stat_score = 0;
        let previous_square = self.previous_square(ply);
        let excluded = self.at(ply, 0).excluded;
        let key = if excluded == Move::NULL {
            self.position.key()
        } else {
            excluded_key(self.position.key(), excluded)
        };
        let rule50 = i32::from(self.position.halfmove_clock());
        // A stored move that is not legal here marks a key collision: the entry is ignored.
        let hit = self
            .tt
            .probe(key)
            .filter(|record| record.mv == Move::NULL || self.position.is_legal_move(record.mv));
        let tt_value = hit.map_or(VALUE_NONE, |record| {
            value_from_tt(record.score, ply, rule50)
        });
        let tt_move = hit.map_or(Move::NULL, |record| record.mv);
        let tt_capture = tt_move != Move::NULL && is_capture(tt_move);
        self.at_mut(ply, 0).tt_hit = hit.is_some();
        if excluded == Move::NULL {
            self.at_mut(ply, 0).tt_pv = PV || hit.is_some_and(|record| record.pv);
        }
        let tt_pv = self.at(ply, 0).tt_pv;
        if let Some(record) = hit.filter(|record| {
            !PV && tt_value != VALUE_NONE
                && record.depth > depth - i32::from(record.bound == Bound::Exact)
                && record.bound.covers(tt_value >= beta)
        }) {
            if record.mv != Move::NULL {
                if tt_value >= beta {
                    if !tt_capture {
                        self.update_quiet_stats(ply, tt_move, stat_bonus(depth));
                    }
                    if self.at(ply, 1).move_count <= 2 && !prior_capture {
                        if let Some(square) = previous_square {
                            self.update_continuations(ply - 1, square, -stat_bonus(depth + 1));
                        }
                    }
                } else if !tt_capture {
                    let penalty = -stat_bonus(depth);
                    self.memory.histories.update_main(us, tt_move, penalty);
                    let square = moved_square(&self.position, tt_move);
                    self.update_continuations(ply, square, penalty);
                }
            }
            if rule50 < 90 {
                return tt_value;
            }
        }
        let mut best = -INF;
        let mut max_value = INF;
        if excluded == Move::NULL {
            if let Some((score, bound)) = self.probe_tablebases(ply) {
                if bound == Bound::Exact
                    || (bound == Bound::Lower && score >= beta)
                    || (bound == Bound::Upper && score <= alpha)
                {
                    self.tt.store(
                        key,
                        Record {
                            mv: Move::NULL,
                            score: value_to_tt(score, ply),
                            eval: VALUE_NONE,
                            depth: (depth + 6).min(MAX_PLY as i32 - 1),
                            bound,
                            pv: tt_pv,
                        },
                    );
                    return score;
                }
                if PV {
                    if bound == Bound::Lower {
                        best = score;
                        alpha = alpha.max(score);
                    } else {
                        max_value = score;
                    }
                }
            }
        }
        let mut raw_eval = VALUE_NONE;
        let mut static_eval = VALUE_NONE;
        let mut improving = false;
        if in_check {
            self.at_mut(ply, 0).static_eval = VALUE_NONE;
        } else {
            let mut eval;
            if excluded != Move::NULL {
                static_eval = self.at(ply, 0).static_eval;
                eval = static_eval;
            } else {
                raw_eval = match hit {
                    Some(record) if record.eval != VALUE_NONE => record.eval,
                    _ => self.static_evaluation(ply),
                };
                static_eval = self.corrected(raw_eval);
                eval = static_eval;
                match hit {
                    // A stored search result bounds the value more tightly than the static
                    // evaluation where its bound points the right way.
                    Some(record) => {
                        if tt_value != VALUE_NONE && record.bound.covers(tt_value > eval) {
                            eval = tt_value;
                        }
                    }
                    None => self.tt.store(
                        key,
                        Record {
                            mv: Move::NULL,
                            score: VALUE_NONE,
                            eval: raw_eval,
                            depth: DEPTH_NONE,
                            bound: Bound::None,
                            pv: tt_pv,
                        },
                    ),
                }
                self.at_mut(ply, 0).static_eval = static_eval;
            }
            // The static evaluation's swing after the previous quiet move says how good
            // that move was.
            let previous = *self.at(ply, 1);
            if previous.mv != Move::NULL
                && !previous.in_check
                && !prior_capture
                && previous.static_eval != VALUE_NONE
            {
                let swing = (previous.static_eval + static_eval) * 208 / 100;
                let bonus = (-19 * swing).clamp(-1914, 1914);
                self.memory
                    .histories
                    .update_main(side.other().index(), previous.mv, bonus);
            }
            let improvement = if self.at(ply, 2).static_eval != VALUE_NONE {
                static_eval - self.at(ply, 2).static_eval
            } else if self.at(ply, 4).static_eval != VALUE_NONE {
                static_eval - self.at(ply, 4).static_eval
            } else {
                v(168)
            };
            improving = improvement > 0;
            // Razoring: far below alpha, only a tactic could help, which quiescence sees.
            if eval < alpha - v(369 + 254 * depth * depth) {
                let value = self.quiescence::<false>(alpha - 1, alpha, ply, 0);
                if value < alpha {
                    return value;
                }
            }
            // Reverse futility: far enough above beta at low depth to stay there.
            if !tt_pv
                && depth < 8
                && eval - futility_margin(depth, improving) - v(self.at(ply, 1).stat_score / 303)
                    >= beta
                && eval >= beta
                && eval < v(28_031)
            {
                return eval;
            }
            // Null move: passing and still failing high means some move surely does too.
            let previous = *self.at(ply, 1);
            if !PV
                && !previous.null
                && previous.stat_score < 17_139
                && eval >= beta
                && eval >= static_eval
                && static_eval >= beta - v(20 * depth) - improvement / 13 + v(233)
                && excluded == Move::NULL
                && self.position.has_non_pawn_material(side)
                && (ply >= self.nmp_min_ply || us != self.nmp_side)
            {
                let reduction = ((eval - beta) / v(168)).min(7) + depth / 3 + 4;
                self.make_null(ply);
                let mut null_value =
                    -self.negamax::<false>(depth - reduction, -beta, -beta + 1, ply + 1, !cut_node);
                self.position.unmake();
                self.at_mut(ply, 0).null = false;
                if self.aborted {
                    return 0;
                }
                if null_value >= beta {
                    if null_value >= TB_WIN_IN_MAX_PLY {
                        null_value = beta;
                    }
                    if self.nmp_min_ply != 0 || (beta.abs() < KNOWN_WIN && depth < 14) {
                        return null_value;
                    }
                    // At high depth, verify with null moves off for this side for a while,
                    // against zugzwang.
                    self.nmp_min_ply = ply + (3 * (depth - reduction) / 4).max(0) as usize;
                    self.nmp_side = us;
                    let value =
                        self.negamax::<false>(depth - reduction, beta - 1, beta, ply, false);
                    self.nmp_min_ply = 0;
                    if self.aborted {
                        return 0;
                    }
                    if value >= beta {
                        return null_value;
                    }
                }
            }
            // ProbCut: a capture that beats beta by a margin in quiescence and then in a
            // reduced search is taken to hold at full depth as well.
            let probcut_beta = beta + v(191) - v(54) * i32::from(improving);
            if !PV
                && depth > 4
                && beta.abs() < TB_WIN_IN_MAX_PLY
                && !hit.is_some_and(|record| {
                    record.depth >= depth - 3 && tt_value != VALUE_NONE && tt_value < probcut_beta
                })
            {
                let threshold = (probcut_beta - static_eval) * 208 / 100;
                let mut picker = Picker::probcut(&self.position, tt_move, threshold);
                while let Some(mv) = picker.next(&self.position, &self.memory.histories, false) {
                    if mv == excluded {
                        continue;
                    }
                    let square = moved_square(&self.position, mv);
                    self.make(mv, ply);
                    self.at_mut(ply, 0).continuation = continuation_table(false, true, square);
                    let mut value =
                        -self.quiescence::<false>(-probcut_beta, -probcut_beta + 1, ply + 1, 0);
                    if value >= probcut_beta && !self.aborted {
                        value = -self.negamax::<false>(
                            depth - 4,
                            -probcut_beta,
                            -probcut_beta + 1,
                            ply + 1,
                            !cut_node,
                        );
                    }
                    self.position.unmake();
                    if self.aborted {
                        return 0;
                    }
                    if value >= probcut_beta {
                        self.tt.store(
                            key,
                            Record {
                                mv,
                                score: value_to_tt(value, ply),
                                eval: raw_eval,
                                depth: depth - 3,
                                bound: Bound::Lower,
                                pv: tt_pv,
                            },
                        );
                        return value;
                    }
                }
            }
            // Internal iterative reduction: a node without a stored move was not searched
            // before, so it is searched shallower rather than with poor ordering.
            if PV && tt_move == Move::NULL {
                depth -= 3;
            }
            if depth <= 0 {
                return self.quiescence::<PV>(alpha, beta, ply, 0);
            }
            if cut_node && depth >= 9 && tt_move == Move::NULL {
                depth -= 2;
            }
        }
        // In check, a stored capture well above beta is trusted outright.
        let probcut_beta = beta + v(417);
        if in_check
            && !PV
            && depth >= 2
            && tt_capture
            && hit.is_some_and(|record| record.bound.covers(true) && record.depth >= depth - 3)
            && tt_value >= probcut_beta
            && tt_value.abs() <= KNOWN_WIN
            && beta.abs() <= KNOWN_WIN
        {
            return probcut_beta;
        }
        let continuations = self.continuations(ply);
        let counter =
            previous_square.map_or(Move::NULL, |square| self.memory.histories.counter(square));
        let killers = self.at(ply, 0).killers;
        let mut picker = Picker::main(
            &self.position,
            tt_move,
            depth,
            killers,
            counter,
            continuations,
        );
        let likely_fail_low = PV
            && tt_move != Move::NULL
            && hit.is_some_and(|record| record.bound.covers(false) && record.depth >= depth);
        let mut move_count = 0;
        let mut move_count_pruning = false;
        let mut singular_quiet_lmr = false;
        let mut best_move = Move::NULL;
        let mut quiets = [Move::NULL; 64];
        let mut quiet_count = 0;
        let mut captures = [Move::NULL; 32];
        let mut capture_count = 0;
        let non_pawn = self.position.has_non_pawn_material(side);
        while let Some(mv) = picker.next(&self.position, &self.memory.histories, move_count_pruning)
        {
            if mv == excluded {
                continue;
            }
            move_count += 1;
            self.at_mut(ply, 0).move_count = move_count;
            let capture = is_capture(mv);
            let square = moved_square(&self.position, mv);
            let gives_check = self.position.gives_check(mv);
            let mut new_depth = depth - 1;
            let delta = beta - alpha;
            // Pruning at shallow depth.
            if non_pawn && best > -TB_WIN_IN_MAX_PLY {
                move_count_pruning = move_count >= futility_move_count(improving, depth);
                let lmr_depth =
                    (new_depth - self.reduction(improving, depth, move_count, delta)).max(0);
                if capture || gives_check {
                    let taken = victim(&self.position, mv);
                    if !gives_check
                        && !PV
                        && lmr_depth < 7
                        && !in_check
                        && static_eval
                            + v(180
                                + 201 * lmr_depth
                                + taken.map_or(0, |kind| ENDGAME[kind.index()])
                                + self.memory.histories.capture(square, victim_slot(taken)) / 6)
                            < alpha
                    {
                        continue;
                    }
                    if !self.position.see_ge(mv, -222 * depth) {
                        continue;
                    }
                } else {
                    let mut history = self.continuation_score(ply, square);
                    if lmr_depth < 5 && history < -3875 * (depth - 1) {
                        continue;
                    }
                    history += 2 * self.memory.histories.main(us, mv);
                    if !in_check
                        && lmr_depth < 13
                        && static_eval + v(106 + 145 * lmr_depth + history / 52) <= alpha
                    {
                        continue;
                    }
                    if !self
                        .position
                        .see_ge(mv, -24 * lmr_depth * lmr_depth - 15 * lmr_depth)
                    {
                        continue;
                    }
                }
            }
            // Extensions.
            let mut extension = 0;
            if (ply as i32) < self.root_depth * 2 {
                let singular = hit.filter(|record| {
                    mv == tt_move
                        && excluded == Move::NULL
                        && depth
                            >= 4 - i32::from(self.completed_depth > 24)
                                + 2 * i32::from(PV && record.pv)
                        && tt_value.abs() < KNOWN_WIN
                        && record.bound.covers(true)
                        && record.depth >= depth - 3
                });
                if singular.is_some() {
                    // Singular extension: if every other move fails low against a margin
                    // below the stored score, the stored move alone holds and is extended.
                    let singular_beta = tt_value - v((3 + i32::from(tt_pv && !PV)) * depth);
                    let tt_hit = self.at(ply, 0).tt_hit;
                    self.at_mut(ply, 0).excluded = mv;
                    let value = self.negamax::<false>(
                        (depth - 1) / 2,
                        singular_beta - 1,
                        singular_beta,
                        ply,
                        cut_node,
                    );
                    let frame = self.at_mut(ply, 0);
                    frame.excluded = Move::NULL;
                    frame.move_count = move_count;
                    frame.tt_hit = tt_hit;
                    if self.aborted {
                        return 0;
                    }
                    if value < singular_beta {
                        extension = 1;
                        singular_quiet_lmr = !tt_capture;
                        if !PV
                            && value < singular_beta - v(25)
                            && self.at(ply, 0).double_extensions <= 9
                        {
                            extension = 2;
                        }
                    } else if singular_beta >= beta {
                        // Multi-cut: another move also beats beta, so the node fails high.
                        return singular_beta;
                    } else if tt_value >= beta {
                        extension = -2;
                    } else if tt_value <= alpha && tt_value <= value {
                        extension = -1;
                    }
                } else if (gives_check && depth > 9 && (in_check || static_eval.abs() > v(82)))
                    || (PV
                        && mv == tt_move
                        && mv == killers[0]
                        && self.memory.histories.continuation(continuations[0], square) >= 5177)
                {
                    extension = 1;
                }
            }
            new_depth += extension;
            self.at_mut(ply, 0).double_extensions =
                self.at(ply, 1).double_extensions + i32::from(extension == 2);
            let stat_score = 2 * self.memory.histories.main(us, mv)
                + self.continuation_score(ply, square)
                - 4433;
            let threatened = picker.threatened.contains(mv.from());
            self.make(mv, ply);
            let mut value = -INF;
            // Late move reductions.
            if depth >= 2
                && move_count > 1 + i32::from(PV && ply <= 1)
                && (!tt_pv || !capture || (cut_node && self.at(ply, 1).move_count > 1))
            {
                let mut r = self.reduction(improving, depth, move_count, delta);
                if tt_pv && !likely_fail_low {
                    r -= 2;
                }
                if self.at(ply, 1).move_count > 7 {
                    r -= 1;
                }
                if cut_node {
                    r += 2;
                }
                if tt_capture {
                    r += 1;
                }
                if PV {
                    r -= 1 + 11 / (3 + depth);
                }
                if singular_quiet_lmr {
                    r -= 1;
                }
                if depth > 9 && threatened {
                    r -= 1;
                }
                if self.at(ply + 1, 0).cutoff_count > 3 {
                    r += 1;
                }
                self.at_mut(ply, 0).stat_score = stat_score;
                r -= stat_score / (13_628 + 4000 * i32::from(depth > 7 && depth < 19));
                let reduced = (new_depth - r).clamp(1, new_depth + 1);
                value = -self.negamax::<false>(reduced, -(alpha + 1), -alpha, ply + 1, true);
                if value > alpha && reduced < new_depth && !self.aborted {
                    let deeper = value > alpha + v(64 + 11 * (new_depth - reduced));
                    let shallower = value < best + v(new_depth);
                    new_depth += i32::from(deeper) - i32::from(shallower);
                    if new_depth > reduced {
                        value = -self.negamax::<false>(
                            new_depth,
                            -(alpha + 1),
                            -alpha,
                            ply + 1,
                            !cut_node,
                        );
                    }
                    let mut bonus = if value > alpha {
                        stat_bonus(new_depth)
                    } else {
                        -stat_bonus(new_depth)
                    };
                    if capture {
                        bonus /= 6;
                    }
                    self.update_continuations(ply, square, bonus);
                }
            } else if !PV || move_count > 1 {
                value = -self.negamax::<false>(new_depth, -(alpha + 1), -alpha, ply + 1, !cut_node);
            }
            if PV && (move_count == 1 || (value > alpha && value < beta)) && !self.aborted {
                value =
                    -self.negamax::<true>(new_depth.min(depth + 1), -beta, -alpha, ply + 1, false);
            }
            self.position.unmake();
            if self.aborted {
                return 0;
            }
            if value > best {
                best = value;
                if value > alpha {
                    best_move = mv;
                    if PV {
                        self.pv[ply][0] = mv;
                        let child_len = self.pv_len[ply + 1].min(MAX_PLY - ply - 1);
                        for index in 0..child_len {
                            self.pv[ply][index + 1] = self.pv[ply + 1][index];
                        }
                        self.pv_len[ply] = child_len + 1;
                    }
                    if PV && value < beta {
                        alpha = value;
                        if depth > 1 && depth < 6 && beta < KNOWN_WIN && alpha > -KNOWN_WIN {
                            depth -= 1;
                        }
                    } else {
                        self.at_mut(ply, 0).cutoff_count += 1;
                        break;
                    }
                }
            }
            if mv != best_move {
                if capture && capture_count < captures.len() {
                    captures[capture_count] = mv;
                    capture_count += 1;
                } else if !capture && quiet_count < quiets.len() {
                    quiets[quiet_count] = mv;
                    quiet_count += 1;
                }
            }
        }
        if move_count == 0 {
            best = if excluded != Move::NULL {
                alpha
            } else if in_check {
                -MATE + ply as i32
            } else {
                0
            };
        } else if best_move != Move::NULL {
            self.update_all_stats(
                ply,
                best_move,
                best,
                beta,
                &quiets[..quiet_count],
                &captures[..capture_count],
                depth,
            );
        } else if (depth >= 5 || PV) && !prior_capture {
            // The previous move refuted everything here, so it gets credit.
            if let Some(square) = previous_square {
                let extra = PV || cut_node || best < alpha - v(62 * depth);
                let bonus = stat_bonus(depth) * (1 + i32::from(extra));
                self.update_continuations(ply - 1, square, bonus);
            }
        }
        if PV {
            best = best.min(max_value);
        }
        if best <= alpha {
            let frame_pv = self.at(ply, 0).tt_pv || (self.at(ply, 1).tt_pv && depth > 3);
            self.at_mut(ply, 0).tt_pv = frame_pv;
        }
        if excluded == Move::NULL {
            let bound = if best >= beta {
                Bound::Lower
            } else if PV && best_move != Move::NULL {
                Bound::Exact
            } else {
                Bound::Upper
            };
            if !in_check
                && (best_move == Move::NULL || !is_capture(best_move))
                && best.abs() < TB_WIN_IN_MAX_PLY
                && !(bound == Bound::Lower && best <= static_eval)
                && !(bound == Bound::Upper && best >= static_eval)
            {
                self.update_correction(best - static_eval, depth);
            }
            self.tt.store(
                key,
                Record {
                    mv: best_move,
                    score: value_to_tt(best, ply),
                    eval: raw_eval,
                    depth,
                    bound,
                    pv: self.at(ply, 0).tt_pv,
                },
            );
        }
        best
    }

    /// Pawn-structure correction history: the static evaluation tends to misjudge the
    /// same pawn structure in the same direction, so the average error that searches
    /// found for it is added back to later evaluations of positions sharing it.
    fn corrected(&self, raw: i32) -> i32 {
        let (side, slot) = self.correction_slot();
        let correction = self.memory.correction[side][slot] / CORRECTION_GRAIN;
        (raw + correction).clamp(-TB_WIN_IN_MAX_PLY + 1, TB_WIN_IN_MAX_PLY - 1)
    }

    fn update_correction(&mut self, error: i32, depth: i32) {
        let weight = (depth + 1).min(16);
        let (side, slot) = self.correction_slot();
        let entry = &mut self.memory.correction[side][slot];
        *entry = ((*entry * (256 - weight) + error * CORRECTION_GRAIN * weight) / 256)
            .clamp(-CORRECTION_MAX, CORRECTION_MAX);
    }

    fn correction_slot(&self) -> (usize, usize) {
        let side = self.position.side_to_move().index();
        (side, self.position.pawn_key() as usize % CORRECTION_ENTRIES)
    }

    /// Quiescence search after Stockfish 15.1: captures, and quiet checks at its first
    /// ply, until the position is quiet; stand pat when not in check.
    #[allow(clippy::too_many_lines)]
    fn quiescence<const PV: bool>(
        &mut self,
        mut alpha: i32,
        beta: i32,
        ply: usize,
        depth: i32,
    ) -> i32 {
        self.pv_len[ply] = 0;
        if self.visit(ply) {
            return 0;
        }
        let in_check = self.position.checkers().0 != 0;
        self.at_mut(ply, 0).in_check = in_check;
        if self.is_draw() {
            return 0;
        }
        if ply >= MAX_PLY - 1 {
            return if in_check { 0 } else { self.evaluate(ply) };
        }
        let tt_depth = if in_check || depth >= 0 { 0 } else { -1 };
        let key = self.position.key();
        let rule50 = i32::from(self.position.halfmove_clock());
        let hit = self
            .tt
            .probe(key)
            .filter(|record| record.mv == Move::NULL || self.position.is_legal_move(record.mv));
        let tt_value = hit.map_or(VALUE_NONE, |record| {
            value_from_tt(record.score, ply, rule50)
        });
        let tt_move = hit.map_or(Move::NULL, |record| record.mv);
        let pv_hit = hit.is_some_and(|record| record.pv);
        if !PV
            && tt_value != VALUE_NONE
            && hit.is_some_and(|record| {
                record.depth >= tt_depth && record.bound.covers(tt_value >= beta)
            })
        {
            return tt_value;
        }
        let mut raw_eval = VALUE_NONE;
        let mut best;
        let futility_base;
        if in_check {
            self.at_mut(ply, 0).static_eval = VALUE_NONE;
            best = -INF;
            futility_base = -INF;
        } else {
            raw_eval = match hit {
                Some(record) if record.eval != VALUE_NONE => record.eval,
                _ => self.static_evaluation(ply),
            };
            let static_eval = self.corrected(raw_eval);
            self.at_mut(ply, 0).static_eval = static_eval;
            best = static_eval;
            if hit.is_some_and(|record| {
                tt_value != VALUE_NONE && record.bound.covers(tt_value > best)
            }) {
                best = tt_value;
            }
            if best >= beta {
                if hit.is_none() {
                    self.tt.store(
                        key,
                        Record {
                            mv: Move::NULL,
                            score: value_to_tt(best, ply),
                            eval: raw_eval,
                            depth: DEPTH_NONE,
                            bound: Bound::Lower,
                            pv: false,
                        },
                    );
                }
                return best;
            }
            if PV && best > alpha {
                alpha = best;
            }
            futility_base = best + v(153);
        }
        let continuations = self.continuations(ply);
        let previous = self.at(ply, 1).mv;
        let recapture = (previous != Move::NULL).then(|| previous.to());
        let mut picker =
            Picker::quiescence(&self.position, tt_move, depth, recapture, continuations);
        let mut best_move = Move::NULL;
        let mut move_count = 0;
        let mut quiet_evasions = 0;
        while let Some(mv) = picker.next(&self.position, &self.memory.histories, false) {
            let gives_check = self.position.gives_check(mv);
            let capture = is_capture(mv);
            move_count += 1;
            if best > -TB_WIN_IN_MAX_PLY
                && !gives_check
                && Some(mv.to()) != recapture
                && futility_base > -KNOWN_WIN
                && mv.promotion().is_none()
            {
                if move_count > 2 {
                    continue;
                }
                let futility_value = futility_base
                    + v(victim(&self.position, mv).map_or(0, |kind| ENDGAME[kind.index()]));
                if futility_value <= alpha {
                    best = best.max(futility_value);
                    continue;
                }
                if futility_base <= alpha && !self.position.see_ge(mv, 1) {
                    best = best.max(futility_base);
                    continue;
                }
            }
            if best > -TB_WIN_IN_MAX_PLY && !self.position.see_ge(mv, 0) {
                continue;
            }
            let square = moved_square(&self.position, mv);
            if !capture
                && best > -TB_WIN_IN_MAX_PLY
                && self.memory.histories.continuation(continuations[0], square) < 0
                && self.memory.histories.continuation(continuations[1], square) < 0
            {
                continue;
            }
            if best > -TB_WIN_IN_MAX_PLY && quiet_evasions > 1 {
                break;
            }
            quiet_evasions += i32::from(!capture && in_check);
            self.make(mv, ply);
            let value = -self.quiescence::<PV>(-beta, -alpha, ply + 1, depth - 1);
            self.position.unmake();
            if self.aborted {
                return 0;
            }
            if value > best {
                best = value;
                if value > alpha {
                    best_move = mv;
                    if PV {
                        self.pv[ply][0] = mv;
                        let child_len = self.pv_len[ply + 1].min(MAX_PLY - ply - 1);
                        for index in 0..child_len {
                            self.pv[ply][index + 1] = self.pv[ply + 1][index];
                        }
                        self.pv_len[ply] = child_len + 1;
                    }
                    if PV && value < beta {
                        alpha = value;
                    } else {
                        break;
                    }
                }
            }
        }
        if in_check && best == -INF {
            return -MATE + ply as i32;
        }
        self.tt.store(
            key,
            Record {
                mv: best_move,
                score: value_to_tt(best, ply),
                eval: raw_eval,
                depth: tt_depth,
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

/// Win, draw and loss chances in permille for the side to move. The logistic model
/// `win = 1 / (1 + exp((a - score) / b))`, with loss mirrored, was fitted by maximum
/// likelihood to inphish self-play evaluations at 1+0.01, so it is only an estimate.
pub fn wdl(score: i32) -> (u16, u16, u16) {
    const A: f64 = 714.0;
    const B: f64 = 290.0;
    if score >= MATE_BOUND {
        return (1000, 0, 0);
    }
    if score <= -MATE_BOUND {
        return (0, 0, 1000);
    }
    let score = f64::from(score);
    let win = (1000.0 / (1.0 + ((A - score) / B).exp())).round() as u16;
    let loss = (1000.0 / (1.0 + ((A + score) / B).exp())).round() as u16;
    (win, 1000 - win - loss, loss)
}

pub fn uci_score(score: i32) -> String {
    if score.abs() >= MATE - MAX_PLY as i32 {
        let plies = MATE - score.abs();
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
        for score in (-MATE..=MATE).step_by(7) {
            let (win, draw, loss) = wdl(score);
            assert_eq!(win + draw + loss, 1000, "{score}");
            assert!(win >= previous.0 && loss <= previous.2, "{score}");
            previous = (win, draw, loss);
        }
        assert_eq!(wdl(0).0, wdl(0).2);
        assert_eq!(wdl(MATE - 3), (1000, 0, 0));
    }

    #[test]
    fn stopped_search_keeps_legal_fallback() {
        let control = control();
        control.stop.store(true, Ordering::Relaxed);
        let position = Position::startpos();
        let result = search(position, Limits::default(), &control, |_| {});
        assert!(result.best.is_some());
    }
}
