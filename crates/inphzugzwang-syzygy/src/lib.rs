//! Syzygy endgame tablebase probing (WDL and DTZ), ported from Stockfish 13's
//! `syzygy/tbprobe.cpp` (GPL-3.0), itself based on Ronald de Man's original code. The
//! indexing, decompression and probing logic follow that code step by step so that every
//! probe returns the value Stockfish 13 returns.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use inphzugzwang_core::{king_attacks, CastlingRights, Color, Move, PieceType, Position, Square};

mod file;

use file::TableFile;

/// Probe outcome besides the value, as in Stockfish.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProbeState {
    Fail,
    Ok,
    /// The DTZ table stores the other side to move.
    ChangeStm,
    /// The best move is a capture or pawn move, so DTZ is derived instead of read.
    ZeroingBestMove,
}

pub const WDL_LOSS: i32 = -2;
pub const WDL_BLESSED_LOSS: i32 = -1;
pub const WDL_DRAW: i32 = 0;
pub const WDL_CURSED_WIN: i32 = 1;
pub const WDL_WIN: i32 = 2;

const TB_PIECES: usize = 7;
const FLAG_STM: u8 = 1;
const FLAG_MAPPED: u8 = 2;
const FLAG_WIN_PLIES: u8 = 4;
const FLAG_LOSS_PLIES: u8 = 8;
const FLAG_WIDE: u8 = 16;
const FLAG_SINGLE_VALUE: u8 = 128;
const WDL_MAGIC: [u8; 4] = [0x71, 0xE8, 0x23, 0x5D];
const DTZ_MAGIC: [u8; 4] = [0xD7, 0x66, 0x0C, 0xA5];

/// Index tables shared by every table, built once.
struct Maps {
    pawns: [i32; 64],
    b1h1h7: [i32; 64],
    a1d1d4: [i32; 64],
    kk: [[i32; 64]; 10],
    binomial: [[u64; 64]; 6],
    lead_pawn_idx: [[u64; 64]; 6],
    lead_pawns_size: [[u64; 4]; 6],
}

fn off_a1h8(square: usize) -> i32 {
    (square >> 3) as i32 - (square & 7) as i32
}

fn maps() -> &'static Maps {
    static MAPS: OnceLock<Maps> = OnceLock::new();
    MAPS.get_or_init(|| {
        let mut maps = Maps {
            pawns: [0; 64],
            b1h1h7: [0; 64],
            a1d1d4: [0; 64],
            kk: [[0; 64]; 10],
            binomial: [[0; 64]; 6],
            lead_pawn_idx: [[0; 64]; 6],
            lead_pawns_size: [[0; 4]; 6],
        };
        let mut code = 0;
        for square in 0..64 {
            if off_a1h8(square) < 0 {
                maps.b1h1h7[square] = code;
                code += 1;
            }
        }
        let mut diagonal = Vec::new();
        code = 0;
        for square in 0..=27 {
            if off_a1h8(square) < 0 && square & 7 <= 3 {
                maps.a1d1d4[square] = code;
                code += 1;
            } else if off_a1h8(square) == 0 && square & 7 <= 3 {
                diagonal.push(square);
            }
        }
        for square in diagonal {
            maps.a1d1d4[square] = code;
            code += 1;
        }
        let mut both_on_diagonal = Vec::new();
        code = 0;
        for idx in 0..10 {
            for first in 0..=27 {
                if maps.a1d1d4[first] != idx || (idx == 0 && first != 1) {
                    continue;
                }
                let near = king_attacks(Square::new((first & 7) as u8, (first >> 3) as u8));
                for second in 0..64 {
                    if second == first || near.0 & (1 << second) != 0 {
                        continue;
                    }
                    if off_a1h8(first) == 0 && off_a1h8(second) > 0 {
                        continue;
                    }
                    if off_a1h8(first) == 0 && off_a1h8(second) == 0 {
                        both_on_diagonal.push((idx as usize, second));
                    } else {
                        maps.kk[idx as usize][second] = code;
                        code += 1;
                    }
                }
            }
        }
        for (idx, second) in both_on_diagonal {
            maps.kk[idx][second] = code;
            code += 1;
        }
        maps.binomial[0][0] = 1;
        for n in 1..64 {
            for k in 0..6.min(n + 1) {
                maps.binomial[k][n] = if k > 0 {
                    maps.binomial[k - 1][n - 1]
                } else {
                    0
                } + if k < n { maps.binomial[k][n - 1] } else { 0 };
            }
        }
        let mut available = 47;
        for lead in 1..=5 {
            for file in 0..4 {
                let mut idx = 0;
                for rank in 1..=6 {
                    let square = rank * 8 + file;
                    if lead == 1 {
                        maps.pawns[square] = available;
                        available -= 1;
                        maps.pawns[square ^ 7] = available;
                        available -= 1;
                    }
                    maps.lead_pawn_idx[lead][square] = idx;
                    idx += maps.binomial[lead - 1][maps.pawns[square] as usize];
                }
                maps.lead_pawns_size[lead][file] = idx;
            }
        }
        maps
    })
}

