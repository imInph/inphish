# inphish

inphish is a UCI chess engine written in Rust. It is intended to run inside a chess GUI or tournament manager.

Run the executable with no arguments to speak UCI over standard input and output. Add that executable as a UCI engine in a chess GUI; inphish has no graphical interface of its own.

## Releases

Download the archive for your system from [Releases](https://github.com/imInph/inphish/releases), extract it, and select `inphish` (`inphish.exe` on Windows) as the UCI executable in your chess GUI. Release archives are built for Linux x86-64, Windows x86-64, macOS Apple Silicon, and macOS Intel. No book, network file, or runtime download is required. Pick `macos-aarch64` for Apple Silicon Macs and `macos-x86_64` for Intel Macs. On macOS, a downloaded binary may need `xattr -d com.apple.quarantine inphish` before a GUI can start it.

## Build

Use stable Rust:

```sh
cargo build --release
```

The executable is `target/release/inphish` (`inphish.exe` on Windows). Building for the local CPU can improve speed:

```sh
RUSTFLAGS="-C target-cpu=native" cargo build --release
```

The engine uses iterative deepening, principal variation search, a transposition table, null-move and futility pruning, late move reductions, killer, history and static-exchange move ordering, and quiescence search. The evaluation is a tapered hand-written function with PeSTO piece-square tables, mobility, pawn structure, passed pawns, and king safety terms. UCI options are `Hash` (16 MiB by default), `Clear Hash`, `Move Overhead` (20 ms by default), and `UCI_Chess960`. Standard chess is the default; with `UCI_Chess960` on, the engine accepts X-FEN and Shredder-FEN castling rights and reads and writes castling as the king capturing its own rook.

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

Correctness tests and their reference data are described in [docs/TESTING.md](docs/TESTING.md). The engine architecture is described in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). The depth-4 bench signature is `86020` nodes. Some earlier search changes were measured by SPRT against preceding revisions; later changes are checked with short bounded matches recorded in the testing notes. Absolute playing strength remains unmeasured.

## Strength

No rating-list Elo has been measured. Version 0.1.0 played two 20-game matches at 10+0.1 with Hash 16 MiB, one thread, and ten openings played with colors swapped: 19 wins, 0 losses, 1 draw against Morstilia 6.0.0, and 18 wins, 1 loss, 1 draw against MaiEngine. These are small informal samples, not a rating. Version 1.0.0 was checked at 1+0.01 on the committed balanced openings with colors swapped: 30 of 40 against 0.1.0 (26 wins, 6 losses, 8 draws) and 19 of 20 against Morstilia 6.0.0 (18 wins, 2 draws). It also won all 20 games against MaiEngine, but 16 of those were MaiEngine losing on time, so that result says little about playing strength. See [docs/TESTING.md](docs/TESTING.md).

## Limitations

inphish is single-threaded and has no opening book, endgame tablebases, NNUE, pondering, MultiPV, or strength limiting. [docs/ROADMAP.md](docs/ROADMAP.md) lists what was and was not built against the original plan.

## Acknowledgements

The Chess Programming Wiki documents most of the techniques used here. The piece-square tables are Ronald Friederich's PeSTO tables as published on the wiki.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
