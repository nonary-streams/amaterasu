//! Bounded, copy-only import of predecessor configuration.
//!
//! The installer stops predecessor services before invoking this module. It then
//! supplies explicit source roots so an elevated process does not accidentally
//! import the installing account's profile instead of the predecessor user's
//! profile. Import never edits a predecessor root. Each source is copied to a
//! durable Amaterasu backup before a byte-identical, conflict-free merge into
//! the legacy import directory. The runtime can consume that preserved layout
//! once its product-schema importer is available.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::{PackageResult, create_private_directory, random_identifier, write_json_atomically};

/// Directory under the Amaterasu data root containing preserved predecessor data.
pub const LEGACY_IMPORT_DIRECTORY: &str = "legacy-imports";
/// Directory under the Amaterasu data root containing immutable source snapshots.
pub const MIGRATION_BACKUP_DIRECTORY: &str = "migration-backups";

const MAX_CONFIGURATION_FILE_BYTES: u64 = 64 * 1024 * 1024;
const CONFIGURATION_FILES: &[&str] = &[
    "apollo.conf",
    "sunshine.conf",
    "vibeshine.conf",
    "vibepollo.conf",
    "apps.json",
    "apollo_state.json",
    "sunshine_state.json",
    "vibeshine_state.json",
    "vibepollo_state.json",
    "virtual_display_cache.json",
    "nvprefs_undo.json",
];
const CONFIGURATION_DIRECTORIES: &[&str] = &["credentials"];

/// A predecessor product whose settings can be preserved for Amaterasu.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyProduct {
    Apollo,
    Sunshine,
    Vibeshine,
    Vibepollo,
}

impl LegacyProduct {
    pub const ALL: [Self; 4] = [
        Self::Apollo,
        Self::Sunshine,
        Self::Vibeshine,
        Self::Vibepollo,
    ];

    pub fn parse(value: &str) -> Option<Self> {
        if value.eq_ignore_ascii_case("apollo") {
            Some(Self::Apollo)
        } else if value.eq_ignore_ascii_case("sunshine") {
            Some(Self::Sunshine)
        } else if value.eq_ignore_ascii_case("vibeshine") {
            Some(Self::Vibeshine)
        } else if value.eq_ignore_ascii_case("vibepollo") {
            Some(Self::Vibepollo)
        } else {
            None
        }
    }

    pub const fn directory_name(self) -> &'static str {
        match self {
            Self::Apollo => "apollo",
            Self::Sunshine => "sunshine",
            Self::Vibeshine => "vibeshine",
            Self::Vibepollo => "vibepollo",
        }
    }

    const fn profile_directory_name(self) -> &'static str {
        match self {
            Self::Apollo => "Apollo",
            Self::Sunshine => "Sunshine",
            Self::Vibeshine => "Vibeshine",
            Self::Vibepollo => "Vibepollo",
        }
    }
}

/// A verified predecessor root supplied by the installer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyConfigSource {
    pub product: LegacyProduct,
    pub root: PathBuf,
}

/// App-data bases to inspect when an installer has already resolved user profiles.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LegacySearchPaths {
    /// Each path is an AppData or LocalAppData base, not a product directory.
    pub profile_data_roots: Vec<PathBuf>,
    /// Explicit predecessor installation roots. A root may contain `config`.
    pub installation_roots: Vec<LegacyConfigSource>,
}

/// A copy-only import transaction. It is serializable so an installer can retain
/// it until its own service/product replacement transaction has committed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MigrationReceipt {
    pub backup_directory: PathBuf,
    pub imported_files: Vec<ImportedFile>,
    pub plan: ImportPlan,
}

/// A target file created by this transaction. The digest guards rollback from
/// deleting a file changed after import.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImportedFile {
    pub path: PathBuf,
    pub sha256: String,
}

/// A plan containing only settings and application fields that have a defined
/// destination in Amaterasu. It intentionally excludes credential and identity
/// values. A non-empty `critical_items` means the installer must not remove the
/// predecessor because Amaterasu cannot safely take over that behavior yet.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImportPlan {
    pub schema: u32,
    pub applications: Vec<ImportedApplication>,
    pub settings: BTreeMap<String, String>,
    pub unsupported_items: Vec<PlanItem>,
    pub critical_items: Vec<PlanItem>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImportedApplication {
    pub product: LegacyProduct,
    pub name: String,
    pub executable: String,
    pub arguments: Vec<String>,
    pub working_directory: Option<String>,
}

/// Identifies a category or legacy key without retaining its value.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct PlanItem {
    pub product: LegacyProduct,
    pub item: String,
}

struct CollectedConfigurationFiles {
    files: Vec<PathBuf>,
    copy_sources: BTreeMap<PathBuf, PathBuf>,
    configured_state_files: BTreeSet<PathBuf>,
    configured_authentication_files: BTreeSet<PathBuf>,
}

