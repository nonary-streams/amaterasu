//! Emit driver metadata using hashes of the actual publisher-built artifacts.
//! Usage: driver_requirement DRIVER_ID FEATURE COMPATIBILITY HELPER [PAYLOAD...]
use amaterasu_module_sdk::drivers::{Artifact, DriverRequirement, SCHEMA};
use sha2::{Digest, Sha256};
use std::path::Path;

fn artifact(file: &str) -> Result<Artifact, String> {
    let path = Path::new(file);
    let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > 256 * 1024 * 1024
    {
        return Err("expected a regular artifact of at most 256 MiB".into());
    }
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("artifact filename must be UTF-8")?;
    Ok(Artifact {
        file: name.into(),
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        size: bytes.len() as u64,
    })
}
fn run() -> Result<(), String> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    let [id, feature, compatibility, helper, payloads @ ..] = arguments.as_slice() else {
        return Err(
            "usage: driver_requirement DRIVER_ID FEATURE COMPATIBILITY HELPER [PAYLOAD...]".into(),
        );
    };
    let requirement = DriverRequirement {
        schema: SCHEMA,
        id: id.clone(),
        name: id.clone(),
        target: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        feature: feature.clone(),
        reason: format!("Required for {feature}"),
        compatibility: compatibility.clone(),
        may_require_reboot: true,
        offered_version: None,
        installer: artifact(helper)?,
        payloads: payloads
            .iter()
            .map(|file| artifact(file))
            .collect::<Result<_, _>>()?,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&requirement).map_err(|error| error.to_string())?
    );
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
