use crate::protocol::{Operation, StateError};
use crate::snapshot;
use amaterasu_storage::{
    RootLock, create_private_directory, read_bounded, sync_directory, write_json_atomically,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const MAX_KEY_BYTES: usize = 256;
const MAX_VALUE_BYTES: usize = snapshot::MAX_RESPONSE_BYTES;
const MAX_NAMESPACE_BYTES: u64 = 1024 * 1024;
const MAX_KEYS: usize = 4096;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Namespace {
    format_version: u32,
    revision: u64,
    values: BTreeMap<String, Value>,
}

impl Default for Namespace {
    fn default() -> Self {
        Self {
            format_version: 1,
            revision: 0,
            values: BTreeMap::new(),
        }
    }
}

pub(crate) struct StateStore {
    directory: PathBuf,
    // An exclusive lifetime lock prevents lost updates across module processes.
    _lock: RootLock,
}

impl StateStore {
    pub fn open(directory: PathBuf) -> Result<Self, String> {
        create_private_directory(&directory)?;
        let lock = RootLock::acquire(&directory)?;
        Ok(Self {
            directory,
            _lock: lock,
        })
    }

    pub fn handle(&self, caller: &str, payload: Value) -> Value {
        let response = self
            .execute(caller, payload)
            .unwrap_or_else(StateError::payload);
        if snapshot::response_fits(&response) {
            response
        } else {
            StateError::new("response_too_large", "encoded response exceeds 16384 bytes").payload()
        }
    }

    fn execute(&self, caller: &str, payload: Value) -> Result<Value, StateError> {
        let operation: Operation = serde_json::from_value(payload)
            .map_err(|error| StateError::new("invalid_request", error.to_string()))?;
        validate_operation(&operation)?;
        if let Operation::InitializeFeedbackHost { claim } = &operation {
            return self.initialize_feedback_host(caller, claim.clone());
        }
        let path = self.namespace_path(caller)?;
        match operation {
            Operation::InitializeFeedbackHost { .. } => unreachable!("handled above"),
            Operation::CollectionGet { collection, key } => {
                return crate::collection::Collection::open(&self.directory, &path, &collection)?
                    .get(key.as_deref());
            }
            Operation::CollectionPage {
                collection,
                revision,
                after,
                limit,
            } => {
                return crate::collection::Collection::open(&self.directory, &path, &collection)?
                    .page(revision, after.as_deref(), limit);
            }
            Operation::CollectionCommit {
                collection,
                expected_revision,
                metadata,
                edits,
            } => {
                return crate::collection::Collection::open(&self.directory, &path, &collection)?
                    .commit(expected_revision, metadata, edits);
            }
            Operation::CollectionDraftBegin {
                collection,
                draft_id,
                expected_revision,
                content_id,
            } => {
                return crate::draft::begin(
                    &self.directory,
                    &path,
                    &collection,
                    &draft_id,
                    expected_revision,
                    content_id,
                );
            }
            Operation::CollectionDraftAppend {
                collection,
                draft_id,
                sequence,
                edits,
            } => {
                return crate::draft::append(&path, &collection, &draft_id, sequence, edits);
            }
            Operation::CollectionDraftPublish {
                collection,
                draft_id,
                expected_revision,
                metadata,
            } => {
                return crate::draft::publish(
                    &self.directory,
                    &path,
                    &collection,
                    &draft_id,
                    expected_revision,
                    metadata,
                );
            }
            Operation::CollectionDraftAbort {
                collection,
                draft_id,
            } => {
                return crate::draft::abort(&path, &collection, &draft_id);
            }
            _ => (),
        }
        let mut namespace = load_namespace(&path)?;
        match operation {
            Operation::InitializeFeedbackHost { .. } => unreachable!("handled above"),
            Operation::CollectionGet { .. }
            | Operation::CollectionPage { .. }
            | Operation::CollectionCommit { .. }
            | Operation::CollectionDraftBegin { .. }
            | Operation::CollectionDraftAppend { .. }
            | Operation::CollectionDraftPublish { .. }
            | Operation::CollectionDraftAbort { .. } => unreachable!("collections handled above"),
            Operation::Get { key } => Ok(json!({
                "ok": true, "revision": namespace.revision,
                "found": namespace.values.contains_key(&key),
                "value": namespace.values.get(&key),
            })),
            Operation::Snapshot { cursor, limit } => {
                snapshot::page(&namespace.values, namespace.revision, cursor, limit)
            }
            Operation::Set {
                key,
                value,
                expected_revision,
            } => {
                check_revision(&namespace, expected_revision)?;
                namespace.values.insert(key, value);
                persist_mutation(&self.directory, &path, &mut namespace)?;
                Ok(json!({"ok": true, "revision": namespace.revision}))
            }
            Operation::Delete {
                key,
                expected_revision,
            } => {
                check_revision(&namespace, expected_revision)?;
                let deleted = namespace.values.remove(&key).is_some();
                persist_mutation(&self.directory, &path, &mut namespace)?;
                Ok(json!({"ok": true, "revision": namespace.revision, "deleted": deleted}))
            }
        }
    }

    fn namespace_path(&self, caller: &str) -> Result<PathBuf, StateError> {
        if caller.is_empty() || caller.len() > 128 {
            return Err(StateError::new(
                "invalid_caller",
                "caller must contain 1 to 128 UTF-8 bytes",
            ));
        }
        // Hex components cannot contain separators, dots, or platform path prefixes.
        // Chunking also stays below filesystem component limits for long principals.
        let encoded: String = caller
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let mut path = self.directory.join("namespaces");
        validate_directory(&path)?;
        for component in encoded.as_bytes().chunks(64) {
            path.push(std::str::from_utf8(component).expect("hex is ASCII"));
            validate_directory(&path)?;
        }
        Ok(path.join("state.json"))
    }

    fn initialize_feedback_host(
        &self,
        caller: &str,
        claim: amaterasu_module_sdk::state::FeedbackBootstrapClaim,
    ) -> Result<Value, StateError> {
        if caller != amaterasu_module_sdk::HOST_LIFECYCLE_CONTROL_PRINCIPAL {
            return Err(StateError::new(
                "permission_denied",
                "only the trusted host may initialize State",
            ));
        }
        if claim.version != 1
            || !valid_hex(&claim.installation_id)
            || !valid_hex(&claim.claim)
            || !amaterasu_module_sdk::valid_name(&claim.pairing_owner)
            || !amaterasu_module_sdk::valid_name(&claim.effect_owner)
        {
            return Err(StateError::new(
                "invalid_request",
                "invalid feedback bootstrap claim",
            ));
        }
        match std::fs::symlink_metadata(self.directory.join("namespaces")) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(StateError::new(
                    "history_present",
                    "State is not a new store",
                ));
            }
            Err(error) => return Err(storage_error(error)),
        }
        let path = self.namespace_path(&claim.pairing_owner)?;
        let mut namespace = Namespace::default();
        namespace.values.insert(
            amaterasu_module_sdk::state::FEEDBACK_BOOTSTRAP_KEY.into(),
            json!({"claim":claim,"phase":"prepared"}),
        );
        persist_mutation(&self.directory, &path, &mut namespace)?;
        Ok(json!({"ok":true,"revision":namespace.revision,"claim":claim}))
    }
}

