# inphzugzwang changelog

## 0.1.0 - 2026-09-24

- Tapered evaluation with PeSTO piece-square tables, mobility, pawn structure, passed pawns, rook files, bishop pair, king attackers and pawn shield, and pawnless minor-piece draw scaling.
- Selective search: check extension, reverse futility, null-move, late move and futility pruning, late move reductions, history and static-exchange move ordering, and SEE-pruned quiescence.
- Fix instant depth-0 moves whenever less than about half a second remained on the clock.
- Bounded matches at 10+0.1, Hash 16 MiB, one thread, ten openings with colors swapped: 19 W / 0 L / 1 D against Morstilia 6.0.0 and 18 W / 1 L / 1 D against MaiEngine, all normal terminations. Informal samples, not Elo measurements.
- Depth-4 bench signature: 95766 nodes.

## 0.1.0-preview.2 - 2026-09-23

- Fix preview publishing by checking out the annotated tag before creating the release.
- No engine behavior change; depth-4 bench signature remains 499138 nodes.

## 0.1.0-preview.1 - 2026-09-23

- Foundation: board state, legal move generation, Chess960 castling, position hashing, rule detection, and perft diagnostics.
- Playing engine: UCI input and search threads, iterative-deepening alpha-beta, quiescence, material and piece-square evaluation, clock control, and a deterministic bench.
- Principal variation search: 65.83 ± 18.56 Elo against the previous revision over 988 games at 8+0.08; SPRT [0, 5] accepted H1 at LLR 2.96.
- Transposition table: 143.04 ± 29.00 Elo against the previous revision over 536 games at 8+0.08; SPRT [0, 5] accepted H1 at LLR 2.95.
- Killer move ordering: 34.59 ± 13.10 Elo against the previous revision over 1,804 games at 8+0.08; SPRT [0, 5] accepted H1 at LLR 2.95.
- Depth-4 bench signature: 499138 nodes.
