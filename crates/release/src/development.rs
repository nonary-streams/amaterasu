//! Local-only publisher tooling. Generated keys are not production release identities.
use amaterasu_contracts::Grants;
use amaterasu_package::{
    ActivationState, BootstrapConfig, ComponentManifest, PackageResult, ReleaseManifest,
    ReleaseSignature, RootLock, VerificationPolicy, create_machine_private_directory,
    create_private_directory, current_target, decode_hex, encode_hex, package_directory,
    protect_machine_private_file, random_identifier, read_bounded, read_json, sha256,
    stage_package, unix_timestamp, verify_package, write_json_atomically,
};
use amaterasu_storage::write_atomically;
use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::c_void;

use crate::profiles::linux_session::{
    apply_management_bind, linux_session_policy, options::LinuxSessionOptions,
};
use crate::profiles::{
    ComponentDefinition, PackageProfile, RUNTIME_ID, desktop_stream_smoke_policy,
    development_policy, installer_policy,
};
use std::fs;
use std::path::{Component, Path, PathBuf};

const DEVELOPMENT_KEY_ID: &str = "local-development";
const INSTALLER_KEY_ID: &str = "installer-release";
pub const WEB_DIRECTORY_TOKEN: &str = "__AMATERASU_WEB_DIRECTORY__";
const INSTALLER_RELEASE_EXPIRY: u64 = u64::MAX;
const PROVISION_JOURNAL: &str = ".installer-provision.json";
const POLICY_TRANSITION_JOURNAL: &str = ".installer-policy-transition.json";
const POLICY_TRANSITION_BACKUPS: &str = "installer-policy-backups";
const MAX_TRANSITION_FILE_BYTES: u64 = 4 * 1024 * 1024;
const X264_BUILD_MARKER: &[u8] = b"AMATERASU_X264_BUILD=";
const TLS_BIND_ENV: &str = "AMATERASU_MANAGEMENT_TLS_BIND";
const TLS_CERT_ENV: &str = "AMATERASU_MANAGEMENT_TLS_CERTIFICATE_PATH";
const TLS_KEY_ENV: &str = "AMATERASU_MANAGEMENT_TLS_PRIVATE_KEY_PATH";
const WORKER_STATE_ROOT_TOKEN: &str = "__AMATERASU_WORKER_STATE_ROOT__";

struct InstallerTls {
    bind: std::net::SocketAddr,
    certificate: Vec<u8>,
    private_key: Vec<u8>,
}

impl InstallerTls {
    fn from_environment() -> PackageResult<Option<Self>> {
        let values = [TLS_BIND_ENV, TLS_CERT_ENV, TLS_KEY_ENV].map(std::env::var_os);
        if values.iter().all(Option::is_none) {
            return Ok(None);
        }
        let [Some(bind), Some(certificate), Some(private_key)] = values else {
            return Err(
                "management TLS bind, certificate and private key must be supplied together".into(),
            );
        };
        let bind: std::net::SocketAddr = bind
            .to_str()
            .ok_or("management TLS bind is invalid")?
            .parse()
            .map_err(|_| "management TLS bind is invalid")?;
        if !bind.ip().is_loopback() || bind.port() == 0 {
            return Err("management TLS bind must be an explicit loopback port".into());
        }
        let certificate = PathBuf::from(certificate);
        let private_key = PathBuf::from(private_key);
        if !certificate.is_absolute() || !private_key.is_absolute() || certificate == private_key {
            return Err("management TLS requires distinct absolute source paths".into());
        }
        for path in [&certificate, &private_key] {
            let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
            if !metadata.is_file() || is_reparse_or_symlink(&metadata) {
                return Err("management TLS source must be a regular file".into());
            }
        }
        let certificate = read_bounded(&certificate, 64 * 1024)?;
        let private_key = read_bounded(&private_key, 16 * 1024)?;
        if certificate.is_empty() || private_key.is_empty() {
            return Err("management TLS source cannot be empty".into());
        }
        Ok(Some(Self {
            bind,
            certificate,
            private_key,
        }))
    }

    fn paths(&self, root: &Path) -> (PathBuf, PathBuf) {
        let directory = root.join("management-tls");
        (
            directory.join(format!("management-cert-{}.pem", sha256(&self.certificate))),
            directory.join(format!("management-key-{}.pem", sha256(&self.private_key))),
        )
    }

    fn apply_settings(&self, bootstrap: &mut BootstrapConfig, root: &Path) -> PackageResult<()> {
        let (certificate, private_key) = self.paths(root);
        let certificate = format!(
            "{WORKER_STATE_ROOT_TOKEN}/management-tls/{}",
            certificate
                .file_name()
                .ok_or("TLS filename missing")?
                .to_string_lossy()
        );
        let private_key = format!(
            "{WORKER_STATE_ROOT_TOKEN}/management-tls/{}",
            private_key
                .file_name()
                .ok_or("TLS filename missing")?
                .to_string_lossy()
        );
        let management = bootstrap
            .policy
            .settings
            .get_mut("amaterasu.management")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or("installer bootstrap lacks management policy")?;
        if management.get("bind").and_then(serde_json::Value::as_str)
            == Some(self.bind.to_string().as_str())
        {
            return Err("management TLS bind conflicts with HTTP bind".into());
        }
        management.insert("tls_bind".into(), serde_json::json!(self.bind));
        management.insert(
            "tls_certificate_path".into(),
            serde_json::json!(certificate),
        );
        management.insert(
            "tls_private_key_path".into(),
            serde_json::json!(private_key),
        );
        Ok(())
    }

    fn write(
        &self,
        root: &Path,
        machine: bool,
        mut record: impl FnMut(&Path, &[u8]) -> PackageResult<()>,
    ) -> PackageResult<()> {
        let directory = root.join("management-tls");
        create_machine_private_directory(&directory)?;
        let metadata = fs::symlink_metadata(&directory).map_err(|error| error.to_string())?;
        if !metadata.is_dir() || is_reparse_or_symlink(&metadata) {
            return Err("management TLS destination is not a real directory".into());
        }
        let (certificate, private_key) = self.paths(root);
        for (path, bytes) in [
            (&certificate, &self.certificate),
            (&private_key, &self.private_key),
        ] {
            if path.exists() {
                if read_bounded(path, bytes.len() as u64)? != *bytes {
                    return Err("management TLS destination changed".into());
                }
            } else {
                record(
                    path.strip_prefix(root).map_err(|error| error.to_string())?,
                    bytes,
                )?;
                write_atomically(path, bytes).map_err(|error| error.to_string())?;
            }
            if machine {
                protect_machine_private_file(path)?;
            }
        }
        Ok(())
    }
}

fn is_reparse_or_symlink(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    false
}

pub fn initialize(root: &Path) -> PackageResult<()> {
    initialize_profile(root, DevelopmentProfile::Standard)
}

pub fn initialize_desktop_stream_smoke(root: &Path, allow_remote: bool) -> PackageResult<()> {
    initialize_profile(root, DevelopmentProfile::DesktopStreamSmoke(allow_remote))
}

pub fn initialize_linux_session(root: &Path, options: &LinuxSessionOptions) -> PackageResult<()> {
    initialize_profile(root, DevelopmentProfile::LinuxSession(options))
}

enum DevelopmentProfile<'a> {
    Standard,
    DesktopStreamSmoke(bool),
    LinuxSession(&'a LinuxSessionOptions),
}

fn initialize_profile(root: &Path, profile: DevelopmentProfile<'_>) -> PackageResult<()> {
    let tls = InstallerTls::from_environment()?;
    if root.join("bootstrap.json").exists() || root.join("development-key.json").exists() {
        return Err(
            "development root is already initialized; refusing to replace its trust identity"
                .into(),
        );
    }
    create_private_directory(root)?;

    let mut secret = [0; 32];
    getrandom::fill(&mut secret).map_err(|error| error.to_string())?;
    let signing_key = SigningKey::from_bytes(&secret);
    let mut bootstrap = BootstrapConfig {
        development: true,
        allow_unsigned: false,
        trusted_keys: BTreeMap::from([(
            DEVELOPMENT_KEY_ID.into(),
            encode_hex(signing_key.verifying_key().as_bytes()),
        )]),
        policy: match &profile {
            DevelopmentProfile::Standard => development_policy(root)?,
            DevelopmentProfile::DesktopStreamSmoke(allow_remote) => {
                desktop_stream_smoke_policy(root, *allow_remote)?
            }
            DevelopmentProfile::LinuxSession(options) => linux_session_policy(root, options)?,
        },
    };
    let web_directory = root.join("web");
    create_private_directory(&web_directory)?;
    bootstrap.policy.settings.insert("amaterasu.management".into(), serde_json::json!({
        "enabled": false,
        "web": {"directory": web_directory.canonicalize().map_err(|error| error.to_string())?, "trusted_keys": bootstrap.trusted_keys}
    }));
    if let DevelopmentProfile::LinuxSession(options) = &profile {
        let management = bootstrap
            .policy
            .settings
            .get_mut("amaterasu.management")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or("development bootstrap lacks management settings")?;
        apply_management_bind(management, options.management_port);
    }
    if let Some(tls) = &tls {
        let state_root = root.join("data/state");
        create_private_directory(&state_root)?;
        tls.write(&state_root, false, |_, _| Ok(()))?;
        let management = bootstrap
            .policy
            .settings
            .get_mut("amaterasu.management")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or("development bootstrap lacks management settings")?;
        management.insert("enabled".into(), serde_json::json!(true));
        management.insert("bind".into(), serde_json::json!("127.0.0.1:0"));
        tls.apply_settings(&mut bootstrap, &state_root)?;
    }

    write_json_atomically(&root.join("development-key.json"), &encode_hex(&secret))?;
    write_json_atomically(&root.join("bootstrap.json"), &bootstrap)?;
    write_json_atomically(&root.join("selection.json"), &ActivationState::default())
}

pub fn build_package(
    root: &Path,
    artifacts: &Path,
    output: &Path,
    sequence: u64,
    profile: PackageProfile,
) -> PackageResult<()> {
    let bootstrap: BootstrapConfig = read_json(&root.join("bootstrap.json"))?;
    if !bootstrap.development {
        return Err("development publisher cannot use a production root".into());
    }
    let secret: String = read_json(&root.join("development-key.json"))?;
    let signing_key = SigningKey::from_bytes(&decode_hex::<32>(&secret)?);
    build_signed_package(
        artifacts,
        output,
        sequence,
        profile,
        &signing_key,
        DEVELOPMENT_KEY_ID,
    )
}