/// Low-level indexing data for one sub-table, as offsets into the mapped file.
#[derive(Clone, Default)]
struct Pairs {
    flags: u8,
    max_sym_len: u8,
    min_sym_len: u8,
    blocks_num: u32,
    sizeof_block: usize,
    span: usize,
    lowest_sym: usize,
    btree: usize,
    block_length: usize,
    block_length_size: u32,
    sparse_index: usize,
    sparse_index_size: usize,
    data: usize,
    base64: Vec<u64>,
    symlen: Vec<u8>,
    pieces: [u8; TB_PIECES],
    group_idx: [u64; TB_PIECES + 1],
    group_len: [usize; TB_PIECES + 1],
    map_idx: [u16; 4],
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Wdl,
    Dtz,
}

/// Material description of one table, known from its file name.
struct Entry {
    name: String,
    piece_count: usize,
    has_pawns: bool,
    has_unique_pieces: bool,
    /// Pawns of the leading colour, then of the other.
    pawn_count: [usize; 2],
    symmetric: bool,
    wdl: OnceLock<Option<Loaded>>,
    dtz: OnceLock<Option<Loaded>>,
}

struct Loaded {
    file: TableFile,
    /// `[side][file]`, sides 0 and 1 for white and black to move.
    items: Vec<Pairs>,
    map: usize,
}

impl Loaded {
    fn get(&self, stm: usize, file: usize, kind: Kind, has_pawns: bool) -> &Pairs {
        let sides = if kind == Kind::Wdl { 2 } else { 1 };
        &self.items[(stm % sides) * 4 + if has_pawns { file } else { 0 }]
    }
}

pub struct Tablebases {
    paths: Vec<PathBuf>,
    tables: HashMap<String, Arc<Entry>>,
    max_pieces: usize,
}

const PIECE_CHARS: [char; 6] = ['P', 'N', 'B', 'R', 'Q', 'K'];

/// Stockfish piece code: 1 to 6 for white pawn to king, 9 to 14 for black.
fn piece_code(color: Color, kind: PieceType) -> u8 {
    (if color == Color::White { 0 } else { 8 }) + kind.index() as u8 + 1
}

fn material(position: &Position, color: Color) -> String {
    let mut text = String::new();
    for kind in [
        PieceType::King,
        PieceType::Queen,
        PieceType::Rook,
        PieceType::Bishop,
        PieceType::Knight,
        PieceType::Pawn,
    ] {
        for _ in 0..position.pieces(color, kind).count() {
            text.push(PIECE_CHARS[kind.index()]);
        }
    }
    text
}

impl Tablebases {
    /// Registers every `.rtbw` table found in the given directories, separated by `:`
    /// (`;` on Windows). Files are opened on first use.
    pub fn open(paths: &str) -> Self {
        let separator = if cfg!(windows) { ';' } else { ':' };
        let paths: Vec<PathBuf> = paths
            .split(separator)
            .filter(|path| !path.is_empty() && *path != "<empty>")
            .map(PathBuf::from)
            .collect();
        let mut tables = HashMap::new();
        let mut max_pieces = 0;
        for directory in &paths {
            let Ok(listing) = std::fs::read_dir(directory) else {
                continue;
            };
            for item in listing.flatten() {
                let name = item.file_name().to_string_lossy().into_owned();
                let Some(stem) = name.strip_suffix(".rtbw") else {
                    continue;
                };
                if let Some(entry) = Entry::from_name(stem) {
                    max_pieces = max_pieces.max(entry.piece_count);
                    tables.entry(stem.to_owned()).or_insert(Arc::new(entry));
                }
            }
        }
        Self {
            paths,
            tables,
            max_pieces,
        }
    }

    /// Number of tables found.
    pub fn len(&self) -> usize {
        self.tables.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tables.is_empty()
    }

    /// Largest number of pieces, kings included, that the tables cover.
    pub fn max_pieces(&self) -> usize {
        self.max_pieces
    }

    /// WDL value for the side to move, from -2 (loss) to 2 (win), with the probe state.
    pub fn probe_wdl(&self, position: &mut Position) -> (i32, ProbeState) {
        let mut state = ProbeState::Ok;
        let value = self.search(position, &mut state, false);
        (value, state)
    }

