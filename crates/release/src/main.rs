mod activation_review;
#[cfg(target_os = "linux")]
mod capture_authority;
mod capture_authority_package;
mod capture_precheck;
mod development;
mod linux_owned_display;
#[cfg(target_os = "linux")]
mod linux_user_service;
mod linux_video_runtime;
mod preflight;
mod processing_policy;
mod profiles;
mod repositories;
mod rtx_hdr_policy;
mod rtx_repository_publisher;
mod vhf_install;

use amaterasu_package::{
    BootstrapConfig, PackageResult, RootLock, VerificationPolicy, read_json, stage_package,
    unix_timestamp, verify_package,
};
use std::io::Read;
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
        [command, root, repository, bundle, digest] if command == "import-local-native" => {
            let catalog = amaterasu_package::repository::import_local_native(
                Path::new(root),
                repository,
                Path::new(bundle),
                digest,
                unix_timestamp(),
            )?;
            println!(
                "Imported verified local package {digest}; catalog {catalog}. Activate through the installed host; no publication is required."
            );
            Ok(())
        }
        [
            command,
            bootstrap,
            package,
            output,
            repository,
            location,
            key,
            sequence,
            expiry,
        ] if command == "publish-rtx-hdr-repository" => {
            let sequence = sequence
                .parse()
                .map_err(|_| "catalog sequence must be an unsigned integer")?;
            let expiry = expiry
                .parse()
                .map_err(|_| "catalog expiry must be a Unix timestamp")?;
            rtx_repository_publisher::publish(
                Path::new(bootstrap),
                Path::new(package),
                Path::new(output),
                repository,
                location,
                Path::new(key),
                sequence,
                expiry,
            )
        }
        [command, root, expected, publisher, output]
            if command == "review-package-activation-policy" =>
        {
            activation_review::prepare(
                Path::new(root),
                expected,
                publisher,
                Path::new(output),
                &profiles::linux_session::default_rtx_hdr_selection(),
            )
        }
        [command, root, expected, publisher, output, flag, selection]
            if command == "review-package-activation-policy"
                && flag == "--processing-selection" =>
        {
            let capabilities = amaterasu_package::read_json(Path::new(selection))?;
            activation_review::prepare(
                Path::new(root),
                expected,
                publisher,
                Path::new(output),
                &capabilities,
            )
        }
        #[cfg(target_os = "linux")]
        [command, root] if command == "provision-default-package-activation" => {
            activation_review::provision_default(Path::new(root))
        }
        #[cfg(target_os = "linux")]
        [command, root, report, digest] if command == "provision-package-activation" => {
            activation_review::provision(Path::new(root), Path::new(report), digest)
        }
        [
            command,
            root,
            digest,
            output,
            selection,
            component,
            settings,
        ] if command == "prepare-processing-bootstrap" => processing_policy::prepare(
            Path::new(root),
            digest,
            Path::new(output),
            Path::new(selection),
            Path::new(component),
            Path::new(settings),
            None,
        ),
        [
            command,
            root,
            digest,
            output,
            selection,
            component,
            settings,
            flag,
            metadata,
        ] if command == "prepare-processing-bootstrap" && flag == "--metadata" => {
            processing_policy::prepare(
                Path::new(root),
                digest,
                Path::new(output),
                Path::new(selection),
                Path::new(component),
                Path::new(settings),
                Some(Path::new(metadata)),
            )
        }
        [command, root] if command == "init" => development::initialize(Path::new(root)),
        [command, root, expected, output] if command == "prepare-rtx-hdr-bootstrap" => {
            rtx_hdr_policy::prepare(
                Path::new(root),
                expected,
                Path::new(output),
                &profiles::linux_session::default_rtx_hdr_selection(),
            )
        }
        [command, root, expected, output, flag, selection]
            if command == "prepare-rtx-hdr-bootstrap" && flag == "--processing-selection" =>
        {
            let capabilities: profiles::linux_session::ProcessingSelection =
                amaterasu_package::read_json(Path::new(selection))?;
            rtx_hdr_policy::prepare(Path::new(root), expected, Path::new(output), &capabilities)
        }
        #[cfg(target_os = "linux")]
        [command, bootstrap, package, worker, uid, gid]
            if command == "capture-authority-review" =>
        {
            capture_authority::review(
                Path::new(bootstrap),
                Path::new(package),
                Path::new(worker),
                uid,
                gid,
            )
        }
        #[cfg(target_os = "linux")]
        [
            command,
            bootstrap,
            package,
            worker,
            uid,
            gid,
            installation,
            confirmed,
        ] if command == "capture-authority-approve" => capture_authority::approve(
            Path::new(bootstrap),
            Path::new(package),
            Path::new(worker),
            uid,
            gid,
            Path::new(installation),
            confirmed,
        ),
        #[cfg(target_os = "linux")]
        [command, installation, id] if command == "capture-authority-revoke" => {
            capture_authority::revoke(Path::new(installation), id)
        }
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
                    | "pack-extension-example"
                    | "pack-registry-fixture"
                    | "pack-media-probe"
                    | "pack-stream-probe"
                    | "pack-software-probe"
                    | "pack-linux-probe"
                    | "pack-desktop-stream-smoke"
                    | "pack-linux-session"
                    | "pack-linux-session-rtx-hdr"
            ) =>
        {
            let sequence = sequence
                .parse()
                .map_err(|_| "sequence must be an unsigned integer")?;
            let profile = if command == "pack-linux-session-rtx-hdr" {
                profiles::PackageProfile::LinuxSessionRtxHdr
            } else if command == "pack-linux-session" {
                profiles::PackageProfile::LinuxSession
            } else if command == "pack-linux-probe" {
                profiles::PackageProfile::LinuxProbe
            } else if command == "pack-software-probe" {
                profiles::PackageProfile::SoftwareProbe
            } else if command == "pack-stream-probe" {
                profiles::PackageProfile::StreamProbe
            } else if command == "pack-desktop-stream-smoke" {
                profiles::PackageProfile::DesktopStreamSmoke
            } else if command == "pack-media-probe" {
                profiles::PackageProfile::MediaProbe
            } else if command == "pack-registry-fixture" {
                profiles::PackageProfile::RegistryFixture
            } else if command == "pack-extension-example" {
                profiles::PackageProfile::ExtensionExample
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
        [command, root, artifacts, output, sequence, options @ ..]
            if matches!(
                command.as_str(),
                "pack-linux-session"
                    | "pack-linux-session-rtx-hdr"
                    | "pack-linux-session-extensions"
                    | "pack-registry-fixture"
            ) && !options.is_empty() =>
        {
            let sequence = sequence
                .parse()
                .map_err(|_| "sequence must be an unsigned integer")?;
            let profile = match command.as_str() {
                "pack-linux-session" => profiles::PackageProfile::LinuxSession,
                "pack-linux-session-rtx-hdr" => profiles::PackageProfile::LinuxSessionRtxHdr,
                "pack-linux-session-extensions" => profiles::PackageProfile::LinuxSessionExtensions,
                _ => profiles::PackageProfile::RegistryFixture,
            };
            let mut external = Vec::new();
            let mut calls = None;
            let mut replacement = None;
            let mut bindings = Vec::new();
            let mut omitted = std::collections::BTreeSet::new();
            let mut remaining = options;
            while !remaining.is_empty() {
                match remaining {
                    [flag, template, artifact, rest @ ..] if flag == "--external-component" => {
                        external.push((Path::new(template), Path::new(artifact)));
                        remaining = rest;
                    }
                    [flag, path, rest @ ..] if flag == "--call-grants" && calls.is_none() => {
                        calls = Some(Path::new(path));
                        remaining = rest;
                    }
                    [flag, id, rest @ ..]
                        if flag == "--replace-library" && replacement.is_none() =>
                    {
                        replacement = Some(id.as_str());
                        remaining = rest;
                    }
                    [flag, consumer, provider, service, rest @ ..]
                        if flag == "--native-binding" || flag == "--api-binding" =>
                    {
                        bindings.push(development::ComponentBinding {
                            consumer: consumer.clone(),
                            provider: provider.clone(),
                            service: service.clone(),
                            native: flag == "--native-binding",
                        });
                        remaining = rest;
                    }
                    [flag, id, rest @ ..] if flag == "--omit-component" => {
                        if !omitted.insert(id.clone()) {
                            return Err("duplicate omitted component".into());
                        }
                        remaining = rest;
                    }
                    _ => return Err("invalid or duplicate external component options".into()),
                }
            }
            if (external.is_empty() && bindings.is_empty() && omitted.is_empty())
                || (command == "pack-linux-session-extensions" && calls.is_none())
            {
                return Err("profile requires external components and explicit call grants".into());
            }
            development::build_package_with_bindings(
                Path::new(root),
                Path::new(artifacts),
                Path::new(output),
                sequence,
                profile,
                &external,
                calls,
                replacement,
                &bindings,
                &omitted,
            )
        }
        [command, descriptor] if command == "extension-descriptor-digest" => {
            println!(
                "{}",
                development::read_extension_descriptor_digest(Path::new(descriptor))?
            );
            Ok(())
        }
        [command, descriptor] if command == "extension-validate-settings" => {
            let mut input = Vec::new();
            std::io::stdin()
                .take(1024 * 1024 + 1)
                .read_to_end(&mut input)
                .map_err(|error| error.to_string())?;
            if input.len() > 1024 * 1024 {
                return Err("settings validation input exceeds 1 MiB".into());
            }
            let batches: Vec<serde_json::Value> =
                serde_json::from_slice(&input).map_err(|error| error.to_string())?;
            development::validate_extension_settings(Path::new(descriptor), &batches)
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
    "usage:\n  amaterasu-release import-local-native ROOT REPOSITORY BUNDLE MANIFEST_DIGEST\n  amaterasu-release init ROOT\n  amaterasu-release review-package-activation-policy ROOT EXPECTED_BOOTSTRAP_SHA256 PUBLISHER_FINGERPRINT OUTPUT [--processing-selection SELECTION_JSON]\n  amaterasu-release provision-package-activation ROOT REVIEW_REPORT CONFIRMED_REPORT_SHA256\n  amaterasu-release provision-default-package-activation ROOT\n  amaterasu-release prepare-rtx-hdr-bootstrap ROOT EXPECTED_SHA256 OUTPUT [--processing-selection SELECTION_JSON]\n  amaterasu-release prepare-processing-bootstrap ROOT EXPECTED_SHA256 OUTPUT SELECTION_JSON COMPONENT_JSON SETTINGS_JSON [--metadata METADATA_JSON]\n  amaterasu-release init-linux-session ROOT [--external-extension-composition] [--lan ADDRESS:PORT [--allow-remote]] [--catalog-owner management|console] [--management-port PORT] [--capture-output ID] [--audio-endpoint NAME] [--enable-capture] [--enable-desktop-output-capture] [--enable-audio] [--enable-virtual-display] [--enable-virtual-screen-capture] [--enable-input] [--enable-display-mutation] [--rtx-hdr-runtime ABSOLUTE_PATH] [--frame-helper ABSOLUTE_PATH | --capture-authority-helper ABSOLUTE_PATH [--capture-authority-card CARD] [--capture-authority-precheck]]\n  amaterasu-release capture-authority-review BOOTSTRAP PACKAGE HOST UID GID (read-only)\n  amaterasu-release capture-authority-approve BOOTSTRAP PACKAGE HOST UID GID INSTALL_ROOT CONFIRMED_ID (explicit administrator action)\n  amaterasu-release capture-authority-revoke INSTALL_ROOT APPROVAL_ID (explicit administrator action)\n  amaterasu-release prepare-linux-user-service ROOT HOST_EXECUTABLE OUTPUT_DIRECTORY (Linux only; emits a reviewable bundle)\n  amaterasu-release preflight-linux-user-service ROOT HOST_EXECUTABLE BUNDLE_DIRECTORY (Linux only; read-only deployment checks)\n  amaterasu-release pack-linux-session ROOT ARTIFACTS OUTPUT SEQUENCE [--external-component TEMPLATE_JSON ARTIFACT ...] [--call-grants CALLS_JSON] [--native-binding CONSUMER PROVIDER SERVICE] [--api-binding CONSUMER PROVIDER SERVICE] [--omit-component ID ...] [--replace-library EXTERNAL_ID]\n  amaterasu-release pack-linux-session-rtx-hdr ROOT ARTIFACTS OUTPUT SEQUENCE [--omit-component ID ...] [--external-component TEMPLATE_JSON ARTIFACT ...] [--native-binding CONSUMER PROVIDER SERVICE] [--api-binding CONSUMER PROVIDER SERVICE]\n  amaterasu-release publish-rtx-hdr-repository BOOTSTRAP PACKAGE OUTPUT REPOSITORY_ID HTTPS_URL SIGNING_KEY_JSON CATALOG_SEQUENCE CATALOG_EXPIRY_UNIX\n  amaterasu-release pack-linux-session-extensions ROOT ARTIFACTS OUTPUT SEQUENCE --external-component TEMPLATE_JSON ARTIFACT [--external-component TEMPLATE_JSON ARTIFACT ...] --call-grants CALLS_JSON [--replace-library EXTERNAL_ID]\n  amaterasu-release init-desktop-stream-smoke ROOT [--allow-remote]\n  amaterasu-release preflight-desktop-stream-smoke ROOT [VHF_INSTALL_ROOT]\n  amaterasu-release pack ROOT ARTIFACTS OUTPUT SEQUENCE\n  amaterasu-release pack-platform ROOT ARTIFACTS OUTPUT SEQUENCE\n  amaterasu-release pack-extension-example ROOT ARTIFACTS OUTPUT SEQUENCE\n  amaterasu-release extension-descriptor-digest DESCRIPTOR_JSON\n  amaterasu-release pack-registry-fixture ROOT ARTIFACTS OUTPUT SEQUENCE [--external-component TEMPLATE_JSON ARTIFACT [--call-grants CALLS_JSON]]\n  amaterasu-release pack-media-probe ROOT ARTIFACTS OUTPUT SEQUENCE\n  amaterasu-release pack-stream-probe ROOT ARTIFACTS OUTPUT SEQUENCE\n  amaterasu-release pack-desktop-stream-smoke ROOT ARTIFACTS OUTPUT SEQUENCE\n  amaterasu-release pack-software-probe ROOT ARTIFACTS OUTPUT SEQUENCE\n  amaterasu-release pack-linux-probe ROOT ARTIFACTS OUTPUT SEQUENCE\n  amaterasu-release verify ROOT PACKAGE\n  amaterasu-release stage ROOT PACKAGE [--historical]\n  amaterasu-release pack-web ROOT DIST OUTPUT SEQUENCE\n  amaterasu-release stage-web ROOT PACKAGE\n  amaterasu-release repository-approve ROOT ID HTTPS_URL PUBLIC_KEY_HEX CONFIRMED_SHA256\n  amaterasu-release repository-revoke ROOT ID\n  amaterasu-release repository-refresh ROOT ID\n  amaterasu-release repository-list ROOT\n  amaterasu-release repository-download ROOT ID SLOT IMPLEMENTATION VERSION OUTPUT\n  amaterasu-release repository-install-ui ROOT ID IMPLEMENTATION VERSION OUTPUT\n  amaterasu-release repository-stage-native ROOT ID SLOT IMPLEMENTATION VERSION SOURCE\n  amaterasu-release repository-activate-native ROOT DIGEST\n  amaterasu-release repository-restore-ui ROOT [DIGEST]\n  amaterasu-release prepare-installer ARTIFACTS WEB_DIST OUTPUT\n  amaterasu-release seal-vhf-package PACKAGE CONFIRMED_FINGERPRINT SIGNTOOL\n  amaterasu-release verify-vhf-package PACKAGE CONFIRMED_FINGERPRINT SIGNTOOL\n  amaterasu-release install-vhf-package PACKAGE INSTALL_ROOT RUNTIME_ROOT CONFIRMED_FINGERPRINT SIGNTOOL DEVCON\n  amaterasu-release rollback-vhf-package INSTALL_ROOT RUNTIME_ROOT DEVCON\n  amaterasu-release grant-vhf-input INSTALL_ROOT RUNTIME_ROOT ACTIVE_SET_DIGEST KEYBOARD_0_OR_1 MOUSE_0_OR_1\n  amaterasu-release revoke-vhf-input INSTALL_ROOT RUNTIME_ROOT\n  Desktop smoke requires replacing SELECT_OUTPUT_ID with a current DXGI output ID in bootstrap.json.\n  --allow-remote selects wildcard stream listeners only; it does not alter firewall or machine network settings."
}