fn valid_hex(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_operation(operation: &Operation) -> Result<(), StateError> {
    match operation {
        Operation::InitializeFeedbackHost { .. } => Ok(()),
        Operation::CollectionGet { collection, .. }
        | Operation::CollectionPage { collection, .. }
        | Operation::CollectionCommit { collection, .. }
        | Operation::CollectionDraftBegin { collection, .. }
        | Operation::CollectionDraftAppend { collection, .. }
        | Operation::CollectionDraftPublish { collection, .. }
        | Operation::CollectionDraftAbort { collection, .. } => validate_key(collection),
        Operation::Get { key } | Operation::Delete { key, .. } => validate_key(key),
        Operation::Set { key, value, .. } => {
            validate_key(key)?;
            validate_value(value)?;
            snapshot::validate_entry(key, value)
        }
        Operation::Snapshot { cursor, limit } => {
            if limit.is_some_and(|limit| limit == 0 || limit > snapshot::MAX_PAGE_KEYS) {
                return Err(StateError::new(
                    "invalid_request",
                    "snapshot limit must be between 1 and 128",
                ));
            }
            if let Some(cursor) = cursor {
                validate_key(&cursor.after)?;
            }
            Ok(())
        }
    }
}

pub(crate) fn validate_key(key: &str) -> Result<(), StateError> {
    if key.is_empty() || key.len() > MAX_KEY_BYTES || key.chars().any(char::is_control) {
        return Err(StateError::new(
            "invalid_key",
            "key must contain 1 to 256 UTF-8 bytes without control characters",
        ));
    }
    Ok(())
}

fn validate_value(value: &Value) -> Result<(), StateError> {
    let bytes = serde_json::to_vec(value).map_err(storage_error)?;
    if bytes.len() > MAX_VALUE_BYTES {
        return Err(StateError::new(
            "value_too_large",
            "encoded value exceeds 16384 bytes",
        ));
    }
    Ok(())
}

fn load_namespace(path: &Path) -> Result<Namespace, StateError> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Namespace::default());
        }
        Err(error) => return Err(storage_error(error)),
        Ok(_) => (),
    }
    let bytes = read_bounded(path, MAX_NAMESPACE_BYTES).map_err(storage_error)?;
    let namespace: Namespace = serde_json::from_slice(&bytes)
        .map_err(|error| StateError::new("corrupt_storage", error.to_string()))?;
    if namespace.format_version != 1 {
        return Err(StateError::new(
            "corrupt_storage",
            "unsupported state format version",
        ));
    }
    validate_namespace(&namespace).map_err(|_| {
        StateError::new(
            "corrupt_storage",
            "stored namespace violates size or key limits",
        )
    })?;
    Ok(namespace)
}