    /// Distance to zeroing in plies for the side to move, signed like the WDL value and
    /// offset by 100 for results the fifty-move rule turns into draws.
    pub fn probe_dtz(&self, position: &mut Position) -> (i32, ProbeState) {
        let mut state = ProbeState::Ok;
        let value = self.dtz(position, &mut state);
        (value, state)
    }

    fn dtz(&self, position: &mut Position, state: &mut ProbeState) -> i32 {
        *state = ProbeState::Ok;
        let wdl = self.search(position, state, true);
        if *state == ProbeState::Fail || wdl == WDL_DRAW {
            return 0;
        }
        if *state == ProbeState::ZeroingBestMove {
            return dtz_before_zeroing(wdl);
        }
        let dtz = self.probe_table(position, Kind::Dtz, state, wdl);
        if *state == ProbeState::Fail {
            return 0;
        }
        if *state != ProbeState::ChangeStm {
            let cursed = wdl == WDL_BLESSED_LOSS || wdl == WDL_CURSED_WIN;
            return (dtz + if cursed { 100 } else { 0 }) * wdl.signum();
        }
        // The table stores the other side to move: search one ply for the move that
        // keeps the result with the smallest distance.
        let mut min_dtz = 0xFFFF;
        for mv in position.legal_moves().iter() {
            let zeroing = is_capture(mv) || is_pawn_move(position, mv);
            position.make(mv);
            let mut dtz = if zeroing {
                -dtz_before_zeroing(self.search(position, state, false))
            } else {
                -self.dtz(position, state)
            };
            if dtz == 1 && position.checkers().0 != 0 && position.legal_moves().is_empty() {
                min_dtz = 1;
            }
            if !zeroing {
                dtz += dtz.signum();
            }
            if dtz < min_dtz && dtz.signum() == wdl.signum() {
                min_dtz = dtz;
            }
            position.unmake();
            if *state == ProbeState::Fail {
                return 0;
            }
        }
        if min_dtz == 0xFFFF {
            -1
        } else {
            min_dtz
        }
    }

    /// Resolves captures (and, for DTZ, pawn moves) first, because tables store "don't
    /// care" values for positions a capture decides, then probes the table itself.
    fn search(&self, position: &mut Position, state: &mut ProbeState, zeroing: bool) -> i32 {
        let mut best = WDL_LOSS;
        let moves = position.legal_moves();
        let mut count = 0;
        for mv in moves.iter() {
            if !is_capture(mv) && (!zeroing || !is_pawn_move(position, mv)) {
                continue;
            }
            count += 1;
            position.make(mv);
            let value = -self.search(position, state, false);
            position.unmake();
            if *state == ProbeState::Fail {
                return WDL_DRAW;
            }
            if value > best {
                best = value;
                if value >= WDL_WIN {
                    *state = ProbeState::ZeroingBestMove;
                    return value;
                }
            }
        }
        let no_more_moves = count > 0 && count == moves.len();
        let value = if no_more_moves {
            best
        } else {
            let value = self.probe_table(position, Kind::Wdl, state, WDL_DRAW);
            if *state == ProbeState::Fail {
                return WDL_DRAW;
            }
            value
        };
        if best >= value {
            *state = if best > WDL_DRAW || no_more_moves {
                ProbeState::ZeroingBestMove
            } else {
                ProbeState::Ok
            };
            return best;
        }
        *state = ProbeState::Ok;
        value
    }

    fn lookup(&self, position: &Position) -> Option<(&Arc<Entry>, bool)> {
        let white = material(position, Color::White);
        let black = material(position, Color::Black);
        if let Some(entry) = self.tables.get(&format!("{white}v{black}")) {
            return Some((entry, false));
        }
        self.tables
            .get(&format!("{black}v{white}"))
            .map(|entry| (entry, true))
    }

    fn probe_table(
        &self,
        position: &Position,
        kind: Kind,
        state: &mut ProbeState,
        wdl: i32,
    ) -> i32 {
        if position.occupied().count() == 2 {
            return WDL_DRAW;
        }
        let Some((entry, black_stronger)) = self.lookup(position) else {
            *state = ProbeState::Fail;
            return 0;
        };
        let Some(loaded) = entry.load(kind, &self.paths) else {
            *state = ProbeState::Fail;
            return 0;
        };
        probe_loaded(position, entry, loaded, kind, black_stronger, state, wdl)
    }
}

fn is_capture(mv: Move) -> bool {
    mv.flag() & 4 != 0
}

