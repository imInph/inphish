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

The depth-4 signature is `1445613` nodes. It must match in debug and release builds and on supported platforms. Search strength has not yet been SPRT-tested.