/// Non-secret evidence that a product-specific importer validated a sensitive
/// predecessor pairing identity. Package migration does not parse identity
/// keys itself: it only removes the corresponding preflight gates after the
/// dedicated pairing process has accepted the source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ValidatedPairingSource {
    pub product: LegacyProduct,
}

/// Non-secret approvals returned by the dedicated migration helper programs.
/// Each flag removes only the critical records owned by that helper.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ValidatedSensitiveSource {
    pub product: LegacyProduct,
    pub pairing: bool,
    pub authentication: bool,
}

/// Finds roots containing supported predecessor configuration. Missing roots are
/// ignored. The caller still chooses which discovered roots to import.
pub fn discover_legacy_sources(
    paths: &LegacySearchPaths,
) -> PackageResult<Vec<LegacyConfigSource>> {
    let mut candidates = Vec::new();
    for base in &paths.profile_data_roots {
        for product in LegacyProduct::ALL {
            candidates.push(LegacyConfigSource {
                product,
                root: base.join(product.profile_directory_name()),
            });
        }
    }
    for source in &paths.installation_roots {
        candidates.push(source.clone());
        candidates.push(LegacyConfigSource {
            product: source.product,
            root: source.root.join("config"),
        });
    }

    let mut discovered = Vec::new();
    let mut seen = BTreeSet::<(LegacyProduct, PathBuf)>::new();
    for source in candidates {
        let Some(identity) = canonical_existing_source_root(&source.root)? else {
            continue;
        };
        if !seen.insert((source.product, identity)) {
            continue;
        }
        if collect_configuration_files(&source.root)?.is_some() {
            discovered.push(source);
        }
    }
    Ok(discovered)
}

/// Returns the filesystem identity for an existing predecessor configuration root.
/// Missing roots return `None`; unsafe roots and other filesystem errors fail.
pub fn canonical_existing_source_root(root: &Path) -> PackageResult<Option<PathBuf>> {
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(error)),
    };
    if !metadata.is_dir() || is_reparse_or_symlink(&metadata) {
        return Err("predecessor configuration root is not a directory".into());
    }

    let canonical_root = root.canonicalize().map_err(io_error)?;
    #[cfg(windows)]
    {
        Ok(Some(resolved_existing_absolute_path(&canonical_root)?))
    }
    #[cfg(not(windows))]
    {
        Ok(Some(canonical_root))
    }
}

/// Builds a non-secret, reviewable import plan from explicit predecessor roots.
/// The plan does not modify either predecessor or Amaterasu data.
pub fn analyze_legacy_configuration(sources: &[LegacyConfigSource]) -> PackageResult<ImportPlan> {
    if sources.is_empty() {
        return Err("no predecessor configuration sources were supplied".into());
    }
    let mut plan = ImportPlan {
        schema: 1,
        applications: Vec::new(),
        settings: BTreeMap::new(),
        unsupported_items: Vec::new(),
        critical_items: Vec::new(),
    };
    for source in sources {
        let collected = collect_configuration_files(&source.root)?
            .ok_or("a predecessor configuration source no longer contains supported files")?;
        for relative in &collected.files {
            analyze_source_file(
                source.product,
                &source.root.join(relative),
                collected.configured_state_files.contains(relative),
                collected.configured_authentication_files.contains(relative),
                &mut plan,
            )?;
        }
    }
    plan.unsupported_items.sort();
    plan.unsupported_items.dedup();
    plan.critical_items.sort();
    plan.critical_items.dedup();
    Ok(plan)
}

/// Rejects a migration before a caller stops or uninstalls a predecessor when
/// its identity, pairing, listener, TLS, or remote-access configuration cannot
/// be faithfully consumed by Amaterasu.
pub fn preflight_legacy_configuration(sources: &[LegacyConfigSource]) -> PackageResult<ImportPlan> {
    preflight_legacy_configuration_with_pairing(sources, &[])
}

/// Performs preflight with pairing approvals supplied by the isolated pairing
/// importer. Every other sensitive item remains a blocking condition.
pub fn preflight_legacy_configuration_with_pairing(
    sources: &[LegacyConfigSource],
    validated_pairing: &[ValidatedPairingSource],
) -> PackageResult<ImportPlan> {
    let mut plan = analyze_legacy_configuration(sources)?;
    let products: BTreeSet<_> = validated_pairing.iter().map(|item| item.product).collect();
    plan.critical_items.retain(|item| {
        !products.contains(&item.product)
            || !matches!(
                item.item.as_str(),
                "pairing_identity_state" | "pairing_credentials" | "pkey" | "cert"
            )
    });
    if !plan.critical_items.is_empty() {
        return Err(
            "predecessor configuration contains settings Amaterasu cannot safely import".into(),
        );
    }
    Ok(plan)
}