fn is_pawn_move(position: &Position, mv: Move) -> bool {
    position
        .piece_at(mv.from())
        .is_some_and(|piece| piece.kind == PieceType::Pawn)
}

/// DTZ of the move before a capture or pawn move, from the WDL value after it.
pub fn dtz_before_zeroing(wdl: i32) -> i32 {
    match wdl {
        WDL_WIN => 1,
        WDL_CURSED_WIN => 101,
        WDL_BLESSED_LOSS => -101,
        WDL_LOSS => -1,
        _ => 0,
    }
}

/// Whether probing applies: few enough pieces and no castling rights.
pub fn probeable(position: &Position, max_pieces: usize) -> bool {
    position.occupied().count() as usize <= max_pieces
        && position.castling_rights() == CastlingRights::NONE
}

impl Entry {
    fn from_name(name: &str) -> Option<Self> {
        let (strong, weak) = name.split_once('v')?;
        if !strong.starts_with('K') || !weak.starts_with('K') {
            return None;
        }
        let count = |side: &str, piece: char| side.chars().filter(|&c| c == piece).count();
        if strong
            .chars()
            .chain(weak.chars())
            .any(|c| !PIECE_CHARS.contains(&c))
            || count(strong, 'K') != 1
            || count(weak, 'K') != 1
        {
            return None;
        }
        let piece_count = strong.len() + weak.len();
        if piece_count > TB_PIECES {
            return None;
        }
        let white_pawns = count(strong, 'P');
        let black_pawns = count(weak, 'P');
        let has_unique_pieces = [strong, weak].iter().any(|side| {
            PIECE_CHARS[..5]
                .iter()
                .any(|&piece| count(side, piece) == 1)
        });
        // The leading colour is the side with fewer pawns, as that compresses better.
        let white_leads = black_pawns == 0 || (white_pawns > 0 && black_pawns >= white_pawns);
        let pawn_count = if white_leads {
            [white_pawns, black_pawns]
        } else {
            [black_pawns, white_pawns]
        };
        Some(Self {
            name: name.to_owned(),
            piece_count,
            has_pawns: white_pawns + black_pawns > 0,
            has_unique_pieces,
            pawn_count,
            symmetric: strong == weak,
            wdl: OnceLock::new(),
            dtz: OnceLock::new(),
        })
    }

    fn load(&self, kind: Kind, paths: &[PathBuf]) -> Option<&Loaded> {
        let cell = if kind == Kind::Wdl {
            &self.wdl
        } else {
            &self.dtz
        };
        cell.get_or_init(|| {
            let extension = if kind == Kind::Wdl { "rtbw" } else { "rtbz" };
            let file_name = format!("{}.{extension}", self.name);
            let file = paths
                .iter()
                .find_map(|directory| TableFile::open(&directory.join(&file_name)))?;
            let magic = if kind == Kind::Wdl {
                WDL_MAGIC
            } else {
                DTZ_MAGIC
            };
            if file.bytes().len() % 64 != 16 || file.bytes()[..4] != magic {
                return None;
            }
            self.parse(file, kind)
        })
        .as_ref()
    }

