//! Local-only publisher tooling. Generated keys are not production release identities.
use amaterasu_package::{
    ActivationState, BootstrapConfig, ComponentManifest, PackageResult, ReleaseManifest,
    ReleaseSignature, RootLock, VerificationPolicy, create_machine_private_directory,
    create_private_directory, current_target, decode_hex, encode_hex, package_directory,
    random_identifier, read_bounded, read_json, sha256, stage_package, unix_timestamp,
    verify_package, write_json_atomically,
};
use amaterasu_storage::write_atomically;
use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::profiles::{
    ComponentDefinition, PackageProfile, RUNTIME_ID, development_policy, installer_policy,
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

pub fn initialize(root: &Path) -> PackageResult<()> {
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
        policy: development_policy(root)?,
    };
    let web_directory = root.join("web");
    create_private_directory(&web_directory)?;
    bootstrap.policy.settings.insert("amaterasu.management".into(), serde_json::json!({
        "enabled": false,
        "web": {"directory": web_directory.canonicalize().map_err(|error| error.to_string())?, "trusted_keys": bootstrap.trusted_keys}
    }));

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
    if root
        .join("bootstrap.json")
        .try_exists()
        .map_err(|error| error.to_string())?
    {
        return Err("installer runtime root already exists; refusing to replace it".into());
    }
    create_machine_private_directory(root)?;
    let result = provision_installer_root_inner(template, package, root);
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
/// authorization policies under the same signer. Locally customized policy and
/// key rotation require an explicit migration rather than an MSI self-upgrade.
fn install_existing_installer_root(
    template: &Path,
    package: &Path,
    root: &Path,
    inject_failure_after_switch: bool,
) -> PackageResult<()> {
    let bootstrap_path = root.join("bootstrap.json");
    let selection_path = root.join("selection.json");
    let bootstrap_bytes = read_bounded(&bootstrap_path, MAX_TRANSITION_FILE_BYTES)?;
    let current: BootstrapConfig =
        serde_json::from_slice(&bootstrap_bytes).map_err(|error| error.to_string())?;
    let candidate = materialize_installer_bootstrap(template, root)?;
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

    // A generated production authorization policy may gain a scope under the
    // same signer. Everything else in an established root stays local.
    if !current.development {
        let (upgraded, authorization_result) =
            production_authorization_upgrade(&current, &candidate);
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
            )?;
        } else if selection.pending.as_deref() == Some(next_package.digest.as_str()) {
            verify_package(
                &package_directory(root, &next_package.digest)?,
                &current,
                policy,
            )?;
        }
        eprintln!("installer authorization policy: {authorization_result}");
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
) -> PackageResult<()> {
    let bootstrap_path = root.join("bootstrap.json");
    let transition = PolicyTransition::begin(root, bootstrap_bytes, selection_bytes)?;
    let result = (|| {
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
    if let Some(digest) = &journal.created_set {
        let directory = package_directory(root, digest)?;
        if directory.try_exists().map_err(|error| error.to_string())? {
            fs::remove_dir_all(&directory).map_err(|error| error.to_string())?;
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

fn provision_installer_root_inner(
    template: &Path,
    package: &Path,
    root: &Path,
) -> PackageResult<()> {
    let _lock = RootLock::acquire(root)?;
    let bootstrap = materialize_installer_bootstrap(template, root)?;
    let mut journal = ProvisionJournal::default();
    write_journal(root, &mut journal)?;
    create_journaled_directory(root, Path::new("data"), &mut journal)?;
    create_journaled_directory(root, Path::new("data/state"), &mut journal)?;
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
    if journal.schema != 1 || journal.directories.len() > 4 || journal.files.len() > 64 {
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
    ) || (path.components().count() == 3
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
        write_json_atomically(&runtime.join("bootstrap.json"), &bootstrap).unwrap();
        runtime
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
        let bootstrap_bytes = fs::read(runtime.join("bootstrap.json")).unwrap();
        install_installer_root(
            &seed.join("bootstrap-template.json"),
            &seed.join("package"),
            &runtime,
        )
        .unwrap();
        assert_eq!(
            fs::read(runtime.join("bootstrap.json")).unwrap(),
            bootstrap_bytes
        );
        assert!(!runtime.join(POLICY_TRANSITION_JOURNAL).exists());
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
}

fn build_signed_package(
    artifacts: &Path,
    output: &Path,
    sequence: u64,
    profile: PackageProfile,
    signing_key: &SigningKey,
    key_id: &str,
) -> PackageResult<()> {
    if output.try_exists().map_err(|error| error.to_string())? {
        return Err("package output already exists; use a new directory for each release".into());
    }
    create_private_directory(output)?;

    let mut components = Vec::new();
    for definition in profile.components() {
        let version = if definition.id == "amaterasu.example-echo" {
            format!("0.1.{sequence}")
        } else {
            "0.1.0".to_owned()
        };
        components.push(copy_component(artifacts, output, definition, &version)?);
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

fn copy_component(
    artifacts: &Path,
    output: &Path,
    definition: &ComponentDefinition,
    version: &str,
) -> PackageResult<ComponentManifest> {
    let filename = library_filename(definition.library);
    let bytes = read_bounded(&artifacts.join(&filename), 256 * 1024 * 1024)?;
    fs::write(output.join(&filename), &bytes).map_err(|error| error.to_string())?;

    Ok(ComponentManifest {
        id: definition.id.into(),
        version: version.into(),
        file: filename,
        sha256: sha256(&bytes),
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
