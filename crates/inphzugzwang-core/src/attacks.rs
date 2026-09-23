use std::sync::OnceLock;

use crate::{Bitboard, Color, Square};

const ROOK_DIRS: [(i8, i8); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
const BISHOP_DIRS: [(i8, i8); 4] = [(1, 1), (1, -1), (-1, 1), (-1, -1)];
const ROOK_MAGICS: [u64; 64] = [
    0x0080008010204002,
    0x224000a000100340,
    0x2100110020000844,
    0x8100041000220900,
    0x2680228800040080,
    0x8080040002008001,
    0x0200210a0008009c,
    0x4480005080002100,
    0x4400800080304000,
    0x0856004100208200,
    0x3800801000802000,
    0x0204801002800800,
    0x0008800400080080,
    0x1852000430089201,
    0x0505000100020004,
    0x8040802041000080,
    0x0238808008400020,
    0x0400810021004000,
    0x0800808020001004,
    0x4008008010050980,
    0x1004008004080081,
    0x5000808002000400,
    0x0000040010020108,
    0x0008120018451084,
    0x0240800480204000,
    0x2000200540045000,
    0x080a124100200100,
    0x0711002100100009,
    0x0141110100050800,
    0x0800040080020080,
    0x0040020080800100,
    0x0000008200190044,
    0x0220e04001800080,
    0x00a0100826400041,
    0x0008410811002000,
    0x2000800800801004,
    0x0208020040400400,
    0x8080040080800200,
    0x4000100804000281,
    0x0012110482002044,
    0x0000804000208000,
    0x015000200140c00a,
    0x4010100020008080,
    0x0004100500210008,
    0x0049000800110004,
    0xa202001020040400,
    0x8000040200010100,
    0x8210009044020011,
    0x0201208001401980,
    0x4150844000210100,
    0x2020a00109484d00,
    0x2800230208100100,
    0x0908000880040180,
    0x8804040002008080,
    0x8003000462001100,
    0x0008800100004080,
    0x0002110024824206,
    0x0e00a08500334001,
    0x2200200109110041,
    0x4000200900041001,
    0x1011003088000423,
    0x000200441008051a,
    0x0003484082100104,
    0x0080890402a08042,
];
const BISHOP_MAGICS: [u64; 64] = [
    0x4088081000820014,
    0x84380800b0820810,
    0x011008a200520003,
    0x440804810f800100,
    0x0464042000200201,
    0x0002886009001040,
    0x01084904a0200009,
    0x040e002084100804,
    0x4080081044480442,
    0x9001630c04041043,
    0x0118100080a10a00,
    0x0010044100200002,
    0x0042040420220800,
    0x4004810420844420,
    0x3280018818821049,
    0x04442c8e45082018,
    0x0029806008036810,
    0x232008081820c090,
    0x0028000401441600,
    0x0000802802004000,
    0x0022000412020084,
    0x1002000040422002,
    0x0000410402280400,
    0x422200090b820500,
    0x0020702014840800,
    0x6c02500120241880,
    0x0224101042002042,
    0x2002020838008048,
    0x5209001013004020,
    0x7001010002008084,
    0x402c808822221011,
    0x8044960001230400,
    0x0808200602104410,
    0x0c84240202200204,
    0x0060403010080040,
    0x2401020080180080,
    0x0040044100441100,
    0x4d20021408808088,
    0x40040100420a0800,
    0x200a084042010420,
    0xa000826860004003,
    0x12008804301e0200,
    0x0208216130000803,
    0x0180060202080420,
    0x0100411020823400,
    0x08c0901c00c00320,
    0x0082024202024c00,
    0xc20112208601c100,
    0x0a04010403200010,
    0x004c210128210100,
    0x0090042402220021,
    0x04181801420a0a00,
    0xc004045002120094,
    0x0404202082408008,
    0xc04290620a014090,
    0x40200e2082128022,
    0x000e114210105812,
    0x0888204100901000,
    0x0404134104290400,
    0x02040240220a0200,
    0x0202800290120622,
    0x8010000420442110,
    0x0090210801282080,
    0x0008101418002020,
];

#[derive(Clone, Copy, Default)]
struct Magic {
    mask: u64,
    factor: u64,
    shift: u8,
    offset: usize,
}

struct SliderTable {
    entries: [Magic; 64],
    magic_attacks: Vec<u64>,
    pext_attacks: Vec<u64>,
}

impl SliderTable {
    fn new(dirs: &[(i8, i8); 4], magics: &[u64; 64]) -> Self {
        let mut table = Self {
            entries: [Magic::default(); 64],
            magic_attacks: Vec::new(),
            pext_attacks: Vec::new(),
        };
        for (index, &factor) in magics.iter().enumerate() {
            let square = Square(index as u8);
            let mask = slider_mask(square, dirs);
            let bits = mask.count_ones() as usize;
            let size = 1_usize << bits;
            let mut occupancies = Vec::with_capacity(size);
            let mut attacks = Vec::with_capacity(size);
            for subset in 0..size {
                let occupancy = deposit(subset as u64, mask);
                occupancies.push(occupancy);
                attacks.push(ray_attacks(square, occupancy, dirs));
            }
            let mut slots = vec![u64::MAX; size];
            for (&occupancy, &attack) in occupancies.iter().zip(&attacks) {
                let slot = (occupancy.wrapping_mul(factor) >> (64 - bits)) as usize;
                assert!(
                    slots[slot] == u64::MAX || slots[slot] == attack,
                    "invalid slider magic"
                );
                slots[slot] = attack;
            }
            let offset = table.magic_attacks.len();
            table.entries[index] = Magic {
                mask,
                factor,
                shift: (64 - bits) as u8,
                offset,
            };
            table.magic_attacks.extend(slots);
            table.pext_attacks.extend(attacks);
        }
        table
    }

    fn get(&self, square: Square, occupancy: Bitboard) -> Bitboard {
        let entry = self.entries[square.0 as usize];
        let blockers = occupancy.0 & entry.mask;
        let index = if fast_pext_available() {
            hardware_pext(blockers, entry.mask) as usize
        } else {
            (blockers.wrapping_mul(entry.factor) >> entry.shift) as usize
        };
        Bitboard(if fast_pext_available() {
            self.pext_attacks[entry.offset + index]
        } else {
            self.magic_attacks[entry.offset + index]
        })
    }
}

struct AttackTables {
    rooks: SliderTable,
    bishops: SliderTable,
    knights: [u64; 64],
    kings: [u64; 64],
    pawns: [[u64; 64]; 2],
    between: [[u64; 64]; 64],
    line: [[u64; 64]; 64],
}

static TABLES: OnceLock<AttackTables> = OnceLock::new();

fn tables() -> &'static AttackTables {
    TABLES.get_or_init(|| {
        let mut tables = AttackTables {
            rooks: SliderTable::new(&ROOK_DIRS, &ROOK_MAGICS),
            bishops: SliderTable::new(&BISHOP_DIRS, &BISHOP_MAGICS),
            knights: [0; 64],
            kings: [0; 64],
            pawns: [[0; 64]; 2],
            between: [[0; 64]; 64],
            line: [[0; 64]; 64],
        };
        for from in 0..64 {
            let square = Square(from as u8);
            tables.knights[from] = leaper_attacks(
                square,
                &[
                    (1, 2),
                    (2, 1),
                    (-1, 2),
                    (-2, 1),
                    (1, -2),
                    (2, -1),
                    (-1, -2),
                    (-2, -1),
                ],
            );
            tables.kings[from] = leaper_attacks(
                square,
                &[
                    (1, 0),
                    (-1, 0),
                    (0, 1),
                    (0, -1),
                    (1, 1),
                    (1, -1),
                    (-1, 1),
                    (-1, -1),
                ],
            );
            tables.pawns[0][from] = leaper_attacks(square, &[(1, 1), (-1, 1)]);
            tables.pawns[1][from] = leaper_attacks(square, &[(1, -1), (-1, -1)]);
            for to in 0..64 {
                if from == to {
                    continue;
                }
                let other = Square(to as u8);
                let df = other.file() as i8 - square.file() as i8;
                let dr = other.rank() as i8 - square.rank() as i8;
                if df == 0 || dr == 0 || df.abs() == dr.abs() {
                    let step_f = df.signum();
                    let step_r = dr.signum();
                    let mut file = square.file() as i8 + step_f;
                    let mut rank = square.rank() as i8 + step_r;
                    while file != other.file() as i8 || rank != other.rank() as i8 {
                        tables.between[from][to] |= 1_u64 << (rank * 8 + file);
                        file += step_f;
                        rank += step_r;
                    }
                    let mut file = square.file() as i8;
                    let mut rank = square.rank() as i8;
                    while on_board(file - step_f, rank - step_r) {
                        file -= step_f;
                        rank -= step_r;
                    }
                    while on_board(file, rank) {
                        tables.line[from][to] |= 1_u64 << (rank * 8 + file);
                        file += step_f;
                        rank += step_r;
                    }
                }
            }
        }
        tables
    })
}

