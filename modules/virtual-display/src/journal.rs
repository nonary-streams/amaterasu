//! Durable ownership intent. Every native mutation follows a synced journal event.
use crate::model::Specification;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub owner: String,
    pub specification: Specification,
    pub display_id: u64,
    pub lease: u64,
    pub prospective_lease: Option<u64>,
    pub capability: [u8; 32],
    pub binding: Option<crate::protocol::Binding>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "event", deny_unknown_fields)]
enum Event {
    Upsert { record: Record },
    Delete { owner: String, identity: String },
}

pub struct Journal {
    file: File,
    records: BTreeMap<(String, String), Record>,
    poisoned: bool,
}

impl Journal {
    pub fn open_default() -> Result<Self, String> {
        let root = std::env::var_os("LOCALAPPDATA")
            .or_else(|| std::env::var_os("APPDATA"))
            .ok_or("virtual display state directory unavailable")?;
        let root = PathBuf::from(root);
        if !root.is_absolute() {
            return Err("virtual display state directory is not absolute".into());
        }
        Self::open(
            &root
                .join("Amaterasu")
                .join("virtual-display")
                .join("leases.jsonl"),
        )
    }

    pub fn open(path: &Path) -> Result<Self, String> {
        let parent = path.parent().ok_or("journal has no parent")?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        for component in path.ancestors() {
            if let Ok(metadata) = fs::symlink_metadata(component)
                && (metadata.file_type().is_symlink() || is_reparse_point(&metadata))
            {
                return Err("virtual display journal path crosses a link".into());
            }
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // A second host cannot concurrently issue effects using this journal.
            options.share_mode(0);
        }
        let mut file = options.open(path).map_err(|error| error.to_string())?;
        if file.metadata().map_err(|error| error.to_string())?.len() > 64 * 1024 * 1024 {
            return Err("virtual display journal is too large".into());
        }
        let mut records = BTreeMap::new();
        for line in BufReader::new(&file).lines() {
            let line = line.map_err(|error| error.to_string())?;
            let event: Event = serde_json::from_str(&line).map_err(|error| error.to_string())?;
            match event {
                Event::Upsert { record } => {
                    if record.specification.validate().is_err()
                        || record.display_id
                            != crate::model::display_id(
                                &record.owner,
                                &record.specification.identity,
                            )
                        || record.owner.is_empty()
                        || record.lease & 0x6000_0000_0000_0000 != 0x6000_0000_0000_0000
                        || record.prospective_lease == Some(record.lease)
                        || record.prospective_lease.is_some_and(|lease| {
                            lease & 0x6000_0000_0000_0000 != 0x6000_0000_0000_0000
                        })
                        || record.capability == [0; 32]
                    {
                        return Err("invalid virtual display journal record".into());
                    }
                    records.insert(
                        (record.owner.clone(), record.specification.identity.clone()),
                        record,
                    );
                }
                Event::Delete { owner, identity } => {
                    records.remove(&(owner, identity));
                }
            }
        }
        let mut displays = BTreeSet::new();
        let mut leases = BTreeSet::new();
        for record in records.values() {
            if !displays.insert(record.display_id)
                || !leases.insert(record.lease)
                || record
                    .prospective_lease
                    .is_some_and(|lease| !leases.insert(lease))
            {
                return Err("conflicting virtual display journal records".into());
            }
        }
        file.seek(SeekFrom::End(0))
            .map_err(|error| error.to_string())?;
        Ok(Self {
            file,
            records,
            poisoned: false,
        })
    }

    pub fn records(&self) -> impl Iterator<Item = &Record> {
        self.records.values()
    }

    pub fn upsert(&mut self, record: Record) -> Result<(), String> {
        if self.records.values().any(|existing| {
            (existing.owner != record.owner
                || existing.specification.identity != record.specification.identity)
                && (existing.display_id == record.display_id
                    || existing.lease == record.lease
                    || existing.prospective_lease == Some(record.lease)
                    || record.prospective_lease == Some(existing.lease)
                    || record
                        .prospective_lease
                        .is_some_and(|lease| existing.prospective_lease == Some(lease)))
        }) {
            return Err("conflicting virtual display journal records".into());
        }
        self.write(&Event::Upsert {
            record: record.clone(),
        })?;
        self.records.insert(
            (record.owner.clone(), record.specification.identity.clone()),
            record,
        );
        Ok(())
    }

    pub fn delete(&mut self, owner: &str, identity: &str) -> Result<(), String> {
        self.write(&Event::Delete {
            owner: owner.into(),
            identity: identity.into(),
        })?;
        self.records.remove(&(owner.into(), identity.into()));
        Ok(())
    }

    fn write(&mut self, event: &Event) -> Result<(), String> {
        if self.poisoned {
            return Err("virtual display journal is unavailable".into());
        }
        let mut encoded = serde_json::to_vec(event).map_err(|error| error.to_string())?;
        encoded.push(b'\n');
        let result = self
            .file
            .write_all(&encoded)
            .and_then(|()| self.file.sync_all());
        if result.is_err() {
            self.poisoned = true;
        }
        result.map_err(|error| error.to_string())
    }
}

#[cfg(windows)]
fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_: &fs::Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synced_ownership_replays_and_corruption_blocks_startup() {
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).unwrap();
        let root = std::env::temp_dir().join(format!(
            "amaterasu-vd-journal-{:016x}",
            u64::from_le_bytes(nonce)
        ));
        let path = root.join("leases.jsonl");
        let specification = Specification {
            identity: "00000000-0000-0000-0000-000000000001".into(),
            width: 1920,
            height: 1080,
            refresh_millihz: 60000,
            name: "Test display".into(),
        };
        let record = Record {
            owner: "alice".into(),
            display_id: crate::model::display_id("alice", &specification.identity),
            specification,
            lease: 0x6000_0000_0000_0001,
            prospective_lease: None,
            capability: [7; 32],
            binding: None,
        };
        let mut journal = Journal::open(&path).unwrap();
        journal.upsert(record.clone()).unwrap();
        drop(journal);
        let mut journal = Journal::open(&path).unwrap();
        assert_eq!(journal.records().next().unwrap().lease, record.lease);
        journal
            .delete("alice", &record.specification.identity)
            .unwrap();
        drop(journal);
        assert_eq!(Journal::open(&path).unwrap().records().count(), 0);
        fs::write(&path, b"{\"event\":\"upsert\"").unwrap();
        assert!(Journal::open(&path).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
