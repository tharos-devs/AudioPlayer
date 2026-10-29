//! Scan en ligne de commande : `scan <base.db> <bibliothèque> [--full]`.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use audioplayer_db::Database;
use audioplayer_scanner::{scan, Mode, Progress};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(db_path), Some(name)) = (args.first(), args.get(1)) else {
        eprintln!("usage: scan <library.db> <library name> [--full]");
        return ExitCode::from(2);
    };
    let mode = if args.iter().any(|a| a == "--full") { Mode::Full } else { Mode::Changes };
    let db_path = PathBuf::from(db_path);
    let library = match Database::open(&db_path).and_then(|db| db.libraries()) {
        Ok(libraries) => libraries.into_iter().find(|l| &l.name == name),
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let Some(library) = library else {
        eprintln!("{name}: no such library");
        return ExitCode::FAILURE;
    };
    let start = Instant::now();
    let result = scan(&db_path, library.id, library.root_path.as_ref(), mode, &AtomicBool::new(false), &|progress| {
        if let Progress::Reading { done, total, .. } = progress {
            if done % 500 == 0 || done == total {
                eprintln!("{done}/{total} ({:.0?})", start.elapsed());
            }
        }
    });
    match result {
        Ok(report) => {
            println!("{report:?} in {:.1?}", start.elapsed());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("scan failed: {error}");
            ExitCode::FAILURE
        }
    }
}
