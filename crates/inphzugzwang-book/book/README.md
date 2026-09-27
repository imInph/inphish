# inphish.bin

inphish's opening book, in the Polyglot format: 6,378,170 entries, 102,050,720 bytes.

## Source

Lichess standard rated games of August 2026, `lichess_db_standard_rated_2026-08.pgn.zst` from https://database.lichess.org (SHA-256 `6bf6fa8a5dee7bb81d1874ac312160060daf12f18a29dc2740a3bf6f5e5e6248`), released under the Creative Commons CC0 licence. Of its 91,741,946 games, 5,928,101 passed the filters below; none had a move the builder could not read.

## Filters

- Both players rated at least 2000.
- A base time of at least 180 seconds.
- Ended normally or on time (Lichess `Termination` of `Normal` or `Time forfeit`), with a result.
- Only the first 40 plies of each game.

A move is kept for a position when it was played there in at least 2 games and in at least 5% of the games that reached the position, and, unless it scored best there, when it scored at least 40% for the side playing it. Its weight is two points per win and one per draw for that side, scaled to fit 16 bits.

## Rebuilding

```
zstd -dc lichess_db_standard_rated_2026-08.pgn.zst | cargo run --release -p inphzugzwang-book-builder -- collect runs 2000
cargo run --release -p inphzugzwang-book-builder -- merge runs totals.bin
cargo run --release -p inphzugzwang-book-builder -- write totals.bin inphish.bin 2
```

`sizes totals.bin` prints the entry counts other minimum game counts would give.