pub fn preflight_legacy_configuration_with_sensitive_sources(
    sources: &[LegacyConfigSource],
    validated: &[ValidatedSensitiveSource],
) -> PackageResult<ImportPlan> {
    let mut plan = analyze_legacy_configuration(sources)?;
    for item in &mut plan.critical_items {
        if let Some(source) = validated
            .iter()
            .find(|source| source.product == item.product)
        {
            if source.pairing
                && matches!(
                    item.item.as_str(),
                    "pairing_identity_state" | "pairing_credentials" | "pkey" | "cert"
                )
            {
                item.item.clear();
            }
            if source.authentication
                && matches!(
                    item.item.as_str(),
                    "credentials_file" | "legacy_administrator_credential"
                )
            {
                item.item.clear();
            }
        }
    }
    plan.critical_items.retain(|item| !item.item.is_empty());
    if !plan.critical_items.is_empty() {
        return Err(
            "predecessor configuration contains settings Amaterasu cannot safely import".into(),
        );
    }
    Ok(plan)
}

/// Copies supported files from explicit predecessor roots into `data_root`.
///
/// Sources are never modified. Existing imported files merge only when their
/// bytes match exactly; different bytes are a conflict and leave every target
/// unchanged. Source snapshots are retained in `migration-backups` even after
/// a successful import, allowing the surrounding installer transaction to
/// remove the created imported files with [`rollback_legacy_configuration`].
pub fn import_legacy_configuration(
    data_root: &Path,
    sources: &[LegacyConfigSource],
) -> PackageResult<MigrationReceipt> {
    import_legacy_configuration_with_pairing(data_root, sources, &[])
}

/// Copies predecessor configuration only after generic preflight and an
/// isolated pairing validator both succeed. The approval is intentionally
/// narrow: custom credentials files, remote-access settings, and any unknown
/// critical state remain blocked.
pub fn import_legacy_configuration_with_pairing(
    data_root: &Path,
    sources: &[LegacyConfigSource],
    validated_pairing: &[ValidatedPairingSource],
) -> PackageResult<MigrationReceipt> {
    let plan = preflight_legacy_configuration_with_pairing(sources, validated_pairing)?;
    import_legacy_configuration_after_preflight(data_root, sources, plan)
}

fn import_legacy_configuration_after_preflight(
    data_root: &Path,
    sources: &[LegacyConfigSource],
    plan: ImportPlan,
) -> PackageResult<MigrationReceipt> {
    create_private_directory(data_root)?;

    let transaction = random_identifier()?;
    let backup_directory = data_root
        .join(MIGRATION_BACKUP_DIRECTORY)
        .join(&transaction);
    let import_root = data_root.join(LEGACY_IMPORT_DIRECTORY);
    create_private_directory(&backup_directory)?;
    create_private_directory(&import_root)?;

    let snapshots = snapshot_sources(sources, &backup_directory)?;
    preflight_merge(&import_root, &snapshots)?;

    let mut created = Vec::new();
    let result = merge_snapshots(&import_root, &snapshots, &mut created);
    if let Err(error) = result {
        let receipt = MigrationReceipt {
            backup_directory: backup_directory.clone(),
            imported_files: created,
            plan: plan.clone(),
        };
        let _ = rollback_legacy_configuration(&receipt);
        return Err(error);
    }

    let receipt = MigrationReceipt {
        backup_directory,
        imported_files: created,
        plan,
    };
    write_json_atomically(
        &receipt.backup_directory.join("import-plan.json"),
        &receipt.plan,
    )?;
    write_json_atomically(&receipt.backup_directory.join("receipt.json"), &receipt)?;
    Ok(receipt)
}

pub fn import_legacy_configuration_with_sensitive_sources(
    data_root: &Path,
    sources: &[LegacyConfigSource],
    validated: &[ValidatedSensitiveSource],
) -> PackageResult<MigrationReceipt> {
    let plan = preflight_legacy_configuration_with_sensitive_sources(sources, validated)?;
    import_legacy_configuration_after_preflight(data_root, sources, plan)
}

/// Removes only files created by a successful import. A changed target causes
/// rollback to stop rather than erase a post-import user change. Source backups
/// remain available for recovery and audit.
pub fn rollback_legacy_configuration(receipt: &MigrationReceipt) -> PackageResult<()> {
    for imported in receipt.imported_files.iter().rev() {
        if !imported.path.try_exists().map_err(io_error)? {
            continue;
        }
        if digest_regular_file(&imported.path)? != imported.sha256 {
            return Err("an imported configuration file changed after migration".into());
        }
        fs::remove_file(&imported.path).map_err(io_error)?;
        remove_empty_parents(&imported.path);
    }
    Ok(())
}

