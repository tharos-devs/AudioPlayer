//! Crée une base neuve à partir de celle de la v0.1 : `import-v01 <ancienne.db> <nouvelle.db>`.

use std::path::PathBuf;
use std::process::ExitCode;

use audioplayer_db::{legacy, Database};

fn main() -> ExitCode {
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let [legacy_path, target] = args.as_slice() else {
        eprintln!("usage: import-v01 <legacy.db> <new.db>");
        return ExitCode::from(2);
    };
    if !legacy_path.is_file() {
        eprintln!("{}: not found", legacy_path.display());
        return ExitCode::FAILURE;
    }
    if target.exists() {
        eprintln!("{}: already exists, refusing to overwrite", target.display());
        return ExitCode::FAILURE;
    }

    let result = Database::open(target).and_then(|mut db| legacy::import(&mut db, legacy_path));
    match result {
        Ok(report) => {
            println!("{report:#?}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("import failed: {error}");
            // Ne pas laisser une base à moitié créée derrière soi.
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", target.display()));
            }
            ExitCode::FAILURE
        }
    }
}
