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

The release test suite includes UCI subprocess smoke and edge-case tests (ponder and ponderhit, illegal moves in `position`, `go` before `position`, node and movetime limits, quit during an infinite search) and a depth-4 bench signature check. Run them separately with:

```sh
cargo test --release -p inphish --test uci
cargo test --release -p inphish --test bench
target/release/inphish bench 4
```

The depth-4 signature is `91377` nodes. It must match in debug and release builds and on supported platforms.

The first search SPRT compared PVS with the preceding alpha-beta revision at 8+0.08, one thread per engine, without a hash table. It accepted H1 on [0, 5] Elo after 988 paired-opening games: 549 wins, 364 losses, 75 draws, LLR 2.96 against ±2.94 bounds. The estimated gain was 65.83 ± 18.56 Elo (95%). All 988 games terminated normally. This measures the change, not an absolute rating.

The transposition-table SPRT compared the 16 MiB default table against the preceding hashless revision at 8+0.08 and one thread per engine. It accepted H1 on [0, 5] Elo after 536 paired-opening games: 350 wins, 141 losses, 45 draws, LLR 2.95 against ±2.94 bounds. The estimated gain was 143.04 ± 29.00 Elo (95%). All counted games terminated normally; fastchess also wrote one normal, unpaired game to the PGN while stopping.

The killer-move SPRT compared two quiet killers per ply against the preceding transposition-table revision at 8+0.08, Hash 16 MiB, and one thread per engine. It accepted H1 on [0, 5] Elo after 1,804 paired-opening games: 948 wins, 769 losses, 87 draws, LLR 2.95 against ±2.94 bounds. The estimated gain was 34.59 ± 13.10 Elo (95%). All games terminated normally. The bench signature changed from `686260` to `499138`.

The direct tactical move generator was compared with the preceding revision at 8+0.08, Hash 16 MiB, and one thread per engine. The run was stopped after 980 games: 471 wins, 460 losses, 49 draws, LLR 0.08 against ±2.94 bounds on [0, 5] Elo. It was inconclusive, so no strength gain is claimed. The bench signature changed from `499138` to `487865`.

## Development matches

Correctness checks remain required. For search and evaluation changes, run a short paired match after a meaningful batch of work or at a phase boundary. On the development Mac, use at most 40 games at 1+0.01 with two concurrent games and a 20-minute wall-clock cap. Record completed games, wins, losses, draws, time control, and any abnormal termination. Stop at the cap rather than extending the run. A small match is a regression signal, not an Elo measurement or proof that a change is stronger.

The first bounded match after adding the tapered evaluation and selective search, with the short-clock fix, played 20 games against Morstilia 6.0.0 at 1+0.01, Hash 16 MiB, one thread, two concurrent games, and ten openings from the development EPD set with colors swapped: 20 wins, 0 losses, 0 draws, all terminated normally. A MaiEngine batch at 1+0.01 on the preceding build ended with 17 of 20 games lost on time by MaiEngine, which spends about 50 ms per move regardless of its clock, so that control does not measure play against it.

The release matches used revision `9bb9790` at 10+0.1, Hash 16 MiB, one thread, two concurrent games, and the same ten openings with colors swapped. Against Morstilia 6.0.0: 19 wins, 0 losses, 1 draw (19.5 of 20). Against MaiEngine: 18 wins, 1 loss, 1 draw (18.5 of 20). All 40 games ended by checkmate or threefold repetition, with no time losses, crashes, or illegal moves. The 0.1.0 version bump that followed changes only the reported version.

`tests/openings/balanced.epd` holds 28 common, roughly equal opening positions of eight to ten plies, each labelled with its ECO code and move sequence; a test checks that every line parses. `tools/match.sh` builds a git revision in a temporary directory, freezes the binary, and plays it against a named opponent through fastchess on that book with colors swapped:

```sh
MATCH_FASTCHESS=/path/to/fastchess \
MATCH_OPPONENT_OPTIONS="option.Hash=16 option.Threads=1 option.BookEnabled=false" \
tools/match.sh main Morstilia-6 /path/to/morstilia
```

It defaults to 20 games at 1+0.01 with two concurrent games and always stops at 20 minutes of wall clock. It refuses a slower control, more than 40 games, or more concurrency unless `MATCH_OWNER_APPROVED=yes` records a specific owner approval. The PGN, log, bench line, and frozen binary go to `~/inphish-evidence/`, outside the repository. The script prints the score and a count of game terminations. An earlier revision can be the opponent by passing its frozen binary as the opponent command.

The first batch on this book, revision `f52f88a` (0.1.0) at 1+0.01, scored 19 wins, 0 losses, 1 draw against Morstilia 6.0.0 (19 checkmates, one threefold repetition) and 20 wins against MaiEngine, of which 18 were MaiEngine time forfeits and 2 checkmates. MaiEngine results at 1+0.01 therefore say little about its play.

## Evaluation tuning

`tools/tune` fits the evaluation weights to game results (Texel tuning). It replays fastchess PGNs through the engine's own move generator, skips games not ended normally or by adjudication, keeps every third position after ply 16 that is not in check and has no capture winning material by static exchange, fits the sigmoid scale, and runs full-batch Adam with an L2 pull toward the starting weights. Material stays fixed because it is collinear with the piece-square entries. One tenth of the positions is held out for validation.

```sh
cargo run --release -p inphzugzwang-tune -- --epochs 1000 --prior 1e-8 \
    crates/inphzugzwang-eval/src/weights.rs games/*.pgn
```

The first run used 14,125 older inphish games (89,270 positions). Without a prior the fit collapsed middlegame material and moved piece-square entries by hundreds of centipawns. With a prior of 1e-8 the validation loss fell from 0.1186 to 0.1160, but the fully tuned weights scored 15 of 40 against 0.1.0 in a 1+0.01 batch (13 wins, 23 losses, 4 draws), so they were not adopted. Those games came from weaker revisions on lopsided random openings, which is the likely limit. Only the new threat, hanging-piece, outpost and passed-pawn king-distance terms were enabled, at moderate values guided by that fit; that build scored 21.5 of 40 against 0.1.0 (15 wins, 12 losses, 13 draws, all normal terminations), which is no measurable change either way.

Longer matches and formal SPRTs are optional when a specific strength question warrants them and sufficient compute is available. An undecided SPRT remains inconclusive even if its point estimate is positive.

## Optional SPRT

The runner compares committed revisions with fastchess and a balanced EPD opening set. It builds both revisions in a temporary directory, plays each opening with colors swapped, and writes a PGN to the requested path. It leaves the working tree untouched.

```sh
tools/sprt.sh HEAD HEAD^ /path/to/openings.epd /tmp/inphish-sprt.pgn
```

The default test uses 8+0.08 seconds, one search thread per engine, a 0 to 5 Elo SPRT, and four concurrent games. `SPRT_TC`, `SPRT_ROUNDS`, `SPRT_CONCURRENCY`, `SPRT_ELO0`, `SPRT_ELO1`, and `SPRT_FASTCHESS` override the defaults. Use `SPRT_ELO0=-5 SPRT_ELO1=0` for a non-regression test. Keep the fastchess terminal output and PGN with the result. If the test reaches a decision, record the LLR, bounds, W/L/D counts, time control, and bench signature; otherwise label the result inconclusive.