fn analyze_source_file(
    product: LegacyProduct,
    path: &Path,
    is_configured_state_file: bool,
    is_configured_authentication_file: bool,
    plan: &mut ImportPlan,
) -> PackageResult<()> {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let is_state_document = is_configured_state_file
        || (!is_configured_authentication_file && filename.ends_with("_state.json"));
    if is_state_document {
        plan.critical_items.push(PlanItem {
            product,
            item: "pairing_identity_state".into(),
        });
        classify_legacy_state_credentials(product, path, plan)?;
    }
    if filename == "apps.json" {
        return analyze_applications(product, path, plan);
    } else if filename.ends_with(".conf") {
        return analyze_settings(product, path, plan);
    } else if is_state_document {
        return Ok(());
    } else if path
        .components()
        .any(|component| component.as_os_str().eq_ignore_ascii_case("credentials"))
    {
        plan.critical_items.push(PlanItem {
            product,
            item: if is_configured_authentication_file {
                "credentials_file".into()
            } else {
                "pairing_credentials".into()
            },
        });
    } else {
        plan.unsupported_items.push(PlanItem {
            product,
            item: filename.into(),
        });
    }
    Ok(())
}

fn classify_legacy_state_credentials(
    product: LegacyProduct,
    path: &Path,
    plan: &mut ImportPlan,
) -> PackageResult<()> {
    let value: serde_json::Value = match serde_json::from_slice(&read_configuration_file(path)?) {
        Ok(value) => value,
        Err(_) => return Ok(()),
    };
    if ["username", "password", "salt"]
        .iter()
        .all(|key| value.get(key).is_some_and(serde_json::Value::is_string))
    {
        plan.critical_items.push(PlanItem {
            product,
            item: "legacy_administrator_credential".into(),
        });
    }
    if value
        .pointer("/root/api_tokens")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|tokens| !tokens.is_empty())
    {
        plan.critical_items.push(PlanItem {
            product,
            item: "legacy_api_tokens".into(),
        });
    }
    Ok(())
}

fn analyze_applications(
    product: LegacyProduct,
    path: &Path,
    plan: &mut ImportPlan,
) -> PackageResult<()> {
    let bytes = read_configuration_file(path)?;
    let values: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(values) => values,
        Err(_) => {
            plan.unsupported_items.push(PlanItem {
                product,
                item: "apps_json_invalid".into(),
            });
            return Ok(());
        }
    };
    let Some(applications) = values.as_array() else {
        plan.unsupported_items.push(PlanItem {
            product,
            item: "apps_json_schema".into(),
        });
        return Ok(());
    };
    for application in applications {
        let Some(application) = application.as_object() else {
            plan.unsupported_items.push(PlanItem {
                product,
                item: "application_schema".into(),
            });
            continue;
        };
        let (Some(name), Some(command)) = (
            application.get("name").and_then(serde_json::Value::as_str),
            application.get("cmd").and_then(serde_json::Value::as_str),
        ) else {
            plan.unsupported_items.push(PlanItem {
                product,
                item: "application_missing_name_or_command".into(),
            });
            continue;
        };
        let Some(executable) = parse_exact_executable(command) else {
            plan.unsupported_items.push(PlanItem {
                product,
                item: "application_command_requires_review".into(),
            });
            continue;
        };
        let working_directory = application
            .get("working-dir")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned);
        if !bounded_application_text(name, 160)
            || !is_absolute_path(&executable)
            || working_directory
                .as_deref()
                .is_some_and(|directory| !is_absolute_path(directory))
        {
            plan.unsupported_items.push(PlanItem {
                product,
                item: "application_outside_import_bounds".into(),
            });
            continue;
        }
        plan.applications.push(ImportedApplication {
            product,
            name: name.into(),
            executable,
            arguments: Vec::new(),
            working_directory,
        });
    }
    Ok(())
}

fn analyze_settings(
    product: LegacyProduct,
    path: &Path,
    plan: &mut ImportPlan,
) -> PackageResult<()> {
    let contents = String::from_utf8(read_configuration_file(path)?)
        .map_err(|_| "predecessor configuration contains a non-text settings file".to_owned())?;
    for line in contents.lines() {
        let line = strip_unquoted_comment(line).trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            plan.unsupported_items.push(PlanItem {
                product,
                item: "settings_syntax".into(),
            });
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        if is_critical_setting(&key) {
            plan.critical_items.push(PlanItem { product, item: key });
        } else if let Some(destination) = supported_setting_destination(&key) {
            plan.settings.insert(destination.into(), value.into());
        } else if key != "file_apps" {
            plan.unsupported_items.push(PlanItem { product, item: key });
        }
    }
    Ok(())
}

fn read_configuration_file(path: &Path) -> PackageResult<Vec<u8>> {
    validate_regular_file(path)?;
    fs::read(path).map_err(io_error)
}

fn parse_exact_executable(command: &str) -> Option<String> {
    let command = command.trim();
    if let Some(command) = command
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    {
        return (!command.contains('"')).then(|| command.to_owned());
    }
    (!command.chars().any(char::is_whitespace)).then(|| command.to_owned())
}

fn bounded_application_text(value: &str, limit: usize) -> bool {
    !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}

fn is_absolute_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    value.starts_with('/')
        || value.starts_with("\\\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'))
}

