#[cfg(windows)]
mod interactive_worker;
mod loading;
mod logging_config;
mod protocol;
mod service;
#[cfg(windows)]
mod state_root;
mod supervisor;
#[cfg(windows)]
mod windows_service;
mod worker;

use amaterasu_package::PackageResult;
use std::path::Path;

fn main() {
    if let Err(error) = run() {
        eprintln!("amaterasu: {error}");
        std::process::exit(1);
    }
}

fn run() -> PackageResult<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments.as_slice() {
        [command, root] if command == "serve" => service::serve(Path::new(root))?,
        [command, root] if command == "service" => service::service(Path::new(root))?,
        [command, root] if command == "service-ready" => {
            service::probe_service_readiness(Path::new(root))?;
            println!("service ready");
        }
        [command, root] if command == "operational-ready" => {
            service::probe_operational_readiness(Path::new(root))?;
            println!("operationally ready");
        }
        [command, root, digest] if command == "worker-serve" => {
            worker::serve(Path::new(root), digest)?;
        }
        [command, root, digest, state_root] if command == "worker-user-serve" => {
            worker::serve_interactive(Path::new(root), digest, Path::new(state_root))?;
        }
        [command, root] if command == "probe" => {
            supervisor::probe(Path::new(root), None)?;
            println!("Component set passed the development lifecycle probe.");
        }
        [command, root, input] if command == "probe" => {
            if let Some(reply) = supervisor::probe(Path::new(root), Some(input))? {
                println!("{reply}");
            }
        }
        [command, root, digest] if command == "worker" => {
            worker::run(Path::new(root), digest, None)?
        }
        [command, root, digest, input] if command == "worker" => {
            worker::run(Path::new(root), digest, Some(input))?
        }
        _ => {
            return Err(
                "usage: amaterasu probe ROOT [CONTROL_MESSAGE_JSON] | service-ready ROOT | operational-ready ROOT | serve ROOT | service ROOT"
                    .into(),
            );
        }
    }
    Ok(())
}
