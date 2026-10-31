//! Durable, caller-scoped collection edit drafts. Drafts are never read by the
//! public collection APIs and publication is one collection manifest swap.
use crate::collection::{Collection, DraftReceipt, Edit, validate_commit};
use crate::protocol::StateError;
use crate::store::{create_namespace_directory, storage_error, validate_directory};
use amaterasu_storage::{read_bounded, sync_directory, write_atomically, write_json_atomically};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const MAX_DRAFTS: usize = 16;
const MAX_CHUNKS: u64 = 1024;
const MAX_EDITS: usize = 100_000;
const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_CHUNK_BYTES: usize = 12_000;
const DRAFT_IDLE_LIMIT: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Draft {
    collection: String,
    expected_revision: u64,
    next_sequence: u64,
    edit_count: usize,
    total_bytes: usize,
}

fn directory(namespace: &Path, id: &str) -> Result<PathBuf, StateError> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(StateError::new(
            "invalid_request",
            "draft_id must contain 1..64 ASCII letters, digits, hyphens or underscores",
        ));
    }
    let base = namespace.parent().expect("namespace parent").join("drafts");
    validate_directory(&base)?;
    let path = base.join(id);
    validate_directory(&path)?;
    Ok(path)
}
// StateStore holds an exclusive lifetime root lock and handles requests
// serially, so cleanup cannot race an append or publication in this namespace.
fn reclaim_stale_drafts(base: &Path, keep: &Path) {
    let now = SystemTime::now();
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    let mut removed = false;
    for entry in entries.flatten().take(256) {
        let path = entry.path();
        if path == keep || !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            continue;
        }
        let state_path = path.join("draft.json");
        let timestamp = match std::fs::symlink_metadata(&state_path) {
            Ok(meta) if meta.is_file() => {
                let valid = read_bounded(&state_path, 4096)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Draft>(&bytes).ok())
                    .is_some_and(|draft| {
                        crate::store::validate_key(&draft.collection).is_ok()
                            && draft.next_sequence <= MAX_CHUNKS
                            && draft.edit_count <= MAX_EDITS
                            && draft.total_bytes <= MAX_BYTES
                    });
                if !valid {
                    continue;
                }
                meta.modified()
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                entry.metadata().and_then(|m| m.modified())
            }
            _ => continue,
        };
        if timestamp
            .ok()
            .and_then(|time| now.duration_since(time).ok())
            .is_some_and(|age| age >= DRAFT_IDLE_LIMIT)
            && std::fs::remove_dir_all(path).is_ok()
        {
            removed = true;
        }
    }
    if removed {
        let _ = sync_directory(base);
    }
}

fn reclaim_temporary_files(path: &Path) {
    if let Ok(files) = std::fs::read_dir(path) {
        for file in files.flatten() {
            if file.file_name().to_string_lossy().starts_with(".write-")
                && file.file_type().is_ok_and(|kind| kind.is_file())
            {
                let _ = std::fs::remove_file(file.path());
            }
        }
    }
}
fn load(path: &Path, collection: &str) -> Result<Draft, StateError> {
    reclaim_temporary_files(path);
    let draft: Draft = serde_json::from_slice(
        &read_bounded(&path.join("draft.json"), 4096).map_err(storage_error)?,
    )
    .map_err(storage_error)?;
    if draft.collection != collection {
        return Err(StateError::new(
            "draft_conflict",
            "draft belongs to another collection",
        ));
    }
    if draft.next_sequence > MAX_CHUNKS
        || draft.edit_count > MAX_EDITS
        || draft.total_bytes > MAX_BYTES
    {
        return Err(StateError::new("storage_error", "invalid draft bounds"));
    }
    Ok(draft)
}
fn chunk(path: &Path, sequence: u64) -> PathBuf {
    path.join(format!("chunk-{sequence:016x}.json"))
}
fn save(path: &Path, draft: &Draft) -> Result<(), StateError> {
    write_json_atomically(&path.join("draft.json"), draft).map_err(storage_error)
}
fn exhausted() -> StateError {
    StateError::new(
        "resource_exhausted",
        "collection draft exceeds its resource limits",
    )
}

pub(crate) fn begin(
    root: &Path,
    namespace: &Path,
    collection: &str,
    id: &str,
    expected: u64,
) -> Result<Value, StateError> {
    let path = directory(namespace, id)?;
    let collection_view = Collection::open(root, namespace, collection)?;
    if let Some(receipt) = collection_view.draft_receipt()
        && receipt.id == id
    {
        return Err(StateError::new(
            "draft_conflict",
            "draft ID has already been published",
        ));
    }
    let current = collection_view.get(None)?["revision"]
        .as_u64()
        .expect("collection revision");
    if current != expected {
        return Err(StateError::conflict(current));
    }
    if path.join("draft.json").exists() {
        let draft = load(&path, collection)?;
        if draft.expected_revision != expected {
            return Err(StateError::new(
                "draft_conflict",
                "draft base revision differs",
            ));
        }
        save(&path, &draft)?;
        return Ok(json!({"ok":true,"next_sequence":draft.next_sequence}));
    }
    let base = path.parent().expect("draft parent");
    reclaim_stale_drafts(base, &path);
    if base.exists()
        && std::fs::read_dir(base).map_err(storage_error)?.count() >= MAX_DRAFTS
        && !path.exists()
    {
        return Err(exhausted());
    }
    create_namespace_directory(root, &path)?;
    reclaim_temporary_files(&path);
    let draft = Draft {
        collection: collection.into(),
        expected_revision: expected,
        next_sequence: 0,
        edit_count: 0,
        total_bytes: 0,
    };
    save(&path, &draft)?;
    Ok(json!({"ok":true,"next_sequence":0}))
}