fn is_critical_setting(key: &str) -> bool {
    matches!(
        key,
        "port"
            | "address_family"
            | "upnp"
            | "origin_web_ui_allowed"
            | "lan_encryption_mode"
            | "wan_encryption_mode"
            | "allow_remote"
            | "pkey"
            | "cert"
            | "credentials_file"
    ) || key.contains("tls")
        || key.contains("credential")
        || key.contains("certificate")
        || key.contains("pairing")
        || key.contains("password")
        || key.contains("token")
}

fn supported_setting_destination(key: &str) -> Option<&'static str> {
    match key {
        "fps" => Some("stream.default_fps"),
        "resolution" => Some("stream.default_resolution"),
        "encoder" => Some("video.encoder_preference"),
        "output_name" => Some("capture.output_name"),
        "adapter_name" => Some("capture.adapter_name"),
        "audio_sink" => Some("audio.sink"),
        "keyboard" => Some("input.keyboard_enabled"),
        "gamepad" => Some("input.gamepad_backend"),
        _ => None,
    }
}

#[derive(Clone, Debug)]
struct SourceSnapshot {
    product: LegacyProduct,
    directory: PathBuf,
    files: Vec<SnapshotFile>,
}

#[derive(Clone, Debug)]
struct SnapshotFile {
    relative: PathBuf,
    sha256: String,
}

fn snapshot_sources(
    sources: &[LegacyConfigSource],
    backup_directory: &Path,
) -> PackageResult<Vec<SourceSnapshot>> {
    let mut snapshots = Vec::with_capacity(sources.len());
    for (index, source) in sources.iter().enumerate() {
        let collected = collect_configuration_files(&source.root)?
            .ok_or("a predecessor configuration source no longer contains supported files")?;
        let directory = backup_directory
            .join("source")
            .join(source.product.directory_name())
            .join(index.to_string());
        create_private_directory(&directory)?;
        let mut copied = Vec::with_capacity(collected.files.len());
        for relative in collected.files {
            let destination = directory.join(&relative);
            let source_path = collected
                .copy_sources
                .get(&relative)
                .cloned()
                .unwrap_or_else(|| source.root.join(&relative));
            copy_regular_file(&source_path, &destination)?;
            copied.push(SnapshotFile {
                relative,
                sha256: digest_regular_file(&destination)?,
            });
        }
        snapshots.push(SourceSnapshot {
            product: source.product,
            directory,
            files: copied,
        });
    }
    Ok(snapshots)
}

fn preflight_merge(import_root: &Path, snapshots: &[SourceSnapshot]) -> PackageResult<()> {
    for snapshot in snapshots {
        let product_root = import_root.join(snapshot.product.directory_name());
        for file in &snapshot.files {
            let destination = product_root.join(&file.relative);
            if destination.try_exists().map_err(io_error)?
                && digest_regular_file(&destination)? != file.sha256
            {
                return Err("predecessor configuration conflicts with an existing import".into());
            }
        }
    }
    Ok(())
}

fn merge_snapshots(
    import_root: &Path,
    snapshots: &[SourceSnapshot],
    created: &mut Vec<ImportedFile>,
) -> PackageResult<()> {
    for snapshot in snapshots {
        let product_root = import_root.join(snapshot.product.directory_name());
        for file in &snapshot.files {
            let destination = product_root.join(&file.relative);
            if destination.try_exists().map_err(io_error)? {
                continue;
            }
            copy_regular_file(&snapshot.directory.join(&file.relative), &destination)?;
            created.push(ImportedFile {
                path: destination,
                sha256: file.sha256.clone(),
            });
        }
    }
    Ok(())
}

fn collect_configuration_files(root: &Path) -> PackageResult<Option<CollectedConfigurationFiles>> {
    if !root.try_exists().map_err(io_error)? {
        return Ok(None);
    }
    if !is_real_directory(root)? {
        return Err("predecessor configuration root is not a directory".into());
    }

    let mut files = Vec::new();
    let mut copy_sources = BTreeMap::new();
    let mut configured_state_files = BTreeSet::new();
    let mut configured_authentication_files = BTreeSet::new();
    for name in CONFIGURATION_FILES {
        let path = root.join(name);
        if path.try_exists().map_err(io_error)? {
            validate_regular_file(&path)?;
            files.push(PathBuf::from(name));
        }
    }
    collect_configured_sensitive_files(
        root,
        &mut files,
        &mut configured_authentication_files,
        &mut copy_sources,
    )?;
    collect_configured_state_files(
        root,
        &mut files,
        &mut configured_state_files,
        &mut copy_sources,
    )?;
    for name in CONFIGURATION_DIRECTORIES {
        let path = root.join(name);
        if path.try_exists().map_err(io_error)? {
            collect_directory_files(root, &path, &mut files, &copy_sources)?;
        }
    }
    deduplicate_collected_paths(
        root,
        &mut files,
        &mut copy_sources,
        &mut configured_state_files,
        &mut configured_authentication_files,
    )?;
    files.sort();
    Ok((!files.is_empty()).then_some(CollectedConfigurationFiles {
        files,
        copy_sources,
        configured_state_files,
        configured_authentication_files,
    }))
}