/// Make signed installer seed assets without writing a signing private key into
/// them. Provisioning later substitutes only the machine-local state path.
pub fn prepare_installer_assets(
    artifacts: &Path,
    web_source: &Path,
    output: &Path,
) -> PackageResult<()> {
    let signing_key = installer_signing_key()?;
    prepare_installer_assets_with_key(artifacts, web_source, output, &signing_key)
}

fn prepare_installer_assets_with_key(
    artifacts: &Path,
    web_source: &Path,
    output: &Path,
    signing_key: &SigningKey,
) -> PackageResult<()> {
    if output.try_exists().map_err(|error| error.to_string())? {
        return Err("installer asset output already exists".into());
    }
    create_private_directory(output)?;
    let bootstrap = BootstrapConfig {
        development: false,
        allow_unsigned: false,
        trusted_keys: BTreeMap::from([(
            INSTALLER_KEY_ID.into(),
            encode_hex(signing_key.verifying_key().as_bytes()),
        )]),
        policy: installer_policy(
            WEB_DIRECTORY_TOKEN,
            BTreeMap::from([(
                INSTALLER_KEY_ID.into(),
                encode_hex(signing_key.verifying_key().as_bytes()),
            )]),
        ),
    };
    write_json_atomically(&output.join("bootstrap-template.json"), &bootstrap)?;
    build_signed_package(
        artifacts,
        &output.join("package"),
        1,
        PackageProfile::Platform,
        signing_key,
        INSTALLER_KEY_ID,
    )?;
    let web_package = output.join("web-package");
    let web_manifest = amaterasu_package::web::prepare_bundle(
        web_source,
        &web_package,
        1,
        INSTALLER_RELEASE_EXPIRY,
    )?;
    sign_manifest_with_key(&web_package, &web_manifest, signing_key, INSTALLER_KEY_ID)?;
    amaterasu_package::web::stage_bundle(
        &web_package,
        &output.join("web"),
        &bootstrap.trusted_keys,
        unix_timestamp(),
    )?;
    fs::remove_dir_all(web_package).map_err(|error| error.to_string())
}

fn installer_signing_key() -> PackageResult<SigningKey> {
    let encoded = std::env::var("AMATERASU_INSTALLER_SIGNING_KEY")
        .map_err(|_| "AMATERASU_INSTALLER_SIGNING_KEY is required for installer seed creation")?;
    Ok(SigningKey::from_bytes(&decode_hex::<32>(&encoded)?))
}

/// Provision the signed installer package into a fresh machine-local root.
/// The MSI runs this after its files are present, so the signed package itself
/// is never rebuilt or re-signed on the installed machine.
pub fn provision_installer_root(template: &Path, package: &Path, root: &Path) -> PackageResult<()> {
    let tls = InstallerTls::from_environment()?;
    if root
        .join("bootstrap.json")
        .try_exists()
        .map_err(|error| error.to_string())?
    {
        return Err("installer runtime root already exists; refusing to replace it".into());
    }
    create_machine_private_directory(root)?;
    let result = provision_installer_root_inner(template, package, root, tls.as_ref());
    if result.is_err() {
        let _ = unprovision_if_created(root);
    }
    result
}

/// MSI rollback boundary. Only journaled, unchanged files are removable; the
/// runtime root itself and preexisting migration backups are never targets.
pub fn unprovision_if_created(root: &Path) -> PackageResult<()> {
    let journal_path = root.join(PROVISION_JOURNAL);
    if !journal_path
        .try_exists()
        .map_err(|error| error.to_string())?
    {
        return rollback_policy_transition(root);
    }
    let _lock = RootLock::acquire(root)?;
    let journal: ProvisionJournal = read_json(&journal_path)?;
    validate_journal(&journal)?;
    for file in &journal.files {
        let path = root.join(&file.path);
        if path.try_exists().map_err(|error| error.to_string())?
            && sha256(&read_bounded(&path, 256 * 1024 * 1024)?) != file.sha256
        {
            return Err("installer provision artifact changed; refusing rollback".into());
        }
    }
    for file in journal.files.iter().rev() {
        let path = root.join(&file.path);
        if path.try_exists().map_err(|error| error.to_string())? {
            fs::remove_file(path).map_err(|error| error.to_string())?;
        }
    }
    for directory in journal.directories.iter().rev() {
        let path = root.join(directory);
        if path.try_exists().map_err(|error| error.to_string())? {
            match fs::remove_dir(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {}
                Err(error) => return Err(error.to_string()),
            }
        }
    }
    fs::remove_file(journal_path).map_err(|error| error.to_string())
}

/// Marks a completed installer transaction permanent by removing its rollback
/// journal. It never alters provisioned runtime files.
pub fn finalize_installer_provision(root: &Path) -> PackageResult<()> {
    let path = root.join(PROVISION_JOURNAL);
    if !path.try_exists().map_err(|error| error.to_string())? {
        return finalize_policy_transition(root);
    }
    let _lock = RootLock::acquire(root)?;
    let journal: ProvisionJournal = read_json(&path)?;
    validate_journal(&journal)?;
    fs::remove_file(path).map_err(|error| error.to_string())
}

/// Install a signed seed into a new root or stage a newer seed into an existing
/// root. Existing roots retain their trust configuration; a seed signed by a
/// different key is rejected before it can change selection state.
pub fn install_installer_root(template: &Path, package: &Path, root: &Path) -> PackageResult<()> {
    if root
        .join("bootstrap.json")
        .try_exists()
        .map_err(|error| error.to_string())?
    {
        let _lock = RootLock::acquire(root)?;
        install_existing_installer_root(template, package, root, false)
    } else {
        provision_installer_root(template, package, root)
    }
}

/// Transitions installer-owned development roots and exact generated production
/// authorization settings and host grants under the same signer. Locally
/// customized policy and key rotation require an explicit migration.
fn install_existing_installer_root(
    template: &Path,
    package: &Path,
    root: &Path,
    inject_failure_after_switch: bool,
) -> PackageResult<()> {
    let tls = InstallerTls::from_environment()?;
    let bootstrap_path = root.join("bootstrap.json");
    let selection_path = root.join("selection.json");
    let bootstrap_bytes = read_bounded(&bootstrap_path, MAX_TRANSITION_FILE_BYTES)?;
    let current: BootstrapConfig =
        serde_json::from_slice(&bootstrap_bytes).map_err(|error| error.to_string())?;
    let current_has_tls = validate_existing_tls_settings(&current)?;
    if current.development && current_has_tls && tls.is_none() {
        return Err(
            "development TLS material must be supplied again for production transition".into(),
        );
    }
    let mut candidate = materialize_installer_bootstrap(template, root)?;
    if let Some(tls) = &tls {
        tls.apply_settings(&mut candidate, root)?;
    }
    require_installer_signer_continuity(&current, &candidate)?;

    let selection_bytes = read_bounded(&selection_path, MAX_TRANSITION_FILE_BYTES)?;
    let selection: ActivationState =
        serde_json::from_slice(&selection_bytes).map_err(|error| error.to_string())?;
    let active = selection
        .active
        .as_deref()
        .ok_or("existing installer runtime has no active package")?;
    let policy = VerificationPolicy {
        minimum_sequence: 0,
        allow_historical: true,
        now: unix_timestamp(),
    };
    let current_package = verify_package(&package_directory(root, active)?, &current, policy)?;
    if !current_package.signed {
        return Err("existing installer runtime package is unsigned".into());
    }
    let next_package = verify_package(package, &candidate, policy)?;
    if !next_package.signed {
        return Err("installer update package is unsigned".into());
    }
    verify_installer_web_bundle(template, &candidate)?;

    // Exact generated production grants and authorization settings may gain
    // capabilities under the same signer. Everything else stays local.
    if !current.development {
        let (upgraded_auth, authorization_result) =
            production_authorization_upgrade(&current, &candidate);
        let (upgraded_host, host_result) =
            production_host_configuration_upgrade(&current, &candidate);
        let has_upgrade = upgraded_auth.is_some() || upgraded_host.is_some() || tls.is_some();
        let mut upgraded = upgraded_auth.unwrap_or_else(|| current.clone());
        if let Some(grants) = upgraded_host {
            upgraded
                .policy
                .principals
                .insert("amaterasu.host-configuration".into(), grants);
        }
        if let Some(tls) = &tls {
            tls.apply_settings(&mut upgraded, root)?;
        }
        let upgraded = has_upgrade.then_some(upgraded);
        if let Some(upgraded) = upgraded {
            transition_installer_policy(
                root,
                package,
                &current,
                Some(&upgraded),
                &next_package.digest,
                active,
                &bootstrap_bytes,
                &selection_bytes,
                inject_failure_after_switch,
                tls.as_ref(),
            )?;
        } else if next_package.digest != active
            && selection.pending.as_deref() != Some(next_package.digest.as_str())
        {
            transition_installer_policy(
                root,
                package,
                &current,
                None,
                &next_package.digest,
                active,
                &bootstrap_bytes,
                &selection_bytes,
                inject_failure_after_switch,
                None,
            )?;
        } else if selection.pending.as_deref() == Some(next_package.digest.as_str()) {
            verify_package(
                &package_directory(root, &next_package.digest)?,
                &current,
                policy,
            )?;
        }
        eprintln!("installer authorization policy: {authorization_result}");
        eprintln!("installer host configuration grant: {host_result}");
        return Ok(());
    }

    transition_installer_policy(
        root,
        package,
        &current,
        Some(&candidate),
        &next_package.digest,
        active,
        &bootstrap_bytes,
        &selection_bytes,
        inject_failure_after_switch,
        tls.as_ref(),
    )
}

fn validate_existing_tls_settings(bootstrap: &BootstrapConfig) -> PackageResult<bool> {
    let Some(management) = bootstrap
        .policy
        .settings
        .get("amaterasu.management")
        .and_then(serde_json::Value::as_object)
    else {
        return Ok(false);
    };
    let fields = ["tls_bind", "tls_certificate_path", "tls_private_key_path"];
    let present = fields.map(|field| management.get(field).is_some_and(|value| !value.is_null()));
    if present.iter().any(|value| *value) && !present.iter().all(|value| *value) {
        return Err("installed management TLS settings are incomplete".into());
    }
    Ok(present[0])
}

