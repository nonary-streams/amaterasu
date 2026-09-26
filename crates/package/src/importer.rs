//! Materializes a reviewed legacy import plan into the runtime's durable state.
//!
//! This module consumes only [`ImportPlan`] values. It never rereads a legacy
//! file, so the reviewer-visible plan remains the boundary between untrusted
//! predecessor data and the Amaterasu state format.

use crate::migration::{ImportPlan, ImportedApplication};
use crate::{PackageResult, create_private_directory, read_json, write_json_atomically};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const CATALOG_CALLER: &str = "amaterasu.catalog";
const CONFIGURATION_CALLER: &str = "amaterasu.configuration";
const CATALOG_COLLECTION: &str = "principal/amaterasu.management";
const CONFIGURATION_KEY: &str = "principal/amaterasu.management";
const IMPORT_TRANSACTION_FILE: &str = ".legacy-runtime-import.json";

/// The outcome of materializing a reviewed legacy plan.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RuntimeImportReceipt {
    pub state_directory: PathBuf,
    pub applications: usize,
    pub settings: usize,
}

/// Stores supported applications and settings from `plan` in a provisioned
/// runtime root. A completed import may be repeated only while its published
/// bootstrap records still match the same plan exactly.
///
/// The catalog and configuration records are installer-owned bootstrap records.
/// This operation refuses to replace changed records, so it cannot overwrite
/// a user's Amaterasu catalog or configuration. Catalog state
/// is published through the same immutable-shard and atomic-manifest format
/// used by the state module; configuration is the same caller-scoped document
/// read by the configuration module.
pub fn materialize_import_plan(
    runtime_root: &Path,
    plan: &ImportPlan,
) -> PackageResult<RuntimeImportReceipt> {
    materialize_import_plan_inner(runtime_root, plan, false)
}

fn materialize_import_plan_inner(
    runtime_root: &Path,
    plan: &ImportPlan,
    fail_before_catalog: bool,
) -> PackageResult<RuntimeImportReceipt> {
    if plan.schema != 1 {
        return Err("legacy import plan schema is unsupported".into());
    }
    let state_directory = runtime_state_directory(runtime_root)?;
    let catalog = catalog_document(&plan.applications)?;
    let configuration = configuration_document(&plan.settings)?;

    let catalog_directory =
        collection_directory(&state_directory, CATALOG_CALLER, CATALOG_COLLECTION);
    let configuration_path = namespace_path(&state_directory, CONFIGURATION_CALLER);
    recover_incomplete_import(
        &state_directory,
        &catalog_directory,
        &configuration_path,
        catalog.as_ref(),
        configuration.as_ref(),
    )?;
    if existing_import_matches(
        &catalog_directory,
        &configuration_path,
        catalog.as_ref(),
        configuration.as_ref(),
    )? {
        return Ok(RuntimeImportReceipt {
            state_directory,
            applications: catalog
                .as_ref()
                .map_or(0, |document| document.entries.len()),
            settings: configuration
                .as_ref()
                .map_or(0, |document| document.values.len()),
        });
    }
    preflight_empty_targets(
        &catalog_directory,
        &configuration_path,
        catalog.is_some(),
        configuration.is_some(),
    )?;

    let mut transaction = ImportTransaction {
        schema: 1,
        configuration_published: false,
        catalog_published: false,
    };
    let transaction_path = state_directory.join(IMPORT_TRANSACTION_FILE);
    write_json_atomically(&transaction_path, &transaction)?;

    // Every target is checked before the first mutation. Each individual state
    // record has one atomic publication point: configuration's file replacement
    // or catalog's manifest replacement.
    if let Some(document) = configuration.as_ref() {
        // Persist destructive intent before the file operation. A process can
        // die after the OS commits a replacement but before it reports success.
        transaction.configuration_published = true;
        write_json_atomically(&transaction_path, &transaction)?;
        if let Err(error) = write_configuration(&configuration_path, document) {
            let _ = remove_import_transaction(
                &transaction_path,
                &catalog_directory,
                &configuration_path,
                &transaction,
            );
            return Err(error);
        }
    }
    if let Some(document) = catalog.as_ref() {
        transaction.catalog_published = true;
        write_json_atomically(&transaction_path, &transaction)?;
        if fail_before_catalog {
            let _ = remove_import_transaction(
                &transaction_path,
                &catalog_directory,
                &configuration_path,
                &transaction,
            );
            return Err("injected catalog publication failure".into());
        }
        if let Err(error) = write_catalog(&catalog_directory, document) {
            let _ = remove_import_transaction(
                &transaction_path,
                &catalog_directory,
                &configuration_path,
                &transaction,
            );
            return Err(error);
        }
    }
    fs::remove_file(&transaction_path).map_err(|error| error.to_string())?;

    Ok(RuntimeImportReceipt {
        state_directory,
        applications: catalog
            .as_ref()
            .map_or(0, |document| document.entries.len()),
        settings: configuration
            .as_ref()
            .map_or(0, |document| document.values.len()),
    })
}

