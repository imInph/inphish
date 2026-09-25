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

The engine uses iterative deepening, principal variation search, a transposition table, null-move and futility pruning, late move reductions, killer, history and static-exchange move ordering, and quiescence search. Since 3.0.0 the evaluation is Stockfish 13's NNUE network `nn-62ef826d1a6d` (HalfKP 256x2-32-32, GPL-3.0), bundled in the binary and run by inphish's own inference code, which matches Stockfish 13's output exactly. inphish's evaluation is therefore Stockfish's network, not one of its own. Bare endings with less than two rooks' worth of pieces and at most one pawn, where Stockfish 13 also set the network aside, use inphish's hand-written evaluation. UCI options are `Hash` (16 MiB by default), `Clear Hash`, `Threads` (1 by default), `Move Overhead` (20 ms by default), `MultiPV` (1 by default), `UCI_LimitStrength` with `UCI_Elo` (1320 to 2600, approximate), `UCI_ShowWDL`, and `UCI_Chess960`. Standard chess is the default; with `UCI_Chess960` on, the engine accepts X-FEN and Shredder-FEN castling rights and reads and writes castling as the king capturing its own rook.

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

No rating-list Elo has been measured. Version 0.1.0 played two 20-game matches at 10+0.1 with Hash 16 MiB, one thread, and ten openings played with colors swapped: 19 wins, 0 losses, 1 draw against Morstilia 6.0.0, and 18 wins, 1 loss, 1 draw against MaiEngine. These are small informal samples, not a rating. Version 1.0.0 was checked at 1+0.01 on the committed balanced openings with colors swapped: 30 of 40 against 0.1.0 (26 wins, 6 losses, 8 draws) and 19 of 20 against Morstilia 6.0.0 (18 wins, 2 draws). It also won all 20 games against MaiEngine, but 16 of those were MaiEngine losing on time, so that result says little about playing strength. Version 2.0.0 adds features rather than measured strength: against 1.0.0 its search has scored 43 of 100 over three small samples, within noise of an even score. Against Stockfish 19 limited with `UCI_Elo`, 20 games per setting at 1+0.01, it scored 14 of 20 at 2400, 12.5 at 2600 and 6 at 2800, where 1.0.0 scored 11.5, 12 and 3. Version 3.0.0, with the network, scored 34.5 of 40 against 2.0.0 (33 wins, 4 losses, 3 draws), 19 of 20 against Morstilia 6.0.0, and 16, 11 and 5.5 of 20 against Stockfish at 2600, 2800 and 3000. Those `UCI_Elo` numbers are Stockfish's labels calibrated at far longer time controls, not ratings. See [docs/TESTING.md](docs/TESTING.md).

## Limitations

inphish has no opening book, endgame tablebases, or pondering, and no evaluation network of its own. [docs/ROADMAP.md](docs/ROADMAP.md) lists what was and was not built against the original plan.

## Acknowledgements

The Chess Programming Wiki documents most of the techniques used here. The piece-square tables are Ronald Friederich's PeSTO tables as published on the wiki. The evaluation network `nn-62ef826d1a6d` and the HalfKP architecture it uses come from the [Stockfish](https://github.com/official-stockfish/Stockfish) project and its contributors, and are distributed under the GPL-3.0.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