fn validate_namespace(namespace: &Namespace) -> Result<(), StateError> {
    if namespace.values.len() > MAX_KEYS {
        return Err(StateError::new(
            "namespace_full",
            "namespace exceeds 4096 keys",
        ));
    }
    for (key, value) in &namespace.values {
        validate_key(key)?;
        validate_value(value)?;
    }
    if serde_json::to_vec_pretty(namespace)
        .map_err(storage_error)?
        .len() as u64
        > MAX_NAMESPACE_BYTES
    {
        return Err(StateError::new(
            "namespace_full",
            "encoded namespace exceeds 1048576 bytes",
        ));
    }
    Ok(())
}

fn check_revision(namespace: &Namespace, expected: Option<u64>) -> Result<(), StateError> {
    if expected.is_some_and(|revision| revision != namespace.revision) {
        return Err(StateError::conflict(namespace.revision));
    }
    Ok(())
}

fn persist_mutation(root: &Path, path: &Path, namespace: &mut Namespace) -> Result<(), StateError> {
    namespace.revision = namespace.revision.checked_add(1).ok_or_else(|| {
        StateError::new("revision_exhausted", "namespace revision cannot advance")
    })?;
    validate_namespace(namespace)?;
    let directory = path.parent().expect("namespace files have a parent");
    create_namespace_directory(root, directory)?;
    write_json_atomically(path, namespace).map_err(storage_error)
}

pub(crate) fn validate_directory(path: &Path) -> Result<(), StateError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(storage_error(error)),
        Ok(_) => Err(StateError::new(
            "storage_error",
            "namespace directory is not a real directory",
        )),
    }
}

pub(crate) fn create_namespace_directory(root: &Path, directory: &Path) -> Result<(), StateError> {
    let relative = directory.strip_prefix(root).map_err(storage_error)?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let parent = current.clone();
        current.push(component);
        create_private_directory(&current).map_err(storage_error)?;
        // Persist each directory entry before acknowledging its first state mutation.
        sync_directory(&parent).map_err(storage_error)?;
    }
    Ok(())
}

pub(crate) fn storage_error(error: impl std::fmt::Display) -> StateError {
    StateError::new("storage_error", error.to_string())
}

#[cfg(test)]
mod tests;
