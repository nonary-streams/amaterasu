//! Copy-on-write ordered collections. A durable manifest is the sole commit
//! point; its leaf files stay immutable until a replacement manifest is durable.
use crate::protocol::StateError;
use crate::store::{create_namespace_directory, storage_error, validate_directory, validate_key};
use amaterasu_storage::{read_bounded, write_json_atomically};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound::{Excluded, Unbounded};
use std::path::{Path, PathBuf};

const MAX_ITEMS: usize = 100_000;
const MAX_SHARD_ENTRIES: usize = 256;
const TARGET_SHARD_BYTES: usize = 256 * 1024;
const MAX_SHARD_BYTES: u64 = 16 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ENTRY_BYTES: usize = 12_000;
const PAGE_BYTES: usize = 14_000;
type Shard = BTreeMap<String, Value>;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ShardReference {
    revision: u64,
    slot: usize,
    count: usize,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format_version: u32,
    revision: u64,
    metadata: Value,
    count: usize,
    // Lower inclusive key bounds index ordered leaf files. The first bound is
    // always empty, so inserts below the current minimum still have an owner.
    shards: BTreeMap<String, ShardReference>,
}
impl Default for Manifest {
    fn default() -> Self {
        Self {
            format_version: 1,
            revision: 0,
            metadata: Value::Null,
            count: 0,
            shards: BTreeMap::new(),
        }
    }
}
impl Manifest {
    fn validate(&self) -> Result<(), StateError> {
        let count = self
            .shards
            .values()
            .try_fold(0usize, |total, shard| total.checked_add(shard.count))
            .ok_or_else(corrupt)?;
        if self.format_version != 1
            || self.revision == 0
            || self.count > MAX_ITEMS
            || count != self.count
            || self.shards.len() > self.count + 1
            || encoded_size(&self.metadata)? > 1024
            || self
                .shards
                .first_key_value()
                .is_some_and(|(bound, _)| !bound.is_empty())
        {
            return Err(corrupt());
        }
        let mut files = BTreeSet::new();
        for (bound, shard) in &self.shards {
            if (!bound.is_empty() && validate_key(bound).is_err())
                || shard.revision == 0
                || shard.revision > self.revision
                || shard.count > MAX_SHARD_ENTRIES
                || (shard.count == 0 && !bound.is_empty())
                || !files.insert((shard.revision, shard.slot))
            {
                return Err(corrupt());
            }
        }
        Ok(())
    }
    fn owner(&self, key: &str) -> String {
        self.shards
            .range(..=key.to_owned())
            .next_back()
            .map_or_else(String::new, |(bound, _)| bound.clone())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edit {
    pub key: String,
    pub value: Option<Value>,
}

pub struct Collection<'a> {
    root: &'a Path,
    directory: PathBuf,
    manifest: Manifest,
    #[cfg(test)]
    fail_before_manifest: bool,
    #[cfg(test)]
    loaded_shards: std::cell::Cell<usize>,
}
impl<'a> Collection<'a> {
    pub fn open(root: &'a Path, namespace: &Path, name: &str) -> Result<Self, StateError> {
        let directory = collection_directory(namespace, name)?;
        let manifest_path = directory.join("manifest.json");
        let manifest = match std::fs::symlink_metadata(&manifest_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Manifest::default(),
            Err(error) => return Err(storage_error(error)),
            Ok(_) => {
                let manifest: Manifest = serde_json::from_slice(
                    &read_bounded(&manifest_path, MAX_MANIFEST_BYTES).map_err(storage_error)?,
                )
                .map_err(|_| corrupt())?;
                manifest.validate()?;
                manifest
            }
        };
        Ok(Self {
            root,
            directory,
            manifest,
            #[cfg(test)]
            fail_before_manifest: false,
            #[cfg(test)]
            loaded_shards: std::cell::Cell::new(0),
        })
    }
    fn shard_path(&self, revision: u64, slot: usize) -> PathBuf {
        self.directory
            .join(format!("shard-{revision:016x}-{slot:08x}.json"))
    }
    fn load(&self, bound: &str) -> Result<Shard, StateError> {
        let Some(reference) = self.manifest.shards.get(bound) else {
            return Ok(Shard::new());
        };
        #[cfg(test)]
        self.loaded_shards.set(self.loaded_shards.get() + 1);
        let path = self.shard_path(reference.revision, reference.slot);
        let shard: Shard =
            serde_json::from_slice(&read_bounded(&path, MAX_SHARD_BYTES).map_err(storage_error)?)
                .map_err(|_| corrupt())?;
        let upper = self
            .manifest
            .shards
            .range((Excluded(bound.to_owned()), Unbounded))
            .next()
            .map(|(bound, _)| bound.as_str());
        if shard.len() != reference.count {
            return Err(corrupt());
        }
        for (key, value) in &shard {
            if validate_key(key).is_err()
                || validate_entry(key, value).is_err()
                || key.as_str() < bound
                || upper.is_some_and(|upper| key.as_str() >= upper)
            {
                return Err(corrupt());
            }
        }
        Ok(shard)
    }
    fn check_revision(&self, expected: u64) -> Result<(), StateError> {
        if expected != self.manifest.revision {
            return Err(StateError::conflict(self.manifest.revision));
        }
        Ok(())
    }
    pub fn get(&self, key: Option<&str>) -> Result<Value, StateError> {
        let entry = if let Some(key) = key {
            validate_key(key)?;
            self.load(&self.manifest.owner(key))?.remove(key)
        } else {
            None
        };
        Ok(
            json!({"ok":true,"revision":self.manifest.revision,"found":self.manifest.revision != 0,"metadata":self.manifest.metadata,"count":self.manifest.count,"value":entry}),
        )
    }
    pub fn page(
        &self,
        revision: u64,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Value, StateError> {
        self.check_revision(revision)?;
        if !(1..=128).contains(&limit) {
            return Err(StateError::new(
                "invalid_request",
                "collection page limit must be 1..128",
            ));
        }
        let first = self.manifest.owner(after.unwrap_or(""));
        let first_shard = self.load(&first)?;
        if let Some(after) = after {
            validate_key(after)?;
            if !first_shard.contains_key(after) {
                return Err(StateError::new(
                    "invalid_cursor",
                    "collection cursor key is missing",
                ));
            }
        }
        let mut page = Page::new(limit);
        if page.append(first_shard, after)? {
            return Ok(page.reply(revision, true));
        }
        for (bound, _) in self.manifest.shards.range((Excluded(first), Unbounded)) {
            if page.append(self.load(bound)?, after)? {
                return Ok(page.reply(revision, true));
            }
        }
        Ok(page.reply(revision, false))
    }
    pub fn commit(
        mut self,
        expected: u64,
        metadata: Value,
        edits: Vec<Edit>,
    ) -> Result<Value, StateError> {
        self.check_revision(expected)?;
        validate_commit(&metadata, &edits)?;
        let revision = self.manifest.revision.checked_add(1).ok_or_else(|| {
            StateError::new("revision_exhausted", "collection revision exhausted")
        })?;
        let changed = self.apply_edits(edits)?;
        let changed = split_shards(changed)?;
        self.publish(revision, metadata, changed)?;
        self.reclaim_orphans();
        Ok(json!({"ok":true,"revision":revision}))
    }
    fn apply_edits(&mut self, edits: Vec<Edit>) -> Result<BTreeMap<String, Shard>, StateError> {
        let mut changed = BTreeMap::<String, Shard>::new();
        for edit in edits {
            let bound = self.manifest.owner(&edit.key);
            if let std::collections::btree_map::Entry::Vacant(slot) = changed.entry(bound.clone()) {
                slot.insert(self.load(&bound)?);
            }
            let shard = changed.get_mut(&bound).expect("loaded shard");
            if shard.remove(&edit.key).is_some() {
                self.manifest.count = self.manifest.count.checked_sub(1).ok_or_else(corrupt)?;
            }
            if let Some(value) = edit.value {
                shard.insert(edit.key, value);
                self.manifest.count += 1;
            }
        }
        if self.manifest.count > MAX_ITEMS {
            return Err(StateError::new(
                "collection_full",
                "collection exceeds 100000 entries",
            ));
        }
        // Remove old references only after every touched leaf has been loaded
        // and validated against the original range index.
        for bound in changed.keys() {
            self.manifest.shards.remove(bound);
        }
        Ok(changed)
    }
    fn publish(
        &mut self,
        revision: u64,
        metadata: Value,
        changed: BTreeMap<String, Shard>,
    ) -> Result<(), StateError> {
        let leaves: Vec<_> = changed
            .into_iter()
            .filter(|(bound, shard)| bound.is_empty() || !shard.is_empty())
            .collect();
        for (slot, (bound, shard)) in leaves.iter().enumerate() {
            self.manifest.shards.insert(
                bound.clone(),
                ShardReference {
                    revision,
                    slot,
                    count: shard.len(),
                },
            );
        }
        self.manifest.revision = revision;
        self.manifest.metadata = metadata;
        self.manifest.validate()?;
        if serde_json::to_vec_pretty(&self.manifest)
            .map_err(storage_error)?
            .len() as u64
            > MAX_MANIFEST_BYTES
        {
            return Err(StateError::new(
                "collection_full",
                "collection index exceeds size limit",
            ));
        }
        create_namespace_directory(self.root, &self.directory)?;
        for (slot, (_, shard)) in leaves.iter().enumerate() {
            write_json_atomically(&self.shard_path(revision, slot), shard)
                .map_err(storage_error)?;
        }
        #[cfg(test)]
        if self.fail_before_manifest {
            return Err(storage_error("injected manifest write failure"));
        }
        // The atomic manifest replacement publishes all leaves and metadata.
        // Until this succeeds the old manifest and all its files remain intact.
        write_json_atomically(&self.directory.join("manifest.json"), &self.manifest)
            .map_err(storage_error)
    }
    fn reclaim_orphans(&self) {
        let referenced: BTreeSet<_> = self
            .manifest
            .shards
            .values()
            .map(|shard| self.shard_path(shard.revision, shard.slot))
            .collect();
        if let Ok(files) = std::fs::read_dir(&self.directory) {
            for file in files.flatten() {
                let name = file.file_name();
                let name = name.to_string_lossy();
                if ((name.starts_with("shard-") && name.ends_with(".json"))
                    || name.starts_with(".write-"))
                    && !referenced.contains(&file.path())
                    && file.file_type().is_ok_and(|kind| kind.is_file())
                {
                    let _ = std::fs::remove_file(file.path());
                }
            }
        }
    }
}
struct Page {
    entries: Vec<Value>,
    bytes: usize,
    limit: usize,
}
impl Page {
    fn new(limit: usize) -> Self {
        Self {
            entries: Vec::new(),
            bytes: 0,
            limit,
        }
    }
    // Returns true on the first row that cannot fit. Ordered leaves mean no
    // other leaf needs to be read after that point.
    fn append(&mut self, shard: Shard, after: Option<&str>) -> Result<bool, StateError> {
        for (key, value) in shard {
            if after.is_some_and(|after| key.as_str() <= after) {
                continue;
            }
            let row = json!({"key":key,"value":value});
            let bytes = encoded_size(&row)?;
            if self.entries.len() == self.limit || self.bytes + bytes > PAGE_BYTES {
                return Ok(true);
            }
            self.bytes += bytes;
            self.entries.push(row);
        }
        Ok(false)
    }
    fn reply(self, revision: u64, more: bool) -> Value {
        let next = if more {
            self.entries.last().map(|entry| entry["key"].clone())
        } else {
            None
        };
        json!({"ok":true,"revision":revision,"entries":self.entries,"next":next})
    }
}
fn split_shards(changed: BTreeMap<String, Shard>) -> Result<BTreeMap<String, Shard>, StateError> {
    let mut pending: Vec<_> = changed.into_iter().collect();
    let mut leaves = BTreeMap::new();
    while let Some((bound, mut shard)) = pending.pop() {
        let bytes = serde_json::to_vec_pretty(&shard)
            .map_err(storage_error)?
            .len();
        if shard.len() > 1 && (shard.len() > MAX_SHARD_ENTRIES || bytes > TARGET_SHARD_BYTES) {
            let middle = shard
                .keys()
                .nth(shard.len() / 2)
                .expect("nonempty shard")
                .clone();
            let right = shard.split_off(&middle);
            pending.push((bound, shard));
            pending.push((middle, right));
        } else {
            if bytes as u64 > MAX_SHARD_BYTES {
                return Err(StateError::new(
                    "value_too_large",
                    "encoded collection shard exceeds size limit",
                ));
            }
            leaves.insert(bound, shard);
        }
    }
    Ok(leaves)
}
fn collection_directory(namespace: &Path, name: &str) -> Result<PathBuf, StateError> {
    validate_key(name)?;
    let encoded: String = name
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let mut directory = namespace
        .parent()
        .expect("namespace parent")
        .join("collections");
    validate_directory(&directory)?;
    for component in encoded.as_bytes().chunks(64) {
        directory.push(std::str::from_utf8(component).expect("hex"));
        validate_directory(&directory)?;
    }
    Ok(directory)
}
fn validate_commit(metadata: &Value, edits: &[Edit]) -> Result<(), StateError> {
    if encoded_size(metadata)? > 1024 || edits.len() > 128 {
        return Err(StateError::new(
            "invalid_request",
            "collection commit exceeds metadata or edit limit",
        ));
    }
    let mut keys = BTreeSet::new();
    for edit in edits {
        validate_key(&edit.key)?;
        if !keys.insert(&edit.key) {
            return Err(StateError::new(
                "invalid_request",
                "duplicate collection edit",
            ));
        }
        if let Some(value) = &edit.value {
            validate_entry(&edit.key, value)?;
        }
    }
    Ok(())
}
fn validate_entry(key: &str, value: &Value) -> Result<(), StateError> {
    if value.is_null() || encoded_size(&json!({"key":key,"value":value}))? > MAX_ENTRY_BYTES {
        return Err(StateError::new(
            "value_too_large",
            "collection entry must be non-null and fit 12000 encoded bytes",
        ));
    }
    Ok(())
}
fn encoded_size(value: &Value) -> Result<usize, StateError> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(storage_error)
}
fn corrupt() -> StateError {
    StateError::new("corrupt_storage", "invalid collection manifest or shard")
}
#[cfg(test)]
mod tests;