/// Installer restart may repeat a completed import. Accept only the exact
/// bootstrap records that this plan would have published; user edits and
/// partially populated targets remain conflicts.
fn existing_import_matches(
    catalog_directory: &Path,
    configuration_path: &Path,
    catalog: Option<&CatalogDocument>,
    configuration: Option<&ConfigurationDocument>,
) -> PackageResult<bool> {
    let expected_targets = catalog.is_some() || configuration.is_some();
    if !expected_targets {
        return Ok(false);
    }
    let configuration_matches = match configuration {
        Some(document) => json_file_matches(configuration_path, &configuration_snapshot(document))?,
        None => !configuration_path
            .try_exists()
            .map_err(|error| error.to_string())?,
    };
    let catalog_matches = match catalog {
        Some(document) => {
            let expected_names = ["manifest.json", "shard-0000000000000001-00000000.json"];
            let exact_entries = match fs::read_dir(catalog_directory) {
                Ok(entries) => {
                    let names = entries
                        .map(|entry| {
                            entry
                                .map(|entry| entry.file_name())
                                .map_err(|error| error.to_string())
                        })
                        .collect::<PackageResult<Vec<_>>>()?;
                    names.len() == expected_names.len()
                        && names
                            .iter()
                            .all(|name| expected_names.iter().any(|expected| name == expected))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(error) => return Err(error.to_string()),
            };
            exact_entries
                && json_file_matches(
                    &catalog_directory.join("manifest.json"),
                    &catalog_manifest_snapshot(document),
                )?
                && json_file_matches(
                    &catalog_directory.join("shard-0000000000000001-00000000.json"),
                    &json!(document.entries),
                )?
        }
        None => !directory_has_entries(catalog_directory)?,
    };
    Ok(configuration_matches && catalog_matches)
}

fn configuration_snapshot(document: &ConfigurationDocument) -> Value {
    let mut values = BTreeMap::new();
    values.insert(CONFIGURATION_KEY, json!(document));
    json!(Namespace {
        format_version: 1,
        revision: 1,
        values,
    })
}

fn catalog_manifest_snapshot(document: &CatalogDocument) -> Value {
    let mut shards = BTreeMap::new();
    shards.insert(
        String::new(),
        ShardReference {
            revision: 1,
            slot: 0,
            count: document.entries.len(),
        },
    );
    json!(CollectionManifest {
        format_version: 1,
        revision: 1,
        metadata: json!({"revision": document.revision, "next_id": document.next_id}),
        count: document.entries.len(),
        shards,
    })
}

fn json_file_matches(path: &Path, expected: &Value) -> PackageResult<bool> {
    if !path.try_exists().map_err(|error| error.to_string())? {
        return Ok(false);
    }
    let actual: Value = read_json(path)?;
    Ok(actual == *expected)
}

#[derive(Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportTransaction {
    schema: u32,
    configuration_published: bool,
    catalog_published: bool,
}