#[derive(Debug, PartialEq, Eq)]
enum HostConfigurationGrantResult {
    Upgraded,
    AlreadyAvailable,
    PreservedWithoutDiagnosticsGrant,
}

impl std::fmt::Display for HostConfigurationGrantResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Upgraded => write!(
                formatter,
                "generated grant upgraded; diagnostics apply available"
            ),
            Self::AlreadyAvailable => write!(
                formatter,
                "local grant preserved; diagnostics apply available"
            ),
            Self::PreservedWithoutDiagnosticsGrant => write!(
                formatter,
                "local grant preserved; diagnostics apply remains unavailable"
            ),
        }
    }
}

fn production_host_configuration_upgrade(
    current: &BootstrapConfig,
    candidate: &BootstrapConfig,
) -> (Option<Grants>, HostConfigurationGrantResult) {
    let name = "amaterasu.host-configuration";
    // Exact generated grants before host logging could call diagnostics.
    let previous = Grants {
        provide: BTreeSet::from(["amaterasu.host-configuration/1".into()]),
        call: BTreeSet::from(["amaterasu.state/1".into()]),
        ..Default::default()
    };
    let mut expected = previous.clone();
    expected.call.insert("amaterasu.diagnostics/1".into());
    let installed = current.policy.principals.get(name);
    let offered = candidate.policy.principals.get(name);
    let matches = |actual: Option<&Grants>, expected: &Grants| {
        actual.and_then(|grants| serde_json::to_value(grants).ok())
            == serde_json::to_value(expected).ok()
    };
    if matches(installed, &previous) && matches(offered, &expected) {
        return (Some(expected), HostConfigurationGrantResult::Upgraded);
    }
    let has_grant = installed.is_some_and(|grants| grants.call.contains("amaterasu.diagnostics/1"));
    (
        None,
        if has_grant {
            HostConfigurationGrantResult::AlreadyAvailable
        } else {
            HostConfigurationGrantResult::PreservedWithoutDiagnosticsGrant
        },
    )
}

#[derive(Debug, PartialEq, Eq)]
enum ProductionAuthorizationResult {
    Upgraded,
    ScopeListedInCustomPolicy,
    PreservedWithoutLutrisScope,
}

impl std::fmt::Display for ProductionAuthorizationResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Upgraded => write!(
                formatter,
                "generated policy upgraded; lutris.read available"
            ),
            Self::ScopeListedInCustomPolicy => {
                write!(formatter, "custom policy preserved; lutris.read is listed")
            }
            Self::PreservedWithoutLutrisScope => write!(
                formatter,
                "custom policy preserved; lutris.read remains unavailable"
            ),
        }
    }
}

