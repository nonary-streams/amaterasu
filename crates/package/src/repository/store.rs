//! Durable administrator approvals and catalog replay checkpoints.
use super::{
    ApprovedRepository, Candidate, CatalogCheckpoint, PackageResult, VerifiedCatalog,
    verify_catalog,
};
use crate::{RootLock, create_private_directory, read_bounded, write_json_atomically};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

const MAX_STATE_BYTES: u64 = 5 * 1024 * 1024;
const MAX_REPOSITORIES: usize = 16;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedCatalog {
    raw: String,
    signature: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RepositoryRecord {
    approval: ApprovedRepository,
    revoked: bool,
    checkpoint: CatalogCheckpoint,
    catalog: Option<SavedCatalog>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema: u32,
    repositories: BTreeMap<String, RepositoryRecord>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            schema: 1,
            repositories: BTreeMap::new(),
        }
    }
}

#[derive(Serialize)]
pub struct RepositoryListing {
    pub id: String,
    pub location: String,
    pub fingerprint: String,
    pub revoked: bool,
    pub candidates: Vec<Candidate>,
}

/// An administrator-controlled directory with one atomic approval/checkpoint
/// record. Mutations hold a lock separate from the native activation root.
pub struct RepositoryStore {
    directory: PathBuf,
}

impl RepositoryStore {
    pub fn new(directory: PathBuf) -> PackageResult<Self> {
        if !directory.is_absolute() {
            return Err("repository state directory must be absolute".into());
        }
        Ok(Self { directory })
    }

    pub fn approve(&self, approval: ApprovedRepository) -> PackageResult<()> {
        approval.validate()?;
        self.change(|state| {
            if state.repositories.len() >= MAX_REPOSITORIES {
                return Err("repository approval limit reached".into());
            }
            if state.repositories.contains_key(&approval.id) {
                return Err("repository identity already enrolled; rotate under a new ID".into());
            }
            state.repositories.insert(
                approval.id.clone(),
                RepositoryRecord {
                    approval,
                    revoked: false,
                    checkpoint: CatalogCheckpoint::default(),
                    catalog: None,
                },
            );
            Ok(())
        })
    }

    /// Revocation is permanent for this ID. It retains the replay checkpoint.
    pub fn revoke(&self, id: &str) -> PackageResult<()> {
        self.change(|state| {
            let record = state.repositories.get_mut(id).ok_or("unknown repository")?;
            record.revoked = true;
            Ok(())
        })
    }

    /// Atomically accept both verified metadata and its replay checkpoint.
    pub fn accept_catalog(
        &self,
        id: &str,
        raw: &[u8],
        signature: &str,
        target: &str,
        now: u64,
    ) -> PackageResult<()> {
        self.change(|state| {
            let record = active_record(state, id)?;
            let verified = verify_catalog(
                &record.approval,
                raw,
                signature,
                &record.checkpoint,
                target,
                now,
            )?;
            record.checkpoint = verified.next_checkpoint().clone();
            record.catalog = Some(SavedCatalog {
                raw: String::from_utf8(raw.to_vec()).map_err(|error| error.to_string())?,
                signature: signature.into(),
            });
            Ok(())
        })
    }

    pub fn approved(&self, id: &str) -> PackageResult<ApprovedRepository> {
        let state = self.read()?;
        Ok(active_record_ref(&state, id)?.approval.clone())
    }

    pub fn catalog(&self, id: &str, target: &str, now: u64) -> PackageResult<VerifiedCatalog> {
        let state = self.read()?;
        let record = active_record_ref(&state, id)?;
        let catalog = record
            .catalog
            .as_ref()
            .ok_or("repository catalog not loaded")?;
        verify_catalog(
            &record.approval,
            catalog.raw.as_bytes(),
            &catalog.signature,
            &record.checkpoint,
            target,
            now,
        )
    }

    pub fn list(&self, target: &str, now: u64) -> PackageResult<Vec<RepositoryListing>> {
        let state = self.read()?;
        state
            .repositories
            .iter()
            .map(|(id, record)| {
                let candidates = if !record.revoked {
                    if let Some(saved) = &record.catalog {
                        verify_catalog(
                            &record.approval,
                            saved.raw.as_bytes(),
                            &saved.signature,
                            &record.checkpoint,
                            target,
                            now,
                        )?
                        .candidates()
                        .to_vec()
                    } else {
                        Vec::new()
                    }
                } else {
                    Vec::new()
                };
                Ok(RepositoryListing {
                    id: id.clone(),
                    location: record.approval.location.clone(),
                    fingerprint: record.approval.fingerprint.clone(),
                    revoked: record.revoked,
                    candidates,
                })
            })
            .collect()
    }

