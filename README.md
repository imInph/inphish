# inphish

inphish is a UCI chess engine written in Rust. It is intended to run inside a chess GUI or tournament manager.

Run the executable with no arguments to speak UCI over standard input and output. Add that executable as a UCI engine in a chess GUI; inphish has no graphical interface of its own.

## Releases

Download the archive for your system from [Releases](https://github.com/imInph/inphish/releases), extract it, and select `inphish` (`inphish.exe` on Windows) as the UCI executable in your chess GUI. Release archives are built for Linux x86-64, Windows x86-64, macOS Apple Silicon, and macOS Intel. No book, network file, or runtime download is required. The current public version is a preview; absolute playing strength is unmeasured.

## Build

Use stable Rust:

```sh
cargo build --release
```

The executable is `target/release/inphish` (`inphish.exe` on Windows). Building for the local CPU can improve speed:

```sh
RUSTFLAGS="-C target-cpu=native" cargo build --release
```

The current playing engine uses iterative deepening, principal variation search, a transposition table, killer move ordering, capture/evasion quiescence, and material plus piece-square evaluation. UCI options are `Hash` (16 MiB by default), `Clear Hash`, and `Move Overhead` (20 ms by default). Standard chess is the UCI default. Chess960 board rules and perft are implemented, but the full Chess960 UCI game option is not yet available.

## Diagnostic commands

```sh
target/release/inphish perft 5
target/release/inphish divide 4
target/release/inphish d
target/release/inphish eval
target/release/inphish bench
target/release/inphish perft 4 --fen "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1"
```

`perft` counts leaf positions, `divide` prints counts by root move, `d` prints the board and position state, and `bench` searches 50 fixed positions at depth 4. The command-line FEN must be quoted as one argument.

Correctness tests and their reference data are described in [docs/TESTING.md](docs/TESTING.md). The engine architecture is described in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). The depth-4 bench signature is `487865` nodes. Some search changes have been measured by SPRT against preceding revisions; the most recent tactical-generation match was inconclusive. Absolute playing strength remains unmeasured.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