    /// Reads the header of a mapped file into its sub-tables, following Stockfish's
    /// `set()`. Offsets count from the start of the file, whose mapping is page aligned,
    /// so the alignment steps match those on the original pointers.
    fn parse(&self, file: TableFile, kind: Kind) -> Option<Loaded> {
        let bytes = file.bytes();
        let mut at = 4;
        let header = *bytes.get(at)?;
        if (header & 2 != 0) != self.has_pawns || (header & 1 != 0) != !self.symmetric {
            return None;
        }
        at += 1;
        let sides = if kind == Kind::Wdl && !self.symmetric {
            2
        } else {
            1
        };
        let max_file = if self.has_pawns { 3 } else { 0 };
        let pp = self.has_pawns && self.pawn_count[1] > 0;
        let mut items = vec![Pairs::default(); 8];
        let slot = |side: usize, file: usize| side * 4 + file;
        for f in 0..=max_file {
            let first = *bytes.get(at)?;
            let second = if pp { *bytes.get(at + 1)? } else { 0xFF };
            let order = [
                [first & 0xF, if pp { second & 0xF } else { 0xF }],
                [first >> 4, if pp { second >> 4 } else { 0xF }],
            ];
            at += 1 + usize::from(pp);
            for k in 0..self.piece_count {
                let byte = *bytes.get(at)?;
                for side in 0..sides {
                    items[slot(side, f)].pieces[k] = if side == 1 { byte >> 4 } else { byte & 0xF };
                }
                at += 1;
            }
            for side in 0..sides {
                self.set_groups(&mut items[slot(side, f)], order[side], f);
            }
        }
        at += at & 1;
        for f in 0..=max_file {
            for side in 0..sides {
                at = set_sizes(&mut items[slot(side, f)], bytes, at)?;
            }
        }
        let mut map = 0;
        if kind == Kind::Dtz {
            map = at;
            for f in 0..=max_file {
                let flags = items[slot(0, f)].flags;
                if flags & FLAG_MAPPED == 0 {
                    continue;
                }
                if flags & FLAG_WIDE != 0 {
                    at += at & 1;
                    for i in 0..4 {
                        items[slot(0, f)].map_idx[i] = ((at - map) / 2 + 1) as u16;
                        at += 2 * usize::from(read_u16_le(bytes, at)?) + 2;
                    }
                } else {
                    for i in 0..4 {
                        items[slot(0, f)].map_idx[i] = (at - map + 1) as u16;
                        at += usize::from(*bytes.get(at)?) + 1;
                    }
                }
            }
            at += at & 1;
        }
        for f in 0..=max_file {
            for side in 0..sides {
                let pairs = &mut items[slot(side, f)];
                pairs.sparse_index = at;
                at += pairs.sparse_index_size * 6;
            }
        }
        for f in 0..=max_file {
            for side in 0..sides {
                let pairs = &mut items[slot(side, f)];
                pairs.block_length = at;
                at += pairs.block_length_size as usize * 2;
            }
        }
        for f in 0..=max_file {
            for side in 0..sides {
                at = (at + 0x3F) & !0x3F;
                let pairs = &mut items[slot(side, f)];
                pairs.data = at;
                at += pairs.blocks_num as usize * pairs.sizeof_block;
            }
        }
        if at > bytes.len() {
            return None;
        }
        Some(Loaded { file, items, map })
    }

    /// Groups pieces encoded together and the multiplier of each group, following
    /// Stockfish's `set_groups()`.
    fn set_groups(&self, pairs: &mut Pairs, order: [u8; 2], f: usize) {
        let maps = maps();
        let mut n = 0;
        let mut first_len: i32 = if self.has_pawns {
            0
        } else if self.has_unique_pieces {
            3
        } else {
            2
        };
        pairs.group_len[n] = 1;
        for i in 1..self.piece_count {
            first_len -= 1;
            if first_len > 0 || pairs.pieces[i] == pairs.pieces[i - 1] {
                pairs.group_len[n] += 1;
            } else {
                n += 1;
                pairs.group_len[n] = 1;
            }
        }
        n += 1;
        pairs.group_len[n] = 0;
        let pp = self.has_pawns && self.pawn_count[1] > 0;
        let mut next = if pp { 2 } else { 1 };
        let mut free_squares = 64 - pairs.group_len[0] - if pp { pairs.group_len[1] } else { 0 };
        let mut idx: u64 = 1;
        let mut k = 0;
        while next < n || k == usize::from(order[0]) || k == usize::from(order[1]) {
            if k == usize::from(order[0]) {
                pairs.group_idx[0] = idx;
                idx *= if self.has_pawns {
                    maps.lead_pawns_size[pairs.group_len[0]][f]
                } else if self.has_unique_pieces {
                    31332
                } else {
                    462
                };
            } else if k == usize::from(order[1]) {
                pairs.group_idx[1] = idx;
                idx *= maps.binomial[pairs.group_len[1]][48 - pairs.group_len[0]];
            } else {
                pairs.group_idx[next] = idx;
                idx *= maps.binomial[pairs.group_len[next]][free_squares];
                free_squares -= pairs.group_len[next];
                next += 1;
            }
            k += 1;
        }
        pairs.group_idx[n] = idx;
    }
}

