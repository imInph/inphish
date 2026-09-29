# inphzugzwang changelog

## 6.0.0 - 2026-09-29

- The search is a port of Stockfish 19's (GPL-3.0): its principal variation search and quiescence search with every pruning, reduction and extension rule and constant, singular and multi-cut extensions, the staged move picker, butterfly, low-ply, capture, continuation and pawn histories, pawn, minor-piece, non-pawn and continuation correction histories, the transposition table's replacement and aging rules, iterative deepening with aspiration windows from each root move's running average, optimism, Stockfish's time management and Lazy SMP thread voting, all in Stockfish's internal units. The move picker no longer uses killers or counter moves, as Stockfish 19 does not. Histories and time-management state now carry over between the moves of a game. inphish's own board, move generator, network inference, tablebases, book, `MultiPV`, strength limit and UCI stay.
- Repetitions follow Stockfish: each position stores the distance to its last repetition, a cuckoo table finds moves that would repeat, and the table key changes every eight plies once the fifty-move counter reaches 14. A null move no longer advances that counter.
- The static evaluation includes Stockfish 19's optimism term, set from the root score.
- `Move Overhead` now defaults to 10 ms, down from 20.
- With several threads, every thread now stops the search at a node limit (`go nodes` and the `UCI_Elo` budget) when it adds a batch of 1,024 nodes to the shared total; before, only the main thread checked, and helpers could run on past the limit while it waited for a processor.
- Faster: moves record the threats they add and remove as they are made, as in Stockfish 19, and the network's accumulators are updated only when a position is evaluated, one perspective at a time, a king move refreshing only its own side through the cache, with the positions before it derived backwards; the first layer finds its nonzero inputs with a bitmask; the position keeps each king's slider blockers so checks and pins need no replay. Search results are unchanged by these. On an Apple M2 with Hash 64, one thread, ten seconds on each of two positions: about 795,000 nodes per second, against 741,000 for 5.0.0 and 1.06 million for Stockfish 19 built for the machine.
- `UCI_Elo` node budgets were recalibrated for the stronger search, whose old budgets scored 14/20 at 1320 and 14.5/20 at 3000 against Stockfish 19 at the same setting. With the new budgets: 11/20 at 1320, 12/20 at 2800, 13.5/20 at 3000; 2600 kept its budget at 10/20, and 1600 and 2200, which scored 11.5 and 11 of 20, were lowered slightly without a new run.
- Checks at 1+0.01, Hash 16 MiB, one thread, balanced book, opening book off, on `c96a2bc` (the same play at full strength as the tag, whose later commits change only the `UCI_Elo` budgets, documents and version): 37.5/40 against 5.0.0 (35 W / 0 L / 5 D); Stockfish 19 at `UCI_Elo` 3000 and 3190: 19 and 18.5 of 20 (5.0.0: 16 and 12.5), a ladder fit of 3588 with a 95% range of 3397 to 3864, against 3264 for 5.0.0, where both rungs are near the top so the estimate is loose; Stockfish 19 at full strength: 8/20 (2 W / 6 L / 12 D); Chess960 against Stockfish at 2800: 20/20 (5.0.0: 17.5); 20/20 against Morstilia 6.0.0, all checkmates; 20/20 against MaiEngine, 15 of them MaiEngine time forfeits; with the opponents' books off, 20/20 against Morstilia v7-pre and 20/20 against Mai V2, 3 of them Mai V2 time forfeits; 12.5/20 against Mai v3 (6 W / 1 L / 13 D), against which 5.0.0 scored 1.5/20. No inphish time losses or illegal moves. Small samples, not Elo measurements.
- `bench` now searches at depth 13 by default. Bench signature: 2408299 nodes.

## 5.0.0 - 2026-09-27

