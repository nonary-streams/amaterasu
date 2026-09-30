mod development;
mod profiles;

use amaterasu_package::{
    BootstrapConfig, PackageResult, RootLock, VerificationPolicy, read_json, stage_package,
    unix_timestamp, verify_package,
};
use std::path::Path;

fn main() {
    if let Err(error) = run() {
        eprintln!("amaterasu-release: {error}");
        std::process::exit(1);
    }
}

fn run() -> PackageResult<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments.as_slice() {
        [command, root] if command == "init" => development::initialize(Path::new(root)),
        [command, root, artifacts, output, sequence]
            if matches!(
                command.as_str(),
                "pack"
                    | "pack-platform"
                    | "pack-media-probe"
                    | "pack-stream-probe"
                    | "pack-software-probe"
                    | "pack-linux-probe"
            ) =>
        {
            let sequence = sequence
                .parse()
                .map_err(|_| "sequence must be an unsigned integer")?;
            let profile = if command == "pack-linux-probe" {
                profiles::PackageProfile::LinuxProbe
            } else if command == "pack-software-probe" {
                profiles::PackageProfile::SoftwareProbe
            } else if command == "pack-stream-probe" {
                profiles::PackageProfile::StreamProbe
            } else if command == "pack-media-probe" {
                profiles::PackageProfile::MediaProbe
            } else if command == "pack-platform" {
                profiles::PackageProfile::Platform
            } else {
                profiles::PackageProfile::Example
            };
            development::build_package(
                Path::new(root),
                Path::new(artifacts),
                Path::new(output),
                sequence,
                profile,
            )
        }
        [command, root, source] if command == "stage" => stage(root, source, false),
        [command, root, source, option] if command == "stage" && option == "--historical" => {
            stage(root, source, true)
        }
        [command, root, source] if command == "verify" => verify(root, source),
        [command, root, source, output, sequence] if command == "pack-web" => {
            pack_web(root, source, output, sequence)
        }
        [command, root, source] if command == "stage-web" => stage_web(root, source),
        [command, artifacts, web, output] if command == "prepare-installer" => {
            development::prepare_installer_assets(
                Path::new(artifacts),
                Path::new(web),
                Path::new(output),
            )
        }
        [command, template, package, root] if command == "provision-installer" => {
            development::provision_installer_root(
                Path::new(template),
                Path::new(package),
                Path::new(root),
            )
        }
        [command, template, package, root] if command == "install-installer" => {
            development::install_installer_root(
                Path::new(template),
                Path::new(package),
                Path::new(root),
            )
        }
        [command, root] if command == "unprovision-if-created" => {
            development::unprovision_if_created(Path::new(root))
        }
        [command, root] if command == "finalize-installer-provision" => {
            development::finalize_installer_provision(Path::new(root))
        }
        _ => Err(usage().into()),
    }
}

fn pack_web(root: &str, source: &str, output: &str, sequence: &str) -> PackageResult<()> {
    let root = Path::new(root);
    let bootstrap: BootstrapConfig = read_json(&root.join("bootstrap.json"))?;
    if !bootstrap.development {
        return Err("development publisher requires a development root".into());
    }
    let sequence = sequence
        .parse()
        .map_err(|_| "invalid web release sequence")?;
    let manifest = amaterasu_package::web::prepare_bundle(
        Path::new(source),
        Path::new(output),
        sequence,
        unix_timestamp() + 7 * 24 * 60 * 60,
    )?;
    development::sign_manifest(root, Path::new(output), &manifest)
}

fn stage_web(root: &str, source: &str) -> PackageResult<()> {
    let root = Path::new(root);
    let bootstrap: BootstrapConfig = read_json(&root.join("bootstrap.json"))?;
    let digest = amaterasu_package::web::stage_bundle(
        Path::new(source),
        &root.join("web"),
        &bootstrap.trusted_keys,
        unix_timestamp(),
    )?;
    println!("Staged web bundle {digest}; refresh the browser to use it.");
    Ok(())
}

fn stage(root: &str, source: &str, historical: bool) -> PackageResult<()> {
    let root = Path::new(root);
    let _lock = RootLock::acquire(root)?;
    let bootstrap = read_json(&root.join("bootstrap.json"))?;
    let digest = stage_package(Path::new(source), root, &bootstrap, historical)?;
    println!("Staged {digest}; activation requires a new host process.");
    Ok(())
}

fn verify(root: &str, source: &str) -> PackageResult<()> {
    let bootstrap: BootstrapConfig = read_json(&Path::new(root).join("bootstrap.json"))?;
    let package = verify_package(
        Path::new(source),
        &bootstrap,
        VerificationPolicy {
            minimum_sequence: 0,
            allow_historical: true,
            now: unix_timestamp(),
        },
    )?;
    println!("Verified {} (signed: {}).", package.digest, package.signed);
    Ok(())
}

fn usage() -> &'static str {
    "usage:\n  amaterasu-release init ROOT\n  amaterasu-release pack ROOT ARTIFACTS OUTPUT SEQUENCE\n  amaterasu-release pack-platform ROOT ARTIFACTS OUTPUT SEQUENCE\n  amaterasu-release pack-media-probe ROOT ARTIFACTS OUTPUT SEQUENCE\n  amaterasu-release pack-stream-probe ROOT ARTIFACTS OUTPUT SEQUENCE\n  amaterasu-release pack-software-probe ROOT ARTIFACTS OUTPUT SEQUENCE\n  amaterasu-release pack-linux-probe ROOT ARTIFACTS OUTPUT SEQUENCE\n  amaterasu-release verify ROOT PACKAGE\n  amaterasu-release stage ROOT PACKAGE [--historical]\n  amaterasu-release pack-web ROOT DIST OUTPUT SEQUENCE\n  amaterasu-release stage-web ROOT PACKAGE\n  amaterasu-release prepare-installer ARTIFACTS WEB_DIST OUTPUT"
}