const fn on_board(file: i8, rank: i8) -> bool {
    file >= 0 && file < 8 && rank >= 0 && rank < 8
}

fn leaper_attacks(square: Square, offsets: &[(i8, i8)]) -> u64 {
    let mut attacks = 0;
    for &(df, dr) in offsets {
        let file = square.file() as i8 + df;
        let rank = square.rank() as i8 + dr;
        if on_board(file, rank) {
            attacks |= 1_u64 << (rank * 8 + file);
        }
    }
    attacks
}

fn ray_attacks(square: Square, occupancy: u64, dirs: &[(i8, i8); 4]) -> u64 {
    let mut attacks = 0;
    for &(df, dr) in dirs {
        let mut file = square.file() as i8 + df;
        let mut rank = square.rank() as i8 + dr;
        while on_board(file, rank) {
            let bit = 1_u64 << (rank * 8 + file);
            attacks |= bit;
            if occupancy & bit != 0 {
                break;
            }
            file += df;
            rank += dr;
        }
    }
    attacks
}

fn slider_mask(square: Square, dirs: &[(i8, i8); 4]) -> u64 {
    let mut mask = 0;
    for &(df, dr) in dirs {
        let mut file = square.file() as i8 + df;
        let mut rank = square.rank() as i8 + dr;
        while on_board(file + df, rank + dr) {
            mask |= 1_u64 << (rank * 8 + file);
            file += df;
            rank += dr;
        }
    }
    mask
}