fn strip_unquoted_comment(line: &str) -> &str {
    let mut inside_double_quotes = false;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if inside_double_quotes && character == '\\' {
            escaped = true;
            continue;
        }
        match character {
            '"' => inside_double_quotes = !inside_double_quotes,
            '#' if !inside_double_quotes => return &line[..index],
            _ => {}
        }
    }
    line
}

/// Preserve only configured identity/credential files whose canonical regular
/// file remains inside the predecessor root. This makes an approved custom
/// `pkey`, `cert`, or `credentials_file` usable after copy without allowing a
/// legacy config to cause an elevated installer to read another location.
fn collect_configured_sensitive_files(
    root: &Path,
    files: &mut Vec<PathBuf>,
    configured_authentication_files: &mut BTreeSet<PathBuf>,
    copy_sources: &mut BTreeMap<PathBuf, PathBuf>,
) -> PackageResult<()> {
    let canonical_root = root.canonicalize().map_err(io_error)?;
    for name in [
        "apollo.conf",
        "sunshine.conf",
        "vibeshine.conf",
        "vibepollo.conf",
    ] {
        let config = root.join(name);
        if !config.try_exists().map_err(io_error)? {
            continue;
        }
        let text = String::from_utf8(read_configuration_file(&config)?).map_err(|_| {
            "predecessor configuration contains a non-text settings file".to_owned()
        })?;
        for line in text.lines() {
            let line = strip_unquoted_comment(line).trim();
            if line.is_empty() || line.starts_with(';') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            if !matches!(key.as_str(), "pkey" | "cert" | "credentials_file") {
                continue;
            }
            let configured_path = value.trim().trim_matches('"');
            if key == "credentials_file" && configured_path.is_empty() {
                continue;
            }
            let relative = Path::new(configured_path);
            if relative.as_os_str().is_empty()
                || !relative
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_)))
            {
                return Err(
                    "predecessor configuration has an unsafe configured identity path".into(),
                );
            }
            let resolved_relative = resolved_existing_relative_path(root, relative)?;
            let canonical = root
                .join(resolved_relative)
                .canonicalize()
                .map_err(io_error)?;
            if !canonical.starts_with(&canonical_root) {
                return Err(
                    "predecessor configuration has an escaping configured identity path".into(),
                );
            }
            validate_regular_file(&canonical)?;
            let copied = relative.to_path_buf();
            copy_sources.insert(copied.clone(), canonical);
            if key == "credentials_file" {
                configured_authentication_files.insert(copied.clone());
            }
            if !files.contains(&copied) {
                files.push(copied);
            }
        }
    }
    Ok(())
}

/// Preserve configured primary and auxiliary state files for the migration
/// importers. State paths are followed only when their canonical regular files
/// remain inside the supplied predecessor root.
fn collect_configured_state_files(
    root: &Path,
    files: &mut Vec<PathBuf>,
    configured_state_files: &mut BTreeSet<PathBuf>,
    copy_sources: &mut BTreeMap<PathBuf, PathBuf>,
) -> PackageResult<()> {
    let canonical_root = root.canonicalize().map_err(io_error)?;
    let mut configured = BTreeMap::<String, PathBuf>::new();
    for name in [
        "apollo.conf",
        "sunshine.conf",
        "vibeshine.conf",
        "vibepollo.conf",
    ] {
        let config = root.join(name);
        if !config.try_exists().map_err(io_error)? {
            continue;
        }
        let text = String::from_utf8(read_configuration_file(&config)?).map_err(|_| {
            "predecessor configuration contains a non-text settings file".to_owned()
        })?;
        for line in text.lines() {
            let line = strip_unquoted_comment(line).trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            if !matches!(key.as_str(), "file_state" | "vibeshine_file_state") {
                continue;
            }
            let value = value.trim().trim_matches('"');
            if value.is_empty() {
                continue;
            }
            let relative = Path::new(value);
            if !relative
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)))
            {
                return Err("predecessor configuration has an unsafe state path".into());
            }
            if configured
                .insert(key, relative.to_path_buf())
                .is_some_and(|previous| previous != relative.to_path_buf())
            {
                return Err("predecessor configuration has conflicting state paths".into());
            }
            let Some(canonical) = optional_contained_state_file(root, &canonical_root, relative)?
            else {
                continue;
            };
            let copied = relative.to_path_buf();
            configured_state_files.insert(copied.clone());
            copy_sources.insert(copied.clone(), canonical);
            if !files.contains(&copied) {
                files.push(copied);
            }
        }
    }
    Ok(())
}

