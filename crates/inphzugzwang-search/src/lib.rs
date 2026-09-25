use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use inphzugzwang_core::{Move, MoveList, PieceType, Position};
use inphzugzwang_eval::{evaluate, VALUES};

mod tt;

pub use tt::TranspositionTable;
use tt::{Bound, Record};

const MAX_PLY: usize = 128;
const MATE: i32 = 30_000;
const INF: i32 = 32_000;
const MATE_BOUND: i32 = MATE - MAX_PLY as i32;
const HISTORY_MAX: i32 = 16_384;
const PIECE_SQUARES: usize = 12 * 64;
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
}

pub struct Result {
    pub best: Option<Move>,
    pub info: Info,
}

pub struct Control {
    pub stop: Arc<AtomicBool>,
    pub ponderhit: Arc<AtomicBool>,
}

struct Search<'a> {
    position: Position,
    limits: Limits,
    control: &'a Control,
    tt: &'a TranspositionTable,
    started: Instant,
    timed_started: Option<Instant>,
    nodes: u64,
    seldepth: usize,
    aborted: bool,
    killers: [[Move; 2]; MAX_PLY],
    history: Box<[[[i32; 64]; 64]; 2]>,
    continuation: Box<[i32]>,
    counters: Box<[Move]>,
    correction: Box<[[i32; CORRECTION_ENTRIES]; 2]>,
    played: [Option<usize>; MAX_PLY],
    reductions: [[u8; 64]; 64],
    pv: [[Move; MAX_PLY]; MAX_PLY],
    pv_len: [usize; MAX_PLY],
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

