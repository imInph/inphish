# Roadmap

The original plan divided the engine into nine phases, ending with NNUE, tablebases, multithreading, and rating-list submission. On 24 September 2026 that plan was retired in favour of a smaller goal: a finished, dependable, single-threaded UCI engine that friends can drop into a chess GUI and that competes with their engines. This page records what was and was not built against the old phases so nothing is presented as more complete than it is.

## Old phases

| Phase | Status |
|---|---|
| 1. Foundation: board, legal move generation, FEN, hashing, rules, perft | Built. Standard and Chess960 perft suites match. |
| 2. Playing engine: UCI loop, iterative deepening, quiescence, time management, bench | Built. |
| 3. Search infrastructure | Superseded in 6.0 by the Stockfish 19 search port: staged move ordering, transposition-table replacement and aging, butterfly, low-ply, capture, continuation and pawn histories, and evaluation correction histories. Earlier versions used killers and counter moves; the current port does not. |
| 4. Pruning and reductions | Superseded in 6.0 by Stockfish 19's rules, including razoring, reverse futility, verified null-move pruning, ProbCut, singular and multi-cut extensions, late move reductions and pruning, futility, history and static-exchange pruning. The September development changes followed in 6.2. |
| 5. Evaluation | Superseded in 3.0 by the network for play. Partly built: tapered hand-written evaluation with PeSTO starting tables, mobility, pawn structure, passed pawns, king safety, threats, and outposts, plus a Texel tuner. A full tune on the available games did worse in play and was not adopted; the weights are mostly hand-set. |
| 6. Parallelism and polish | Mostly built in 2.0: Lazy SMP through `Threads`, `MultiPV`, WDL output, strength limiting through `UCI_Elo`, and the `UCI_Chess960` option. A Polyglot book followed in 5.0, and full UCI pondering in 7.0. |
| 7. Syzygy tablebases | Built in 3.1: WDL and DTZ probing ported from Stockfish 13, exact against it on 3,974 reference positions, with DTZ ranking at the root and WDL cutoffs in the search. |
| 8. NNUE | Built in 3.0 with Stockfish 13's network `nn-62ef826d1a6d`, in 4.0 with Stockfish 15.1's `nn-ad9b42354671`, in 5.x and 6.x with Stockfish 19's `nn-1a298aa575a0`, which adds threat and pawn-pair inputs, and in 7.0 with development network `nn-252f33942263` (SFNNv17, no separate piece-square output), rather than a network of inphish's own: exact inference, incremental accumulators with a king-square refresh cache, and vector dot products are inphish's code. |
| 9. Release engineering | Built in a reduced form: four release archives (Linux x86-64, Windows x86-64, macOS Apple Silicon and Intel) without CPU-specific flavours. |

Earlier strength-changing commits were measured by SPRT and those results stand as recorded in `docs/TESTING.md`. Later changes were checked with small bounded matches, which are directional evidence only.

## After 3.1

Version 2.0 completed the parts of phase 6 that matter in a chess GUI. Version 3.0 replaced the hand-written evaluation with Stockfish 13's NNUE network, since training a network on inphish's own games is not practical on the available hardware; Stockfish and its networks are GPL-3.0, like inphish, and the network is named in the README and release notes. Version 3.1 added Syzygy tablebase probing. Version 4.0 aimed at playing strength alone: a faster search, Stockfish 15.1's larger network, and more selective pruning, which moved inphish from about 2835 to about 3150 on the Stockfish 19 `UCI_Elo` ladder at 1+0.01. Of the original phases, a Polyglot book and pondering beyond the UCI plumbing remained unbuilt after 4.0.

## 5.0

Version 5.0 moved to Stockfish 19's network, whose threat and pawn-pair inputs make each node about twice as expensive but scored 23 of 40 against 4.0.0 and placed inphish at about 3264 on the Stockfish 19 `UCI_Elo` ladder at 1+0.01, and added a bundled Polyglot opening book built from Lichess games, off by default. `UCI_Elo` was recalibrated for the new network. Of the original phases only pondering beyond the UCI plumbing remains unbuilt.

## 6.0

Version 6.0 replaced inphish's own search with a port of Stockfish 19's, including its move ordering, histories, correction histories, transposition-table rules, time management and thread voting, and followed Stockfish 19 in recording each move's threat changes and updating the network's accumulators only when a position is evaluated. It scored 37.5 of 40 against 5.0.0 and placed inphish at about 3588 on the Stockfish 19 `UCI_Elo` ladder at 1+0.01, where the scale is close to saturated, and 8 of 20 against Stockfish 19 at full strength. `UCI_Elo` was recalibrated for the stronger search. Pondering beyond the UCI plumbing remains unbuilt.

## 6.1

Version 6.1 kept 6.0's play and made it about 6% faster, mainly by loading table and correction-history entries before they are read, and added the `Contempt` and `UCI_AnalyseMode` options. Contempt, a longer time allocation, and several further speed changes were measured and are recorded in `docs/TESTING.md`; none that changed play scored better. The remaining speed gap to Stockfish 19 is spread across move making, threat indexing and move ordering, with no single large item left.

## 6.2

Version 6.2 ported the search changes of Stockfish's development build `dev-20260930-49ea5ded` since Stockfish 19, a fix against chains of late-move extensions, and less time when behind on the clock, keeping Stockfish 19's network. The development build's new network, which drops the piece-square output, is planned for a 7.0.0 pre-release, since Stockfish may still change it before its next release.

## 7.0 stable candidate

Version 7.0 keeps the 6.2 search and adopts Stockfish development build `49ea5ded`'s SFNNv17 network `nn-252f33942263` and evaluation formula. It completes UCI pondering, adds compatible external networks through `EvalFile`, permits builds without the bundled opening book, and refits WDL estimates for the bundled network. The features were introduced in the two 7.0 pre-releases; the stable candidate checks strength limiting and release behaviour and updates the public documentation. The original pondering gap is closed. No new network training, search overhaul, rating-list claim, or CPU-specific release flavours are planned.