pub(crate) fn append(
    namespace: &Path,
    collection: &str,
    id: &str,
    sequence: u64,
    edits: Vec<Edit>,
) -> Result<Value, StateError> {
    let path = directory(namespace, id)?;
    let mut draft = load(&path, collection)?;
    validate_commit(&json!({}), &edits)?;
    let bytes = serde_json::to_vec(&edits).map_err(storage_error)?;
    if bytes.len() > MAX_CHUNK_BYTES {
        return Err(exhausted());
    }
    if sequence.checked_add(1) == Some(draft.next_sequence) {
        let existing =
            read_bounded(&chunk(&path, sequence), MAX_CHUNK_BYTES as u64).map_err(storage_error)?;
        if existing != bytes {
            return Err(StateError::new("sequence_conflict", "draft replay differs"));
        }
        save(&path, &draft)?;
        return Ok(json!({"ok":true,"next_sequence":draft.next_sequence,"replayed":true}));
    }
    if sequence != draft.next_sequence {
        return Err(StateError::new(
            "sequence_conflict",
            "draft sequence is not next",
        ));
    }
    if sequence >= MAX_CHUNKS
        || draft.edit_count.saturating_add(edits.len()) > MAX_EDITS
        || draft.total_bytes.saturating_add(bytes.len()) > MAX_BYTES
    {
        return Err(exhausted());
    }
    let file = chunk(&path, sequence);
    if file.exists() {
        let existing = read_bounded(&file, MAX_CHUNK_BYTES as u64).map_err(storage_error)?;
        if existing != bytes {
            return Err(StateError::new(
                "sequence_conflict",
                "unacknowledged draft chunk differs",
            ));
        }
    } else {
        write_atomically(&file, &bytes).map_err(storage_error)?;
    }
    draft.next_sequence += 1;
    draft.edit_count += edits.len();
    draft.total_bytes += bytes.len();
    save(&path, &draft)?;
    Ok(json!({"ok":true,"next_sequence":draft.next_sequence}))
}

pub(crate) fn publish(
    root: &Path,
    namespace: &Path,
    collection: &str,
    id: &str,
    expected: u64,
    metadata: Value,
) -> Result<Value, StateError> {
    let path = directory(namespace, id)?;
    let view = Collection::open(root, namespace, collection)?;
    if let Some(receipt) = view.draft_receipt()
        && receipt.id == id
    {
        if receipt.expected_revision != expected || receipt.metadata != metadata {
            return Err(StateError::new(
                "draft_conflict",
                "published draft retry differs",
            ));
        }
        if load(&path, collection).is_ok_and(|draft| draft.expected_revision == expected) {
            let _ = remove(&path);
        }
        return Ok(
            json!({"ok":true,"revision":expected.checked_add(1).ok_or_else(|| StateError::new("storage_error", "invalid draft receipt"))?,"replayed":true}),
        );
    }
    let draft = load(&path, collection)?;
    if draft.expected_revision != expected {
        return Err(StateError::new(
            "draft_conflict",
            "draft base revision differs",
        ));
    }
    let mut edits = Vec::with_capacity(draft.edit_count);
    let mut total_bytes = 0usize;
    for sequence in 0..draft.next_sequence {
        let bytes =
            read_bounded(&chunk(&path, sequence), MAX_CHUNK_BYTES as u64).map_err(storage_error)?;
        total_bytes += bytes.len();
        let chunk: Vec<Edit> = serde_json::from_slice(&bytes).map_err(storage_error)?;
        validate_commit(&json!({}), &chunk)?;
        edits.extend(chunk);
    }
    if edits.len() != draft.edit_count || total_bytes != draft.total_bytes {
        return Err(StateError::new("storage_error", "draft accounting differs"));
    }
    let receipt = DraftReceipt {
        id: id.into(),
        expected_revision: expected,
        metadata: metadata.clone(),
    };
    let result = view.commit_draft(expected, metadata, edits, receipt)?;
    // A persisted receipt makes a lost acknowledgement replayable even if
    // removing this now-unneeded draft is interrupted.
    let _ = remove(&path);
    Ok(result)
}
fn remove(path: &Path) -> Result<(), StateError> {
    if path.exists() {
        std::fs::remove_dir_all(path).map_err(storage_error)?;
        sync_directory(path.parent().expect("draft parent")).map_err(storage_error)?;
    }
    Ok(())
}
pub(crate) fn abort(namespace: &Path, collection: &str, id: &str) -> Result<Value, StateError> {
    let path = directory(namespace, id)?;
    if path.join("draft.json").exists() {
        load(&path, collection)?;
    }
    remove(&path)?;
    Ok(json!({"ok":true}))
}
