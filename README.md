# inphish

inphish is a UCI chess engine written in Rust. It is intended to run inside a chess GUI or tournament manager.

The current foundation build provides legal move generation and diagnostic commands. It does not yet search for a move or play games through UCI.

## Build

Use stable Rust:

```sh
cargo build --release
```

The executable is `target/release/inphish` (`inphish.exe` on Windows). Building for the local CPU can improve speed:

```sh
RUSTFLAGS="-C target-cpu=native" cargo build --release
```

## Foundation commands

```sh
target/release/inphish perft 5
target/release/inphish divide 4
target/release/inphish d
target/release/inphish perft 4 --fen "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1"
```

`perft` counts leaf positions, `divide` prints counts by root move, and `d` prints the board and position state. The command-line FEN must be quoted as one argument.

Correctness tests and their reference data are described in [docs/TESTING.md](docs/TESTING.md). The engine architecture is described in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). Playing strength is unmeasured.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
