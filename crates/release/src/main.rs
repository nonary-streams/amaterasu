mod development;
#[cfg(target_os = "linux")]
mod linux_user_service;
mod linux_video_runtime;
mod preflight;
mod profiles;
mod repositories;
mod vhf_install;

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
        [command, root, options @ ..] if command == "init-linux-session" => {
            let options =
                profiles::linux_session::options::LinuxSessionOptions::from_arguments(options)
                    .map_err(|error| error.to_string())?;
            development::initialize_linux_session(Path::new(root), &options)
        }
        #[cfg(target_os = "linux")]
        [command, root, host_executable, output] if command == "prepare-linux-user-service" => {
            linux_user_service::prepare(
                Path::new(root),
                Path::new(host_executable),
                Path::new(output),
            )
        }
        #[cfg(target_os = "linux")]
        [command, root, host_executable, bundle] if command == "preflight-linux-user-service" => {
            linux_user_service::preflight(
                Path::new(root),
                Path::new(host_executable),
                Path::new(bundle),
            )
        }
        [command, root] if command == "preflight-desktop-stream-smoke" => {
            preflight::run(Path::new(root), None)
        }
        [command, root, vhf_install_root] if command == "preflight-desktop-stream-smoke" => {
            preflight::run(Path::new(root), Some(Path::new(vhf_install_root)))
        }
        [command, root] if command == "init-desktop-stream-smoke" => {
            development::initialize_desktop_stream_smoke(Path::new(root), false)
        }
        [command, root, option]
            if command == "init-desktop-stream-smoke" && option == "--allow-remote" =>
        {
            development::initialize_desktop_stream_smoke(Path::new(root), true)
        }
        [command, root, artifacts, output, sequence]
            if matches!(
                command.as_str(),
                "pack"
                    | "pack-platform"
                    | "pack-media-probe"
                    | "pack-stream-probe"
                    | "pack-desktop-stream-smoke"
                    | "pack-linux-session"
            ) =>
        {
            let sequence = sequence
                .parse()
                .map_err(|_| "sequence must be an unsigned integer")?;
            let profile = if command == "pack-linux-session" {
                profiles::PackageProfile::LinuxSession
            } else if command == "pack-stream-probe" {
                profiles::PackageProfile::StreamProbe
            } else if command == "pack-desktop-stream-smoke" {
                profiles::PackageProfile::DesktopStreamSmoke
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
        [command, root, id, location, public_key, fingerprint]
            if command == "repository-approve" =>
        {
            repositories::approve(root, id, location, public_key, fingerprint)
        }
        [command, root, id] if command == "repository-revoke" => repositories::revoke(root, id),
        [command, root, id] if command == "repository-refresh" => repositories::refresh(root, id),
        [command, root] if command == "repository-list" => repositories::list(root),
        [command, root, id, slot, implementation, version, output]
            if command == "repository-download" =>
        {
            repositories::download(root, id, slot, implementation, version, Path::new(output))
        }
        [command, root, id, implementation, version, output]
            if command == "repository-install-ui" =>
        {
            repositories::install_ui(root, id, implementation, version, Path::new(output))
        }
        [command, root, id, slot, implementation, version, source]
            if command == "repository-stage-native" =>
        {
            repositories::stage_native(root, id, slot, implementation, version, Path::new(source))
        }
        [command, root, digest] if command == "repository-activate-native" => {
            repositories::activate_native(root, digest)
        }
        [command, root] if command == "repository-restore-ui" => {
            repositories::restore_ui(root, None)
        }
        [command, root, digest] if command == "repository-restore-ui" => {
            repositories::restore_ui(root, Some(digest))
        }
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
        [command, package, fingerprint, signtool] if command == "verify-vhf-package" => {
            vhf_install::verify_command(Path::new(package), fingerprint, Path::new(signtool))
        }
        [command, package, fingerprint, signtool] if command == "seal-vhf-package" => {
            vhf_install::seal_command(Path::new(package), fingerprint, Path::new(signtool))
        }
        [
            command,
            package,
            install_root,
            runtime_root,
            fingerprint,
            signtool,
            devcon,
        ] if command == "install-vhf-package" => vhf_install::install_command(
            Path::new(package),
            Path::new(install_root),
            Path::new(runtime_root),
            fingerprint,
            Path::new(signtool),
            Path::new(devcon),
        ),
        [command, install_root, runtime_root, devcon] if command == "rollback-vhf-package" => {
            vhf_install::rollback_command(
                Path::new(install_root),
                Path::new(runtime_root),
                Path::new(devcon),
            )
        }
        [command, install_root, runtime_root, digest, keyboard, mouse]
            if command == "grant-vhf-input" =>
        {
            vhf_install::grant_command(
                Path::new(install_root),
                Path::new(runtime_root),
                digest,
                keyboard,
                mouse,
            )
        }
        [command, install_root, runtime_root] if command == "revoke-vhf-input" => {
            vhf_install::revoke_command(Path::new(install_root), Path::new(runtime_root))
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
    "usage:\n  amaterasu-release init ROOT\n  amaterasu-release init-linux-session ROOT [--lan ADDRESS:PORT [--allow-remote]] [--catalog-owner management|console] [--management-port PORT] [--audio-endpoint NAME] [--enable-capture] [--enable-audio] [--enable-virtual-display] [--enable-input] [--enable-display-mutation]\n  amaterasu-release prepare-linux-user-service ROOT HOST_EXECUTABLE OUTPUT_DIRECTORY (Linux only; emits a reviewable bundle)\n  amaterasu-release preflight-linux-user-service ROOT HOST_EXECUTABLE BUNDLE_DIRECTORY (Linux only; read-only deployment checks)\n  amaterasu-release pack-linux-session ROOT ARTIFACTS OUTPUT SEQUENCE\n  amaterasu-release init-desktop-stream-smoke ROOT [--allow-remote]\n  amaterasu-release preflight-desktop-stream-smoke ROOT [VHF_INSTALL_ROOT]\n  amaterasu-release pack ROOT ARTIFACTS OUTPUT SEQUENCE\n  amaterasu-release pack-platform ROOT ARTIFACTS OUTPUT SEQUENCE\n  amaterasu-release pack-media-probe ROOT ARTIFACTS OUTPUT SEQUENCE\n  amaterasu-release pack-stream-probe ROOT ARTIFACTS OUTPUT SEQUENCE\n  amaterasu-release pack-desktop-stream-smoke ROOT ARTIFACTS OUTPUT SEQUENCE\n  amaterasu-release verify ROOT PACKAGE\n  amaterasu-release stage ROOT PACKAGE [--historical]\n  amaterasu-release pack-web ROOT DIST OUTPUT SEQUENCE\n  amaterasu-release stage-web ROOT PACKAGE\n  amaterasu-release repository-approve ROOT ID HTTPS_URL PUBLIC_KEY_HEX CONFIRMED_SHA256\n  amaterasu-release repository-revoke ROOT ID\n  amaterasu-release repository-refresh ROOT ID\n  amaterasu-release repository-list ROOT\n  amaterasu-release repository-download ROOT ID SLOT IMPLEMENTATION VERSION OUTPUT\n  amaterasu-release repository-install-ui ROOT ID IMPLEMENTATION VERSION OUTPUT\n  amaterasu-release repository-stage-native ROOT ID SLOT IMPLEMENTATION VERSION SOURCE\n  amaterasu-release repository-activate-native ROOT DIGEST\n  amaterasu-release repository-restore-ui ROOT [DIGEST]\n  amaterasu-release prepare-installer ARTIFACTS WEB_DIST OUTPUT\n  amaterasu-release seal-vhf-package PACKAGE CONFIRMED_FINGERPRINT SIGNTOOL\n  amaterasu-release verify-vhf-package PACKAGE CONFIRMED_FINGERPRINT SIGNTOOL\n  amaterasu-release install-vhf-package PACKAGE INSTALL_ROOT RUNTIME_ROOT CONFIRMED_FINGERPRINT SIGNTOOL DEVCON\n  amaterasu-release rollback-vhf-package INSTALL_ROOT RUNTIME_ROOT DEVCON\n  amaterasu-release grant-vhf-input INSTALL_ROOT RUNTIME_ROOT ACTIVE_SET_DIGEST KEYBOARD_0_OR_1 MOUSE_0_OR_1\n  amaterasu-release revoke-vhf-input INSTALL_ROOT RUNTIME_ROOT\n  Desktop smoke requires replacing SELECT_OUTPUT_ID with a current DXGI output ID in bootstrap.json.\n  --allow-remote selects wildcard stream listeners only; it does not alter firewall or machine network settings."
}
