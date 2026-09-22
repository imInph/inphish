use inphzugzwang_core::{perft, Position};

fn check_cases(data: &str, maximum_depth: u8) {
    for record in data.lines() {
        let mut fields = record.splitn(4, '|');
        let name = fields.next().expect("case name");
        let depth: u8 = fields
            .next()
            .expect("depth")
            .parse()
            .expect("numeric depth");
        let expected: u64 = fields
            .next()
            .expect("nodes")
            .parse()
            .expect("numeric nodes");
        let fen = fields.next().expect("FEN");
        if depth <= maximum_depth {
            let mut position = Position::from_fen(fen).expect(name);
            assert_eq!(
                perft(&mut position, depth),
                expected,
                "{name} depth {depth}"
            );
        }
    }
}

#[test]
fn quick_standard_perft() {
    check_cases(include_str!("../../../tests/perft/standard.txt"), 4);
}

#[test]
fn quick_chess960_perft() {
    check_cases(include_str!("../../../tests/perft/chess960.txt"), 3);
}

#[test]
#[ignore = "full perft suite"]
fn full_standard_perft() {
    check_cases(include_str!("../../../tests/perft/standard.txt"), 6);
}

#[test]
#[ignore = "full perft suite"]
fn full_chess960_perft() {
    check_cases(include_str!("../../../tests/perft/chess960.txt"), 5);
}

#[test]
#[ignore = "all 960 starting positions"]
fn all_chess960_starts() {
    check_cases(include_str!("../../../tests/perft/chess960_starts.txt"), 3);
}
