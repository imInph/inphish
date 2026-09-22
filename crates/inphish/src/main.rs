use std::env;
use std::io::{self, Write};
use std::process::ExitCode;
use std::time::Instant;

use inphzugzwang_core::{perft, Position, Square};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let command = args.next().ok_or_else(usage)?;
    let depth = if command == "perft" || command == "divide" {
        Some(
            args.next()
                .ok_or_else(usage)?
                .parse::<u8>()
                .map_err(|_| usage())?,
        )
    } else {
        None
    };
    let mut position = match args.next() {
        None => Position::startpos(),
        Some(flag) if flag == "--fen" => {
            let fen = args.next().ok_or_else(usage)?;
            if args.next().is_some() {
                return Err(usage());
            }
            Position::from_fen(&fen).map_err(|error| error.to_string())?
        }
        _ => return Err(usage()),
    };
    match command.as_str() {
        "perft" => {
            let depth = depth.expect("perft has depth");
            let start = Instant::now();
            let nodes = perft(&mut position, depth);
            println!("{nodes} nodes in {} ms", start.elapsed().as_millis());
        }
        "divide" => {
            let depth = depth.expect("divide has depth");
            if depth == 0 {
                return Err("divide depth must be positive".to_owned());
            }
            let mut total = 0;
            for mv in position.legal_moves().iter() {
                let label = position.format_move(mv, false);
                position.make(mv);
                let nodes = perft(&mut position, depth - 1);
                position.unmake();
                println!("{label}: {nodes}");
                total += nodes;
            }
            println!("Total: {total}");
        }
        "d" => {
            for rank in (0..8).rev() {
                for file in 0..8 {
                    let square = Square::new(file, rank);
                    let symbol = position.piece_at(square).map_or('.', |piece| piece.fen());
                    print!("{symbol} ");
                }
                println!();
            }
            println!("FEN: {}", position.fen());
            println!("Key: {:016x}", position.key());
            println!("Checkers: {:016x}", position.checkers().0);
            io::stdout().flush().map_err(|error| error.to_string())?;
        }
        _ => return Err(usage()),
    }
    Ok(())
}

fn usage() -> String {
    "usage: inphish <perft <depth>|divide <depth>|d> [--fen <FEN>]".to_owned()
}