- NNUE evaluation with Stockfish 19's network `nn-1a298aa575a0` (GPL-3.0) in place of Stockfish 15.1's `nn-ad9b42354671`. Besides the king-bucketed piece squares it reads threats of one piece on another and pairs of pawns on the same or neighbouring files, into the same 1024-wide accumulator with eight layer stacks. inphish's inference matches Stockfish 19 exactly on 2,253 reference positions, including its final evaluation formula, which the static evaluation now uses without the optimism term. Changed threats and pawn pairs are found around the squares each move changes, all changed rows are applied in one tiled Neon or AVX2 pass, and the first layer reads only non-zero input groups. The search runs at about half the nodes per second of 4.0.0 and still scored 23 of 40 against it.
- A bundled Polyglot opening book of 6,378,170 entries (102 MB) built from Lichess games of August 2026 between players rated 2000 and above (CC0), behind the new options `OwnBook` (off by default), `BookFile` for another Polyglot book, `Book Depth` (40 plies) and `Book Best Move`. The book is not used in analysis, pondering, `searchmoves`, `MultiPV` above 1 or Chess960. `tools/book` rebuilds it; the source and filters are in `crates/inphzugzwang-book/book/README.md`. Polyglot keys follow the format exactly, including the en-passant file for a pinned pawn.
- `UCI_Elo` node budgets now come from calibration points interpolated between, since the stronger network made the old doubling rule too strong in the middle of the range. Against Stockfish 19 at the same setting: 13.5/20 at 1600, 12.5/20 at 2200, 10.5/20 at 2600 and at 2800, 10/20 at 3000.
- The win/draw/loss model behind `UCI_ShowWDL` was refitted to the new network, and now reads more drawish.
- The binary is about 200 MB with the network and the book.
- The license file is again the unmodified GPL-3.0 text; the copyright notice moved to the README.
- Checks at 1+0.01, Hash 16 MiB, one thread: 23/40 against 4.0.0 (17 W / 11 L / 12 D); 20/20 against Morstilia 6.0.0, all checkmates; 20/20 against MaiEngine, 15 of them MaiEngine time forfeits; on the tagged revision with the opponents' books off, 20/20 against Morstilia v7-pre (`511a656`), all checkmates, and 20/20 against Mai V2 (`e5d71bc`), 9 of them Mai V2 time forfeits; Stockfish 19 at `UCI_Elo` 3000 and 3190: 16 and 12.5 of 20 (4.0.0: 13.5 and 9.5), a ladder fit of 3264 ± 123 against 3149 ± 102 for 4.0.0; Chess960 against Stockfish at 2800: 17.5/20 (4.0.0: 12.5/20). No inphish time losses or illegal moves. Small samples, not Elo measurements.
- Depth-4 bench signature: 74474 nodes.

## 4.0.0 - 2026-09-26

- NNUE evaluation with Stockfish 15.1's HalfKAv2_hm network `nn-ad9b42354671` (GPL-3.0) in place of Stockfish 13's `nn-62ef826d1a6d`: king-bucketed, mirrored features including the kings, a 1024-wide accumulator with eight piece-square buckets, pairwise-multiplied inputs and eight layer stacks. inphish's inference matches Stockfish 15.1 exactly on 2,086 reference positions, plain and material-adjusted. King moves refresh through a per-thread cache of accumulators by king square. The binary grows to about 48 MB. The static evaluation follows Stockfish 15.1's scaling without its optimism term, and no longer falls back to the hand-written evaluation in bare endings.
- Faster search with the play unchanged: moves are generated into one list per ply, the transposition table is probed before moves are generated, and the network's hidden layers are computed four rows at a time. Fixed-depth searches ran at 2.07 million nodes per second on an Apple M2 against 1.55 million before, with the old network.
- Search: the stored score replaces the static evaluation for pruning where its bound makes it tighter; internal iterative reductions; history and static-exchange pruning near the leaves; delta pruning in quiescence; two plies more reduction at expected cut nodes; ProbCut. Capture history and a second continuation history were tried and dropped.
- `UCI_Elo` now reaches 3000, up from 2600, and its node budget is 200 nodes at the lowest setting, doubling every 240 Elo. Moves are weakened as before up to 2600; above it only the node budget limits play. Against Stockfish 19 at the same setting it scored 14/20 at 1600, 13/20 at 2200, 8/20 at 2800 and 9.5/20 at 3000. The win/draw/loss model was refitted to the new network.
- `tests/openings/random8.epd` adds 300 balanced random openings from `tools/random_openings.py` for development matches.
- Checks at 1+0.01, Hash 16 MiB, one thread: 32/40 against 3.1.2 (25 W / 1 L / 14 D); 20/20 against Morstilia 6.0.0, all checkmates; 20/20 against MaiEngine, 11 of them MaiEngine time forfeits; Stockfish 19 at `UCI_Elo` 2800, 3000 and 3190: 17.5, 13.5 and 9.5 of 20 (3.0.0: 11 and 5.5 at 2800 and 3000), a ladder fit of 3149 ± 102 against 2835 ± 97 for 3.0.0; Chess960 against Stockfish at 2800: 12.5/20 (3.0.0: 9/20). No inphish time losses or illegal moves. Small samples, not Elo measurements.
- Depth-4 bench signature: 66999 nodes.

