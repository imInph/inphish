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

The engine uses iterative deepening, principal variation search, a transposition table, null-move and futility pruning, late move reductions, killer, history and static-exchange move ordering, and quiescence search. Since 3.0.0 the evaluation is Stockfish 13's NNUE network `nn-62ef826d1a6d` (HalfKP 256x2-32-32, GPL-3.0), bundled in the binary and run by inphish's own inference code, which matches Stockfish 13's output exactly. inphish's evaluation is therefore Stockfish's network, not one of its own. Bare endings with less than two rooks' worth of pieces and at most one pawn, where Stockfish 13 also set the network aside, use inphish's hand-written evaluation. UCI options are `Hash` (16 MiB by default), `Clear Hash`, `Threads` (1 by default), `Move Overhead` (20 ms by default), `MultiPV` (1 by default), `UCI_LimitStrength` with `UCI_Elo` (1320 to 2600, approximate), `UCI_ShowWDL`, `UCI_Chess960`, and `SyzygyPath`. `SyzygyPath` takes one or more directories of Syzygy endgame tables (separated by `:`, or `;` on Windows); the tables are not included and can be downloaded separately, for example the 3-5 piece set of about 1 GB. Standard chess is the default; with `UCI_Chess960` on, the engine accepts X-FEN and Shredder-FEN castling rights and reads and writes castling as the king capturing its own rook.

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

Correctness tests and their reference data are described in [docs/TESTING.md](docs/TESTING.md). The engine architecture is described in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). The depth-4 bench signature is `79837` nodes. Some earlier search changes were measured by SPRT against preceding revisions; later changes are checked with short bounded matches recorded in the testing notes. Absolute playing strength remains unmeasured.

## Strength

No rating-list Elo has been measured. The table places each release on Stockfish 19's `UCI_Elo` scale: Stockfish was limited to a series of `UCI_Elo` settings, inphish played 20 games against each with the openings' colors swapped, and one Elo per version was fitted to all its results by maximum likelihood (95% ranges). Stockfish calibrates `UCI_Elo` at much longer time controls and its limiter does not scale with time the way a normal engine does, so compare versions within one column only, and treat the numbers as labels on a shared scale rather than ratings. Match scores are wins / losses / draws over 20 games.

| Version | Elo, 1+0.01 | Elo, 10+0.1 | Morstilia 6.0.0 | MaiEngine |
|---|---|---|---|---|
| 0.1.0-preview.2 | ~740 ± 345 (1) | 1712 ± 126 | 3 / 16 / 1 (2) | 2 / 18 / 0 (2) |
| 0.1.0 | 2494 ± 90 | 2777 ± 152 | 19 / 0 / 1 (3) | 18 / 1 / 1 (3) |
| 1.0.0 | 2521 ± 84 | | 18 / 0 / 2 | 20 / 0 / 0 (4) |
| 2.0.0 | 2635 ± 97 | | 20 / 0 / 0 | 20 / 0 / 0 (4) |
| 3.0.0 | 2835 ± 97 | | 18 / 0 / 2 | 20 / 0 / 0 (4) |
| 3.1.0 | about 3.0.0 (5) | | 20 / 0 / 0 | 20 / 0 / 0 (4) |

1. At 1+0.01 the preview's clock handling played 47% of its moves instantly without searching, so this measures that bug rather than the engine.
2. At 10+0.1, on a build of the preview's era rather than the tagged revision.
3. At 10+0.1. At 1+0.01 it scored 19 / 0 / 1 against Morstilia and 20 / 0 / 0 against MaiEngine, 18 of them MaiEngine time forfeits.
4. At 1+0.01 MaiEngine lost many of these on time (16, 17, 15 and 12 games for 1.0.0, 2.0.0, 3.0.0 and 3.1.0), so they say little about playing strength. inphish lost no game on time.
5. Not placed on the ladder separately; it scored 21.5 and 19.5 of 40 against 3.0.0, where few games reach five pieces.

The large steps are the preview to 0.1.0, which added the tapered evaluation and selective search, and 3.0.0, which added the NNUE network. Between 0.1.0 and 2.0.0 the differences are within the ranges. Head-to-head matches between versions exaggerate the gaps: 1.0.0 scored 30 of 40 against 0.1.0, 3.0.0 scored 34.5 of 40 against 2.0.0, and 3.1.0 scored 36 and 38.5 of 40 against 1.0.0 and 2.0.0. See [docs/TESTING.md](docs/TESTING.md) for every match.

## Limitations

inphish has no opening book or pondering and no evaluation network of its own. Tablebase support covers WDL and DTZ tables; on Windows each table file used is read into memory rather than mapped. [docs/ROADMAP.md](docs/ROADMAP.md) lists what was and was not built against the original plan.

## Acknowledgements

The Chess Programming Wiki documents most of the techniques used here. The piece-square tables are Ronald Friederich's PeSTO tables as published on the wiki. The evaluation network `nn-62ef826d1a6d` and the HalfKP architecture it uses come from the [Stockfish](https://github.com/official-stockfish/Stockfish) project and its contributors, and are distributed under the GPL-3.0. The Syzygy probing code is ported from Stockfish 13's `tbprobe`, itself based on Ronald de Man's Syzygy tablebase code.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