fn optional_contained_state_file(
    root: &Path,
    canonical_root: &Path,
    relative: &Path,
) -> PackageResult<Option<PathBuf>> {
    let path = root.join(relative);
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            let canonical = path.canonicalize().map_err(io_error)?;
            if !canonical.starts_with(canonical_root) {
                return Err("predecessor configuration has an escaping state path".into());
            }
            validate_regular_file(&canonical)?;
            Ok(Some(canonical))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut ancestor = path.parent();
            while let Some(candidate) = ancestor {
                match fs::symlink_metadata(candidate) {
                    Ok(_) => {
                        let canonical = candidate.canonicalize().map_err(io_error)?;
                        if !canonical.starts_with(canonical_root)
                            || !fs::metadata(canonical).map_err(io_error)?.is_dir()
                        {
                            return Err("predecessor configuration has an unsafe state path".into());
                        }
                        return Ok(None);
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        ancestor = candidate.parent();
                    }
                    Err(error) => return Err(io_error(error)),
                }
            }
            Err("predecessor configuration has an unsafe state path".into())
        }
        Err(error) => Err(io_error(error)),
    }
}

fn collect_directory_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<PathBuf>,
    copy_sources: &BTreeMap<PathBuf, PathBuf>,
) -> PackageResult<()> {
    let directory_metadata = fs::symlink_metadata(directory).map_err(io_error)?;
    if is_reparse_or_symlink(&directory_metadata) {
        let relative = directory
            .strip_prefix(root)
            .map_err(|_| "predecessor configuration layout is invalid")?;
        for configured in copy_sources.keys() {
            if configured_alias_covers_entry(root, configured, relative)? {
                return Ok(());
            }
        }
        return Err("predecessor configuration directory is unsafe".into());
    }
    if !directory_metadata.is_dir() {
        return Err("predecessor configuration directory is unsafe".into());
    }
    for entry in fs::read_dir(directory).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(io_error)?;
        if is_reparse_or_symlink(&metadata) {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| "predecessor configuration layout is invalid")?;
            let mut is_configured_alias = false;
            for configured in copy_sources.keys() {
                if configured_alias_covers_entry(root, configured, relative)? {
                    is_configured_alias = true;
                    break;
                }
            }
            if is_configured_alias {
                continue;
            }
            return Err("predecessor configuration contains a symbolic link".into());
        }
        if metadata.is_dir() {
            collect_directory_files(root, &path, files, copy_sources)?;
        } else if metadata.is_file() {
            validate_regular_file(&path)?;
            let relative = path
                .strip_prefix(root)
                .map_err(|_| "predecessor configuration layout is invalid")?;
            files.push(relative.to_path_buf());
        } else {
            return Err("predecessor configuration contains a non-file entry".into());
        }
    }
    Ok(())
}

fn deduplicate_collected_paths(
    root: &Path,
    files: &mut Vec<PathBuf>,
    copy_sources: &mut BTreeMap<PathBuf, PathBuf>,
    configured_state_files: &mut BTreeSet<PathBuf>,
    configured_authentication_files: &mut BTreeSet<PathBuf>,
) -> PackageResult<()> {
    let mut path_by_identity = BTreeMap::<PathBuf, PathBuf>::new();
    let mut unique_files = Vec::with_capacity(files.len());
    for relative in files.drain(..) {
        let identity = resolved_existing_relative_path(root, &relative)?;
        if let Some(existing_relative) = path_by_identity.get(&identity) {
            if let Some(source) = copy_sources.remove(&relative) {
                copy_sources
                    .entry(existing_relative.clone())
                    .or_insert(source);
            }
            if configured_state_files.remove(&relative) {
                configured_state_files.insert(existing_relative.clone());
            }
            if configured_authentication_files.remove(&relative) {
                configured_authentication_files.insert(existing_relative.clone());
            }
        } else {
            path_by_identity.insert(identity, relative.clone());
            unique_files.push(relative);
        }
    }
    *files = unique_files;
    Ok(())
}

fn configured_alias_covers_entry(
    root: &Path,
    configured: &Path,
    entry: &Path,
) -> PackageResult<bool> {
    let entry_component_count = entry.components().count();
    let configured_components: Vec<_> = configured.components().collect();
    if configured_components.len() < entry_component_count {
        return Ok(false);
    }
    let mut configured_prefix = PathBuf::new();
    for component in configured_components
        .into_iter()
        .take(entry_component_count)
    {
        configured_prefix.push(component.as_os_str());
    }
    Ok(resolved_existing_relative_path(root, &configured_prefix)?
        == resolved_existing_relative_path(root, entry)?)
}

/// Resolves the directory-entry spelling without following the final symlink.
/// On Windows this gives case-insensitive paths the same identity while keeping
/// distinct entries in case-sensitive directories separate.
fn resolved_existing_relative_path(root: &Path, relative: &Path) -> PackageResult<PathBuf> {
    #[cfg(not(windows))]
    {
        let _ = root;
        return Ok(relative.to_path_buf());
    }
    #[cfg(windows)]
    {
        let mut current = root.to_path_buf();
        let mut resolved = PathBuf::new();
        for component in relative.components() {
            let std::path::Component::Normal(requested) = component else {
                return Err("predecessor configuration path is not relative".into());
            };
            let name = existing_windows_path_component(&current, requested)?;
            current.push(&name);
            resolved.push(name);
        }
        Ok(resolved)
    }
}

