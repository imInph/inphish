# Foundation architecture

`inphzugzwang-core` owns the board, move rules, FEN, hashing, and perft traversal. It has no external dependencies or I/O. `inphish` handles command-line input and diagnostic output.

Squares use a1 = 0 through h8 = 63. The board combines piece-type bitboards, color bitboards, and a mailbox. A move stores origin, destination, and flag in 16 bits. Castling destinations in moves are the rook's starting square; the command formatter translates to king destinations for standard UCI notation.

Legal move generation computes checkers and pinned pieces for the side to move. Ordinary non-king moves obey the check mask and pin line. King moves and en passant account for the occupancy after the move. Castling verifies clear paths and king safety with the king and rook removed from their starting squares.

The attack table is initialized once. Rook and bishop lookups use verified magic multipliers on the portable path. On x86-64 with fast BMI2, they use a PEXT-indexed table. Zen 1 and Zen 2 use magics because their PEXT implementation is slow. Knight, king, pawn, between, and line data are precomputed at startup.

Position history is reserved before play and stores full states for exact unmake. Full, pawn, and per-side non-pawn keys use a fixed mixing seed. An en passant square contributes to the full key only when a legal capture exists. Debug builds recompute all keys after every make and unmake.