## 3.1.2 - 2026-09-26

- Node limits (`go nodes`, and the node cap behind `UCI_Elo`) now apply to the total of all search threads. Before, only the main thread checked them and helper threads searched on until it finished, so with several threads on a busy machine `go nodes 20000` could report over 57,000 nodes. The overshoot is now at most about a thousand nodes per thread.
- No change with one thread: the depth-4 bench signature stays 79837 nodes.

## 3.1.1 - 2026-09-26

- The UCI `id author` line now reads `inph`.
- README figures share one light palette, with a new figure for the build and diagnostic commands.
- No change to play: the depth-4 bench signature stays 79837 nodes.

## 3.1.0 - 2026-09-25

- Syzygy endgame tablebases through the `SyzygyPath` option: WDL and DTZ probing ported from Stockfish 13's `tbprobe` (GPL-3.0) in the new `inphzugzwang-syzygy` crate. It matches Stockfish 13 on all 3,974 of 4,000 reference positions (426 with an en passant capture) that Stockfish 13 answers; the other 26 it fails, and Stockfish 19's search agrees with inphish on the four checked. `tests/syzygy/reference.txt` comes from `tools/syzygy_reference.py`; the test needs `SYZYGY_PATH` and is ignored otherwise. Tables are memory mapped on Unix and read into memory on Windows.
- A covered root is ranked by DTZ and only its best-ranked moves searched, reporting the tablebase result; elsewhere the search probes WDL after captures and pawn moves. Info lines report `tbhits`.
- On a nearly spent clock the engine now plays the result of a 1,000-node search instead of the first legal move, which had turned a tablebase-won KQ against KR into a mate in one against Morstilia.
- Checks at 1+0.01, Hash 16 MiB, one thread, with the 3-5 piece tables: 21.5/40 against 3.0.0 (13 W / 10 L / 17 D); 20/20 against Morstilia 6.0.0; 20/20 against MaiEngine (on `685736a`, before the clock fix), 12 of them MaiEngine time forfeits. No inphish time losses or illegal moves.
- Depth-4 bench signature: 79837 nodes, unchanged.

## 3.0.0 - 2026-09-25

- NNUE evaluation with Stockfish 13's HalfKP 256x2-32-32 network `nn-62ef826d1a6d` (GPL-3.0), bundled in the binary. inphish's own integer inference matches Stockfish 13's value exactly on 2,169 reference positions (`tests/nnue/reference.txt`, generated by `tools/nnue_reference.py` from a local Stockfish 13 build). Accumulators are updated per move and checked against fresh ones over depth-3 walks of the perft positions. Bare endings keep the hand-written evaluation, as in Stockfish 13.
- Dot products use the aarch64 dot-product instructions or AVX2 when available, detected at runtime. Bench depth 8 runs at about 1.7 million nodes per second on an Apple M2, against about 2.3 million with the hand-written evaluation.
- The win/draw/loss model was refitted to network self-play, and the `UCI_Elo` node budget lowered to 600 nodes at the lowest setting; at 1+0.01 against Stockfish 19 at the same setting it scored 12.5/20 at 1600 and 13/20 at 2200.
- Checks at 1+0.01, Hash 16 MiB, one thread: 34.5/40 against 2.0.0 (33 W / 4 L / 3 D); 19/20 against Morstilia 6.0.0 (18 W / 2 D); 20/20 against MaiEngine, 15 of them MaiEngine time forfeits; Stockfish 19 at `UCI_Elo` 2600, 2800, 3000: 16, 11 and 5.5 of 20 (2.0.0: 12.5 and 6 at 2600 and 2800); Chess960 against Stockfish at 2800: 9/20. No inphish time losses or illegal moves. Small samples, not Elo measurements.
- Depth-4 bench signature: 79837 nodes.

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