    pub fn active_keys(&self) -> PackageResult<BTreeMap<String, String>> {
        let state = self.read()?;
        Ok(active_keys(&state))
    }

    /// Keep authorization stable while a selected UI pointer is changed.
    /// Revocation uses the same lock and therefore cannot interleave.
    pub fn with_active_keys<T>(
        &self,
        operation: impl FnOnce(&BTreeMap<String, String>) -> PackageResult<T>,
    ) -> PackageResult<T> {
        create_private_directory(&self.directory)?;
        let _lock = RootLock::acquire(&self.directory)?;
        let state = self.read()?;
        operation(&active_keys(&state))
    }

    /// Holds publisher authorization during a native worker's verification and
    /// library load. Revocation cannot complete in that interval.
    pub fn with_active_approval<T>(
        &self,
        id: &str,
        operation: impl FnOnce(&ApprovedRepository) -> PackageResult<T>,
    ) -> PackageResult<T> {
        let _lock = RootLock::acquire(&self.directory)?;
        let state = self.read()?;
        operation(&active_record_ref(&state, id)?.approval)
    }

    /// Holds the approval lock through a selection-changing operation. A
    /// concurrent revocation or catalog replacement cannot land between the
    /// authorization check and the activation record update.
    pub fn with_current_catalog<T>(
        &self,
        id: &str,
        expected_digest: &str,
        target: &str,
        now: u64,
        operation: impl FnOnce(&ApprovedRepository, &VerifiedCatalog) -> PackageResult<T>,
    ) -> PackageResult<T> {
        let _lock = RootLock::acquire(&self.directory)?;
        let state = self.read()?;
        let record = active_record_ref(&state, id)?;
        let saved = record
            .catalog
            .as_ref()
            .ok_or("repository catalog not loaded")?;
        let verified = verify_catalog(
            &record.approval,
            saved.raw.as_bytes(),
            &saved.signature,
            &record.checkpoint,
            target,
            now,
        )?;
        if verified.digest() != expected_digest {
            return Err("repository catalog changed before selection".into());
        }
        operation(&record.approval, &verified)
    }

    fn change(&self, operation: impl FnOnce(&mut State) -> PackageResult<()>) -> PackageResult<()> {
        create_private_directory(&self.directory)?;
        let _lock = RootLock::acquire(&self.directory)?;
        let mut state = self.read()?;
        operation(&mut state)?;
        write_json_atomically(&self.path(), &state)
    }

    fn read(&self) -> PackageResult<State> {
        let path = self.path();
        if !path.try_exists().map_err(|error| error.to_string())? {
            return Ok(State::default());
        }
        let raw = read_bounded(&path, MAX_STATE_BYTES)?;
        let state: State = serde_json::from_slice(&raw).map_err(|error| error.to_string())?;
        validate_state(&state)?;
        Ok(state)
    }

    fn path(&self) -> PathBuf {
        self.directory.join("approved.json")
    }
}

fn active_keys(state: &State) -> BTreeMap<String, String> {
    state
        .repositories
        .values()
        .filter(|record| !record.revoked)
        .map(|record| {
            (
                record.approval.fingerprint.clone(),
                record.approval.public_key.clone(),
            )
        })
        .collect()
}

fn active_record<'a>(state: &'a mut State, id: &str) -> PackageResult<&'a mut RepositoryRecord> {
    let record = state.repositories.get_mut(id).ok_or("unknown repository")?;
    if record.revoked {
        return Err("repository publisher has been revoked".into());
    }
    Ok(record)
}

fn active_record_ref<'a>(state: &'a State, id: &str) -> PackageResult<&'a RepositoryRecord> {
    let record = state.repositories.get(id).ok_or("unknown repository")?;
    if record.revoked {
        return Err("repository publisher has been revoked".into());
    }
    Ok(record)
}

fn validate_state(state: &State) -> PackageResult<()> {
    if state.schema != 1 || state.repositories.len() > MAX_REPOSITORIES {
        return Err("invalid repository approval state".into());
    }
    for (id, record) in &state.repositories {
        record.approval.validate()?;
        if *id != record.approval.id {
            return Err("repository approval identity mismatch".into());
        }
    }
    Ok(())
}
