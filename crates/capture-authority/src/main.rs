fn main() {
    if let Err(error) = run() {
        eprintln!("capture authority: {error}");
        std::process::exit(1);
    }
}

#[cfg(target_os = "linux")]
fn run() -> Result<(), String> {
    use amaterasu_capture_authority::{LaunchOptions, run};
    use std::path::PathBuf;
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 6 || args[0] != "run" {
        return Err("usage: amaterasu-capture-authority run APPROVAL_STORE APPROVAL_ID PACKAGE_ROOT WORKER STATE_ROOT".into());
    }
    run(LaunchOptions {
        approval_store: PathBuf::from(&args[1]),
        approval_id: args[2]
            .to_str()
            .ok_or("approval id is not UTF-8")?
            .to_owned(),
        package_root: PathBuf::from(&args[3]),
        worker: PathBuf::from(&args[4]),
        state_root: PathBuf::from(&args[5]),
    })
}

#[cfg(not(target_os = "linux"))]
fn run() -> Result<(), String> {
    Err("capture authority is supported only on Linux".into())
}