pub fn search_with_table(
    position: Position,
    limits: Limits,
    control: &Control,
    tt: &TranspositionTable,
    mut on_info: impl FnMut(Info),
) -> Result {
    tt.next_generation();
    let started = limits.started.unwrap_or_else(Instant::now);
    let mut worker = Search {
        position,
        timed_started: if limits.ponder { None } else { Some(started) },
        limits,
        control,
        tt,
        started,
        nodes: 0,
        seldepth: 0,
        aborted: false,
        killers: [[Move::NULL; 2]; MAX_PLY],
        history: Box::new([[[0; 64]; 64]; 2]),
        continuation: vec![0; PIECE_SQUARES * PIECE_SQUARES].into_boxed_slice(),
        counters: vec![Move::NULL; PIECE_SQUARES].into_boxed_slice(),
        correction: vec![[0; CORRECTION_ENTRIES]; 2]
            .into_boxed_slice()
            .try_into()
            .expect("two sides"),
        played: [None; MAX_PLY],
        reductions: reduction_table(),
        pv: [[Move::NULL; MAX_PLY]; MAX_PLY],
        pv_len: [0; MAX_PLY],
    };
    let root = worker.position.legal_moves();
    let candidates: Vec<_> = root
        .iter()
        .filter(|mv| !worker.limits.searchmoves_only || worker.limits.searchmoves.contains(mv))
        .collect();
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
    let line_count = worker.limits.multipv.clamp(1, candidates.len());
    let mut lines: Vec<Info> = Vec::new();
    let max_depth = worker
        .limits
        .depth
        .unwrap_or((MAX_PLY - 1) as u8)
        .min((MAX_PLY - 1) as u8);
    for depth in 1..=max_depth {
        if worker.should_stop() {
            break;
        }
        let iteration_start_nodes = worker.nodes;
        let mut ordered = candidates.clone();
        ordered.sort_by_key(|&mv| -worker.move_score(mv, best, 0));
        // Aspiration: search a narrow window around the last score and widen it on a fail,
        // since most iterations land close to the previous one and a narrow window prunes more.
        let mut delta = 25;
        let (mut low, mut high) =
            if depth >= 5 && completed.depth > 0 && completed.score.abs() < MATE_BOUND {
                (completed.score - delta, completed.score + delta)
            } else {
                (-INF, INF)
            };
        let (best_score, iteration_best, best_move_nodes) = loop {
            let (score, mv, nodes) = worker.search_root(depth, &ordered, low, high);
            if worker.aborted {
                break (score, mv, nodes);
            }
            if score <= low && low > -INF {
                high = (low + high) / 2;
                low = score - delta;
            } else if score >= high && high < INF {
                high = score + delta;
            } else {
                break (score, mv, nodes);
            }
            delta *= 2;
            if delta > 400 {
                low = -INF;
                high = INF;
            }
            low = low.max(-INF);
            high = high.min(INF);
        };
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
        completed = Info {
            depth,
            seldepth: worker.seldepth,
            score: best_score,
            nodes: worker.nodes,
            elapsed: worker.started.elapsed(),
            hashfull: worker.tt.hashfull(),
            pv: worker.pv[0][..worker.pv_len[0]].to_vec(),
            multipv: 1,
        };
        on_info(completed.clone());
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
    completed.nodes = worker.nodes;
    completed.seldepth = worker.seldepth;
    completed.elapsed = worker.started.elapsed();
    completed.hashfull = worker.tt.hashfull();
    on_info(completed.clone());
    for line in lines.iter_mut().skip(1) {
        line.nodes = completed.nodes;
        line.seldepth = completed.seldepth;
        line.elapsed = completed.elapsed;
        line.hashfull = completed.hashfull;
        on_info(line.clone());
    }
    Result {
        best,
        info: completed,
    }
}

impl Search<'_> {
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
            remaining.sort_by_key(|&mv| -self.move_score(mv, previous, 0));
            let (score, mv, _) = self.search_root(depth, &remaining, -INF, INF);
            let Some(mv) = mv.filter(|_| !self.aborted && score > -INF) else {
                return;
            };
            excluded.push(mv);
            let info = Info {
                depth,
                seldepth: self.seldepth,
                score,
                nodes: self.nodes,
                elapsed: self.started.elapsed(),
                hashfull: self.tt.hashfull(),
                pv: self.pv[0][..self.pv_len[0]].to_vec(),
                multipv: index + 1,
            };
            on_info(info.clone());
            if index < lines.len() {
                lines[index] = info;
            } else {
                lines.push(info);
            }
        }
    }

    fn search_root(
        &mut self,
        depth: u8,
        moves: &[Move],
        mut alpha: i32,
        beta: i32,
    ) -> (i32, Option<Move>, u64) {
        self.pv_len[0] = 0;
        let mut best_score = -INF;
        let mut iteration_best = None;
        let mut best_move_nodes = 0;
        for (index, &mv) in moves.iter().enumerate() {
            if self.should_stop() {
                break;
            }
            let before = self.nodes;
            self.played[0] = Some(self.piece_square(mv));
            self.position.make(mv);
            let child_depth = depth as i32 - 1;
            let mut score = if index == 0 {
                -self.negamax(child_depth, -beta, -alpha, 1, true)
            } else {
                -self.negamax(child_depth, -alpha - 1, -alpha, 1, true)
            };
            if index > 0 && score > alpha && score < beta && !self.aborted {
                score = -self.negamax(child_depth, -beta, -alpha, 1, true);
            }
            self.position.unmake();
            if self.aborted {
                break;
            }
            if score > best_score {
                best_score = score;
                iteration_best = Some(mv);
                best_move_nodes = self.nodes - before;
                self.pv[0][0] = mv;
                let child_len = self.pv_len[1].min(MAX_PLY - 1);
                for index in 0..child_len {
                    self.pv[0][index + 1] = self.pv[1][index];
                }
                self.pv_len[0] = child_len + 1;
            }
            alpha = alpha.max(score);
            if alpha >= beta {
                break;
            }
        }
        (best_score, iteration_best, best_move_nodes)
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
        if self.limits.nodes.is_some_and(|limit| self.nodes >= limit) {
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
            self.should_stop()
        } else {
            self.aborted
        }
    }

    fn negamax(
        &mut self,
        mut depth: i32,
        mut alpha: i32,
        mut beta: i32,
        ply: usize,
        allow_null: bool,
    ) -> i32 {
        self.pv_len[ply] = 0;
        if self.visit(ply) {
            return 0;
        }
        if self.position.is_repetition() || self.position.is_insufficient_material() {
            return 0;
        }
        if ply >= MAX_PLY - 1 {
            return evaluate(&self.position);
        }
        let in_check = self.position.checkers().0 != 0;
        if in_check {
            depth += 1;
        }
        if depth <= 0 {
            return self.quiescence(alpha, beta, ply);
        }
        let mut moves = self.position.legal_moves();
        if moves.is_empty() {
            return if in_check { -MATE + ply as i32 } else { 0 };
        }
        if self.position.is_fifty_move_draw() {
            return 0;
        }
        // No line from here can mate faster than a mate at the next ply or be mated
        // sooner than now, so a window outside those bounds is already decided.
        alpha = alpha.max(-MATE + ply as i32);
        beta = beta.min(MATE - ply as i32 - 1);
        if alpha >= beta {
            return alpha;
        }
        let key = self.position.key();
        let pv_node = beta - alpha > 1;
        let original_alpha = alpha;
        let hit = self.tt.probe(key, ply);
        let tt_move = hit.and_then(|record| {
            if record.mv != Move::NULL && self.position.is_pseudo_legal(record.mv) {
                Some(record.mv)
            } else {
                None
            }
        });
        if !pv_node {
            if let Some(record) = hit.filter(|record| {
                record.depth >= depth as u8 && (record.mv == Move::NULL || tt_move.is_some())
            }) {
                match record.bound {
                    Bound::Exact => return record.score,
                    Bound::Lower if record.score >= beta => return record.score,
                    Bound::Upper if record.score <= alpha => return record.score,
                    _ => {}
                }
            }
        }
        let raw_eval = if in_check {
            0
        } else {
            hit.map_or_else(|| evaluate(&self.position), |record| record.eval)
        };
        let static_eval = if in_check {
            0
        } else {
            self.corrected(raw_eval)
        };
        if !pv_node && !in_check && beta.abs() < MATE_BOUND {
            // Reverse futility: a quiet position this far above beta at low depth is not
            // expected to fall back below it within the remaining plies.
            if depth <= 6 && static_eval - 80 * depth >= beta {
                return static_eval;
            }
            if allow_null
                && depth >= 3
                && static_eval >= beta
                && self
                    .position
                    .has_non_pawn_material(self.position.side_to_move())
            {
                // Pawn-only positions are excluded because zugzwang is common there and
                // passing would be an illegal advantage the null move cannot represent.
                let reduction = 3 + depth / 4 + ((static_eval - beta) / 200).min(3);
                self.played[ply] = None;
                self.position.make_null();
                let score = -self.negamax(depth - 1 - reduction, -beta, -beta + 1, ply + 1, false);
                self.position.unmake();
                if self.aborted {
                    return 0;
                }
                if score >= beta {
                    return if score >= MATE_BOUND { beta } else { score };
                }
            }
        }
        self.order(&mut moves, tt_move, ply);
        let side = self.position.side_to_move().index();
        let mut best = -INF;
        let mut best_move = Move::NULL;
        let mut quiets_tried = [Move::NULL; 64];
        let mut quiet_count = 0;
        for (index, mv) in moves.iter().enumerate() {
            let quiet = is_quiet(mv);
            let gives_check = self.position.gives_check(mv);
            if !pv_node && !in_check && quiet && !gives_check && best > -MATE_BOUND {
                let late = 3 + (depth * depth) as usize;
                if depth <= 4 && index >= late {
                    continue;
                }
                if depth <= 3 && static_eval + 100 + 100 * depth <= alpha {
                    continue;
                }
            }
            let history = self.history[side][mv.from().index()][mv.to().index()];
            self.played[ply] = Some(self.piece_square(mv));
            self.position.make(mv);
            let new_depth = depth - 1;
            let mut score;
            if index == 0 {
                score = -self.negamax(new_depth, -beta, -alpha, ply + 1, true);
            } else {
                let mut reduction = 0;
                if depth >= 3 && index >= 3 && quiet && !in_check && !gives_check {
                    reduction = i32::from(self.reductions[depth.min(63) as usize][index.min(63)]);
                    if pv_node {
                        reduction -= 1;
                    }
                    if self.killers[ply].contains(&mv) {
                        reduction -= 1;
                    }
                    reduction -= history / 8192;
                    reduction = reduction.clamp(0, new_depth - 1);
                }
                score = -self.negamax(new_depth - reduction, -alpha - 1, -alpha, ply + 1, true);
                if score > alpha && reduction > 0 && !self.aborted {
                    score = -self.negamax(new_depth, -alpha - 1, -alpha, ply + 1, true);
                }
                if score > alpha && score < beta && !self.aborted {
                    score = -self.negamax(new_depth, -beta, -alpha, ply + 1, true);
                }
            }
            self.position.unmake();
            if self.aborted {
                return 0;
            }
            if score > best {
                best = score;
                best_move = mv;
            }
            if score > alpha {
                alpha = score;
                self.pv[ply][0] = mv;
                let child_len = self.pv_len[ply + 1].min(MAX_PLY - ply - 1);
                for index in 0..child_len {
                    self.pv[ply][index + 1] = self.pv[ply + 1][index];
                }
                self.pv_len[ply] = child_len + 1;
            }
            if alpha >= beta {
                if quiet {
                    if self.killers[ply][0] != mv {
                        self.killers[ply][1] = self.killers[ply][0];
                        self.killers[ply][0] = mv;
                    }
                    if let Some(previous) = self.previous_move(ply) {
                        self.counters[previous] = mv;
                    }
                    let bonus = (16 * depth * depth).min(1600);
                    self.update_history(side, mv, bonus, ply);
                    for &tried in &quiets_tried[..quiet_count] {
                        self.update_history(side, tried, -bonus, ply);
                    }
                }
                break;
            }
            if quiet && quiet_count < quiets_tried.len() {
                quiets_tried[quiet_count] = mv;
                quiet_count += 1;
            }
        }
        let bound = if best >= beta {
            Bound::Lower
        } else if best <= original_alpha {
            Bound::Upper
        } else {
            Bound::Exact
        };
        if !in_check
            && (best_move == Move::NULL || is_quiet(best_move))
            && best.abs() < MATE_BOUND
            && !(bound == Bound::Lower && best <= static_eval)
            && !(bound == Bound::Upper && best >= static_eval)
        {
            self.update_correction(best - static_eval, depth);
        }
        self.tt.store(
            key,
            ply,
            Record {
                mv: best_move,
                score: best,
                eval: raw_eval,
                depth: depth as u8,
                bound,
                pv: pv_node,
                age: 0,
            },
        );
        best
    }

    /// Pawn-structure correction history: the static evaluation tends to misjudge the
    /// same pawn structure in the same direction, so the average error that searches
    /// found for it is added back to later evaluations of positions sharing it.
    fn corrected(&self, raw: i32) -> i32 {
        let (side, slot) = self.correction_slot();
        let correction = self.correction[side][slot] / CORRECTION_GRAIN;
        (raw + correction).clamp(-MATE_BOUND + 1, MATE_BOUND - 1)
    }

    fn update_correction(&mut self, error: i32, depth: i32) {
        let weight = (depth + 1).min(16);
        let (side, slot) = self.correction_slot();
        let entry = &mut self.correction[side][slot];
        *entry = ((*entry * (256 - weight) + error * CORRECTION_GRAIN * weight) / 256)
            .clamp(-CORRECTION_MAX, CORRECTION_MAX);
    }

    fn correction_slot(&self) -> (usize, usize) {
        let side = self.position.side_to_move().index();
        (side, self.position.pawn_key() as usize % CORRECTION_ENTRIES)
    }

    fn update_history(&mut self, side: usize, mv: Move, bonus: i32, ply: usize) {
        // Gravity keeps entries inside +-HISTORY_MAX: the closer a value is to the bound,
        // the less a further bonus in the same direction moves it.
        let gravity = |entry: &mut i32| *entry += bonus - *entry * bonus.abs() / HISTORY_MAX;
        gravity(&mut self.history[side][mv.from().index()][mv.to().index()]);
        if let Some(index) = self.continuation_index(mv, ply) {
            gravity(&mut self.continuation[index]);
        }
    }

    fn piece_square(&self, mv: Move) -> usize {
        let piece = self
            .position
            .piece_at(mv.from())
            .expect("legal move has a mover");
        (piece.color.index() * 6 + piece.kind.index()) * 64 + mv.to().index()
    }

    /// Continuation history slot for `mv` as a reply to the move played one ply earlier:
    /// quiet replies that refuted the same piece arriving on the same square tend to work
    /// again, which plain from-to history cannot see.
    fn continuation_index(&self, mv: Move, ply: usize) -> Option<usize> {
        Some(self.previous_move(ply)? * PIECE_SQUARES + self.piece_square(mv))
    }

    /// Piece and destination of the move that led to `ply`, absent after a null move.
    fn previous_move(&self, ply: usize) -> Option<usize> {
        self.played[ply.checked_sub(1)?]
    }

    fn quiescence(&mut self, mut alpha: i32, beta: i32, ply: usize) -> i32 {
        self.pv_len[ply] = 0;
        if self.visit(ply) {
            return 0;
        }
        let in_check = self.position.checkers().0 != 0;
        let mut moves = self.position.tactical_moves();
        if moves.is_empty() && (in_check || self.position.legal_moves().is_empty()) {
            return if in_check { -MATE + ply as i32 } else { 0 };
        }
        if self.position.is_repetition()
            || self.position.is_insufficient_material()
            || self.position.is_fifty_move_draw()
        {
            return 0;
        }
        let key = self.position.key();
        let original_alpha = alpha;
        let hit = self.tt.probe(key, ply);
        let tt_move = hit
            .map(|record| record.mv)
            .filter(|&mv| mv != Move::NULL && self.position.is_pseudo_legal(mv));
        // Any stored bound is at least as deep as quiescence, so it can cut here directly.
        if let Some(record) = hit.filter(|record| record.mv == Move::NULL || tt_move.is_some()) {
            match record.bound {
                Bound::Exact => return record.score,
                Bound::Lower if record.score >= beta => return record.score,
                Bound::Upper if record.score <= alpha => return record.score,
                _ => {}
            }
        }
        let raw_eval = if in_check {
            0
        } else {
            hit.map_or_else(|| evaluate(&self.position), |record| record.eval)
        };
        let stand_pat = if in_check {
            -INF
        } else {
            self.corrected(raw_eval)
        };
        if ply >= MAX_PLY - 1 {
            return if in_check { 0 } else { stand_pat };
        }
        let mut best = stand_pat;
        let mut best_move = Move::NULL;
        if !in_check {
            if stand_pat >= beta {
                return stand_pat;
            }
            alpha = alpha.max(stand_pat);
        }
        self.order(&mut moves, tt_move, ply);
        for mv in moves.iter() {
            if !in_check && !self.position.see_ge(mv, 0) {
                continue;
            }
            self.position.make(mv);
            let score = -self.quiescence(-beta, -alpha, ply + 1);
            self.position.unmake();
            if self.aborted {
                return 0;
            }
            if score > best {
                best = score;
                best_move = mv;
            }
            if score > alpha {
                alpha = score;
                self.pv[ply][0] = mv;
                let child_len = self.pv_len[ply + 1].min(MAX_PLY - ply - 1);
                for index in 0..child_len {
                    self.pv[ply][index + 1] = self.pv[ply + 1][index];
                }
                self.pv_len[ply] = child_len + 1;
            }
            if alpha >= beta {
                break;
            }
        }
        self.tt.store(
            key,
            ply,
            Record {
                mv: best_move,
                score: best,
                eval: raw_eval,
                depth: 0,
                bound: if best >= beta {
                    Bound::Lower
                } else if best <= original_alpha {
                    Bound::Upper
                } else {
                    Bound::Exact
                },
                pv: false,
                age: 0,
            },
        );
        best
    }

    fn move_score(&self, mv: Move, preferred: Option<Move>, ply: usize) -> i32 {
        if Some(mv) == preferred {
            return 1_000_000;
        }
        if is_quiet(mv) {
            if mv == self.killers[ply][0] {
                return 90_000;
            }
            if mv == self.killers[ply][1] {
                return 89_000;
            }
            if self
                .previous_move(ply)
                .is_some_and(|previous| self.counters[previous] == mv)
            {
                return 88_000;
            }
            let side = self.position.side_to_move().index();
            let continuation = self
                .continuation_index(mv, ply)
                .map_or(0, |index| self.continuation[index]);
            return self.history[side][mv.from().index()][mv.to().index()] + continuation;
        }
        let attacker = self
            .position
            .piece_at(mv.from())
            .expect("legal move has mover");
        let victim = if mv.flag() == 5 {
            Some(PieceType::Pawn)
        } else {
            self.position.piece_at(mv.to()).map(|piece| piece.kind)
        };
        let gain = victim.map_or(0, |kind| {
            VALUES[kind.index()] * 16 - VALUES[attacker.kind.index()]
        }) + mv.promotion().map_or(0, |kind| VALUES[kind.index()]);
        if self.position.see_ge(mv, 0) {
            200_000 + gain
        } else {
            -200_000 + gain
        }
    }

    fn order(&self, moves: &mut MoveList, preferred: Option<Move>, ply: usize) {
        moves.sort_by_key(|mv| self.move_score(mv, preferred, ply));
    }
}

/// Late move reductions grow with the logarithm of both depth and move number, the form
/// most engines converged on after Stockfish; the divisor sets how aggressive it is.
fn reduction_table() -> [[u8; 64]; 64] {
    let mut table = [[0; 64]; 64];
    for (depth, row) in table.iter_mut().enumerate().skip(1) {
        for (index, cell) in row.iter_mut().enumerate().skip(1) {
            *cell = (0.75 + (depth as f64).ln() * (index as f64).ln() / 2.25) as u8;
        }
    }
    table
}

fn is_quiet(mv: Move) -> bool {
    mv.flag() & 4 == 0 && mv.promotion().is_none()
}

/// Win, draw and loss chances in permille for the side to move. The logistic model
/// `win = 1 / (1 + exp((a - score) / b))`, with loss mirrored, was fitted by maximum
/// likelihood to inphish self-play evaluations at 1+0.01, so it is only an estimate.
pub fn wdl(score: i32) -> (u16, u16, u16) {
    const A: f64 = 132.0;
    const B: f64 = 152.0;
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