fn read_u16_le(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn read_u32_le(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn read_u32_be(bytes: &[u8], at: usize) -> u32 {
    bytes.get(at..at + 4).map_or(0, |word| {
        u32::from_be_bytes(word.try_into().expect("four bytes"))
    })
}

fn read_u64_be(bytes: &[u8], at: usize) -> u64 {
    bytes.get(at..at + 8).map_or(0, |word| {
        u64::from_be_bytes(word.try_into().expect("eight bytes"))
    })
}

/// Reads one sub-table's Huffman and block parameters, following Stockfish's
/// `set_sizes()`, and returns the offset after them.
fn set_sizes(pairs: &mut Pairs, bytes: &[u8], mut at: usize) -> Option<usize> {
    pairs.flags = *bytes.get(at)?;
    at += 1;
    if pairs.flags & FLAG_SINGLE_VALUE != 0 {
        pairs.blocks_num = 0;
        pairs.block_length_size = 0;
        pairs.span = 0;
        pairs.sparse_index_size = 0;
        pairs.min_sym_len = *bytes.get(at)?;
        return Some(at + 1);
    }
    let zero = pairs.group_len.iter().position(|&len| len == 0)?;
    let tb_size = pairs.group_idx[zero];
    pairs.sizeof_block = 1 << *bytes.get(at)?;
    pairs.span = 1 << *bytes.get(at + 1)?;
    at += 2;
    pairs.sparse_index_size = tb_size.div_ceil(pairs.span as u64) as usize;
    let padding = *bytes.get(at)?;
    at += 1;
    pairs.blocks_num = read_u32_le(bytes, at)?;
    at += 4;
    pairs.block_length_size = pairs.blocks_num + u32::from(padding);
    pairs.max_sym_len = *bytes.get(at)?;
    pairs.min_sym_len = *bytes.get(at + 1)?;
    at += 2;
    pairs.lowest_sym = at;
    let lengths = usize::from(pairs.max_sym_len.checked_sub(pairs.min_sym_len)?) + 1;
    pairs.base64 = vec![0; lengths];
    for i in (0..lengths - 1).rev() {
        let lower = u64::from(read_u16_le(bytes, pairs.lowest_sym + 2 * i)?);
        let upper = u64::from(read_u16_le(bytes, pairs.lowest_sym + 2 * (i + 1))?);
        pairs.base64[i] = pairs.base64[i + 1].wrapping_add(lower).wrapping_sub(upper) / 2;
    }
    for i in 0..lengths {
        let shift = 64 - i as u32 - u32::from(pairs.min_sym_len);
        pairs.base64[i] = pairs.base64[i].checked_shl(shift).unwrap_or(0);
    }
    at += lengths * 2;
    let symbols = usize::from(read_u16_le(bytes, at)?);
    at += 2;
    pairs.btree = at;
    if pairs.btree + symbols * 3 > bytes.len() {
        return None;
    }
    pairs.symlen = vec![0; symbols];
    let mut visited = vec![false; symbols];
    for symbol in 0..symbols {
        if !visited[symbol] {
            pairs.symlen[symbol] = set_symlen(pairs, bytes, symbol, &mut visited);
        }
    }
    Some(at + symbols * 3 + (symbols & 1))
}

fn left_symbol(bytes: &[u8], btree: usize, symbol: usize) -> usize {
    let at = btree + 3 * symbol;
    (usize::from(bytes[at + 1] & 0xF) << 8) | usize::from(bytes[at])
}

fn right_symbol(bytes: &[u8], btree: usize, symbol: usize) -> usize {
    let at = btree + 3 * symbol;
    (usize::from(bytes[at + 2]) << 4) | usize::from(bytes[at + 1] >> 4)
}

/// Number of values, minus one, that a symbol expands to under recursive pairing.
fn set_symlen(pairs: &mut Pairs, bytes: &[u8], symbol: usize, visited: &mut [bool]) -> u8 {
    visited[symbol] = true;
    let right = right_symbol(bytes, pairs.btree, symbol);
    if right == 0xFFF {
        return 0;
    }
    let left = left_symbol(bytes, pairs.btree, symbol);
    if left >= visited.len() || right >= visited.len() {
        return 0;
    }
    if !visited[left] {
        pairs.symlen[left] = set_symlen(pairs, bytes, left, visited);
    }
    if !visited[right] {
        pairs.symlen[right] = set_symlen(pairs, bytes, right, visited);
    }
    pairs.symlen[left]
        .wrapping_add(pairs.symlen[right])
        .wrapping_add(1)
}

/// Finds the value stored at `idx`: locate its block through the sparse index, walk the
/// canonical Huffman symbols to the one covering it, then expand that symbol's pair tree.
fn decompress_pairs(pairs: &Pairs, bytes: &[u8], idx: u64) -> i32 {
    if pairs.flags & FLAG_SINGLE_VALUE != 0 {
        return i32::from(pairs.min_sym_len);
    }
    let span = pairs.span as u64;
    let k = (idx / span) as usize;
    let entry = pairs.sparse_index + 6 * k;
    let mut block = read_u32_le(bytes, entry).unwrap_or(0) as usize;
    let mut offset = i64::from(read_u16_le(bytes, entry + 4).unwrap_or(0));
    offset += (idx % span) as i64 - (span / 2) as i64;
    let block_length =
        |block: usize| i64::from(read_u16_le(bytes, pairs.block_length + 2 * block).unwrap_or(0));
    while offset < 0 {
        block -= 1;
        offset += block_length(block) + 1;
    }
    while offset > block_length(block) {
        offset -= block_length(block) + 1;
        block += 1;
    }
    let mut at = pairs.data + block * pairs.sizeof_block;
    let mut buffer = read_u64_be(bytes, at);
    at += 8;
    let mut buffer_size = 64;
    let min_sym_len = usize::from(pairs.min_sym_len);
    let mut symbol;
    loop {
        let mut len = 0;
        while buffer < pairs.base64[len] {
            len += 1;
        }
        symbol = ((buffer - pairs.base64[len]) >> (64 - len - min_sym_len)) as usize;
        symbol += usize::from(read_u16_le(bytes, pairs.lowest_sym + 2 * len).unwrap_or(0));
        let covered = i64::from(pairs.symlen[symbol]) + 1;
        if offset < covered {
            break;
        }
        offset -= covered;
        len += min_sym_len;
        buffer <<= len;
        buffer_size -= len;
        if buffer_size <= 32 {
            buffer_size += 32;
            buffer |= u64::from(read_u32_be(bytes, at)) << (64 - buffer_size);
            at += 4;
        }
    }
    while pairs.symlen[symbol] != 0 {
        let left = left_symbol(bytes, pairs.btree, symbol);
        let covered = i64::from(pairs.symlen[left]) + 1;
        if offset < covered {
            symbol = left;
        } else {
            offset -= covered;
            symbol = right_symbol(bytes, pairs.btree, symbol);
        }
    }
    left_symbol(bytes, pairs.btree, symbol) as i32
}

/// Maps the position to its table index and reads the value, following Stockfish's
/// `do_probe_table()`.
fn probe_loaded(
    position: &Position,
    entry: &Entry,
    loaded: &Loaded,
    kind: Kind,
    black_stronger: bool,
    state: &mut ProbeState,
    wdl: i32,
) -> i32 {
    let maps = maps();
    let bytes = loaded.file.bytes();
    let black_to_move = position.side_to_move() == Color::Black;
    let symmetric_black_to_move = entry.symmetric && black_to_move;
    let flip = symmetric_black_to_move || black_stronger;
    let flip_color = if flip { 8 } else { 0 };
    let flip_squares = if flip { 56 } else { 0 };
    let stm = usize::from(flip) ^ usize::from(black_to_move);

    let mut squares = [0_usize; TB_PIECES];
    let mut pieces = [0_u8; TB_PIECES];
    let mut size = 0;
    let mut lead_pawns_count = 0;
    let mut lead_pawns = 0_u64;
    let mut tb_file = 0;
    if entry.has_pawns {
        let pawn = loaded.get(0, 0, kind, true).pieces[0] ^ flip_color;
        let color = if pawn & 8 == 0 {
            Color::White
        } else {
            Color::Black
        };
        let pawns = position.pieces(color, PieceType::Pawn);
        lead_pawns = pawns.0;
        for square in pawns {
            squares[size] = square.index() ^ flip_squares;
            size += 1;
        }
        lead_pawns_count = size;
        let mut best = 0;
        for i in 1..lead_pawns_count {
            if maps.pawns[squares[i]] > maps.pawns[squares[best]] {
                best = i;
            }
        }
        squares.swap(0, best);
        let file = squares[0] & 7;
        tb_file = file.min(7 - file);
    }
    if kind == Kind::Dtz {
        let flags = loaded.get(stm, tb_file, kind, entry.has_pawns).flags;
        let matches = usize::from(flags & FLAG_STM) == stm;
        if !matches && !(entry.symmetric && !entry.has_pawns) {
            *state = ProbeState::ChangeStm;
            return 0;
        }
    }
    let rest = position.occupied().0 ^ lead_pawns;
    let mut remaining = rest;
    while remaining != 0 {
        let index = remaining.trailing_zeros() as usize;
        remaining &= remaining - 1;
        let piece = position
            .piece_at(Square::new((index & 7) as u8, (index >> 3) as u8))
            .expect("occupied square");
        squares[size] = index ^ flip_squares;
        pieces[size] = piece_code(piece.color, piece.kind) ^ flip_color;
        size += 1;
    }
    let pairs = loaded.get(stm, tb_file, kind, entry.has_pawns);
    for i in lead_pawns_count..size.saturating_sub(1) {
        for j in i + 1..size {
            if pairs.pieces[i] == pieces[j] {
                pieces.swap(i, j);
                squares.swap(i, j);
                break;
            }
        }
    }
    if squares[0] & 7 > 3 {
        for square in &mut squares[..size] {
            *square ^= 7;
        }
    }
    let mut idx: u64;
    if entry.has_pawns {
        idx = maps.lead_pawn_idx[lead_pawns_count][squares[0]];
        squares[1..lead_pawns_count].sort_by_key(|&square| maps.pawns[square]);
        for (i, &square) in squares.iter().enumerate().take(lead_pawns_count).skip(1) {
            idx += maps.binomial[i][maps.pawns[square] as usize];
        }
    } else {
        if squares[0] >> 3 > 3 {
            for square in &mut squares[..size] {
                *square ^= 56;
            }
        }
        for i in 0..pairs.group_len[0] {
            let off = off_a1h8(squares[i]);
            if off == 0 {
                continue;
            }
            if off > 0 {
                for square in &mut squares[i..size] {
                    *square = ((*square >> 3) | (*square << 3)) & 63;
                }
            }
            break;
        }
        if entry.has_unique_pieces {
            let adjust1 = usize::from(squares[1] > squares[0]);
            let adjust2 =
                usize::from(squares[2] > squares[0]) + usize::from(squares[2] > squares[1]);
            let rank = |square: usize| (square >> 3) as u64;
            idx = if off_a1h8(squares[0]) != 0 {
                (maps.a1d1d4[squares[0]] as u64 * 63 + (squares[1] - adjust1) as u64) * 62
                    + (squares[2] - adjust2) as u64
            } else if off_a1h8(squares[1]) != 0 {
                (6 * 63 + rank(squares[0]) * 28 + maps.b1h1h7[squares[1]] as u64) * 62
                    + (squares[2] - adjust2) as u64
            } else if off_a1h8(squares[2]) != 0 {
                6 * 63 * 62
                    + 4 * 28 * 62
                    + rank(squares[0]) * 7 * 28
                    + (rank(squares[1]) - adjust1 as u64) * 28
                    + maps.b1h1h7[squares[2]] as u64
            } else {
                6 * 63 * 62
                    + 4 * 28 * 62
                    + 4 * 7 * 28
                    + rank(squares[0]) * 7 * 6
                    + (rank(squares[1]) - adjust1 as u64) * 6
                    + (rank(squares[2]) - adjust2 as u64)
            };
        } else {
            idx = maps.kk[maps.a1d1d4[squares[0]] as usize][squares[1]] as u64;
        }
    }
    idx *= pairs.group_idx[0];
    let mut group_start = pairs.group_len[0];
    let mut remaining_pawns = entry.has_pawns && entry.pawn_count[1] > 0;
    let mut next = 1;
    while pairs.group_len[next] != 0 {
        let len = pairs.group_len[next];
        squares[group_start..group_start + len].sort_unstable();
        let mut n = 0;
        for i in 0..len {
            let square = squares[group_start + i];
            let adjust = squares[..group_start]
                .iter()
                .filter(|&&earlier| square > earlier)
                .count();
            n += maps.binomial[i + 1][square - adjust - if remaining_pawns { 8 } else { 0 }];
        }
        remaining_pawns = false;
        idx += n * pairs.group_idx[next];
        group_start += len;
        next += 1;
    }
    let value = decompress_pairs(pairs, bytes, idx);
    if kind == Kind::Wdl {
        return value - 2;
    }
    map_dtz(loaded, bytes, tb_file, entry.has_pawns, value, wdl)
}

/// Undoes the frequency remapping of DTZ values and converts moves to plies, following
/// Stockfish's `map_score()`.
fn map_dtz(
    loaded: &Loaded,
    bytes: &[u8],
    file: usize,
    has_pawns: bool,
    value: i32,
    wdl: i32,
) -> i32 {
    const WDL_MAP: [usize; 5] = [1, 3, 0, 2, 0];
    let pairs = loaded.get(0, file, Kind::Dtz, has_pawns);
    let flags = pairs.flags;
    let mut value = value;
    if flags & FLAG_MAPPED != 0 {
        let index = usize::from(pairs.map_idx[WDL_MAP[(wdl + 2) as usize]]) + value as usize;
        value = if flags & FLAG_WIDE != 0 {
            i32::from(read_u16_le(bytes, loaded.map + 2 * index).unwrap_or(0))
        } else {
            i32::from(bytes.get(loaded.map + index).copied().unwrap_or(0))
        };
    }
    if (wdl == WDL_WIN && flags & FLAG_WIN_PLIES == 0)
        || (wdl == WDL_LOSS && flags & FLAG_LOSS_PLIES == 0)
        || wdl == WDL_CURSED_WIN
        || wdl == WDL_BLESSED_LOSS
    {
        value *= 2;
    }
    value + 1
}
