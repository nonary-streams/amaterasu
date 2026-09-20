use amaterasu_authentication::migration::{
    AuthenticationSeedOutcome, import_legacy_authentication, seed_authentication_state,
};
use std::path::PathBuf;

fn main() {
    if let Err(error) = run() {
        eprintln!("amaterasu-authentication-import: {error}");
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
    let imported = import_legacy_authentication(&source.ok_or("usage")?)?;
    match command.as_str() {
        "validate" if state.is_none() => println!("validated"),
        "seed" => match seed_authentication_state(&state.ok_or("usage")?, &imported)? {
            AuthenticationSeedOutcome::Seeded => println!("seeded"),
            AuthenticationSeedOutcome::ExistingAmaterasuState => {
                println!("existing_amaterasu_state")
            }
        },
        _ => return Err("usage"),
    }
    Ok(())
}