fn production_authorization_upgrade(
    current: &BootstrapConfig,
    candidate: &BootstrapConfig,
) -> (Option<BootstrapConfig>, ProductionAuthorizationResult) {
    let name = "amaterasu.authentication";
    let Some(generated) = installer_policy(WEB_DIRECTORY_TOKEN, current.trusted_keys.clone())
        .settings
        .remove(name)
    else {
        return (
            None,
            ProductionAuthorizationResult::PreservedWithoutLutrisScope,
        );
    };
    // Immutable generated installer setting from before lutris.read was added.
    // Keep this literal independent of future installer_policy changes.
    let previous = serde_json::json!({
        "administrator_principals": ["amaterasu.management"],
        "authorization_policy_generation": 2,
        "session_token_ttl_seconds": 7200,
        "remember_me_refresh_token_ttl_seconds": 604800,
        "allowed_scopes": [
            "catalog.read", "catalog.write", "configuration.read", "configuration.write",
            "session.read", "session.write", "application.read", "application.launch",
            "application.terminate", "steam.read", "steam.refresh", "diagnostics.read",
            "diagnostics.write", "pairing.admin", "display.read", "display.write", "media.read"
        ],
    });
    let mut expected_current = previous.clone();
    expected_current["authorization_policy_generation"] = serde_json::json!(3);
    expected_current["allowed_scopes"]
        .as_array_mut()
        .expect("historical installer scopes are an array")
        .insert(11, serde_json::json!("lutris.read"));
    let installed = current.policy.settings.get(name);
    let offered = candidate.policy.settings.get(name);
    if installed == Some(&previous) && generated == expected_current && offered == Some(&generated)
    {
        let mut upgraded = current.clone();
        upgraded.policy.settings.insert(name.into(), generated);
        return (Some(upgraded), ProductionAuthorizationResult::Upgraded);
    }
    let has_scope = installed
        .and_then(|settings| settings.get("allowed_scopes"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|scopes| scopes.iter().any(|scope| scope == "lutris.read"));
    (
        None,
        if has_scope {
            ProductionAuthorizationResult::ScopeListedInCustomPolicy
        } else {
            ProductionAuthorizationResult::PreservedWithoutLutrisScope
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn transition_installer_policy(
    root: &Path,
    package: &Path,
    current: &BootstrapConfig,
    replacement: Option<&BootstrapConfig>,
    next_digest: &str,
    active: &str,
    bootstrap_bytes: &[u8],
    selection_bytes: &[u8],
    inject_failure_after_switch: bool,
    tls: Option<&InstallerTls>,
) -> PackageResult<()> {
    let bootstrap_path = root.join("bootstrap.json");
    let transition = PolicyTransition::begin(root, bootstrap_bytes, selection_bytes)?;
    let result = (|| {
        if let Some(tls) = tls {
            tls.write(root, true, |path, bytes| {
                transition.record_tls_file(path, bytes)
            })?;
        }
        if next_digest != active {
            transition.record_new_set(next_digest)?;
            let _ = stage_package(package, root, current, false)?;
            transition.record_replacement(root, Path::new("selection.json"))?;
        }
        if let Some(replacement) = replacement {
            write_json_atomically(&bootstrap_path, replacement)?;
            transition.record_replacement(root, Path::new("bootstrap.json"))?;
        }
        if inject_failure_after_switch {
            return Err("injected installer policy switch failure".into());
        }
        Ok(())
    })();
    if let Err(error) = result {
        rollback_policy_transition_locked(root)
            .map_err(|rollback| format!("{error}; policy rollback failed: {rollback}"))?;
        return Err(error);
    }
    Ok(())
}

fn require_installer_signer_continuity(
    current: &BootstrapConfig,
    candidate: &BootstrapConfig,
) -> PackageResult<()> {
    if current.allow_unsigned
        || candidate.allow_unsigned
        || candidate.development
        || current.trusted_keys != candidate.trusted_keys
        || current.trusted_keys.len() != 1
        || !current.trusted_keys.contains_key(INSTALLER_KEY_ID)
    {
        return Err("installer signer continuity check failed; refusing policy transition".into());
    }
    Ok(())
}

fn verify_installer_web_bundle(template: &Path, bootstrap: &BootstrapConfig) -> PackageResult<()> {
    let directory = template
        .parent()
        .ok_or("installer template has no containing directory")?
        .join("web");
    let selected = amaterasu_package::web::load_selected(
        &directory,
        &bootstrap.trusted_keys,
        unix_timestamp(),
    )?;
    if selected.manifest.expires_unix != INSTALLER_RELEASE_EXPIRY {
        return Err("installer web bundle has an unexpected expiry".into());
    }
    Ok(())
}

fn materialize_installer_bootstrap(
    template: &Path,
    _root: &Path,
) -> PackageResult<BootstrapConfig> {
    let mut bootstrap: BootstrapConfig = read_json(template)?;
    if bootstrap.development || bootstrap.allow_unsigned {
        return Err("installer bootstrap must be a signed production policy".into());
    }
    let web_directory = template
        .parent()
        .ok_or("installer template has no containing directory")?
        .join("web")
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let management = bootstrap
        .policy
        .settings
        .get_mut("amaterasu.management")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or("installer bootstrap lacks management policy")?;
    let web = management
        .get_mut("web")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or("installer management policy lacks web settings")?;
    if web.get("directory").and_then(|value| value.as_str()) != Some(WEB_DIRECTORY_TOKEN) {
        return Err("installer management web policy is not a template".into());
    }
    web.insert("directory".into(), serde_json::json!(web_directory));
    Ok(bootstrap)
}

/// A short-lived rollback record for an authenticated development-to-production
/// policy transition. It contains exact copies of only installer-owned
/// metadata; application state remains outside its scope.
struct PolicyTransition {
    root: PathBuf,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PolicyTransitionJournal {
    schema: u32,
    backup_directory: PathBuf,
    backups: Vec<TransitionBackup>,
    replacements: Vec<TransitionReplacement>,
    created_set: Option<String>,
    #[serde(default)]
    created_tls: Vec<JournalFile>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TransitionBackup {
    path: PathBuf,
    backup: PathBuf,
    sha256: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TransitionReplacement {
    path: PathBuf,
    sha256: String,
}

impl PolicyTransition {
    fn begin(root: &Path, bootstrap: &[u8], selection: &[u8]) -> PackageResult<Self> {
        let journal_path = root.join(POLICY_TRANSITION_JOURNAL);
        if journal_path
            .try_exists()
            .map_err(|error| error.to_string())?
        {
            return Err(
                "previous installer policy transition requires rollback or finalization".into(),
            );
        }
        let identifier = random_identifier()?;
        let backup_directory = PathBuf::from(POLICY_TRANSITION_BACKUPS).join(identifier);
        let absolute_backup = root.join(&backup_directory);
        create_private_directory(&absolute_backup)?;
        let backups = [
            (Path::new("bootstrap.json"), bootstrap),
            (Path::new("selection.json"), selection),
        ]
        .into_iter()
        .map(|(path, bytes)| {
            let backup = backup_directory.join(path);
            write_atomically(&root.join(&backup), bytes).map_err(|error| error.to_string())?;
            Ok(TransitionBackup {
                path: path.to_path_buf(),
                backup,
                sha256: sha256(bytes),
            })
        })
        .collect::<PackageResult<Vec<_>>>()?;
        let journal = PolicyTransitionJournal {
            schema: 1,
            backup_directory,
            backups,
            replacements: Vec::new(),
            created_set: None,
            created_tls: Vec::new(),
        };
        validate_policy_transition(&journal)?;
        write_json_atomically(&journal_path, &journal)?;
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    fn record_new_set(&self, digest: &str) -> PackageResult<()> {
        let mut journal = self.read()?;
        if self
            .root
            .join("sets")
            .join(digest)
            .try_exists()
            .map_err(|error| error.to_string())?
        {
            return Ok(());
        }
        if journal.created_set.replace(digest.into()).is_some() {
            return Err("installer policy transition already records a package set".into());
        }
        self.write(&journal)
    }

    fn record_tls_file(&self, path: &Path, bytes: &[u8]) -> PackageResult<()> {
        let mut journal = self.read()?;
        journal.created_tls.push(JournalFile {
            path: path.to_path_buf(),
            sha256: sha256(bytes),
        });
        self.write(&journal)
    }

    fn record_replacement(&self, root: &Path, path: &Path) -> PackageResult<()> {
        let mut journal = self.read()?;
        if journal.replacements.iter().any(|entry| entry.path == path) {
            return Err("installer policy transition recorded a metadata file twice".into());
        }
        let bytes = read_bounded(&root.join(path), MAX_TRANSITION_FILE_BYTES)?;
        journal.replacements.push(TransitionReplacement {
            path: path.to_path_buf(),
            sha256: sha256(&bytes),
        });
        self.write(&journal)
    }

    fn read(&self) -> PackageResult<PolicyTransitionJournal> {
        let journal: PolicyTransitionJournal =
            read_json(&self.root.join(POLICY_TRANSITION_JOURNAL))?;
        validate_policy_transition(&journal)?;
        Ok(journal)
    }

    fn write(&self, journal: &PolicyTransitionJournal) -> PackageResult<()> {
        validate_policy_transition(journal)?;
        write_json_atomically(&self.root.join(POLICY_TRANSITION_JOURNAL), journal)
    }
}

fn rollback_policy_transition(root: &Path) -> PackageResult<()> {
    let _lock = RootLock::acquire(root)?;
    rollback_policy_transition_locked(root)
}

fn rollback_policy_transition_locked(root: &Path) -> PackageResult<()> {
    let journal_path = root.join(POLICY_TRANSITION_JOURNAL);
    if !journal_path
        .try_exists()
        .map_err(|error| error.to_string())?
    {
        return Ok(());
    }
    let journal: PolicyTransitionJournal = read_json(&journal_path)?;
    validate_policy_transition(&journal)?;
    for replacement in &journal.replacements {
        let bytes = read_bounded(&root.join(&replacement.path), MAX_TRANSITION_FILE_BYTES)?;
        if sha256(&bytes) != replacement.sha256 {
            return Err(
                "installer policy metadata changed after transition; refusing rollback".into(),
            );
        }
    }
    for backup in journal.backups.iter().rev() {
        let bytes = read_bounded(&root.join(&backup.backup), MAX_TRANSITION_FILE_BYTES)?;
        if sha256(&bytes) != backup.sha256 {
            return Err("installer policy backup integrity check failed".into());
        }
        write_atomically(&root.join(&backup.path), &bytes).map_err(|error| error.to_string())?;
    }
    for file in &journal.created_tls {
        if root.join(&file.path).exists()
            && sha256(&read_bounded(&root.join(&file.path), 64 * 1024)?) != file.sha256
        {
            return Err("management TLS file changed after transition; refusing rollback".into());
        }
    }
    if let Some(digest) = &journal.created_set {
        let directory = package_directory(root, digest)?;
        if directory.try_exists().map_err(|error| error.to_string())? {
            fs::remove_dir_all(&directory).map_err(|error| error.to_string())?;
        }
    }
    for file in journal.created_tls.iter().rev() {
        if root.join(&file.path).exists() {
            fs::remove_file(root.join(&file.path)).map_err(|error| error.to_string())?;
        }
    }
    let backup = root.join(&journal.backup_directory);
    fs::remove_dir_all(backup).map_err(|error| error.to_string())?;
    fs::remove_file(journal_path).map_err(|error| error.to_string())
}

fn finalize_policy_transition(root: &Path) -> PackageResult<()> {
    let _lock = RootLock::acquire(root)?;
    finalize_policy_transition_locked(root)
}

fn finalize_policy_transition_locked(root: &Path) -> PackageResult<()> {
    let journal_path = root.join(POLICY_TRANSITION_JOURNAL);
    if !journal_path
        .try_exists()
        .map_err(|error| error.to_string())?
    {
        return Ok(());
    }
    let journal: PolicyTransitionJournal = read_json(&journal_path)?;
    validate_policy_transition(&journal)?;
    fs::remove_dir_all(root.join(&journal.backup_directory)).map_err(|error| error.to_string())?;
    fs::remove_file(journal_path).map_err(|error| error.to_string())
}

fn validate_policy_transition(journal: &PolicyTransitionJournal) -> PackageResult<()> {
    if journal.schema != 1
        || journal.backups.len() != 2
        || journal.replacements.len() > 2
        || !normal_relative(&journal.backup_directory)
        || journal.backup_directory.components().count() != 2
        || journal
            .backup_directory
            .components()
            .next()
            .is_none_or(|part| part.as_os_str() != POLICY_TRANSITION_BACKUPS)
        || journal.backups.iter().any(|entry| {
            !matches!(
                entry.path.to_string_lossy().as_ref(),
                "bootstrap.json" | "selection.json"
            ) || !normal_relative(&entry.backup)
                || !entry.backup.starts_with(&journal.backup_directory)
                || !valid_digest(&entry.sha256)
        })
        || journal.replacements.iter().any(|entry| {
            !matches!(
                entry.path.to_string_lossy().as_ref(),
                "bootstrap.json" | "selection.json"
            ) || !valid_digest(&entry.sha256)
        })
        || journal
            .created_set
            .as_deref()
            .is_some_and(|digest| !valid_digest(digest))
        || journal
            .created_tls
            .iter()
            .any(|file| !valid_tls_seed_file(&file.path) || !valid_digest(&file.sha256))
    {
        return Err("installer policy transition journal is invalid".into());
    }
    let paths: std::collections::BTreeSet<_> =
        journal.backups.iter().map(|entry| &entry.path).collect();
    if paths.len() != 2
        || !paths.contains(&PathBuf::from("bootstrap.json"))
        || !paths.contains(&PathBuf::from("selection.json"))
    {
        return Err("installer policy transition backups are incomplete".into());
    }
    Ok(())
}

fn valid_tls_seed_file(path: &Path) -> bool {
    normal_relative(path)
        && path.components().count() == 2
        && path.starts_with("management-tls")
        && path.file_name().is_some_and(|name| {
            let name = name.to_string_lossy();
            let hash = name
                .strip_prefix("management-cert-")
                .or_else(|| name.strip_prefix("management-key-"));
            hash.and_then(|name| name.strip_suffix(".pem"))
                .is_some_and(valid_digest)
        })
}

fn provision_installer_root_inner(
    template: &Path,
    package: &Path,
    root: &Path,
    tls: Option<&InstallerTls>,
) -> PackageResult<()> {
    let _lock = RootLock::acquire(root)?;
    let mut bootstrap = materialize_installer_bootstrap(template, root)?;
    if let Some(tls) = tls {
        tls.apply_settings(&mut bootstrap, root)?;
    }
    let mut journal = ProvisionJournal::default();
    write_journal(root, &mut journal)?;
    create_journaled_directory(root, Path::new("data"), &mut journal)?;
    create_journaled_directory(root, Path::new("data/state"), &mut journal)?;
    if let Some(tls) = tls {
        create_journaled_directory(root, Path::new("management-tls"), &mut journal)?;
        write_journal(root, &mut journal)?;
        tls.write(root, true, |path, bytes| {
            journal.files.push(JournalFile {
                path: path.to_path_buf(),
                sha256: sha256(bytes),
            });
            write_journal(root, &mut journal)
        })?;
    }
    write_journal(root, &mut journal)?;
    write_json_atomically(&root.join("bootstrap.json"), &bootstrap)?;
    record_journal_file(root, Path::new("bootstrap.json"), &mut journal)?;
    write_journal(root, &mut journal)?;
    write_json_atomically(&root.join("selection.json"), &ActivationState::default())?;
    record_journal_file(root, Path::new("selection.json"), &mut journal)?;
    write_journal(root, &mut journal)?;
    create_journaled_directory(root, Path::new("sets"), &mut journal)?;
    let existing_sets: BTreeMap<_, _> = fs::read_dir(root.join("sets"))
        .map_err(|error| error.to_string())?
        .filter_map(Result::ok)
        .map(|entry| (entry.file_name(), ()))
        .collect();
    let digest = stage_package(package, root, &bootstrap, false)?;
    let set = PathBuf::from("sets").join(&digest);
    if existing_sets.contains_key(std::ffi::OsStr::new(&digest)) {
        return Err("installer runtime already contains the package set".into());
    }
    journal.directories.push(set.clone());
    for entry in fs::read_dir(root.join(&set)).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        if !entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_file()
        {
            return Err("installer package contains an unexpected entry".into());
        }
        record_journal_file(root, &set.join(entry.file_name()), &mut journal)?;
    }
    // Installer provisioning has already verified the package against the
    // production bootstrap. Unlike a development staging trial, it must make
    // the first set active before SCM starts: production hosts deliberately
    // refuse an unreviewed `pending` selection. The MSI health gate remains
    // responsible for rolling this transaction back if the service cannot
    // load and operate the signed set.
    let selection_path = root.join("selection.json");
    let mut selection: ActivationState = read_json(&selection_path)?;
    if selection.active.is_some() || selection.pending.as_deref() != Some(&digest) {
        return Err("installer staging produced an unexpected activation state".into());
    }
    selection.active = Some(digest);
    selection.pending = None;
    write_json_atomically(&selection_path, &selection)?;
    record_journal_file(root, Path::new("selection.json"), &mut journal)?;
    write_journal(root, &mut journal)?;
    Ok(())
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProvisionJournal {
    schema: u32,
    directories: Vec<PathBuf>,
    files: Vec<JournalFile>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct JournalFile {
    path: PathBuf,
    sha256: String,
}

fn write_journal(root: &Path, journal: &mut ProvisionJournal) -> PackageResult<()> {
    journal.schema = 1;
    validate_journal(journal)?;
    write_json_atomically(&root.join(PROVISION_JOURNAL), journal)
}
fn create_journaled_directory(
    root: &Path,
    relative: &Path,
    journal: &mut ProvisionJournal,
) -> PackageResult<()> {
    let path = root.join(relative);
    if !path.try_exists().map_err(|error| error.to_string())? {
        create_private_directory(&path)?;
        journal.directories.push(relative.to_path_buf());
    }
    Ok(())
}
fn record_journal_file(
    root: &Path,
    relative: &Path,
    journal: &mut ProvisionJournal,
) -> PackageResult<()> {
    let value = JournalFile {
        path: relative.to_path_buf(),
        sha256: sha256(&read_bounded(&root.join(relative), 256 * 1024 * 1024)?),
    };
    if let Some(previous) = journal
        .files
        .iter_mut()
        .find(|item| item.path == value.path)
    {
        *previous = value;
    } else {
        journal.files.push(value);
    }
    Ok(())
}
fn validate_journal(journal: &ProvisionJournal) -> PackageResult<()> {
    if journal.schema != 1 || journal.directories.len() > 5 || journal.files.len() > 66 {
        return Err("installer provision journal is invalid".into());
    }
    if journal
        .directories
        .iter()
        .any(|path| !normal_relative(path) || !valid_directory(path))
        || journal.files.iter().any(|file| {
            !normal_relative(&file.path) || !valid_file(&file.path) || !valid_digest(&file.sha256)
        })
    {
        return Err("installer provision journal contains an unsafe path".into());
    }
    Ok(())
}
fn normal_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}
fn valid_directory(path: &Path) -> bool {
    path == Path::new("data")
        || path == Path::new("data/state")
        || path == Path::new("management-tls")
        || path == Path::new("sets")
        || (path.components().count() == 2
            && path
                .components()
                .next()
                .is_some_and(|part| part.as_os_str() == "sets")
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(valid_digest))
}
fn valid_file(path: &Path) -> bool {
    matches!(
        path.to_string_lossy().as_ref(),
        "bootstrap.json" | "selection.json"
    ) || valid_tls_seed_file(path)
        || (path.components().count() == 3
            && path
                .components()
                .next()
                .is_some_and(|part| part.as_os_str() == "sets")
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| !name.is_empty() && name.len() <= 256))
}
fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod provision_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn fixture(name: &str) -> PathBuf {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "amaterasu-release-{name}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_artifacts(path: &Path) {
        fs::create_dir_all(path).unwrap();
        for component in PackageProfile::Platform.components() {
            fs::write(
                path.join(library_filename(component.library)),
                component.id.as_bytes(),
            )
            .unwrap();
        }
    }

    fn prepare_seed(root: &Path, artifacts: &Path, signing_key: &SigningKey) -> PathBuf {
        let web = root.join("web-source");
        let output = root.join("seed");
        fs::create_dir_all(&web).unwrap();
        fs::write(
            web.join("index.html"),
            "<html><head></head><body>Amaterasu</body></html>",
        )
        .unwrap();
        prepare_installer_assets_with_key(artifacts, &web, &output, signing_key).unwrap();
        output
    }

    fn legacy_installer_runtime(root: &Path, seed: &Path) -> PathBuf {
        let runtime = root.join("legacy-runtime");
        create_private_directory(&runtime).unwrap();
        let template: BootstrapConfig = read_json(&seed.join("bootstrap-template.json")).unwrap();
        let legacy = BootstrapConfig {
            development: true,
            allow_unsigned: false,
            trusted_keys: template.trusted_keys,
            policy: development_policy(&runtime).unwrap(),
        };
        write_json_atomically(&runtime.join("bootstrap.json"), &legacy).unwrap();
        write_json_atomically(&runtime.join("selection.json"), &ActivationState::default())
            .unwrap();
        let digest = {
            let _lock = RootLock::acquire(&runtime).unwrap();
            stage_package(&seed.join("package"), &runtime, &legacy, false).unwrap()
        };
        let mut selection: ActivationState = read_json(&runtime.join("selection.json")).unwrap();
        selection.active = Some(digest);
        selection.pending = None;
        write_json_atomically(&runtime.join("selection.json"), &selection).unwrap();
        runtime
    }

    fn legacy_production_runtime(root: &Path, seed: &Path) -> PathBuf {
        let runtime = root.join("production-runtime");
        provision_installer_root(
            &seed.join("bootstrap-template.json"),
            &seed.join("package"),
            &runtime,
        )
        .unwrap();
        finalize_installer_provision(&runtime).unwrap();
        let mut bootstrap: BootstrapConfig = read_json(&runtime.join("bootstrap.json")).unwrap();
        let auth = bootstrap
            .policy
            .settings
            .get_mut("amaterasu.authentication")
            .unwrap();
        auth["authorization_policy_generation"] = serde_json::json!(2);
        auth["allowed_scopes"]
            .as_array_mut()
            .unwrap()
            .retain(|scope| scope != "lutris.read");
        bootstrap
            .policy
            .principals
            .get_mut("amaterasu.host-configuration")
            .unwrap()
            .call
            .remove("amaterasu.diagnostics/1");
        write_json_atomically(&runtime.join("bootstrap.json"), &bootstrap).unwrap();
        runtime
    }

    fn offer_host_configuration_diagnostics_grant(seed: &Path) {
        let template_path = seed.join("bootstrap-template.json");
        let mut template: BootstrapConfig = read_json(&template_path).unwrap();
        template
            .policy
            .principals
            .get_mut("amaterasu.host-configuration")
            .unwrap()
            .call
            .insert("amaterasu.diagnostics/1".into());
        write_json_atomically(&template_path, &template).unwrap();
    }

    fn offer_historical_host_configuration_grant(seed: &Path) {
        let template_path = seed.join("bootstrap-template.json");
        let mut template: BootstrapConfig = read_json(&template_path).unwrap();
        template
            .policy
            .principals
            .get_mut("amaterasu.host-configuration")
            .unwrap()
            .call
            .remove("amaterasu.diagnostics/1");
        write_json_atomically(&template_path, &template).unwrap();
    }

    #[test]
    fn supplied_tls_survives_reinstall_and_rotation_rolls_back_with_policy() {
        let root = fixture("tls-provision-rotation");
        let artifacts = root.join("artifacts");
        write_artifacts(&artifacts);
        let seed = prepare_seed(&root, &artifacts, &SigningKey::from_bytes(&[31; 32]));
        let certificate = root.join("supplied-cert.pem");
        let private_key = root.join("supplied-key.pem");
        fs::write(
            &certificate,
            include_bytes!("../../../modules/management/tests/fixtures/browser-cert.pem"),
        )
        .unwrap();
        fs::write(
            &private_key,
            include_bytes!("../../../modules/management/tests/fixtures/browser-key.pem"),
        )
        .unwrap();
        let original = InstallerTls {
            bind: "127.0.0.1:47989".parse().unwrap(),
            certificate: fs::read(&certificate).unwrap(),
            private_key: fs::read(&private_key).unwrap(),
        };
        let runtime = root.join("runtime");
        create_machine_private_directory(&runtime).unwrap();
        provision_installer_root_inner(
            &seed.join("bootstrap-template.json"),
            &seed.join("package"),
            &runtime,
            Some(&original),
        )
        .unwrap();
        let (cert_path, key_path) = original.paths(&runtime);
        assert_eq!(fs::read(&cert_path).unwrap(), original.certificate);
        assert_eq!(fs::read(&key_path).unwrap(), original.private_key);
        let before: BootstrapConfig = read_json(&runtime.join("bootstrap.json")).unwrap();
        let settings = &before.policy.settings["amaterasu.management"];
        assert_eq!(settings["tls_bind"], "127.0.0.1:47989");
        assert!(
            settings["tls_certificate_path"]
                .as_str()
                .unwrap()
                .starts_with(WORKER_STATE_ROOT_TOKEN)
        );
        assert!(
            settings["tls_private_key_path"]
                .as_str()
                .unwrap()
                .starts_with(WORKER_STATE_ROOT_TOKEN)
        );
        finalize_installer_provision(&runtime).unwrap();

        // A later installer run with no TLS input retains locally installed settings.
        install_installer_root(
            &seed.join("bootstrap-template.json"),
            &seed.join("package"),
            &runtime,
        )
        .unwrap();
        assert_eq!(
            read_json::<BootstrapConfig>(&runtime.join("bootstrap.json"))
                .unwrap()
                .policy
                .settings["amaterasu.management"],
            *settings
        );
        let mut rotated = InstallerTls {
            bind: original.bind,
            certificate: original.certificate.clone(),
            private_key: original.private_key.clone(),
        };
        rotated.private_key.extend_from_slice(b"\n");
        let (rotated_cert, rotated_key) = rotated.paths(&runtime);
        let mut replacement = before.clone();
        rotated.apply_settings(&mut replacement, &runtime).unwrap();
        let bootstrap_bytes = fs::read(runtime.join("bootstrap.json")).unwrap();
        let selection_bytes = fs::read(runtime.join("selection.json")).unwrap();
        let active = read_json::<ActivationState>(&runtime.join("selection.json"))
            .unwrap()
            .active
            .unwrap();
        {
            let _lock = RootLock::acquire(&runtime).unwrap();
            assert!(
                transition_installer_policy(
                    &runtime,
                    &seed.join("package"),
                    &before,
                    Some(&replacement),
                    &active,
                    &active,
                    &bootstrap_bytes,
                    &selection_bytes,
                    true,
                    Some(&rotated)
                )
                .is_err()
            );
        }
        assert_eq!(
            fs::read(runtime.join("bootstrap.json")).unwrap(),
            bootstrap_bytes
        );
        assert!(!rotated_key.exists());
        assert!(rotated_cert.exists());
        {
            let _lock = RootLock::acquire(&runtime).unwrap();
            transition_installer_policy(
                &runtime,
                &seed.join("package"),
                &before,
                Some(&replacement),
                &active,
                &active,
                &bootstrap_bytes,
                &selection_bytes,
                false,
                Some(&rotated),
            )
            .unwrap();
        }
        assert_eq!(fs::read(&rotated_key).unwrap(), rotated.private_key);
        let rotated_settings = replacement.policy.settings["amaterasu.management"].clone();
        assert_eq!(
            read_json::<BootstrapConfig>(&runtime.join("bootstrap.json"))
                .unwrap()
                .policy
                .settings["amaterasu.management"],
            rotated_settings
        );
        finalize_installer_provision(&runtime).unwrap();
        install_installer_root(
            &seed.join("bootstrap-template.json"),
            &seed.join("package"),
            &runtime,
        )
        .unwrap();
        assert_eq!(
            read_json::<BootstrapConfig>(&runtime.join("bootstrap.json"))
                .unwrap()
                .policy
                .settings["amaterasu.management"],
            rotated_settings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn signed_production_upgrade_adds_lutris_scope_and_preserves_local_policy() {
        let root = fixture("production-auth-upgrade");
        let artifacts = root.join("artifacts");
        write_artifacts(&artifacts);
        let signing_key = SigningKey::from_bytes(&[21; 32]);
        let seed = prepare_seed(&root, &artifacts, &signing_key);
        let runtime = legacy_production_runtime(&root, &seed);
        let mut before: BootstrapConfig = read_json(&runtime.join("bootstrap.json")).unwrap();
        before.policy.settings.insert(
            "amaterasu.input".into(),
            serde_json::json!({"allow_injection": true}),
        );
        write_json_atomically(&runtime.join("bootstrap.json"), &before).unwrap();
        let previous_selection = fs::read(runtime.join("selection.json")).unwrap();

        let update_root = root.join("update");
        let update_artifacts = update_root.join("artifacts");
        write_artifacts(&update_artifacts);
        fs::write(
            update_artifacts.join(library_filename("amaterasu_runtime")),
            b"updated-runtime-artifact",
        )
        .unwrap();
        let update = prepare_seed(&update_root, &update_artifacts, &signing_key);
        offer_host_configuration_diagnostics_grant(&update);
        install_installer_root(
            &update.join("bootstrap-template.json"),
            &update.join("package"),
            &runtime,
        )
        .unwrap();
        let after: BootstrapConfig = read_json(&runtime.join("bootstrap.json")).unwrap();
        let mut expected = before.clone();
        expected.policy.settings.insert(
            "amaterasu.authentication".into(),
            installer_policy(WEB_DIRECTORY_TOKEN, before.trusted_keys.clone()).settings
                ["amaterasu.authentication"]
                .clone(),
        );
        expected
            .policy
            .principals
            .get_mut("amaterasu.host-configuration")
            .unwrap()
            .call
            .insert("amaterasu.diagnostics/1".into());
        assert_eq!(
            serde_json::to_value(&after).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        assert_eq!(
            after.policy.settings["amaterasu.authentication"]["authorization_policy_generation"],
            3
        );
        assert!(
            after.policy.settings["amaterasu.authentication"]["allowed_scopes"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("lutris.read"))
        );
        assert_ne!(
            fs::read(runtime.join("selection.json")).unwrap(),
            previous_selection
        );
        assert!(runtime.join(POLICY_TRANSITION_JOURNAL).exists());
        finalize_installer_provision(&runtime).unwrap();

        let bootstrap_bytes = fs::read(runtime.join("bootstrap.json")).unwrap();
        let selection_bytes = fs::read(runtime.join("selection.json")).unwrap();
        install_installer_root(
            &update.join("bootstrap-template.json"),
            &update.join("package"),
            &runtime,
        )
        .unwrap();
        assert_eq!(
            fs::read(runtime.join("bootstrap.json")).unwrap(),
            bootstrap_bytes
        );
        assert_eq!(
            fs::read(runtime.join("selection.json")).unwrap(),
            selection_bytes
        );
        assert!(!runtime.join(POLICY_TRANSITION_JOURNAL).exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn custom_production_authorization_is_preserved_with_missing_scope_result() {
        let root = fixture("production-custom-auth");
        let artifacts = root.join("artifacts");
        write_artifacts(&artifacts);
        let signing_key = SigningKey::from_bytes(&[22; 32]);
        let seed = prepare_seed(&root, &artifacts, &signing_key);
        let runtime = legacy_production_runtime(&root, &seed);
        let mut before: BootstrapConfig = read_json(&runtime.join("bootstrap.json")).unwrap();
        before
            .policy
            .settings
            .get_mut("amaterasu.authentication")
            .unwrap()["session_token_ttl_seconds"] = serde_json::json!(3600);
        write_json_atomically(&runtime.join("bootstrap.json"), &before).unwrap();
        offer_host_configuration_diagnostics_grant(&seed);
        let candidate =
            materialize_installer_bootstrap(&seed.join("bootstrap-template.json"), &runtime)
                .unwrap();
        let (upgrade, result) = production_authorization_upgrade(&before, &candidate);
        assert!(upgrade.is_none());
        assert_eq!(
            result,
            ProductionAuthorizationResult::PreservedWithoutLutrisScope
        );
        assert!(
            result
                .to_string()
                .contains("lutris.read remains unavailable")
        );
        let (host_upgrade, host_result) =
            production_host_configuration_upgrade(&before, &candidate);
        assert_eq!(host_result, HostConfigurationGrantResult::Upgraded);
        assert!(host_upgrade.is_some());
        let bootstrap_bytes = fs::read(runtime.join("bootstrap.json")).unwrap();
        install_installer_root(
            &seed.join("bootstrap-template.json"),
            &seed.join("package"),
            &runtime,
        )
        .unwrap();
        let after: BootstrapConfig = read_json(&runtime.join("bootstrap.json")).unwrap();
        assert_eq!(
            after.policy.settings["amaterasu.authentication"],
            before.policy.settings["amaterasu.authentication"]
        );
        assert!(
            after.policy.principals["amaterasu.host-configuration"]
                .call
                .contains("amaterasu.diagnostics/1")
        );
        assert!(runtime.join(POLICY_TRANSITION_JOURNAL).exists());
        unprovision_if_created(&runtime).unwrap();
        assert_eq!(
            fs::read(runtime.join("bootstrap.json")).unwrap(),
            bootstrap_bytes
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn customized_host_configuration_grant_is_not_widened() {
        let root = fixture("production-custom-host-grant");
        let artifacts = root.join("artifacts");
        write_artifacts(&artifacts);
        let seed = prepare_seed(&root, &artifacts, &SigningKey::from_bytes(&[26; 32]));
        let runtime = legacy_production_runtime(&root, &seed);
        let mut before: BootstrapConfig = read_json(&runtime.join("bootstrap.json")).unwrap();
        before
            .policy
            .principals
            .get_mut("amaterasu.host-configuration")
            .unwrap()
            .call
            .insert("amaterasu.extra/1".into());
        write_json_atomically(&runtime.join("bootstrap.json"), &before).unwrap();
        offer_host_configuration_diagnostics_grant(&seed);
        let candidate =
            materialize_installer_bootstrap(&seed.join("bootstrap-template.json"), &runtime)
                .unwrap();
        let (upgrade, result) = production_host_configuration_upgrade(&before, &candidate);
        assert!(upgrade.is_none());
        assert_eq!(
            result,
            HostConfigurationGrantResult::PreservedWithoutDiagnosticsGrant
        );
        assert!(
            result
                .to_string()
                .contains("diagnostics apply remains unavailable")
        );
        install_installer_root(
            &seed.join("bootstrap-template.json"),
            &seed.join("package"),
            &runtime,
        )
        .unwrap();
        let after: BootstrapConfig = read_json(&runtime.join("bootstrap.json")).unwrap();
        assert_eq!(
            serde_json::to_value(&after.policy.principals["amaterasu.host-configuration"]).unwrap(),
            serde_json::to_value(&before.policy.principals["amaterasu.host-configuration"])
                .unwrap()
        );
        assert_eq!(
            after.policy.settings["amaterasu.authentication"]["authorization_policy_generation"],
            3
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn custom_production_policy_stages_signed_update_with_installer_rollback() {
        let root = fixture("production-custom-auth-rollback");
        let artifacts = root.join("artifacts");
        write_artifacts(&artifacts);
        let signing_key = SigningKey::from_bytes(&[25; 32]);
        let seed = prepare_seed(&root, &artifacts, &signing_key);
        let runtime = legacy_production_runtime(&root, &seed);
        let mut bootstrap: BootstrapConfig = read_json(&runtime.join("bootstrap.json")).unwrap();
        bootstrap
            .policy
            .settings
            .get_mut("amaterasu.authentication")
            .unwrap()["session_token_ttl_seconds"] = serde_json::json!(3600);
        write_json_atomically(&runtime.join("bootstrap.json"), &bootstrap).unwrap();
        let bootstrap_before = fs::read(runtime.join("bootstrap.json")).unwrap();
        let selection_before = fs::read(runtime.join("selection.json")).unwrap();

        let update_root = root.join("update");
        let update_artifacts = update_root.join("artifacts");
        write_artifacts(&update_artifacts);
        fs::write(
            update_artifacts.join(library_filename("amaterasu_runtime")),
            b"updated-runtime-artifact",
        )
        .unwrap();
        let update = prepare_seed(&update_root, &update_artifacts, &signing_key);
        offer_historical_host_configuration_grant(&update);
        install_installer_root(
            &update.join("bootstrap-template.json"),
            &update.join("package"),
            &runtime,
        )
        .unwrap();
        assert_eq!(
            fs::read(runtime.join("bootstrap.json")).unwrap(),
            bootstrap_before
        );
        assert_ne!(
            fs::read(runtime.join("selection.json")).unwrap(),
            selection_before
        );
        assert_eq!(fs::read_dir(runtime.join("sets")).unwrap().count(), 2);
        assert!(runtime.join(POLICY_TRANSITION_JOURNAL).exists());

        unprovision_if_created(&runtime).unwrap();
        assert_eq!(
            fs::read(runtime.join("bootstrap.json")).unwrap(),
            bootstrap_before
        );
        assert_eq!(
            fs::read(runtime.join("selection.json")).unwrap(),
            selection_before
        );
        assert_eq!(fs::read_dir(runtime.join("sets")).unwrap().count(), 1);
        assert!(!runtime.join(POLICY_TRANSITION_JOURNAL).exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn failed_production_scope_upgrade_restores_bootstrap_and_selection_bytes() {
        let root = fixture("production-auth-rollback");
        let artifacts = root.join("artifacts");
        write_artifacts(&artifacts);
        let signing_key = SigningKey::from_bytes(&[23; 32]);
        let seed = prepare_seed(&root, &artifacts, &signing_key);
        let runtime = legacy_production_runtime(&root, &seed);
        let bootstrap_before = fs::read(runtime.join("bootstrap.json")).unwrap();
        let selection_before = fs::read(runtime.join("selection.json")).unwrap();

        let update_root = root.join("update");
        let update_artifacts = update_root.join("artifacts");
        write_artifacts(&update_artifacts);
        fs::write(
            update_artifacts.join(library_filename("amaterasu_runtime")),
            b"updated-runtime-artifact",
        )
        .unwrap();
        let update = prepare_seed(&update_root, &update_artifacts, &signing_key);
        offer_host_configuration_diagnostics_grant(&update);
        {
            let _lock = RootLock::acquire(&runtime).unwrap();
            assert!(
                install_existing_installer_root(
                    &update.join("bootstrap-template.json"),
                    &update.join("package"),
                    &runtime,
                    true,
                )
                .is_err()
            );
        }
        assert_eq!(
            fs::read(runtime.join("bootstrap.json")).unwrap(),
            bootstrap_before
        );
        assert_eq!(
            fs::read(runtime.join("selection.json")).unwrap(),
            selection_before
        );
        assert!(!runtime.join(POLICY_TRANSITION_JOURNAL).exists());
        assert_eq!(fs::read_dir(runtime.join("sets")).unwrap().count(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn failed_policy_only_upgrade_restores_exact_bootstrap_and_selection() {
        let root = fixture("production-auth-policy-only-rollback");
        let artifacts = root.join("artifacts");
        write_artifacts(&artifacts);
        let seed = prepare_seed(&root, &artifacts, &SigningKey::from_bytes(&[24; 32]));
        let runtime = legacy_production_runtime(&root, &seed);
        let bootstrap_before = fs::read(runtime.join("bootstrap.json")).unwrap();
        let selection_before = fs::read(runtime.join("selection.json")).unwrap();
        {
            let _lock = RootLock::acquire(&runtime).unwrap();
            assert!(
                install_existing_installer_root(
                    &seed.join("bootstrap-template.json"),
                    &seed.join("package"),
                    &runtime,
                    true,
                )
                .is_err()
            );
        }
        assert_eq!(
            fs::read(runtime.join("bootstrap.json")).unwrap(),
            bootstrap_before
        );
        assert_eq!(
            fs::read(runtime.join("selection.json")).unwrap(),
            selection_before
        );
        assert!(!runtime.join(POLICY_TRANSITION_JOURNAL).exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unprovision_removes_only_unchanged_journaled_artifacts() {
        let root = fixture("rollback");
        fs::create_dir_all(root.join("data/state")).unwrap();
        fs::write(root.join("bootstrap.json"), b"bootstrap").unwrap();
        let mut journal = ProvisionJournal {
            directories: vec![PathBuf::from("data"), PathBuf::from("data/state")],
            ..ProvisionJournal::default()
        };
        record_journal_file(&root, Path::new("bootstrap.json"), &mut journal).unwrap();
        write_journal(&root, &mut journal).unwrap();
        unprovision_if_created(&root).unwrap();
        assert!(!root.join("bootstrap.json").exists());
        assert!(!root.join("data").exists());
        assert!(root.exists());
        unprovision_if_created(&root).unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unprovision_rejects_a_changed_journaled_file() {
        let root = fixture("changed");
        fs::write(root.join("bootstrap.json"), b"original").unwrap();
        let mut journal = ProvisionJournal::default();
        record_journal_file(&root, Path::new("bootstrap.json"), &mut journal).unwrap();
        write_journal(&root, &mut journal).unwrap();
        fs::write(root.join("bootstrap.json"), b"changed").unwrap();
        assert!(unprovision_if_created(&root).is_err());
        assert!(root.join("bootstrap.json").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn finalization_removes_only_the_rollback_journal() {
        let root = fixture("finalize");
        fs::write(root.join("bootstrap.json"), b"bootstrap").unwrap();
        let mut journal = ProvisionJournal::default();
        record_journal_file(&root, Path::new("bootstrap.json"), &mut journal).unwrap();
        write_journal(&root, &mut journal).unwrap();
        finalize_installer_provision(&root).unwrap();
        assert!(root.join("bootstrap.json").exists());
        assert!(!root.join(PROVISION_JOURNAL).exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn installer_seed_is_production_and_stages_a_signed_loopback_ui() {
        let root = fixture("installer-seed");
        let artifacts = root.join("artifacts");
        write_artifacts(&artifacts);

        let signing_key = SigningKey::from_bytes(&[7; 32]);
        let output = prepare_seed(&root, &artifacts, &signing_key);

        let bootstrap: BootstrapConfig =
            read_json(&output.join("bootstrap-template.json")).unwrap();
        assert!(!bootstrap.development);
        assert!(!bootstrap.allow_unsigned);
        assert_eq!(
            bootstrap.policy.settings["amaterasu.management"]["bind"],
            "127.0.0.1:47990"
        );
        assert_eq!(
            bootstrap.policy.settings["amaterasu.nvhttp"]["enabled"],
            true
        );
        assert!(
            bootstrap.policy.settings["amaterasu.nvhttp"]
                .get("streaming")
                .is_none()
        );
        assert!(!output.join("development-key.json").exists());
        let selected = amaterasu_package::web::load_selected(
            &output.join("web"),
            &bootstrap.trusted_keys,
            unix_timestamp(),
        )
        .unwrap();
        assert!(selected.assets.contains_key("index.html"));
        assert_eq!(selected.manifest.expires_unix, INSTALLER_RELEASE_EXPIRY);
        let package: ReleaseManifest = read_json(&output.join("package/manifest.json")).unwrap();
        assert_eq!(package.expires_unix, INSTALLER_RELEASE_EXPIRY);

        let runtime = root.join("runtime");
        provision_installer_root(
            &output.join("bootstrap-template.json"),
            &output.join("package"),
            &runtime,
        )
        .unwrap();
        let provisioned: BootstrapConfig = read_json(&runtime.join("bootstrap.json")).unwrap();
        assert!(!provisioned.development);
        assert!(
            !provisioned.policy.settings.contains_key("amaterasu.state"),
            "the production bootstrap cannot select a user state path"
        );
        assert_eq!(
            provisioned.policy.settings["amaterasu.management"]["web"]["directory"],
            serde_json::json!(
                output
                    .join("web")
                    .canonicalize()
                    .unwrap()
                    .to_string_lossy()
                    .as_ref()
            )
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn signed_development_seed_transitions_without_touching_state_and_reinstall_is_idempotent() {
        let root = fixture("policy-transition");
        let artifacts = root.join("artifacts");
        write_artifacts(&artifacts);
        let signing_key = SigningKey::from_bytes(&[9; 32]);
        let seed = prepare_seed(&root, &artifacts, &signing_key);
        let runtime = legacy_installer_runtime(&root, &seed);
        let state = runtime.join("data/state/user-state.json");
        fs::write(&state, b"preserve-user-state").unwrap();
        let trusted: BootstrapConfig = read_json(&runtime.join("bootstrap.json")).unwrap();

        install_installer_root(
            &seed.join("bootstrap-template.json"),
            &seed.join("package"),
            &runtime,
        )
        .unwrap();
        let transitioned: BootstrapConfig = read_json(&runtime.join("bootstrap.json")).unwrap();
        assert!(!transitioned.development);
        assert_eq!(transitioned.trusted_keys, trusted.trusted_keys);
        assert_eq!(fs::read(&state).unwrap(), b"preserve-user-state");
        assert!(runtime.join(POLICY_TRANSITION_JOURNAL).exists());
        finalize_installer_provision(&runtime).unwrap();
        assert!(!runtime.join(POLICY_TRANSITION_JOURNAL).exists());

        let bootstrap_before = fs::read(runtime.join("bootstrap.json")).unwrap();
        let selection_before = fs::read(runtime.join("selection.json")).unwrap();
        install_installer_root(
            &seed.join("bootstrap-template.json"),
            &seed.join("package"),
            &runtime,
        )
        .unwrap();
        assert_eq!(
            fs::read(runtime.join("bootstrap.json")).unwrap(),
            bootstrap_before
        );
        assert_eq!(
            fs::read(runtime.join("selection.json")).unwrap(),
            selection_before
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn foreign_installer_signer_is_rejected_without_mutating_the_legacy_root() {
        let root = fixture("foreign-signer");
        let artifacts = root.join("artifacts");
        write_artifacts(&artifacts);
        let original = prepare_seed(&root, &artifacts, &SigningKey::from_bytes(&[10; 32]));
        let runtime = legacy_installer_runtime(&root, &original);
        let bootstrap_before = fs::read(runtime.join("bootstrap.json")).unwrap();
        let selection_before = fs::read(runtime.join("selection.json")).unwrap();

        let foreign_root = root.join("foreign");
        fs::create_dir_all(&foreign_root).unwrap();
        let foreign = prepare_seed(
            &foreign_root,
            &artifacts,
            &SigningKey::from_bytes(&[11; 32]),
        );
        assert!(
            install_installer_root(
                &foreign.join("bootstrap-template.json"),
                &foreign.join("package"),
                &runtime,
            )
            .is_err()
        );
        assert_eq!(
            fs::read(runtime.join("bootstrap.json")).unwrap(),
            bootstrap_before
        );
        assert_eq!(
            fs::read(runtime.join("selection.json")).unwrap(),
            selection_before
        );
        assert!(!runtime.join(POLICY_TRANSITION_JOURNAL).exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn failed_policy_switch_restores_exact_bytes_and_removes_attempt_set() {
        let root = fixture("switch-rollback");
        let old_artifacts = root.join("old-artifacts");
        write_artifacts(&old_artifacts);
        let signing_key = SigningKey::from_bytes(&[12; 32]);
        let old_seed = prepare_seed(&root, &old_artifacts, &signing_key);
        let runtime = legacy_installer_runtime(&root, &old_seed);
        let bootstrap_before = fs::read(runtime.join("bootstrap.json")).unwrap();
        let selection_before = fs::read(runtime.join("selection.json")).unwrap();

        let new_root = root.join("new");
        let new_artifacts = new_root.join("artifacts");
        write_artifacts(&new_artifacts);
        fs::write(
            new_artifacts.join(library_filename("amaterasu_runtime")),
            b"changed-runtime-artifact",
        )
        .unwrap();
        let new_seed = prepare_seed(&new_root, &new_artifacts, &signing_key);
        {
            let _lock = RootLock::acquire(&runtime).unwrap();
            assert!(
                install_existing_installer_root(
                    &new_seed.join("bootstrap-template.json"),
                    &new_seed.join("package"),
                    &runtime,
                    true,
                )
                .is_err()
            );
        }
        assert_eq!(
            fs::read(runtime.join("bootstrap.json")).unwrap(),
            bootstrap_before
        );
        assert_eq!(
            fs::read(runtime.join("selection.json")).unwrap(),
            selection_before
        );
        assert!(!runtime.join(POLICY_TRANSITION_JOURNAL).exists());
        let sets = fs::read_dir(runtime.join("sets")).unwrap().count();
        assert_eq!(sets, 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn linux_x264_preflight_rejects_encoder_without_compiled_feature() {
        assert!(
            validate_linux_x264_inputs(b"feature-disabled encoder", |_| true)
                .unwrap_err()
                .contains("lacks an embedded software-x264 ABI marker")
        );
    }

    #[test]
    fn linux_x264_preflight_rejects_missing_or_incompatible_system_runtime() {
        let error = validate_linux_x264_inputs(b"AMATERASU_X264_BUILD=164\0", |build| {
            assert_eq!(build, 164);
            false
        })
        .unwrap_err();
        assert!(error.contains("required system x264 runtime for build 164"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_desktop_x264_preflight_fails_before_package_directory_is_created() {
        let root = fixture("linux-x264-preflight");
        let artifacts = root.join("artifacts");
        fs::create_dir_all(&artifacts).unwrap();
        fs::write(
            artifacts.join(library_filename("amaterasu_encode")),
            b"software-x264 feature absent",
        )
        .unwrap();
        let output = root.join("package");
        let signing_key = SigningKey::from_bytes(&[19; 32]);

        let error = build_signed_package(
            &artifacts,
            &output,
            1,
            PackageProfile::LinuxSession,
            &signing_key,
            DEVELOPMENT_KEY_ID,
        )
        .unwrap_err();

        assert!(error.contains("lacks an embedded software-x264 ABI marker"));
        assert!(
            !output.exists(),
            "failed preflight must not stage a package"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unavailable_x264_runtime_fails_before_signed_desktop_package_is_staged() {
        let root = fixture("linux-x264-runtime-missing");
        let artifacts = root.join("artifacts");
        fs::create_dir_all(&artifacts).unwrap();
        fs::write(
            artifacts.join(library_filename("amaterasu_encode")),
            b"AMATERASU_X264_BUILD=164\0",
        )
        .unwrap();
        let output = root.join("package");
        let signing_key = SigningKey::from_bytes(&[20; 32]);

        let error = build_signed_package_with_runtime_probe(
            &artifacts,
            &output,
            1,
            PackageProfile::LinuxSession,
            &signing_key,
            DEVELOPMENT_KEY_ID,
            |build| build == 0,
        )
        .unwrap_err();

        assert!(error.contains("required system x264 runtime for build 164"));
        assert!(
            !output.exists(),
            "failed preflight must not stage a package"
        );
        let _ = fs::remove_dir_all(root);
    }
}

fn build_signed_package(
    artifacts: &Path,
    output: &Path,
    sequence: u64,
    profile: PackageProfile,
    signing_key: &SigningKey,
    key_id: &str,
) -> PackageResult<()> {
    build_signed_package_with_runtime_probe(
        artifacts,
        output,
        sequence,
        profile,
        signing_key,
        key_id,
        x264_runtime_available,
    )
}

fn build_signed_package_with_runtime_probe(
    artifacts: &Path,
    output: &Path,
    sequence: u64,
    profile: PackageProfile,
    signing_key: &SigningKey,
    key_id: &str,
    runtime_available: impl FnOnce(u32) -> bool,
) -> PackageResult<()> {
    if output.try_exists().map_err(|error| error.to_string())? {
        return Err("package output already exists; use a new directory for each release".into());
    }
    let linux_x264_encoder = matches!(profile, PackageProfile::LinuxSession)
        .then(|| preflight_linux_x264(artifacts, runtime_available))
        .transpose()?;
    create_private_directory(output)?;

    let mut components = Vec::new();
    for definition in profile.components() {
        let version = if definition.id == "amaterasu.example-echo" {
            format!("0.1.{sequence}")
        } else {
            "0.1.0".to_owned()
        };
        let component = if definition.id == "amaterasu.encode" {
            if let Some(encoder) = linux_x264_encoder.as_deref() {
                copy_component_bytes(output, definition, &version, encoder)?
            } else {
                copy_component(artifacts, output, definition, &version)?
            }
        } else {
            copy_component(artifacts, output, definition, &version)?
        };
        components.push(component);
    }

    let manifest = ReleaseManifest {
        schema: 1,
        host_context_schema: 1,
        project: "amaterasu".into(),
        sequence,
        expires_unix: if key_id == INSTALLER_KEY_ID {
            // An MSI may be installed offline long after it was built. Its
            // immutable package is authenticated by the MSI payload and the
            // persistent installer signing key, not a short freshness lease.
            INSTALLER_RELEASE_EXPIRY
        } else {
            unix_timestamp() + 7 * 24 * 60 * 60
        },
        target: current_target(),
        components,
    };
    sign_manifest_with_key(output, &manifest, signing_key, key_id)
}

/// A Linux Desktop package must be built from the x264-enabled encoder and a
/// host with the matching system runtime. The ABI version is read from a
/// marker embedded in the exact encoder artifact this function later signs.
fn preflight_linux_x264(
    artifacts: &Path,
    runtime_available: impl FnOnce(u32) -> bool,
) -> PackageResult<Vec<u8>> {
    if !cfg!(target_os = "linux") {
        return Err("Linux Desktop packages must be staged on a Linux build host".into());
    }
    let encoder = read_bounded(
        &artifacts.join(library_filename("amaterasu_encode")),
        256 * 1024 * 1024,
    )?;
    validate_linux_x264_inputs(&encoder, runtime_available)?;
    Ok(encoder)
}

fn validate_linux_x264_inputs(
    encoder: &[u8],
    runtime_available: impl FnOnce(u32) -> bool,
) -> PackageResult<()> {
    let build = embedded_x264_build(encoder)
        .ok_or("Linux Desktop encoder artifact lacks an embedded software-x264 ABI marker")?;
    if !runtime_available(build) {
        return Err(format!(
            "required system x264 runtime for build {} is unavailable or incompatible",
            build
        ));
    }
    Ok(())
}

pub(crate) fn embedded_x264_build(encoder: &[u8]) -> Option<u32> {
    let offsets = encoder
        .windows(X264_BUILD_MARKER.len())
        .enumerate()
        .filter_map(|(offset, window)| (window == X264_BUILD_MARKER).then_some(offset))
        .collect::<Vec<_>>();
    let [offset] = offsets.as_slice() else {
        return None;
    };
    let digits = encoder[*offset + X264_BUILD_MARKER.len()..]
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .copied()
        .collect::<Vec<_>>();
    if digits.is_empty() {
        return None;
    }
    std::str::from_utf8(&digits)
        .ok()?
        .parse::<u32>()
        .ok()
        .filter(|build| *build > 0)
}

#[cfg(target_os = "linux")]
pub(crate) fn x264_runtime_available(build: u32) -> bool {
    let library_names = [format!("libx264.so.{build}"), "libx264.so".into()];
    for name in library_names {
        // SAFETY: candidates are fixed system loader names derived from the
        // embedded x264 ABI marker; no user path or package artifact is loaded.
        let Ok(library) = (unsafe { libloading::Library::new(name) }) else {
            continue;
        };
        let symbols = [
            "x264_param_default_preset",
            "x264_param_apply_profile",
            "x264_param_cleanup",
            "x264_picture_init",
            "x264_encoder_close",
            "x264_encoder_encode",
            "x264_encoder_delayed_frames",
            "x264_encoder_maximum_delayed_frames",
            "x264_encoder_parameters",
        ];
        let symbols_available = symbols.iter().all(|symbol| {
            // SAFETY: symbol lookup checks presence only; the pointers are not called.
            unsafe { library.get::<*mut c_void>(symbol.as_bytes()).is_ok() }
        });
        let versioned_open = format!("x264_encoder_open_{build}");
        // SAFETY: symbol lookup checks presence only; the pointer is not called.
        let open_available = unsafe {
            library
                .get::<*mut c_void>(versioned_open.as_bytes())
                .is_ok()
        };
        if symbols_available && open_available {
            return true;
        }
    }
    false
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn x264_runtime_available(_: u32) -> bool {
    false
}

fn copy_component(
    artifacts: &Path,
    output: &Path,
    definition: &ComponentDefinition,
    version: &str,
) -> PackageResult<ComponentManifest> {
    let bytes = read_bounded(
        &artifacts.join(library_filename(definition.library)),
        256 * 1024 * 1024,
    )?;
    copy_component_bytes(output, definition, version, &bytes)
}

fn copy_component_bytes(
    output: &Path,
    definition: &ComponentDefinition,
    version: &str,
    bytes: &[u8],
) -> PackageResult<ComponentManifest> {
    let filename = library_filename(definition.library);
    fs::write(output.join(&filename), bytes).map_err(|error| error.to_string())?;

    Ok(ComponentManifest {
        id: definition.id.into(),
        version: version.into(),
        file: filename,
        sha256: sha256(bytes),
        size: bytes.len() as u64,
        abi_major: amaterasu_abi::ABI_MAJOR,
        runtime: definition.id == RUNTIME_ID,
        requires: definition
            .dependencies
            .iter()
            .map(|dependency| ((*dependency).to_owned(), "0.1.0".to_owned()))
            .collect(),
        requested: definition.grants(),
    })
}

pub(super) fn sign_manifest(
    root: &Path,
    output: &Path,
    manifest: &impl serde::Serialize,
) -> PackageResult<()> {
    let secret: String = read_json(&root.join("development-key.json"))?;
    let signing_key = SigningKey::from_bytes(&decode_hex::<32>(&secret)?);
    sign_manifest_with_key(output, manifest, &signing_key, DEVELOPMENT_KEY_ID)
}

fn sign_manifest_with_key(
    output: &Path,
    manifest: &impl serde::Serialize,
    signing_key: &SigningKey,
    key_id: &str,
) -> PackageResult<()> {
    let bytes = serde_json::to_vec_pretty(manifest).map_err(|error| error.to_string())?;
    let signature = ReleaseSignature {
        key_id: key_id.into(),
        signature: encode_hex(&signing_key.sign(&bytes).to_bytes()),
    };

    fs::write(output.join("manifest.json"), bytes).map_err(|error| error.to_string())?;
    write_json_atomically(&output.join("manifest.sig"), &signature)
}

fn library_filename(name: &str) -> String {
    format!(
        "{}{name}{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    )
}
