# Engine testing

Run the normal checks:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --release
```

The full published perft suite runs separately:

```sh
cargo test --release -p inphzugzwang-core --test perft -- --ignored
```

`tests/perft/standard.txt` contains the starting position to depth 6, Kiwipete to depth 5, and positions 3–6 at their specified depths. `tests/perft/chess960.txt` contains Chess960 positions at depths 3 and 5. These counts come from the Chess Programming Wiki's [standard](https://chessprogramming.org/Perft_Results) and [Chess960](https://chessprogramming.org/Chess960_Perft_Results) perft tables. `tests/perft/chess960_starts.txt` records all 960 starting layouts at depth 3, cross-checked against Stockfish 19. The normal test suite runs the quick subsets. Slider tests compare every blocker subset against direct ray tracing. Position tests check FEN round trips, all hash keys, exact make/unmake, draw rules, and Chess960 castling.

The deterministic FEN mutation test runs with `cargo test --release`. For coverage-guided fuzzing, install `cargo-fuzz` and a nightly Rust toolchain, then run:

```sh
cargo +nightly fuzz run fen -- -max_total_time=30
```

The engine itself builds on stable Rust. The fuzz harness is an independent workspace and does not add a runtime dependency to the engine.

The release test suite includes a UCI subprocess smoke test and a depth-4 bench signature check. Run them separately with:

```sh
cargo test --release -p inphish --test uci
cargo test --release -p inphish --test bench
target/release/inphish bench 4
```

The depth-4 signature is `95766` nodes. It must match in debug and release builds and on supported platforms.

The first search SPRT compared PVS with the preceding alpha-beta revision at 8+0.08, one thread per engine, without a hash table. It accepted H1 on [0, 5] Elo after 988 paired-opening games: 549 wins, 364 losses, 75 draws, LLR 2.96 against ±2.94 bounds. The estimated gain was 65.83 ± 18.56 Elo (95%). All 988 games terminated normally. This measures the change, not an absolute rating.

The transposition-table SPRT compared the 16 MiB default table against the preceding hashless revision at 8+0.08 and one thread per engine. It accepted H1 on [0, 5] Elo after 536 paired-opening games: 350 wins, 141 losses, 45 draws, LLR 2.95 against ±2.94 bounds. The estimated gain was 143.04 ± 29.00 Elo (95%). All counted games terminated normally; fastchess also wrote one normal, unpaired game to the PGN while stopping.

The killer-move SPRT compared two quiet killers per ply against the preceding transposition-table revision at 8+0.08, Hash 16 MiB, and one thread per engine. It accepted H1 on [0, 5] Elo after 1,804 paired-opening games: 948 wins, 769 losses, 87 draws, LLR 2.95 against ±2.94 bounds. The estimated gain was 34.59 ± 13.10 Elo (95%). All games terminated normally. The bench signature changed from `686260` to `499138`.

The direct tactical move generator was compared with the preceding revision at 8+0.08, Hash 16 MiB, and one thread per engine. The run was stopped after 980 games: 471 wins, 460 losses, 49 draws, LLR 0.08 against ±2.94 bounds on [0, 5] Elo. It was inconclusive, so no strength gain is claimed. The bench signature changed from `499138` to `487865`.

## Development matches

Correctness checks remain required. For search and evaluation changes, run a short paired match after a meaningful batch of work or at a phase boundary. On the development Mac, use at most 40 games at 1+0.01 with two concurrent games and a 20-minute wall-clock cap. Record completed games, wins, losses, draws, time control, and any abnormal termination. Stop at the cap rather than extending the run. A small match is a regression signal, not an Elo measurement or proof that a change is stronger.

The first bounded match after adding the tapered evaluation and selective search, with the short-clock fix, played 20 games against Morstilia 6.0.0 at 1+0.01, Hash 16 MiB, one thread, two concurrent games, and ten openings from the development EPD set with colors swapped: 20 wins, 0 losses, 0 draws, all terminated normally. A MaiEngine batch at 1+0.01 on the preceding build ended with 17 of 20 games lost on time by MaiEngine, which spends about 50 ms per move regardless of its clock, so that control does not measure play against it.

The release matches used revision `9bb9790` at 10+0.1, Hash 16 MiB, one thread, two concurrent games, and the same ten openings with colors swapped. Against Morstilia 6.0.0: 19 wins, 0 losses, 1 draw (19.5 of 20). Against MaiEngine: 18 wins, 1 loss, 1 draw (18.5 of 20). All 40 games ended by checkmate or threefold repetition, with no time losses, crashes, or illegal moves. The 0.1.0 version bump that followed changes only the reported version.

Longer matches and formal SPRTs are optional when a specific strength question warrants them and sufficient compute is available. An undecided SPRT remains inconclusive even if its point estimate is positive.

## Optional SPRT

The runner compares committed revisions with fastchess and a balanced EPD opening set. It builds both revisions in a temporary directory, plays each opening with colors swapped, and writes a PGN to the requested path. It leaves the working tree untouched.

```sh
tools/sprt.sh HEAD HEAD^ /path/to/openings.epd /tmp/inphish-sprt.pgn
```

The default test uses 8+0.08 seconds, one search thread per engine, a 0 to 5 Elo SPRT, and four concurrent games. `SPRT_TC`, `SPRT_ROUNDS`, `SPRT_CONCURRENCY`, `SPRT_ELO0`, `SPRT_ELO1`, and `SPRT_FASTCHESS` override the defaults. Use `SPRT_ELO0=-5 SPRT_ELO1=0` for a non-regression test. Keep the fastchess terminal output and PGN with the result. If the test reaches a decision, record the LLR, bounds, W/L/D counts, time control, and bench signature; otherwise label the result inconclusive.