fn recover_incomplete_import(
    state_directory: &Path,
    catalog_directory: &Path,
    configuration_path: &Path,
    catalog: Option<&CatalogDocument>,
    configuration: Option<&ConfigurationDocument>,
) -> PackageResult<()> {
    let transaction_path = state_directory.join(IMPORT_TRANSACTION_FILE);
    if !transaction_path
        .try_exists()
        .map_err(|error| error.to_string())?
    {
        return Ok(());
    }
    let transaction: ImportTransaction = read_json(&transaction_path)?;
    if transaction.schema != 1 {
        return Err("legacy import transaction is invalid".into());
    }
    // The journal records intent before publication, but a crash can leave
    // either target absent. Never delete a target whose content is no longer
    // the exact bootstrap record for the plan being retried.
    if transaction.configuration_published
        && configuration_path
            .try_exists()
            .map_err(|error| error.to_string())?
    {
        let matches = match configuration {
            Some(document) => {
                json_file_matches(configuration_path, &configuration_snapshot(document))?
            }
            None => false,
        };
        if !matches {
            return Err("interrupted import configuration changed before recovery".into());
        }
    }
    if transaction.catalog_published && directory_has_entries(catalog_directory)? {
        let Some(document) = catalog else {
            return Err("interrupted import catalog changed before recovery".into());
        };
        let expected = ["manifest.json", "shard-0000000000000001-00000000.json"];
        for entry in fs::read_dir(catalog_directory).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let name = entry.file_name();
            if !expected.iter().any(|candidate| name == *candidate) {
                return Err("interrupted import catalog changed before recovery".into());
            }
            let matches = if name == "manifest.json" {
                json_file_matches(&entry.path(), &catalog_manifest_snapshot(document))?
            } else {
                json_file_matches(&entry.path(), &json!(document.entries))?
            };
            if !matches {
                return Err("interrupted import catalog changed before recovery".into());
            }
        }
    }
    remove_import_transaction(
        &transaction_path,
        catalog_directory,
        configuration_path,
        &transaction,
    )
}

fn remove_import_transaction(
    transaction_path: &Path,
    catalog_directory: &Path,
    configuration_path: &Path,
    transaction: &ImportTransaction,
) -> PackageResult<()> {
    if transaction.catalog_published {
        remove_catalog_import(catalog_directory)?;
    }
    if transaction.configuration_published
        && configuration_path
            .try_exists()
            .map_err(|error| error.to_string())?
    {
        fs::remove_file(configuration_path).map_err(|error| error.to_string())?;
    }
    if transaction_path
        .try_exists()
        .map_err(|error| error.to_string())?
    {
        fs::remove_file(transaction_path).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn remove_catalog_import(directory: &Path) -> PackageResult<()> {
    for name in ["manifest.json", "shard-0000000000000001-00000000.json"] {
        let path = directory.join(name);
        if path.try_exists().map_err(|error| error.to_string())? {
            fs::remove_file(path).map_err(|error| error.to_string())?;
        }
    }
    match fs::remove_dir(directory) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
            Err("legacy catalog import contains unexpected files".into())
        }
        Err(error) => Err(error.to_string()),
    }
}

/// Resolves the machine-private pending seed location owned by the provisioned
/// runtime. The production bootstrap intentionally has no state-directory
/// setting: SCM later assigns the consumed seed to one SID-specific worker
/// root, so neither legacy input nor signed module policy can redirect it.
pub fn runtime_state_directory(runtime_root: &Path) -> PackageResult<PathBuf> {
    let root = runtime_root
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let directory = root.join("data").join("state");
    create_private_directory(&directory)?;
    let directory = directory
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if !directory.starts_with(&root) {
        return Err("runtime state directory escapes its runtime root".into());
    }
    Ok(directory)
}

fn preflight_empty_targets(
    catalog_directory: &Path,
    configuration_path: &Path,
    has_catalog: bool,
    has_configuration: bool,
) -> PackageResult<()> {
    if has_catalog && directory_has_entries(catalog_directory)? {
        return Err("Amaterasu catalog already contains persisted state".into());
    }
    if has_configuration
        && configuration_path
            .try_exists()
            .map_err(|error| error.to_string())?
    {
        return Err("Amaterasu configuration already contains persisted state".into());
    }
    Ok(())
}