fn deposit(mut value: u64, mut mask: u64) -> u64 {
    let mut result = 0;
    while mask != 0 {
        let bit = mask.isolate_lowest_one();
        if value & 1 != 0 {
            result |= bit;
        }
        value >>= 1;
        mask &= mask - 1;
    }
    result
}

#[cfg(target_arch = "x86_64")]
fn fast_pext_available() -> bool {
    static FAST: OnceLock<bool> = OnceLock::new();
    *FAST.get_or_init(|| {
        if !std::is_x86_feature_detected!("bmi2") {
            return false;
        }
        // AMD family 17h and earlier implement PEXT too slowly for this lookup.
        let vendor = std::arch::x86_64::__cpuid(0);
        if (vendor.ebx, vendor.edx, vendor.ecx) != (0x6874_7541, 0x6974_6e65, 0x444d_4163) {
            return true;
        }
        let leaf = std::arch::x86_64::__cpuid(1);
        let base = (leaf.eax >> 8) & 15;
        let ext = (leaf.eax >> 20) & 255;
        base + if base == 15 { ext } else { 0 } >= 0x19
    })
}

#[cfg(not(target_arch = "x86_64"))]
const fn fast_pext_available() -> bool {
    false
}

#[cfg(target_arch = "x86_64")]
fn hardware_pext(value: u64, mask: u64) -> u64 {
    // SAFETY: this function is reached only after BMI2 detection succeeds.
    unsafe { std::arch::x86_64::_pext_u64(value, mask) }
}

#[cfg(not(target_arch = "x86_64"))]
fn hardware_pext(_value: u64, _mask: u64) -> u64 {
    unreachable!()
}

pub fn rook_attacks(square: Square, occupancy: Bitboard) -> Bitboard {
    tables().rooks.get(square, occupancy)
}

pub fn bishop_attacks(square: Square, occupancy: Bitboard) -> Bitboard {
    tables().bishops.get(square, occupancy)
}

pub fn knight_attacks(square: Square) -> Bitboard {
    Bitboard(tables().knights[square.0 as usize])
}

pub fn king_attacks(square: Square) -> Bitboard {
    Bitboard(tables().kings[square.0 as usize])
}

pub fn pawn_attacks(color: Color, square: Square) -> Bitboard {
    Bitboard(tables().pawns[color.index()][square.0 as usize])
}

pub fn between(a: Square, b: Square) -> Bitboard {
    Bitboard(tables().between[a.0 as usize][b.0 as usize])
}

pub fn line(a: Square, b: Square) -> Bitboard {
    Bitboard(tables().line[a.0 as usize][b.0 as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_slider_subsets_match_direct_rays() {
        let tables = tables();
        for (slider, dirs) in [(&tables.rooks, &ROOK_DIRS), (&tables.bishops, &BISHOP_DIRS)] {
            for square_index in 0..64 {
                let square = Square(square_index as u8);
                let entry = slider.entries[square_index];
                for subset in 0..(1_u64 << entry.mask.count_ones()) {
                    let blockers = deposit(subset, entry.mask);
                    let expected = ray_attacks(square, blockers, dirs);
                    let magic_index = (blockers.wrapping_mul(entry.factor) >> entry.shift) as usize;
                    assert_eq!(slider.magic_attacks[entry.offset + magic_index], expected);
                    assert_eq!(
                        slider.pext_attacks[entry.offset + subset as usize],
                        expected
                    );
                }
            }
        }
    }
}
