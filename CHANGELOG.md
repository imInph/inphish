# inphzugzwang changelog

## Unreleased

- Foundation: board state, legal move generation, Chess960 castling, position hashing, rule detection, and perft diagnostics.
- Playing engine: UCI input and search threads, iterative-deepening alpha-beta, quiescence, material and piece-square evaluation, clock control, and a deterministic bench.
- Principal variation search: 65.83 ± 18.56 Elo against the previous revision over 988 games at 8+0.08; SPRT [0, 5] accepted H1 at LLR 2.96.
- Transposition table: 143.04 ± 29.00 Elo against the previous revision over 536 games at 8+0.08; SPRT [0, 5] accepted H1 at LLR 2.95.
- Depth-4 bench signature: 686260 nodes.