fn directory_has_entries(path: &Path) -> PackageResult<bool> {
    match fs::read_dir(path) {
        Ok(mut entries) => Ok(entries
            .next()
            .transpose()
            .map_err(|error| error.to_string())?
            .is_some()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

fn catalog_document(
    applications: &[ImportedApplication],
) -> PackageResult<Option<CatalogDocument>> {
    if applications.is_empty() {
        return Ok(None);
    }
    if applications.len() > 100_000 {
        return Err("legacy import plan exceeds the catalog entry limit".into());
    }
    let mut entries = BTreeMap::new();
    for (index, application) in applications.iter().enumerate() {
        let entry = CatalogEntry {
            name: application.name.clone(),
            description: String::new(),
            tags: vec![
                "imported".into(),
                application.product.directory_name().into(),
            ],
            launch: CatalogLaunch::Executable {
                program: application.executable.clone(),
                arguments: application.arguments.clone(),
                working_directory: application.working_directory.clone(),
                preparation: Vec::new(),
            },
        };
        validate_catalog_entry(&entry)?;
        entries.insert(format!("app-{:016x}", index + 1), entry);
    }
    Ok(Some(CatalogDocument {
        revision: 1,
        next_id: applications.len() as u64 + 1,
        entries,
    }))
}

fn configuration_document(
    settings: &BTreeMap<String, String>,
) -> PackageResult<Option<ConfigurationDocument>> {
    let mut fields = BTreeMap::new();
    let mut values = BTreeMap::new();
    for (key, value) in settings {
        let Some((field, value)) = imported_setting(key, value) else {
            continue;
        };
        fields.insert(key.clone(), field);
        values.insert(key.clone(), value);
    }
    if values.is_empty() {
        return Ok(None);
    }
    Ok(Some(ConfigurationDocument {
        revision: 1,
        fields,
        values,
    }))
}

fn imported_setting(key: &str, raw: &str) -> Option<(ConfigurationField, Value)> {
    match key {
        "stream.default_fps" => {
            let value = raw
                .parse::<i64>()
                .ok()
                .filter(|value| (1..=240).contains(value))?;
            Some((
                ConfigurationField::Integer {
                    default: value,
                    minimum: Some(1),
                    maximum: Some(240),
                },
                json!(value),
            ))
        }
        "stream.default_resolution" => {
            valid_resolution(raw)?;
            string_setting(raw, 3, 32)
        }
        "video.encoder_preference" => {
            let value = raw.to_ascii_lowercase();
            ["nvenc", "amf", "qsv", "software"]
                .contains(&value.as_str())
                .then(|| {
                    (
                        ConfigurationField::String {
                            default: value.clone(),
                            min_length: Some(1),
                            max_length: Some(16),
                        },
                        json!(value),
                    )
                })
        }
        "capture.output_name" => string_setting(raw, 1, 512),
        "capture.adapter_name" => string_setting(raw, 1, 512),
        "audio.sink" => string_setting(raw, 1, 512),
        "input.keyboard_enabled" => match raw.to_ascii_lowercase().as_str() {
            "enabled" | "true" => {
                Some((ConfigurationField::Boolean { default: true }, json!(true)))
            }
            "disabled" | "false" => {
                Some((ConfigurationField::Boolean { default: false }, json!(false)))
            }
            _ => None,
        },
        "input.gamepad_backend" => {
            let value = raw.to_ascii_lowercase();
            ["auto", "vigem", "sdl"].contains(&value.as_str()).then(|| {
                (
                    ConfigurationField::String {
                        default: value.clone(),
                        min_length: Some(1),
                        max_length: Some(16),
                    },
                    json!(value),
                )
            })
        }
        _ => None,
    }
}

fn valid_resolution(value: &str) -> Option<()> {
    let (width, height) = value.split_once(['x', 'X'])?;
    let width = width.parse::<u16>().ok()?;
    let height = height.parse::<u16>().ok()?;
    ((1..=16_384).contains(&width) && (1..=16_384).contains(&height)).then_some(())
}

fn string_setting(
    value: &str,
    minimum: usize,
    maximum: usize,
) -> Option<(ConfigurationField, Value)> {
    let length = value.chars().count();
    (minimum..=maximum)
        .contains(&length)
        .then_some(())
        .filter(|()| !value.chars().any(char::is_control))?;
    Some((
        ConfigurationField::String {
            default: value.into(),
            min_length: Some(minimum),
            max_length: Some(maximum),
        },
        json!(value),
    ))
}

fn write_configuration(path: &Path, document: &ConfigurationDocument) -> PackageResult<()> {
    let parent = path
        .parent()
        .ok_or("configuration state has no parent directory")?;
    create_private_directory(parent)?;
    let mut values = BTreeMap::new();
    values.insert(CONFIGURATION_KEY, json!(document));
    write_json_atomically(
        path,
        &Namespace {
            format_version: 1,
            revision: 1,
            values,
        },
    )
}

fn write_catalog(directory: &Path, document: &CatalogDocument) -> PackageResult<()> {
    create_private_directory(directory)?;
    let shard_path = directory.join("shard-0000000000000001-00000000.json");
    write_json_atomically(&shard_path, &document.entries)?;
    let mut shards = BTreeMap::new();
    shards.insert(
        String::new(),
        ShardReference {
            revision: 1,
            slot: 0,
            count: document.entries.len(),
        },
    );
    write_json_atomically(
        &directory.join("manifest.json"),
        &CollectionManifest {
            format_version: 1,
            revision: 1,
            metadata: json!({"revision": document.revision, "next_id": document.next_id}),
            count: document.entries.len(),
            shards,
        },
    )
}

fn namespace_path(state_directory: &Path, caller: &str) -> PathBuf {
    state_directory
        .join("namespaces")
        .join(hex_component(caller))
        .join("state.json")
}

fn collection_directory(state_directory: &Path, caller: &str, collection: &str) -> PathBuf {
    state_directory
        .join("namespaces")
        .join(hex_component(caller))
        .join("collections")
        .join(hex_component(collection))
}

fn hex_component(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn validate_catalog_entry(entry: &CatalogEntry) -> PackageResult<()> {
    valid_text(&entry.name, 160, true, "application name")?;
    valid_text(&entry.description, 1024, false, "application description")?;
    if entry.tags.len() > 16 {
        return Err("imported application has too many tags".into());
    }
    for tag in &entry.tags {
        valid_text(tag, 64, true, "application tag")?;
    }
    let CatalogLaunch::Executable {
        program,
        arguments,
        working_directory,
        ..
    } = &entry.launch;
    valid_catalog_path(program)?;
    if let Some(directory) = working_directory {
        valid_catalog_path(directory)?;
    }
    if arguments.len() > 32
        || arguments
            .iter()
            .any(|argument| !valid_plain_text(argument, 1024))
    {
        return Err("imported application arguments exceed catalog bounds".into());
    }
    if serde_json::to_vec(entry)
        .map_err(|error| error.to_string())?
        .len()
        > 11_950
    {
        return Err("imported application exceeds catalog bounds".into());
    }
    Ok(())
}

fn valid_text(value: &str, maximum: usize, nonempty: bool, description: &str) -> PackageResult<()> {
    if !valid_plain_text(value, maximum) || (nonempty && value.trim().is_empty()) {
        return Err(format!("{description} violates catalog bounds"));
    }
    Ok(())
}

fn valid_plain_text(value: &str, maximum: usize) -> bool {
    value.len() <= maximum && !value.chars().any(char::is_control)
}

fn valid_catalog_path(value: &str) -> PackageResult<()> {
    let bytes = value.as_bytes();
    let drive_rooted = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\');
    if !valid_plain_text(value, 2048)
        || !(value.starts_with('/') || value.starts_with("\\\\") || drive_rooted)
        || value
            .split(['/', '\\'])
            .any(|part| matches!(part, "." | ".."))
    {
        return Err("imported application path violates catalog bounds".into());
    }
    Ok(())
}

#[derive(Serialize)]
struct Namespace {
    format_version: u32,
    revision: u64,
    values: BTreeMap<&'static str, Value>,
}

#[derive(Serialize)]
struct CollectionManifest {
    format_version: u32,
    revision: u64,
    metadata: Value,
    count: usize,
    shards: BTreeMap<String, ShardReference>,
}

#[derive(Serialize)]
struct ShardReference {
    revision: u64,
    slot: usize,
    count: usize,
}

#[derive(Serialize)]
struct CatalogDocument {
    revision: u64,
    next_id: u64,
    entries: BTreeMap<String, CatalogEntry>,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct CatalogEntry {
    name: String,
    description: String,
    tags: Vec<String>,
    launch: CatalogLaunch,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum CatalogLaunch {
    Executable {
        program: String,
        arguments: Vec<String>,
        working_directory: Option<String>,
        preparation: Vec<Value>,
    },
}

#[derive(Serialize)]
struct ConfigurationDocument {
    revision: u64,
    fields: BTreeMap<String, ConfigurationField>,
    values: BTreeMap<String, Value>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ConfigurationField {
    Boolean {
        default: bool,
    },
    Integer {
        default: i64,
        minimum: Option<i64>,
        maximum: Option<i64>,
    },
    String {
        default: String,
        min_length: Option<usize>,
        max_length: Option<usize>,
    },
}

#[cfg(test)]
mod tests;
