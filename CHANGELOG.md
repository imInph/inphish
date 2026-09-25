# inphzugzwang changelog

## 2.0.0 - 2026-09-25

- Chess960 through the `UCI_Chess960` option, with X-FEN and Shredder-FEN castling rights and king-takes-rook castling notation.
- `MultiPV`, `UCI_ShowWDL` (a logistic win/draw/loss model fitted to self-play by `tools/wdl_fit.py`), `Threads` (Lazy SMP), and `UCI_LimitStrength` with `UCI_Elo` from 1320 to 2600, calibrated approximately against Stockfish 19.
- Search: mate distance pruning, a counter-move table, and pawn-structure correction history. Singular extensions and a lazily picked move list were tried and dropped.
- `tools/match.sh` takes `MATCH_VARIANT` and `MATCH_ENGINE_OPTIONS`; `tests/openings/chess960.epd` holds ten Chess960 starting layouts.
- Checks at 1+0.01, Hash 16 MiB, one thread: 16/40 against 1.0.0 (43/100 over three samples of the same search, within noise); 20/20 against Morstilia 6.0.0, all checkmates; 20/20 against MaiEngine, 17 of them MaiEngine time forfeits; Stockfish 19 at `UCI_Elo` 2400, 2600, 2800: 14, 12.5 and 6 of 20 (1.0.0: 11.5, 12, 3); Chess960 against Stockfish at 2400: 11/20. Small samples, not Elo measurements. Playing strength is about that of 1.0.0.
- Depth-4 bench signature: 86212 nodes.

## 1.0.0 - 2026-09-24

- Evaluation terms for pieces attacked by lesser pieces, undefended attacked pieces, knight outposts, rooks on the seventh rank, and passed-pawn king distance, all read from one indexed weight table.
- Transposition-table probing and storing in quiescence, aspiration windows from depth 5, and a one-ply continuation history for quiet-move ordering.
- A Texel tuner in `tools/tune`; its full fit on the available games played worse and was not adopted.
- A committed balanced opening book, a bounded match runner, and UCI edge-case tests.
- The release title now comes from the annotated tag's subject line.
- `docs/ROADMAP.md` records which of the original nine phases were built, partly built, or not built.
- Development batches at 1+0.01 against the preceding build, 40 games each: new evaluation terms 21.5/40 against 0.1.0, quiescence table 21.5/40, aspiration windows 25/40, continuation history 21.5/40. Directional only.
- Release checks at 1+0.01, Hash 16 MiB, one thread, balanced openings with colors swapped: 30/40 against 0.1.0 (26 W / 6 L / 8 D); 19/20 against Morstilia 6.0.0 (18 W / 0 L / 2 D); 20/20 against MaiEngine, of which 16 were MaiEngine time forfeits and 4 checkmates. Small samples, not Elo measurements.
- Depth-4 bench signature: 86020 nodes.

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
