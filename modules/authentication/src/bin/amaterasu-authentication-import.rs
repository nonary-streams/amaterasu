use amaterasu_authentication::migration::{
    AuthenticationSeedOutcome, import_legacy_authentication, seed_authentication_state,
    seed_initial_administrator, seed_initial_enrollment,
};
use serde::Deserialize;
use std::io::Read;
use std::path::PathBuf;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InitialAdministrator {
    username: String,
    password: String,
}

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
    if command == "prepare-enrollment" && source.is_none() {
        let outcome = seed_initial_enrollment(&state.ok_or("usage")?)?;
        println!(
            "{}",
            match outcome {
                AuthenticationSeedOutcome::Seeded => "seeded",
                AuthenticationSeedOutcome::ExistingAmaterasuState => "existing_amaterasu_state",
            }
        );
        return Ok(());
    }
    if command == "initialize" && source.is_none() {
        let mut input = Vec::new();
        std::io::stdin()
            .take(4097)
            .read_to_end(&mut input)
            .map_err(|_| "invalid_input")?;
        if input.len() > 4096 {
            return Err("invalid_input");
        }
        let credentials: InitialAdministrator =
            serde_json::from_slice(&input).map_err(|_| "invalid_input")?;
        let outcome = seed_initial_administrator(
            &state.ok_or("usage")?,
            &credentials.username,
            &credentials.password,
        )?;
        println!(
            "{}",
            match outcome {
                AuthenticationSeedOutcome::Seeded => "seeded",
                AuthenticationSeedOutcome::ExistingAmaterasuState => "existing_amaterasu_state",
            }
        );
        return Ok(());
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
