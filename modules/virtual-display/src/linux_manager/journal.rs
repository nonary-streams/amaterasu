//! One user-scoped, process-exclusive journal precedes every connector effect.
use super::Record;
use amaterasu_storage::{read_bounded, write_atomically};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema: u32,
    records: Vec<Record>,
}

const MAX_JOURNAL_BYTES: u64 = 1024 * 1024;
const MAX_RECORDS: usize = 8;

impl Document {
    fn validate(&self) -> Result<(), String> {
        if self.schema != 1 || self.records.len() > MAX_RECORDS {
            return Err("invalid virtual display ownership journal".into());
        }
        let mut outputs = std::collections::BTreeSet::new();
        let mut keys = std::collections::BTreeSet::new();
        for record in &self.records {
            if !record.valid()
                || !outputs.insert(record.output.clone())
                || !keys.insert(record.key())
            {
                return Err("invalid virtual display ownership record".into());
            }
        }
        Ok(())
    }
}

pub(super) struct Journal {
    path: PathBuf,
    _lock: File,
    pub records: Vec<Record>,
}
impl Journal {
    pub fn for_user() -> Result<Self, String> {
        let root = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
            })
            .ok_or("virtual display user state root unavailable")?;
        Self::open(&root.join("amaterasu/virtual-display-linux"))
    }
    pub fn open(directory: &Path) -> Result<Self, String> {
        if !directory.is_absolute() {
            return Err("virtual display journal requires an absolute path".into());
        }
        for path in directory.ancestors() {
            if let Ok(metadata) = fs::symlink_metadata(path)
                && (!metadata.is_dir() || metadata.file_type().is_symlink())
            {
                return Err("virtual display state crosses a link or file".into());
            }
        }
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)
            .map_err(|e| e.to_string())?;
        let uid = fs::metadata("/proc/self").map_err(|e| e.to_string())?.uid();
        let metadata = fs::metadata(directory).map_err(|e| e.to_string())?;
        if metadata.uid() != uid || metadata.permissions().mode() & 0o077 != 0 {
            return Err("virtual display state is not private to this user".into());
        }
        let lock_path = directory.join("owner.lock");
        if fs::symlink_metadata(&lock_path)
            .is_ok_and(|m| !m.is_file() || m.file_type().is_symlink() || m.uid() != uid)
        {
            return Err("unsafe virtual display process lock".into());
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(lock_path)
            .map_err(|e| e.to_string())?;
        lock.try_lock()
            .map_err(|_| "another Amaterasu host owns the virtual display journal")?;
        let path = directory.join("connectors.json");
        let records = if path.try_exists().map_err(|e| e.to_string())? {
            let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.uid() != uid
                || metadata.permissions().mode() & 0o077 != 0
            {
                return Err("unsafe virtual display ownership journal".into());
            }
            let document: Document =
                serde_json::from_slice(&read_bounded(&path, MAX_JOURNAL_BYTES)?)
                    .map_err(|_| "invalid virtual display ownership journal")?;
            document.validate()?;
            document.records
        } else {
            Vec::new()
        };
        Ok(Self {
            path,
            _lock: lock,
            records,
        })
    }
    pub fn insert(&mut self, record: Record) -> Result<(), String> {
        let mut records = self.records.clone();
        if records
            .iter()
            .any(|old| old.key() == record.key() || old.output == record.output)
        {
            return Err("virtual display journal conflict".into());
        }
        records.push(record);
        self.replace(records)
    }
    pub fn update(&mut self, record: Record) -> Result<(), String> {
        let mut records = self.records.clone();
        let existing = records
            .iter_mut()
            .find(|old| old.key() == record.key())
            .ok_or("missing owned display record")?;
        *existing = record;
        self.replace(records)
    }
    pub fn replace_parked(&mut self, key: &(String, String), record: Record) -> Result<(), String> {
        let mut records = self.records.clone();
        let index = records
            .iter()
            .position(|old| &old.key() == key && old.parked)
            .ok_or("missing parked display record")?;
        if records.iter().enumerate().any(|(i, old)| {
            i != index && (old.key() == record.key() || old.output == record.output)
        }) {
            return Err("virtual display journal conflict".into());
        }
        records[index] = record;
        self.replace(records)
    }
    pub fn remove(&mut self, key: &(String, String)) -> Result<(), String> {
        let records = self
            .records
            .iter()
            .filter(|record| &record.key() != key)
            .cloned()
            .collect();
        self.replace(records)
    }
    fn replace(&mut self, records: Vec<Record>) -> Result<(), String> {
        let document = Document {
            schema: 1,
            records: records.clone(),
        };
        document.validate()?;
        let encoded = serde_json::to_vec_pretty(&document).map_err(|error| error.to_string())?;
        if encoded.len() as u64 > MAX_JOURNAL_BYTES {
            return Err("virtual display ownership journal exceeds its bound".into());
        }
        write_atomically(&self.path, &encoded)?;
        self.records = records;
        Ok(())
    }
}
