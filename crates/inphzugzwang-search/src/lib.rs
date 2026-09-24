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
                -worker.negamax(depth as i32 - 1, -INF, -alpha, 1)
            } else {
                -worker.negamax(depth as i32 - 1, -alpha - 1, -alpha, 1)
            };
            if index > 0 && score > alpha && !worker.aborted {
                score = -worker.negamax(depth as i32 - 1, -INF, -alpha, 1);
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

    fn negamax(&mut self, depth: i32, mut alpha: i32, beta: i32, ply: usize) -> i32 {
        self.pv_len[ply] = 0;
        if self.visit(ply) {
            return 0;
        }
        if self.position.is_threefold() || self.position.is_insufficient_material() {
            return 0;
        }
        if ply >= MAX_PLY - 1 {
            return evaluate(&self.position);
        }
        if depth <= 0 {
            return self.quiescence(alpha, beta, ply);
        }
        let mut moves = self.position.legal_moves();
        if moves.is_empty() {
            return if self.position.checkers().0 != 0 {
                -MATE + ply as i32
            } else {
                0
            };
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
        let static_eval = hit.map_or_else(|| evaluate(&self.position), |record| record.eval);
        self.order(&mut moves, tt_move, ply);
        let mut best = -INF;
        let mut best_move = Move::NULL;
        for (index, mv) in moves.iter().enumerate() {
            self.position.make(mv);
            let mut score = if index == 0 {
                -self.negamax(depth - 1, -beta, -alpha, ply + 1)
            } else {
                -self.negamax(depth - 1, -alpha - 1, -alpha, ply + 1)
            };
            if index > 0 && score > alpha && score < beta && !self.aborted {
                score = -self.negamax(depth - 1, -beta, -alpha, ply + 1);
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
                if is_quiet(mv) && self.killers[ply][0] != mv {
                    self.killers[ply][1] = self.killers[ply][0];
                    self.killers[ply][0] = mv;
                }
                break;
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
        if self.position.is_threefold()
            || self.position.is_insufficient_material()
            || self.position.is_fifty_move_draw()
        {
            return 0;
        }
        let stand_pat = evaluate(&self.position);
        if ply >= MAX_PLY - 1 {
            return stand_pat;
        }
        if !in_check {
            if stand_pat >= beta {
                return stand_pat;
            }
            alpha = alpha.max(stand_pat);
        }
        self.order(&mut moves, None, ply);
        for mv in moves.iter() {
            self.position.make(mv);
            let score = -self.quiescence(-beta, -alpha, ply + 1);
            self.position.unmake();
            if self.aborted {
                return 0;
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
        alpha
    }

    fn move_score(&self, mv: Move, preferred: Option<Move>, ply: usize) -> i32 {
        if Some(mv) == preferred {
            return 100_000;
        }
        if is_quiet(mv) {
            if mv == self.killers[ply][0] {
                return 2;
            }
            if mv == self.killers[ply][1] {
                return 1;
            }
        }
        let attacker = self
            .position
            .piece_at(mv.from())
            .expect("legal move has mover");
        let victim = if mv.is_castle() {
            None
        } else {
            self.position.piece_at(mv.to()).map(|piece| piece.kind)
        };
        let capture = if mv.flag() == 5 {
            Some(PieceType::Pawn)
        } else {
            victim
        };
        let gain = capture.map_or(0, |kind| {
            VALUES[kind.index()] * 16 - VALUES[attacker.kind.index()]
        });
        let promotion = mv.promotion().map_or(0, |kind| VALUES[kind.index()]);
        gain + promotion
    }

    fn order(&self, moves: &mut MoveList, preferred: Option<Move>, ply: usize) {
        moves.sort_by_key(|mv| self.move_score(mv, preferred, ply));
    }
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
