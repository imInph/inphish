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
    let max_depth = worker
        .limits
        .depth
        .unwrap_or((MAX_PLY - 1) as u8)
        .min((MAX_PLY - 1) as u8);
    for depth in 1..=max_depth {
        if worker.should_stop() {
            break;
        }
        worker.pv_len[0] = 0;
        let mut alpha = -INF;
        let mut best_score = -INF;
        let mut iteration_best = best;
        let iteration_start_nodes = worker.nodes;
        let mut best_move_nodes = 0;
        let mut ordered = candidates.clone();
        ordered.sort_by_key(|&mv| -worker.move_score(mv, best, 0));
        for (index, mv) in ordered.into_iter().enumerate() {
            if worker.should_stop() {
                break;
            }
            let before = worker.nodes;
            worker.position.make(mv);
            let mut score = if index == 0 {
                -worker.negamax(depth as i32 - 1, -INF, -alpha, 1, true)
            } else {
                -worker.negamax(depth as i32 - 1, -alpha - 1, -alpha, 1, true)
            };
            if index > 0 && score > alpha && !worker.aborted {
                score = -worker.negamax(depth as i32 - 1, -INF, -alpha, 1, true);
            }
            worker.position.unmake();
            if worker.aborted {
                break;
            }
            if score > best_score {
                best_score = score;
                iteration_best = Some(mv);
                best_move_nodes = worker.nodes - before;
                worker.pv[0][0] = mv;
                let child_len = worker.pv_len[1].min(MAX_PLY - 1);
                for index in 0..child_len {
                    worker.pv[0][index + 1] = worker.pv[1][index];
                }
                worker.pv_len[0] = child_len + 1;
            }
            alpha = alpha.max(score);
        }
        if worker.aborted || best_score == -INF {
            break;
        }
        let score_drop = completed.depth > 0 && completed.score - best_score > 80;
        stable_best = if completed.depth > 0 && best == iteration_best {
            stable_best.saturating_add(1)
        } else {
            0
        };
        best = iteration_best;
        completed = Info {
            depth,
            seldepth: worker.seldepth,
            score: best_score,
            nodes: worker.nodes,
            elapsed: worker.started.elapsed(),
            hashfull: worker.tt.hashfull(),
            pv: worker.pv[0][..worker.pv_len[0]].to_vec(),
        };
        on_info(completed.clone());
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
    Result {
        best,
        info: completed,
    }
}

impl Search<'_> {
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
        beta: i32,
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
        let static_eval = if in_check {
            0
        } else {
            hit.map_or_else(|| evaluate(&self.position), |record| record.eval)
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
                if depth <= 4 && index >= 3 + (depth * depth) as usize {
                    continue;
                }
                if depth <= 3 && static_eval + 100 + 100 * depth <= alpha {
                    continue;
                }
            }
            let history = self.history[side][mv.from().index()][mv.to().index()];
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
                    let bonus = (16 * depth * depth).min(1600);
                    self.update_history(side, mv, bonus);
                    for &tried in &quiets_tried[..quiet_count] {
                        self.update_history(side, tried, -bonus);
                    }
                }
                break;
            }
            if quiet && quiet_count < quiets_tried.len() {
                quiets_tried[quiet_count] = mv;
                quiet_count += 1;
            }
        }
        self.tt.store(
            key,
            ply,
            Record {
                mv: best_move,
                score: best,
                eval: static_eval,
                depth: depth as u8,
                bound: if best >= beta {
                    Bound::Lower
                } else if best <= original_alpha {
                    Bound::Upper
                } else {
                    Bound::Exact
                },
                pv: pv_node,
                age: 0,
            },
        );
        best
    }

    fn update_history(&mut self, side: usize, mv: Move, bonus: i32) {
        let entry = &mut self.history[side][mv.from().index()][mv.to().index()];
        // Gravity keeps entries inside +-HISTORY_MAX: the closer a value is to the bound,
        // the less a further bonus in the same direction moves it.
        *entry += bonus - *entry * bonus.abs() / HISTORY_MAX;
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
        let stand_pat = if in_check {
            -INF
        } else {
            evaluate(&self.position)
        };
        if ply >= MAX_PLY - 1 {
            return if in_check { 0 } else { stand_pat };
        }
        let mut best = stand_pat;
        if !in_check {
            if stand_pat >= beta {
                return stand_pat;
            }
            alpha = alpha.max(stand_pat);
        }
        self.order(&mut moves, None, ply);
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
            best = best.max(score);
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
            let side = self.position.side_to_move().index();
            return self.history[side][mv.from().index()][mv.to().index()];
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
    fn stopped_search_keeps_legal_fallback() {
        let control = control();
        control.stop.store(true, Ordering::Relaxed);
        let position = Position::startpos();
        let result = search(position, Limits::default(), &control, |_| {});
        assert!(result.best.is_some());
    }
}
