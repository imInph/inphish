# Roadmap

The original plan divided the engine into nine phases, ending with NNUE, tablebases, multithreading, and rating-list submission. On 24 September 2026 that plan was retired in favour of a smaller goal: a finished, dependable, single-threaded UCI engine that friends can drop into a chess GUI and that competes with their engines. This page records what was and was not built against the old phases so nothing is presented as more complete than it is.

## Old phases

| Phase | Status |
|---|---|
| 1. Foundation: board, legal move generation, FEN, hashing, rules, perft | Built. Standard and Chess960 perft suites match. |
| 2. Playing engine: UCI loop, iterative deepening, quiescence, time management, bench | Built. |
| 3. Search infrastructure | Partly built: transposition table, principal variation search, aspiration windows, killers, butterfly and one-ply continuation history, static exchange evaluation for ordering and quiescence pruning, and since 2.0 a counter-move table and mate distance pruning. A lazily picked move list was tried in 2.0 and dropped as no faster. |
| 4. Pruning and reductions | Partly built: reverse futility, null-move, late move and futility pruning, late move reductions, check extensions. Since 2.0, pawn-structure correction history. No ProbCut, razoring, internal iterative reductions, or history pruning. An improving flag and, in 2.0, singular extensions were tried and dropped. |
| 5. Evaluation | Superseded in 3.0 by the network for play. Partly built: tapered hand-written evaluation with PeSTO starting tables, mobility, pawn structure, passed pawns, king safety, threats, and outposts, plus a Texel tuner. A full tune on the available games did worse in play and was not adopted; the weights are mostly hand-set. |
| 6. Parallelism and polish | Mostly built in 2.0: Lazy SMP through `Threads`, `MultiPV`, WDL output, strength limiting through `UCI_Elo`, and the `UCI_Chess960` option. No pondering beyond the UCI plumbing and no Polyglot book. |
| 7. Syzygy tablebases | Not built; planned for 3.1. |
| 8. NNUE | Built in 3.0 with Stockfish 13's network `nn-62ef826d1a6d` rather than a network of inphish's own: exact inference, incremental accumulators, and vector dot products are inphish's code. |
| 9. Release engineering | Built in a reduced form: four release archives (Linux x86-64, Windows x86-64, macOS Apple Silicon and Intel) without CPU-specific flavours. |

Earlier strength-changing commits were measured by SPRT and those results stand as recorded in `docs/TESTING.md`. Later changes were checked with small bounded matches, which are directional evidence only.

## After 3.0

Version 2.0 completed the parts of phase 6 that matter in a chess GUI. Version 3.0 replaced the hand-written evaluation with Stockfish 13's NNUE network, since training a network on inphish's own games is not practical on the available hardware; Stockfish and its networks are GPL-3.0, like inphish, and the network is named in the README and release notes. Syzygy endgame tablebase probing is the next planned item, for 3.1. It needs the 3-5 piece tables, about 1 GB, for testing.
