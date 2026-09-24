# Roadmap

The original plan divided the engine into nine phases, ending with NNUE, tablebases, multithreading, and rating-list submission. On 24 September 2026 that plan was retired in favour of a smaller goal: a finished, dependable, single-threaded UCI engine that friends can drop into a chess GUI and that competes with their engines. This page records what was and was not built against the old phases so nothing is presented as more complete than it is.

## Old phases

| Phase | Status |
|---|---|
| 1. Foundation: board, legal move generation, FEN, hashing, rules, perft | Built. Standard and Chess960 perft suites match. |
| 2. Playing engine: UCI loop, iterative deepening, quiescence, time management, bench | Built. |
| 3. Search infrastructure | Partly built: transposition table, principal variation search, aspiration windows, killers, butterfly and one-ply continuation history, static exchange evaluation for ordering and quiescence pruning. No staged move picker, counter-move table, or mate distance pruning. |
| 4. Pruning and reductions | Partly built: reverse futility, null-move, late move and futility pruning, late move reductions, check extensions. No singular extensions, ProbCut, razoring, internal iterative reductions, history pruning, or correction history. An improving flag was tried and dropped. |
| 5. Evaluation | Partly built: tapered hand-written evaluation with PeSTO starting tables, mobility, pawn structure, passed pawns, king safety, threats, and outposts, plus a Texel tuner. A full tune on the available games did worse in play and was not adopted; the weights are mostly hand-set. |
| 6. Parallelism and polish | Not built: no Lazy SMP, MultiPV, pondering beyond the UCI plumbing, WDL output, strength limiting, Polyglot book, or UCI Chess960 option. |
| 7. Syzygy tablebases | Not built. |
| 8. NNUE | Not built. |
| 9. Release engineering | Built in a reduced form: four release archives (Linux x86-64, Windows x86-64, macOS Apple Silicon and Intel) without CPU-specific flavours. |

Earlier strength-changing commits were measured by SPRT and those results stand as recorded in `docs/TESTING.md`. Later changes were checked with small bounded matches, which are directional evidence only.

## After 1.0

Anything above marked not built is optional future work rather than a prerequisite. NNUE, if ever added, would be trained on inphish's own games with the data source documented; no borrowed network is planned.