#[cfg(windows)]
fn resolved_existing_absolute_path(path: &Path) -> PackageResult<PathBuf> {
    let mut components = path.components();
    let Some(std::path::Component::Prefix(prefix)) = components.next() else {
        return Err("predecessor configuration root is not an absolute path".into());
    };
    let Some(std::path::Component::RootDir) = components.next() else {
        return Err("predecessor configuration root is not an absolute path".into());
    };

    let mut current = PathBuf::from(prefix.as_os_str());
    current.push(std::path::MAIN_SEPARATOR.to_string());
    let mut resolved = current.clone();
    for component in components {
        let std::path::Component::Normal(requested) = component else {
            return Err("predecessor configuration root cannot be resolved".into());
        };
        let name = existing_windows_path_component(&current, requested)?;
        current.push(&name);
        resolved.push(name);
    }
    Ok(resolved)
}

#[cfg(windows)]
fn existing_windows_path_component(
    parent: &Path,
    requested: &std::ffi::OsStr,
) -> PackageResult<std::ffi::OsString> {
    let mut insensitive_match = None;
    for entry in fs::read_dir(parent).map_err(io_error)? {
        let name = entry.map_err(io_error)?.file_name();
        if name.as_os_str() == requested {
            return Ok(name);
        }
        if windows_path_component_matches(requested, &name)? {
            if insensitive_match.is_some() {
                return Err("predecessor configuration path casing is ambiguous".into());
            }
            insensitive_match = Some(name);
        }
    }
    insensitive_match.ok_or_else(|| "predecessor configuration path cannot be resolved".into())
}

#[cfg(windows)]
fn windows_path_component_matches(
    left: &std::ffi::OsStr,
    right: &std::ffi::OsStr,
) -> PackageResult<bool> {
    use std::os::windows::ffi::OsStrExt;

    let left_units: Vec<u16> = left.encode_wide().collect();
    let right_units: Vec<u16> = right.encode_wide().collect();
    if left_units.is_empty() || right_units.is_empty() {
        return Ok(left_units.is_empty() && right_units.is_empty());
    }
    let left_length = i32::try_from(left_units.len())
        .map_err(|_| "predecessor path component is too long to compare")?;
    let right_length = i32::try_from(right_units.len())
        .map_err(|_| "predecessor path component is too long to compare")?;
    // SAFETY: both buffers remain alive for the call, and their lengths are the
    // exact UTF-16 code-unit counts passed to CompareStringOrdinal.
    let comparison = unsafe {
        compare_string_ordinal(
            left_units.as_ptr(),
            left_length,
            right_units.as_ptr(),
            right_length,
            1,
        )
    };
    match comparison {
        1 | 3 => Ok(false),
        2 => Ok(true),
        _ => Err("Windows could not compare predecessor path casing".into()),
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    #[link_name = "CompareStringOrdinal"]
    fn compare_string_ordinal(
        left: *const u16,
        left_length: i32,
        right: *const u16,
        right_length: i32,
        ignore_case: i32,
    ) -> i32;
}

fn is_real_directory(path: &Path) -> PackageResult<bool> {
    let metadata = fs::symlink_metadata(path).map_err(io_error)?;
    Ok(metadata.is_dir() && !is_reparse_or_symlink(&metadata))
}

fn validate_regular_file(path: &Path) -> PackageResult<()> {
    let metadata = fs::symlink_metadata(path).map_err(io_error)?;
    if !metadata.is_file() || is_reparse_or_symlink(&metadata) {
        return Err("predecessor configuration contains an unsafe file".into());
    }
    if metadata.len() > MAX_CONFIGURATION_FILE_BYTES {
        return Err("predecessor configuration file exceeds the migration limit".into());
    }
    Ok(())
}

fn is_reparse_or_symlink(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn copy_regular_file(source: &Path, destination: &Path) -> PackageResult<()> {
    validate_regular_file(source)?;
    let parent = destination
        .parent()
        .ok_or("migration destination has no parent")?;
    create_private_directory(parent)?;
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)
        .map_err(io_error)?;
    let mut input = File::open(source).map_err(io_error)?;
    io::copy(&mut input, &mut output).map_err(io_error)?;
    output.sync_all().map_err(io_error)
}

fn digest_regular_file(path: &Path) -> PackageResult<String> {
    validate_regular_file(path)?;
    let mut file = File::open(path).map_err(io_error)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(io_error)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(crate::encode_hex(&digest.finalize()))
}

fn remove_empty_parents(file: &Path) {
    let mut directory = file.parent();
    while let Some(path) = directory {
        match fs::remove_dir(path) {
            Ok(()) => directory = path.parent(),
            Err(_) => return,
        }
    }
}

fn io_error(_: io::Error) -> String {
    "configuration migration filesystem operation failed".into()
}

#[cfg(test)]
mod tests;
