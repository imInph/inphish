# inphzugzwang changelog

## 0.1.0-preview.1 - 2026-09-23

- Foundation: board state, legal move generation, Chess960 castling, position hashing, rule detection, and perft diagnostics.
- Playing engine: UCI input and search threads, iterative-deepening alpha-beta, quiescence, material and piece-square evaluation, clock control, and a deterministic bench.
- Principal variation search: 65.83 ± 18.56 Elo against the previous revision over 988 games at 8+0.08; SPRT [0, 5] accepted H1 at LLR 2.96.
- Transposition table: 143.04 ± 29.00 Elo against the previous revision over 536 games at 8+0.08; SPRT [0, 5] accepted H1 at LLR 2.95.
- Killer move ordering: 34.59 ± 13.10 Elo against the previous revision over 1,804 games at 8+0.08; SPRT [0, 5] accepted H1 at LLR 2.95.
- Depth-4 bench signature: 499138 nodes.
