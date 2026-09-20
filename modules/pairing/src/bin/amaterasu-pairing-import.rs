//! One-module process boundary for installer pairing migration.
//!
//! Keeping this in the pairing package lets an installer invoke the validated
//! converter without loading more than one `amaterasu_module_v1` export into a
//! process. Errors deliberately contain no predecessor values or key material.

use amaterasu_pairing::migration::{PairingSeedOutcome, import_legacy_pairing, seed_pairing_state};
use std::path::PathBuf;

fn main() {
    if let Err(error) = run() {
        eprintln!("amaterasu-pairing-import: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), &'static str> {
    let mut arguments = std::env::args().skip(1);
    let command = arguments.next().ok_or("usage")?;
    let mut source = None;
    let mut state = None;
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--source" => source = Some(PathBuf::from(arguments.next().ok_or("usage")?)),
            "--state" => state = Some(PathBuf::from(arguments.next().ok_or("usage")?)),
            _ => return Err("usage"),
        }
    }
    let source = source.ok_or("usage")?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "legacy_pairing_clock_failure")?
        .as_secs();
    let imported = import_legacy_pairing(&source, now)?;
    match command.as_str() {
        "validate" if state.is_none() => {
            println!("validated");
            Ok(())
        }
        "seed" => match seed_pairing_state(&state.ok_or("usage")?, &imported)? {
            PairingSeedOutcome::Seeded => {
                println!("seeded");
                Ok(())
            }
            PairingSeedOutcome::ExistingAmaterasuState => {
                println!("existing_amaterasu_state");
                Ok(())
            }
        },
        _ => Err("usage"),
    }
}
