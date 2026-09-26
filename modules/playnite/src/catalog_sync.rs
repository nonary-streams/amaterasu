//! Reconciles only complete Playnite extension snapshots into the fixed owner
//! assigned to this provider by catalog startup policy.

use crate::settings::ProductSettings;
use crate::snapshot::{
    Game, SOURCE_INSTALLED, SelectedGame, Snapshot, playnite_launch, select_game_sources,
};
use amaterasu_module_sdk::{Command, Response, RuntimeClient};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CATALOG_SERVICE: &str = "amaterasu.catalog/1";
const REQUEST_TIMEOUT_MS: u64 = 30_000;
const MAX_BATCH_ENTRIES: usize = 8;
const MAX_BATCH_BYTES: usize = 12_000;
const MAX_CONFLICT_RESTARTS: u8 = 3;
const INVENTORY_PAGE_SIZE: usize = 32;
const MAX_INVENTORY_ENTRIES: usize = 100_000;
const SECONDS_PER_DAY: i64 = 86_400;
static SYNC_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Stage {
    Inventory,
    Begin,
    Batch,
    Finish,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Call {
    Submit { operation_id: String },
    Waiting { request: u64 },
}

enum BarrierCall {
    Submit,
    Waiting { request: u64, deadline: Instant },
}

struct EpochBarrier {
    epoch: Option<u64>,
    revocations: VecDeque<String>,
    read_operation_id: String,
    call: BarrierCall,
    stale_request: Option<u64>,
}

struct LaunchProbe {
    operation_id: String,
    request_id: String,
    provider_epoch: u64,
    request: Option<u64>,
    deadline: Instant,
    verified: bool,
}

impl Default for EpochBarrier {
    fn default() -> Self {
        Self {
            epoch: None,
            revocations: VecDeque::new(),
            read_operation_id: format!("playnite.catalog.epoch-read.{}", new_sync_id(0)),
            call: BarrierCall::Submit,
            stale_request: None,
        }
    }
}

impl EpochBarrier {
    fn ready(&self) -> bool {
        self.epoch.is_some() && self.revocations.is_empty()
    }

    fn revoke(&mut self) {
        // No snapshot or launch is admitted until the current revocation is
        // acknowledged, so further transitions can share its durable fence.
        if self.revocations.is_empty() {
            self.revocations.push_back(new_sync_id(0));
        }
    }

    fn invalidate(&mut self) {
        if let BarrierCall::Waiting { request, .. } = self.call {
            self.stale_request = Some(request);
        }
        self.epoch = None;
        self.call = BarrierCall::Submit;
        self.read_operation_id = format!("playnite.catalog.epoch-read.{}", new_sync_id(0));
        if self.revocations.is_empty() {
            self.revoke();
        }
    }

    fn revoke_conflicted(&mut self) {
        self.revocations.pop_front();
        self.epoch = None;
        self.call = BarrierCall::Submit;
        self.read_operation_id = format!("playnite.catalog.epoch-read.{}", new_sync_id(0));
        self.revoke();
    }

    fn advance_with(
        &mut self,
        now: Instant,
        mut send: impl FnMut(Command) -> Result<Response, String>,
    ) -> Result<(), String> {
        if let Some(request) = self.stale_request {
            if settle_stale_child_with(request, &mut send) {
                self.stale_request = None;
            }
            return Ok(());
        }
        if self.ready() {
            return Ok(());
        }
        let payload = if let Some(epoch) = self.epoch {
            json!({
                "action":"provider_snapshot_revoke",
                "expected_provider_epoch":epoch,
                "revocation_id":self.revocations.front().expect("pending revocation has an ID")
            })
        } else {
            json!({"action":"provider_snapshot_inventory","after":null,"limit":1})
        };
        let operation_id = self
            .revocations
            .front()
            .filter(|_| self.epoch.is_some())
            .map_or_else(
                || self.read_operation_id.clone(),
                |id| format!("playnite.catalog.revoke.{id}"),
            );
        let response = match self.call {
            BarrierCall::Submit => send(Command::Request {
                service: CATALOG_SERVICE.into(),
                payload,
                timeout_ms: REQUEST_TIMEOUT_MS,
                operation_id,
                parent: None,
            }),
            BarrierCall::Waiting { request, deadline } if now < deadline => {
                let result = send(Command::TakeResult { request });
                match result {
                    Ok(Response::Pending) | Err(_) => return Ok(()),
                    Ok(response) => {
                        if !settled(send(Command::ReleaseResult { request })) {
                            return Ok(());
                        }
                        self.call = BarrierCall::Submit;
                        Ok(response)
                    }
                }
            }
            BarrierCall::Waiting { request, .. } => {
                if settled(send(Command::Cancel { request }))
                    && settled(send(Command::ReleaseResult { request }))
                {
                    // The Catalog revoke may have committed before its reply was lost.
                    // Reuse the same revocation ID and expected epoch on replay.
                    self.call = BarrierCall::Submit;
                }
                return Ok(());
            }
        };
        match response {
            Ok(Response::Accepted { id }) => {
                self.call = BarrierCall::Waiting {
                    request: id,
                    deadline: now + Duration::from_millis(REQUEST_TIMEOUT_MS + 5_000),
                };
            }
            Ok(Response::Result { payload }) => {
                if payload.get("ok").and_then(Value::as_bool) != Some(true) {
                    if self.epoch.is_some()
                        && payload.pointer("/error/code").and_then(Value::as_str)
                            == Some("provider_epoch_conflict")
                    {
                        self.revoke_conflicted();
                    }
                    return Err("Catalog rejected Playnite epoch transition".into());
                }
                let epoch = payload
                    .get("provider_epoch")
                    .and_then(Value::as_u64)
                    .ok_or("Catalog omitted Playnite provider_epoch")?;
                if let Some(previous) = self.epoch {
                    if epoch <= previous {
                        return Err("Catalog revocation did not advance Playnite epoch".into());
                    }
                    self.revocations.pop_front();
                }
                self.epoch = Some(epoch);
                self.call = BarrierCall::Submit;
            }
            Ok(Response::Error { code, message }) => {
                if self.epoch.is_some() && code == "provider_epoch_conflict" {
                    self.revoke_conflicted();
                }
                return Err(format!("{code}: {message}"));
            }
            Ok(response) => {
                return Err(format!("unexpected Playnite epoch response: {response:?}"));
            }
            Err(_) => {}
        }
        Ok(())
    }
}

fn settled(response: Result<Response, String>) -> bool {
    matches!(response, Ok(Response::Ok))
        || matches!(response, Ok(Response::Error { code, .. }) if code == "not_found")
}

struct Transaction {
    sync_id: String,
    source_revision: u64,
    settings_epoch: u64,
    provider_epoch: u64,
    canceled_for_process_change: bool,
    launchable_game_ids: std::collections::BTreeSet<String>,
    entries: Vec<Vec<Value>>,
    remove_source_id_batches: Vec<Vec<String>>,
    expire_source_id_batches: Vec<Vec<String>>,
    selection_remove_source_id_batches: Vec<Vec<String>>,
    manual_refresh_batches: Option<Vec<Vec<ManualRefreshEntry>>>,
    remove_uninstalled: bool,
    remove_expired: bool,
    remove_selection: bool,
    publish_launchable_game_ids: bool,
    autosync_delete_after_days: u32,
    autosync_require_replacement: bool,
    sync_all_installed: bool,
    recent_mode: bool,
    now_unix: i64,
    uninstalled_game_ids: std::collections::BTreeSet<String>,
    last_played_by_id: std::collections::BTreeMap<String, i64>,
    selected_source_ids: std::collections::BTreeSet<String>,
    inventory: Vec<InventoryEntry>,
    inventory_total_count: Option<usize>,
    inventory_after: Option<String>,
    collection_revision: Option<u64>,
    generation: Option<u64>,
    revision: Option<u64>,
    sequence: u64,
    batch_index: usize,
    stage: Stage,
    call: Call,
    conflicts: u8,
    operation_sequence: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct InventoryEntry {
    source_id: String,
    added_at_unix: Option<i64>,
    source_flags: u32,
    attached: bool,
}

#[derive(Clone, Copy)]
struct ManualRefreshEntry {
    candidate_batch_index: usize,
    candidate_entry_index: usize,
    source_flags: u32,
}

impl Transaction {
    fn payload(&self) -> Value {
        match self.stage {
            Stage::Inventory => json!({
                "action":"provider_snapshot_inventory",
                "after":self.inventory_after,
                "expected_collection_revision":self.collection_revision,
                "limit":INVENTORY_PAGE_SIZE
            }),
            Stage::Begin => json!({
                "action":"provider_snapshot_begin",
                "sync_id":self.sync_id,
                "source_revision":self.source_revision,
                "provider_epoch":self.provider_epoch,
                "expected_collection_revision":self.collection_revision
            }),
            Stage::Batch => json!({
                "action":"provider_snapshot_batch",
                "sync_id":self.sync_id,
                "generation":self.generation.expect("begin is committed first"),
                "source_revision":self.source_revision,
                "provider_epoch":self.provider_epoch,
                "sequence":self.sequence,
                "expected_revision":self.revision.expect("begin provides revision"),
                "entries":self.entries_for_batch(),
                "remove_source_ids":self.uninstalled_ids_for_batch(),
                "expire_source_ids":self.expired_ids_for_batch(),
                "selection_remove_source_ids":self.selection_ids_for_batch()
            }),
            Stage::Finish => json!({
                "action":"provider_snapshot_finish",
                "sync_id":self.sync_id,
                "generation":self.generation.expect("begin is committed first"),
                "source_revision":self.source_revision,
                "provider_epoch":self.provider_epoch,
                "expected_revision":self.revision.expect("begin provides revision"),
                "next_sequence":self.sequence,
                "complete":true,
                "remove_stale":false,
                "remove_uninstalled":self.remove_uninstalled,
                "remove_expired":self.remove_expired,
                "remove_selection":self.remove_selection
            }),
        }
    }

    fn entry_batch_count(&self) -> usize {
        self.manual_refresh_batches
            .as_ref()
            .map_or(self.entries.len(), Vec::len)
    }

    fn batch_count(&self) -> usize {
        self.entry_batch_count()
            + self.remove_source_id_batches.len()
            + self.expire_source_id_batches.len()
            + self.selection_remove_source_id_batches.len()
    }

    fn entries_for_batch(&self) -> Vec<Value> {
        let Some(manual_batches) = &self.manual_refresh_batches else {
            return self
                .entries
                .get(self.batch_index)
                .cloned()
                .unwrap_or_default();
        };
        manual_batches
            .get(self.batch_index)
            .into_iter()
            .flatten()
            .filter_map(|reference| {
                let mut entry = self
                    .entries
                    .get(reference.candidate_batch_index)?
                    .get(reference.candidate_entry_index)?
                    .clone();
                entry["source_flags"] = json!(reference.source_flags);
                Some(entry)
            })
            .collect()
    }

    fn uninstalled_ids_for_batch(&self) -> Vec<String> {
        let Some(index) = self.batch_index.checked_sub(self.entry_batch_count()) else {
            return Vec::new();
        };
        self.remove_source_id_batches
            .get(index)
            .cloned()
            .unwrap_or_default()
    }

    fn expired_ids_for_batch(&self) -> Vec<String> {
        let index = self
            .batch_index
            .saturating_sub(self.entry_batch_count() + self.remove_source_id_batches.len());
        self.expire_source_id_batches
            .get(index)
            .cloned()
            .unwrap_or_default()
    }

    fn selection_ids_for_batch(&self) -> Vec<String> {
        let index = self.batch_index.saturating_sub(
            self.entry_batch_count()
                + self.remove_source_id_batches.len()
                + self.expire_source_id_batches.len(),
        );
        self.selection_remove_source_id_batches
            .get(index)
            .cloned()
            .unwrap_or_default()
    }

    fn next_operation_id(&mut self) -> String {
        self.operation_sequence = self.operation_sequence.saturating_add(1);
        format!(
            "playnite.catalog.{}.{}",
            self.sync_id, self.operation_sequence
        )
    }

    fn restart(&mut self) {
        self.sync_id = new_sync_id(self.source_revision);
        self.generation = None;
        self.revision = None;
        self.sequence = 0;
        self.batch_index = 0;
        self.stage = Stage::Inventory;
        self.inventory.clear();
        self.inventory_total_count = None;
        self.inventory_after = None;
        self.collection_revision = None;
        self.remove_source_id_batches.clear();
        self.expire_source_id_batches.clear();
        self.selection_remove_source_id_batches.clear();
        self.manual_refresh_batches = None;
        self.call = Call::Submit {
            operation_id: self.next_operation_id(),
        };
    }

    fn consume(&mut self, payload: Value) -> Result<bool, String> {
        if payload.get("ok").and_then(Value::as_bool) != Some(true) {
            let code = payload
                .pointer("/error/code")
                .and_then(Value::as_str)
                .unwrap_or("catalog_error");
            let message = payload
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("catalog rejected the provider snapshot");
            if matches!(
                code,
                "revision_conflict" | "snapshot_conflict" | "sequence_conflict"
            ) && self.conflicts < MAX_CONFLICT_RESTARTS
            {
                self.conflicts += 1;
                self.restart();
                return Ok(false);
            }
            return Err(format!("{code}: {message}"));
        }
        if self.stage == Stage::Inventory {
            return self.consume_inventory_page(payload);
        }
        let revision = payload
            .get("revision")
            .and_then(Value::as_u64)
            .ok_or("catalog response omitted revision")?;
        let generation = payload
            .get("generation")
            .and_then(Value::as_u64)
            .ok_or("catalog response omitted generation")?;
        match self.stage {
            Stage::Inventory => unreachable!("inventory responses are handled before this match"),
            Stage::Begin => {
                let next = payload
                    .get("next_sequence")
                    .and_then(Value::as_u64)
                    .ok_or("catalog begin omitted next_sequence")?;
                if generation == 0 || next != 0 {
                    return Err("catalog begin response was inconsistent".into());
                }
                self.generation = Some(generation);
                self.revision = Some(revision);
                self.stage = if self.batch_count() == 0 {
                    Stage::Finish
                } else {
                    Stage::Batch
                };
            }
            Stage::Batch => {
                let expected_next = self
                    .sequence
                    .checked_add(1)
                    .ok_or("catalog sequence exhausted")?;
                if Some(generation) != self.generation
                    || revision < self.revision.unwrap_or(u64::MAX)
                    || payload.get("next_sequence").and_then(Value::as_u64) != Some(expected_next)
                {
                    return Err("catalog batch response was inconsistent".into());
                }
                self.revision = Some(revision);
                self.sequence = expected_next;
                self.batch_index += 1;
                self.stage = if self.batch_index == self.batch_count() {
                    Stage::Finish
                } else {
                    Stage::Batch
                };
            }
            Stage::Finish => {
                if Some(generation) != self.generation
                    || revision < self.revision.unwrap_or(u64::MAX)
                    || payload.get("complete").and_then(Value::as_bool) != Some(true)
                    || payload.get("cleanup_pending").and_then(Value::as_bool) != Some(false)
                    || payload.get("remove_stale").and_then(Value::as_bool) != Some(false)
                    || payload.get("remove_uninstalled").and_then(Value::as_bool)
                        != Some(self.remove_uninstalled)
                    || payload.get("remove_expired").and_then(Value::as_bool)
                        != Some(self.remove_expired)
                    || payload.get("remove_selection").and_then(Value::as_bool)
                        != Some(self.remove_selection)
                {
                    return Err("catalog finish response was inconsistent".into());
                }
                self.revision = Some(revision);
                return Ok(true);
            }
        }
        self.call = Call::Submit {
            operation_id: self.next_operation_id(),
        };
        Ok(false)
    }

    fn consume_inventory_page(&mut self, payload: Value) -> Result<bool, String> {
        let collection_revision = payload
            .get("collection_revision")
            .and_then(Value::as_u64)
            .ok_or("catalog inventory omitted collection_revision")?;
        if self
            .collection_revision
            .is_some_and(|revision| revision != collection_revision)
        {
            return self.restart_after_conflict();
        }
        let total_count = payload
            .get("total_count")
            .and_then(Value::as_u64)
            .and_then(|count| usize::try_from(count).ok())
            .filter(|count| *count <= MAX_INVENTORY_ENTRIES)
            .ok_or("catalog inventory total_count is invalid")?;
        if self
            .inventory_total_count
            .is_some_and(|previous| previous != total_count)
        {
            return self.restart_after_conflict();
        }
        let entries = payload
            .get("entries")
            .and_then(Value::as_array)
            .ok_or("catalog inventory omitted entries")?;
        if entries.len() > INVENTORY_PAGE_SIZE
            || self.inventory.len().saturating_add(entries.len()) > total_count
        {
            return Err("catalog inventory page exceeded its declared bounds".into());
        }
        let mut previous = self.inventory_after.clone().unwrap_or_default();
        for entry in entries {
            let parsed: InventoryEntry = serde_json::from_value(entry.clone())
                .map_err(|_| "catalog inventory entry is malformed")?;
            if !crate::snapshot::valid_playnite_id(&parsed.source_id)
                || parsed.source_id <= previous
                || parsed
                    .added_at_unix
                    .is_some_and(|timestamp| !(0..=253_402_300_799).contains(&timestamp))
            {
                return Err("catalog inventory entry is invalid or unsorted".into());
            }
            previous = parsed.source_id.clone();
            self.inventory.push(parsed);
        }
        self.collection_revision = Some(collection_revision);
        self.inventory_total_count = Some(total_count);
        let next_after = payload
            .get("next_after")
            .ok_or("catalog inventory omitted next_after")?;
        if let Some(next_after) = next_after.as_str() {
            if entries.len() != INVENTORY_PAGE_SIZE
                || entries
                    .last()
                    .and_then(|entry| entry.get("source_id"))
                    .and_then(Value::as_str)
                    != Some(next_after)
                || self.inventory.len() >= total_count
            {
                return Err("catalog inventory continuation is inconsistent".into());
            }
            self.inventory_after = Some(next_after.to_owned());
        } else if next_after.is_null() {
            if self.inventory.len() != total_count {
                return Err("catalog inventory ended before total_count".into());
            }
            self.prepare_removals();
            self.stage = Stage::Begin;
            self.inventory_after = None;
        } else {
            return Err("catalog inventory next_after is malformed".into());
        }
        self.call = Call::Submit {
            operation_id: self.next_operation_id(),
        };
        Ok(false)
    }

    fn restart_after_conflict(&mut self) -> Result<bool, String> {
        if self.conflicts >= MAX_CONFLICT_RESTARTS {
            return Err("catalog inventory revision kept changing".into());
        }
        self.conflicts += 1;
        self.restart();
        Ok(false)
    }

    fn prepare_removals(&mut self) {
        let existing_ids: std::collections::BTreeSet<_> = self
            .inventory
            .iter()
            .map(|entry| entry.source_id.to_ascii_lowercase())
            .collect();
        if !self.remove_selection {
            self.prepare_manual_refresh_batches();
        }
        let replacements =
            if self.remove_selection && self.recent_mode && self.autosync_require_replacement {
                self.selected_source_ids
                    .iter()
                    .filter(|source_id| !existing_ids.contains(*source_id))
                    .count()
            } else {
                0
            };
        let mut uninstalled = Vec::new();
        let mut expired = Vec::new();
        let mut selection = Vec::new();

        for marker in &self.inventory {
            if !marker.attached {
                continue;
            }
            let normalized_source_id = marker.source_id.to_ascii_lowercase();
            if self.remove_uninstalled && self.uninstalled_game_ids.contains(&normalized_source_id)
            {
                uninstalled.push(marker.source_id.clone());
                continue;
            }
            if self.remove_expired
                && should_expire(
                    marker,
                    self.autosync_delete_after_days,
                    self.now_unix,
                    &self.last_played_by_id,
                )
            {
                expired.push(marker.source_id.clone());
                continue;
            }
            if self.remove_selection
                && !self.sync_all_installed
                && marker.source_flags == SOURCE_INSTALLED
                && !self.selected_source_ids.contains(&normalized_source_id)
            {
                selection.push(marker.source_id.clone());
            }
        }

        if replacements > 0 {
            let already_marked: std::collections::BTreeSet<_> = uninstalled
                .iter()
                .chain(expired.iter())
                .chain(selection.iter())
                .cloned()
                .collect();
            let mut remaining = replacements;
            for marker in &self.inventory {
                if remaining == 0 {
                    break;
                }
                if marker.attached
                    && !self
                        .selected_source_ids
                        .contains(&marker.source_id.to_ascii_lowercase())
                    && !already_marked.contains(&marker.source_id)
                {
                    selection.push(marker.source_id.clone());
                    remaining -= 1;
                }
            }
        }

        self.remove_source_id_batches = uninstalled
            .chunks(MAX_BATCH_ENTRIES)
            .map(<[String]>::to_vec)
            .collect();
        self.expire_source_id_batches = expired
            .chunks(MAX_BATCH_ENTRIES)
            .map(<[String]>::to_vec)
            .collect();
        self.selection_remove_source_id_batches = selection
            .chunks(MAX_BATCH_ENTRIES)
            .map(<[String]>::to_vec)
            .collect();
    }

    fn prepare_manual_refresh_batches(&mut self) {
        let attached_source_flags: std::collections::BTreeMap<_, _> = self
            .inventory
            .iter()
            .filter(|entry| entry.attached)
            .map(|entry| (entry.source_id.to_ascii_lowercase(), entry.source_flags))
            .collect();
        let mut launchable_game_ids = std::collections::BTreeSet::new();
        let manual_refresh_batches = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(candidate_batch_index, candidate_batch)| {
                let matching_entries: Vec<_> = candidate_batch
                    .iter()
                    .enumerate()
                    .filter_map(|(candidate_entry_index, candidate)| {
                        let source_id = candidate.get("source_id")?.as_str()?;
                        let source_flags =
                            attached_source_flags.get(&source_id.to_ascii_lowercase())?;
                        launchable_game_ids.insert(source_id.to_ascii_lowercase());
                        Some(ManualRefreshEntry {
                            candidate_batch_index,
                            candidate_entry_index,
                            source_flags: *source_flags,
                        })
                    })
                    .collect();
                (!matching_entries.is_empty()).then_some(matching_entries)
            })
            .collect();
        self.launchable_game_ids = launchable_game_ids;
        self.manual_refresh_batches = Some(manual_refresh_batches);
    }
}

fn should_expire(
    marker: &InventoryEntry,
    delete_after_days: u32,
    now_unix: i64,
    last_played_by_id: &std::collections::BTreeMap<String, i64>,
) -> bool {
    if delete_after_days == 0 {
        return false;
    }
    let Some(added_at) = marker.added_at_unix else {
        return false;
    };
    let Some(deadline) = i64::from(delete_after_days)
        .checked_mul(SECONDS_PER_DAY)
        .and_then(|age| added_at.checked_add(age))
    else {
        return false;
    };
    now_unix >= deadline
        && last_played_by_id
            .get(&marker.source_id.to_ascii_lowercase())
            .is_none_or(|last_played| *last_played < added_at)
}

#[derive(Default)]
pub struct CatalogSync {
    epoch_barrier: EpochBarrier,
    launch_probe: Option<LaunchProbe>,
    transaction: Option<Transaction>,
    completed_launchable_game_ids: Option<std::collections::BTreeSet<String>>,
    source_revision: u64,
    last_error: Option<String>,
    skipped_launches: u64,
}

impl CatalogSync {
    pub fn epoch_ready(&self) -> bool {
        self.epoch_barrier.ready()
    }

    pub fn revoke_epoch(&mut self) {
        self.completed_launchable_game_ids = None;
        self.cancel_after_process_change();
        self.cancel_launch_probe(None);
        self.epoch_barrier.revoke();
    }

    pub fn cancel_launch_probe(&mut self, operation_id: Option<&str>) {
        let Some(probe) = self.launch_probe.as_mut() else {
            return;
        };
        if operation_id.is_some_and(|operation_id| probe.operation_id != operation_id) {
            return;
        }
        if probe.request.is_some() {
            probe.operation_id.clear();
            probe.verified = false;
        } else {
            self.launch_probe = None;
        }
    }

    /// A launch is admitted only after a fresh authenticated Catalog inventory
    /// reply for this operation, including settlement of an accepted child.
    pub fn verify_launch_epoch(&mut self, operation_id: &str) -> bool {
        if !self.epoch_ready() {
            return false;
        }
        if let Some(probe) = self.launch_probe.as_ref() {
            if probe.operation_id == operation_id
                && probe.verified
                && Some(probe.provider_epoch) == self.epoch_barrier.epoch
            {
                self.launch_probe = None;
                return true;
            }
            if probe.request.is_some() || !probe.verified {
                return false;
            }
        }
        self.launch_probe = Some(LaunchProbe {
            operation_id: operation_id.to_owned(),
            request_id: format!("playnite.catalog.launch-check.{}", new_sync_id(0)),
            provider_epoch: self.epoch_barrier.epoch.expect("ready epoch"),
            request: None,
            deadline: Instant::now(),
            verified: false,
        });
        false
    }

    fn advance_launch_probe_with(
        &mut self,
        now: Instant,
        mut send: impl FnMut(Command) -> Result<Response, String>,
    ) {
        let Some(mut probe) = self.launch_probe.take() else {
            return;
        };
        if probe.operation_id.is_empty() {
            if let Some(request) = probe.request
                && !settle_stale_child_with(request, &mut send)
            {
                self.launch_probe = Some(probe);
            }
            return;
        }
        if Some(probe.provider_epoch) != self.epoch_barrier.epoch {
            if let Some(request) = probe.request
                && !settle_stale_child_with(request, &mut send)
            {
                self.launch_probe = Some(probe);
            }
            return;
        }
        if probe.verified {
            self.launch_probe = Some(probe);
            return;
        }
        if let Some(request) = probe.request {
            if now >= probe.deadline {
                if settled(send(Command::Cancel { request }))
                    && settled(send(Command::ReleaseResult { request }))
                {
                    probe.request = None;
                    probe.request_id = format!("playnite.catalog.launch-check.{}", new_sync_id(0));
                }
                self.launch_probe = Some(probe);
                return;
            }
            match send(Command::TakeResult { request }) {
                Ok(Response::Pending) | Err(_) => {
                    self.launch_probe = Some(probe);
                    return;
                }
                Ok(response) => {
                    if !settled(send(Command::ReleaseResult { request })) {
                        self.launch_probe = Some(probe);
                        return;
                    }
                    probe.request = None;
                    if let Response::Result { payload } = response {
                        probe.verified = payload.get("ok").and_then(Value::as_bool) == Some(true)
                            && payload.get("provider_epoch").and_then(Value::as_u64)
                                == Some(probe.provider_epoch);
                    }
                }
            }
        } else {
            match send(Command::Request {
                service: CATALOG_SERVICE.into(),
                payload: json!({"action":"provider_snapshot_inventory","after":null,"limit":1}),
                timeout_ms: REQUEST_TIMEOUT_MS,
                operation_id: probe.request_id.clone(),
                parent: None,
            }) {
                Ok(Response::Accepted { id }) => {
                    probe.request = Some(id);
                    probe.deadline = now + Duration::from_millis(REQUEST_TIMEOUT_MS + 5_000);
                }
                Ok(Response::Result { payload }) => {
                    probe.verified = payload.get("ok").and_then(Value::as_bool) == Some(true)
                        && payload.get("provider_epoch").and_then(Value::as_u64)
                            == Some(probe.provider_epoch);
                }
                _ => {}
            }
        }
        if probe.verified && Some(probe.provider_epoch) != self.epoch_barrier.epoch {
            probe.verified = false;
        }
        if !probe.verified && probe.request.is_none() {
            self.epoch_barrier.invalidate();
            self.launch_probe = None;
            return;
        }
        self.launch_probe = Some(probe);
    }

    pub fn start(
        &mut self,
        snapshot: &Snapshot,
        settings: &ProductSettings,
        settings_epoch: u64,
        now_unix: i64,
    ) -> Result<(), String> {
        if self.transaction.is_some() {
            return Err("catalog_sync_busy".into());
        }
        let (entries, skipped) = if settings.auto_sync {
            let selected = select_game_sources(snapshot, settings, now_unix);
            prepare_selected_entries(&selected)?
        } else {
            prepare_entries(&snapshot.games)?
        };
        let remove_uninstalled = settings.auto_sync && settings.autosync_remove_uninstalled;
        let remove_expired = settings.auto_sync && settings.autosync_delete_after_days > 0;
        let remove_selection = settings.auto_sync;
        let launchable_game_ids = if settings.auto_sync {
            entries
                .iter()
                .flatten()
                .filter_map(|entry| entry["source_id"].as_str())
                .map(str::to_ascii_lowercase)
                .collect()
        } else {
            Default::default()
        };
        let selected_source_ids = if settings.auto_sync {
            entries
                .iter()
                .flatten()
                .filter_map(|entry| entry["source_id"].as_str())
                .map(str::to_ascii_lowercase)
                .collect()
        } else {
            Default::default()
        };
        let uninstalled_game_ids = if remove_uninstalled {
            snapshot
                .games
                .iter()
                .filter(|game| {
                    game.is_installed() == Some(false)
                        && crate::snapshot::valid_playnite_id(&game.id)
                })
                .map(|game| game.id.to_ascii_lowercase())
                .collect()
        } else {
            Default::default()
        };
        let last_played_by_id = snapshot
            .games
            .iter()
            .filter(|game| game.is_installed() == Some(true))
            .filter_map(|game| {
                crate::snapshot::valid_playnite_id(&game.id)
                    .then(|| crate::snapshot::parse_iso8601_unix(&game.last_played))
                    .flatten()
                    .map(|played| (game.id.to_ascii_lowercase(), played))
            })
            .collect();
        self.skipped_launches = skipped;
        self.source_revision = self
            .source_revision
            .checked_add(1)
            .ok_or("Playnite source revision exhausted")?;
        let mut transaction = Transaction {
            sync_id: new_sync_id(self.source_revision),
            source_revision: self.source_revision,
            settings_epoch,
            provider_epoch: self.epoch_barrier.epoch.unwrap_or(0),
            canceled_for_process_change: false,
            launchable_game_ids,
            entries,
            remove_source_id_batches: Vec::new(),
            expire_source_id_batches: Vec::new(),
            selection_remove_source_id_batches: Vec::new(),
            manual_refresh_batches: None,
            remove_uninstalled,
            remove_expired,
            remove_selection,
            publish_launchable_game_ids: true,
            autosync_delete_after_days: settings.autosync_delete_after_days,
            autosync_require_replacement: settings.autosync_require_replacement,
            sync_all_installed: settings.sync_all_installed,
            recent_mode: settings.recent_games > 0,
            now_unix,
            uninstalled_game_ids,
            last_played_by_id,
            selected_source_ids,
            inventory: Vec::new(),
            inventory_total_count: None,
            inventory_after: None,
            collection_revision: None,
            generation: None,
            revision: None,
            sequence: 0,
            batch_index: 0,
            stage: Stage::Inventory,
            call: Call::Submit {
                operation_id: String::new(),
            },
            conflicts: 0,
            operation_sequence: 0,
        };
        transaction.call = Call::Submit {
            operation_id: transaction.next_operation_id(),
        };
        self.transaction = Some(transaction);
        self.completed_launchable_game_ids = None;
        self.last_error = None;
        Ok(())
    }

    pub fn busy(&self) -> bool {
        self.transaction.is_some()
    }

    pub fn retain_for_settings_epoch(&mut self, settings_epoch: u64, same_catalog_policy: bool) {
        if same_catalog_policy
            && let Some(transaction) = self.transaction.as_mut()
            && !transaction.canceled_for_process_change
        {
            transaction.settings_epoch = settings_epoch;
        }
    }

    pub fn cancel_after_process_change(&mut self) {
        self.completed_launchable_game_ids = None;
        let Some(mut transaction) = self.transaction.take() else {
            return;
        };
        if matches!(transaction.call, Call::Waiting { .. }) {
            transaction.canceled_for_process_change = true;
            self.transaction = Some(transaction);
        }
        self.last_error = Some("catalog import canceled after Playnite process changed".into());
    }

    pub fn take_completed_launchable_game_ids(
        &mut self,
    ) -> Option<std::collections::BTreeSet<String>> {
        self.completed_launchable_game_ids.take()
    }

    pub fn status(&self) -> Value {
        json!({
            "syncing":self.busy(),
            "source_revision":self.source_revision,
            "skipped_unsupported_launches":self.skipped_launches,
            "last_error":self.last_error
        })
    }

    pub fn advance(
        &mut self,
        runtime: &RuntimeClient,
        current_settings_epoch: u64,
    ) -> Result<(), String> {
        if let Err(error) = self
            .epoch_barrier
            .advance_with(Instant::now(), |command| runtime.send(command))
        {
            self.last_error = Some(format!("Playnite catalog epoch unavailable: {error}"));
        }
        if !self.epoch_ready() {
            return Ok(());
        }
        self.advance_launch_probe_with(Instant::now(), |command| runtime.send(command));
        if !self.epoch_ready() {
            return Ok(());
        }
        let Some(mut transaction) = self.transaction.take() else {
            return Ok(());
        };
        if transaction.settings_epoch != current_settings_epoch
            || transaction.canceled_for_process_change
            || Some(transaction.provider_epoch) != self.epoch_barrier.epoch
        {
            let canceled_for_process_change = transaction.canceled_for_process_change;
            if let Call::Waiting { request } = transaction.call
                && !settle_stale_child_with(request, |command| runtime.send(command))
            {
                transaction.call = Call::Waiting { request };
                self.transaction = Some(transaction);
            }
            self.last_error = Some(
                if canceled_for_process_change {
                    "catalog import canceled after Playnite process changed"
                } else {
                    "catalog import canceled after settings changed"
                }
                .into(),
            );
            return Ok(());
        }

        match transaction.call {
            Call::Submit { ref operation_id } => {
                let operation_id = operation_id.clone();
                let payload = transaction.payload();
                match runtime.send(Command::Request {
                    service: CATALOG_SERVICE.into(),
                    payload,
                    timeout_ms: REQUEST_TIMEOUT_MS,
                    operation_id: operation_id.clone(),
                    parent: None,
                }) {
                    Ok(Response::Accepted { id }) => {
                        transaction.call = Call::Waiting { request: id };
                        self.transaction = Some(transaction);
                    }
                    Ok(Response::Result { payload }) => self.finish_call(transaction, payload),
                    Ok(Response::Error { code, message }) => {
                        self.call_failed(transaction, format!("{code}: {message}"));
                    }
                    Ok(response) => self.call_failed(
                        transaction,
                        format!("unexpected catalog response: {response:?}"),
                    ),
                    Err(error) => {
                        transaction.call = Call::Submit { operation_id };
                        self.transaction = Some(transaction);
                        self.epoch_barrier.invalidate();
                        self.last_error = Some(format!("Playnite catalog unavailable: {error}"));
                    }
                }
            }
            Call::Waiting { request } => {
                match runtime.send_ephemeral(Command::TakeResult { request }) {
                    Ok(Response::Pending) => {
                        transaction.call = Call::Waiting { request };
                        self.transaction = Some(transaction);
                    }
                    Err(error) => {
                        transaction.call = Call::Waiting { request };
                        self.transaction = Some(transaction);
                        self.epoch_barrier.invalidate();
                        self.last_error = Some(format!("Playnite catalog unavailable: {error}"));
                    }
                    Ok(Response::Result { payload }) => self.finish_call(transaction, payload),
                    Ok(Response::Error { code, message }) => {
                        self.call_failed(transaction, format!("{code}: {message}"));
                    }
                    Ok(response) => self.call_failed(
                        transaction,
                        format!("unexpected catalog result: {response:?}"),
                    ),
                }
            }
        }
        Ok(())
    }

    fn finish_call(&mut self, mut transaction: Transaction, payload: Value) {
        match transaction.consume(payload) {
            Ok(true) => {
                if transaction.publish_launchable_game_ids
                    && self.epoch_ready()
                    && Some(transaction.provider_epoch) == self.epoch_barrier.epoch
                {
                    self.completed_launchable_game_ids =
                        Some(transaction.launchable_game_ids.clone());
                }
                self.last_error = None;
            }
            Ok(false) => self.transaction = Some(transaction),
            Err(error) => {
                if error.starts_with("provider_epoch_conflict:") {
                    self.epoch_barrier.invalidate();
                }
                self.last_error = Some(error);
            }
        }
    }

    fn call_failed(&mut self, mut transaction: Transaction, error: String) {
        if error.starts_with("provider_epoch_conflict:") {
            self.epoch_barrier.invalidate();
            self.last_error = Some(error);
            return;
        }
        if transaction.conflicts < MAX_CONFLICT_RESTARTS {
            transaction.conflicts += 1;
            transaction.restart();
            self.transaction = Some(transaction);
        } else {
            self.last_error = Some(error);
        }
    }
}

fn settle_stale_child_with(
    request: u64,
    mut send: impl FnMut(Command) -> Result<Response, String>,
) -> bool {
    if settled(send(Command::Cancel { request })) {
        return settled(send(Command::ReleaseResult { request }));
    }
    match send(Command::TakeResult { request }) {
        Ok(Response::Pending) | Err(_) => false,
        Ok(_) => settled(send(Command::ReleaseResult { request })),
    }
}

fn prepare_entries(games: &[Game]) -> Result<(Vec<Vec<Value>>, u64), String> {
    let selected: Vec<_> = games
        .iter()
        .cloned()
        .map(|game| SelectedGame {
            game,
            source_flags: 0,
        })
        .collect();
    prepare_selected_entries(&selected)
}

fn prepare_selected_entries(games: &[SelectedGame]) -> Result<(Vec<Vec<Value>>, u64), String> {
    let mut prepared = Vec::new();
    let mut skipped = 0u64;
    for selected in games {
        let Some(value) = provider_entry_with_flags(&selected.game, selected.source_flags) else {
            skipped = skipped.saturating_add(1);
            continue;
        };
        let entry = &value["entry"];
        let encoded = serde_json::to_vec(entry).map_err(|error| error.to_string())?;
        if encoded.len() > 11_950
            || serde_json::to_vec(&value).map_or(true, |bytes| bytes.len() > MAX_BATCH_BYTES)
        {
            skipped = skipped.saturating_add(1);
            continue;
        }
        prepared.push(value);
    }

    let mut batches = Vec::new();
    let mut current = Vec::new();
    for entry in prepared {
        let mut candidate = current.clone();
        candidate.push(entry.clone());
        let encoded_size = serde_json::to_vec(&candidate).map_or(usize::MAX, |bytes| bytes.len());
        if !current.is_empty()
            && (candidate.len() > MAX_BATCH_ENTRIES || encoded_size > MAX_BATCH_BYTES)
        {
            batches.push(std::mem::take(&mut current));
        }
        current.push(entry);
        if current.len() == 1
            && serde_json::to_vec(&current).map_or(true, |bytes| bytes.len() > MAX_BATCH_BYTES)
        {
            current.clear();
            skipped = skipped.saturating_add(1);
        }
    }
    if !current.is_empty() {
        batches.push(current);
    }
    Ok((batches, skipped))
}

#[cfg(test)]
fn provider_entry(game: &Game) -> Option<Value> {
    provider_entry_with_flags(game, 0)
}

fn provider_entry_with_flags(game: &Game, source_flags: u32) -> Option<Value> {
    if !crate::snapshot::valid_playnite_id(&game.id)
        || game.name.trim().is_empty()
        || game.name.len() > 160
        || game.name.chars().any(char::is_control)
    {
        return None;
    }
    let launch = playnite_launch(game)?;
    Some(json!({
        "source_id":game.id.to_ascii_lowercase(),
        "entry":{"name":game.name,"description":"","tags":[],"launch":launch},
        "source_flags":source_flags
    }))
}

fn new_sync_id(source_revision: u64) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let sequence = SYNC_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!(
        "playnite-{source_revision}-{}-{nanos:x}-{sequence}",
        std::process::id()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{Category, Plugin};
    use amaterasu_module_sdk::abi::{
        AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_INVALID_ARGUMENT, STATUS_OK, Status,
    };
    use std::ffi::c_void;

    #[test]
    fn lost_revocation_reply_reuses_transition_id_and_unblocks_current_epoch() {
        let mut barrier = EpochBarrier::default();
        barrier.revoke();
        let now = Instant::now();
        barrier
            .advance_with(now, |command| {
                let Command::Request { payload, .. } = command else {
                    panic!("expected inventory")
                };
                assert_eq!(payload["action"], "provider_snapshot_inventory");
                Ok(Response::Result {
                    payload: json!({"ok":true,"provider_epoch":7}),
                })
            })
            .unwrap();
        let mut original_id = String::new();
        barrier
            .advance_with(now, |command| {
                let Command::Request { payload, .. } = command else {
                    panic!("expected revoke")
                };
                assert_eq!(payload["expected_provider_epoch"], 7);
                original_id = payload["revocation_id"].as_str().unwrap().to_owned();
                Ok(Response::Accepted { id: 44 })
            })
            .unwrap();
        assert!(!barrier.ready());
        barrier
            .advance_with(now + Duration::from_secs(36), |command| {
                assert!(matches!(
                    command,
                    Command::Cancel { request: 44 } | Command::ReleaseResult { request: 44 }
                ));
                Ok(Response::Ok)
            })
            .unwrap();
        barrier
            .advance_with(now + Duration::from_secs(37), |command| {
                let Command::Request { payload, .. } = command else {
                    panic!("expected retry")
                };
                assert_eq!(payload["revocation_id"], original_id);
                assert_eq!(payload["expected_provider_epoch"], 7);
                Ok(Response::Result {
                    payload: json!({"ok":true,"provider_epoch":8,"revision":9}),
                })
            })
            .unwrap();
        assert!(barrier.ready());
        assert_eq!(barrier.epoch, Some(8));
    }

    #[test]
    fn repeated_process_changes_during_outage_share_one_pending_revocation() {
        let mut barrier = EpochBarrier {
            epoch: Some(6),
            ..Default::default()
        };
        barrier.revoke();
        let first_id = barrier.revocations.front().unwrap().clone();
        for _ in 0..100 {
            barrier.revoke();
            barrier
                .advance_with(Instant::now(), |_| Err("catalog offline".into()))
                .unwrap();
        }
        assert_eq!(barrier.revocations.len(), 1);
        assert_eq!(barrier.revocations.front(), Some(&first_id));
        barrier
            .advance_with(Instant::now(), |command| {
                let Command::Request { payload, .. } = command else {
                    panic!("expected revoke")
                };
                assert_eq!(payload["revocation_id"], first_id);
                Ok(Response::Result {
                    payload: json!({"ok":true,"provider_epoch":7}),
                })
            })
            .unwrap();
        assert!(barrier.ready());
        barrier.revoke();
        assert_ne!(barrier.revocations.front(), Some(&first_id));
    }

    #[test]
    fn revoke_conflict_rereads_epoch_with_new_transition_id() {
        let mut barrier = EpochBarrier {
            epoch: Some(3),
            ..Default::default()
        };
        barrier.revoke();
        let stale_id = barrier.revocations.front().unwrap().clone();
        assert!(
            barrier
                .advance_with(Instant::now(), |command| {
                    let Command::Request { payload, .. } = command else {
                        panic!("expected revoke")
                    };
                    assert_eq!(payload["revocation_id"], stale_id);
                    Ok(Response::Result {
                        payload: json!({"ok":false,"error":{"code":"provider_epoch_conflict"}}),
                    })
                })
                .is_err()
        );
        assert_eq!(barrier.epoch, None);
        assert_ne!(barrier.revocations.front(), Some(&stale_id));
        barrier
            .advance_with(Instant::now(), |command| {
                let Command::Request { payload, .. } = command else {
                    panic!("expected inventory")
                };
                assert_eq!(payload["action"], "provider_snapshot_inventory");
                Ok(Response::Result {
                    payload: json!({"ok":true,"provider_epoch":4}),
                })
            })
            .unwrap();
        barrier
            .advance_with(Instant::now(), |command| {
                let Command::Request { payload, .. } = command else {
                    panic!("expected revoke")
                };
                assert_eq!(payload["expected_provider_epoch"], 4);
                assert_ne!(payload["revocation_id"], stale_id);
                Ok(Response::Result {
                    payload: json!({"ok":true,"provider_epoch":5}),
                })
            })
            .unwrap();
        assert!(barrier.ready());
    }

    #[test]
    fn launch_probe_requires_result_and_release_then_recovers_after_outage() {
        let mut sync = CatalogSync::default();
        sync.epoch_barrier.epoch = Some(3);
        assert!(!sync.verify_launch_epoch("first"));
        let now = Instant::now();
        sync.advance_launch_probe_with(now, |command| {
            assert!(matches!(command, Command::Request { .. }));
            Ok(Response::Accepted { id: 61 })
        });
        assert!(!sync.verify_launch_epoch("first"));
        let mut commands = Vec::new();
        sync.advance_launch_probe_with(now, |command| {
            commands.push(command);
            Ok(if commands.len() == 1 {
                Response::Result {
                    payload: json!({"ok":true,"provider_epoch":3}),
                }
            } else {
                Response::Ok
            })
        });
        assert!(matches!(
            commands.as_slice(),
            [
                Command::TakeResult { request: 61 },
                Command::ReleaseResult { request: 61 }
            ]
        ));
        assert!(sync.verify_launch_epoch("first"));

        assert!(!sync.verify_launch_epoch("outage"));
        sync.advance_launch_probe_with(now, |_| Err("catalog offline".into()));
        assert!(!sync.epoch_ready());
        assert!(!sync.verify_launch_epoch("outage"));
        sync.epoch_barrier
            .advance_with(now, |_| {
                Ok(Response::Result {
                    payload: json!({"ok":true,"provider_epoch":3}),
                })
            })
            .unwrap();
        sync.epoch_barrier
            .advance_with(now, |_| {
                Ok(Response::Result {
                    payload: json!({"ok":true,"provider_epoch":4}),
                })
            })
            .unwrap();
        assert!(sync.epoch_ready());
        assert!(!sync.verify_launch_epoch("recovered"));
        sync.advance_launch_probe_with(now, |_| {
            Ok(Response::Result {
                payload: json!({"ok":true,"provider_epoch":4}),
            })
        });
        assert!(sync.verify_launch_epoch("recovered"));
    }

    #[test]
    fn expired_launch_probe_settles_old_child_before_new_operation() {
        let mut sync = CatalogSync::default();
        sync.epoch_barrier.epoch = Some(5);
        assert!(!sync.verify_launch_epoch("old"));
        sync.advance_launch_probe_with(Instant::now(), |_| Ok(Response::Accepted { id: 62 }));
        sync.cancel_launch_probe(Some("old"));
        assert!(!sync.verify_launch_epoch("new"));
        sync.advance_launch_probe_with(Instant::now(), |command| {
            assert!(matches!(
                command,
                Command::Cancel { request: 62 } | Command::ReleaseResult { request: 62 }
            ));
            Ok(Response::Ok)
        });
        assert!(sync.launch_probe.is_none());
        assert!(!sync.verify_launch_epoch("new"));
        sync.advance_launch_probe_with(Instant::now(), |_| {
            Ok(Response::Result {
                payload: json!({"ok":true,"provider_epoch":5}),
            })
        });
        assert!(sync.verify_launch_epoch("new"));
    }

    #[test]
    fn canceled_finish_child_remains_owned_until_broker_settles() {
        for attempt in 0..8 {
            let mut calls = Vec::new();
            assert!(!settle_stale_child_with(63, |command| {
                calls.push(command);
                if attempt % 2 == 0 && calls.len() == 1 {
                    Err("cancel unavailable".into())
                } else if attempt % 2 == 1 && calls.len() == 2 {
                    Err("release unavailable".into())
                } else if attempt % 2 == 1 {
                    Ok(Response::Ok)
                } else {
                    Ok(Response::Pending)
                }
            }));
        }
        let mut calls = Vec::new();
        assert!(settle_stale_child_with(63, |command| {
            calls.push(command);
            Ok(Response::Ok)
        }));
        assert!(matches!(
            calls.as_slice(),
            [
                Command::Cancel { request: 63 },
                Command::ReleaseResult { request: 63 }
            ]
        ));
    }

    #[test]
    fn old_finish_is_not_resubmitted_after_epoch_revocation() {
        let snapshot = Snapshot {
            games: vec![],
            categories: vec![],
            plugins: vec![],
        };
        let mut sync = CatalogSync::default();
        sync.epoch_barrier.epoch = Some(7);
        sync.start(&snapshot, &ProductSettings::default(), 1, 0)
            .unwrap();
        let transaction = sync.transaction.as_mut().unwrap();
        transaction.stage = Stage::Finish;
        transaction.generation = Some(1);
        transaction.revision = Some(1);
        transaction.call = Call::Waiting { request: 51 };
        assert_eq!(transaction.payload()["provider_epoch"], 7);
        sync.cancel_after_process_change();
        sync.revoke_epoch();
        sync.epoch_barrier
            .advance_with(Instant::now(), |command| {
                let Command::Request { payload, .. } = command else {
                    panic!("expected revoke")
                };
                assert_eq!(payload["action"], "provider_snapshot_revoke");
                Ok(Response::Result {
                    payload: json!({"ok":true,"provider_epoch":8,"revision":2}),
                })
            })
            .unwrap();
        assert!(sync.epoch_ready());
        assert!(
            sync.transaction
                .as_ref()
                .unwrap()
                .canceled_for_process_change
        );
        assert!(sync.take_completed_launchable_game_ids().is_none());
        let mut current = CatalogSync::default();
        current.epoch_barrier.epoch = Some(8);
        current
            .start(&snapshot, &ProductSettings::default(), 2, 0)
            .unwrap();
        assert_eq!(
            current.transaction.as_ref().unwrap().payload()["action"],
            "provider_snapshot_inventory"
        );
        current.transaction.as_mut().unwrap().stage = Stage::Begin;
        assert_eq!(
            current.transaction.as_ref().unwrap().payload()["provider_epoch"],
            8
        );
    }

    #[test]
    fn catalog_epoch_conflict_reloads_durable_epoch_before_new_snapshot() {
        let snapshot = Snapshot {
            games: vec![],
            categories: vec![],
            plugins: vec![],
        };
        let mut sync = CatalogSync::default();
        sync.epoch_barrier.epoch = Some(2);
        sync.start(&snapshot, &ProductSettings::default(), 1, 0)
            .unwrap();
        let transaction = sync.transaction.take().unwrap();
        sync.finish_call(transaction, json!({"ok":false,"error":{"code":"provider_epoch_conflict","message":"stale provider"}}));
        assert!(!sync.epoch_ready());
        assert!(!sync.busy());
        sync.epoch_barrier
            .advance_with(Instant::now(), |_| {
                Ok(Response::Result {
                    payload: json!({"ok":true,"provider_epoch":4}),
                })
            })
            .unwrap();
        sync.epoch_barrier
            .advance_with(Instant::now(), |_| {
                Ok(Response::Result {
                    payload: json!({"ok":true,"provider_epoch":5}),
                })
            })
            .unwrap();
        assert!(sync.epoch_ready());
        sync.start(&snapshot, &ProductSettings::default(), 2, 0)
            .unwrap();
        assert_eq!(sync.transaction.as_ref().unwrap().provider_epoch, 5);
    }

    #[derive(Default)]
    struct FakeCatalogHost {
        commands: Vec<Command>,
    }

    unsafe extern "C" fn invoke_fake_catalog_host(
        context: *mut c_void,
        input: BorrowedBytes,
        reply: ReplySink,
    ) -> Status {
        // SAFETY: the test keeps the boxed host alive through every synchronous
        // runtime call, and the ABI buffers are borrowed only for this call.
        let Ok(input) = (unsafe { input.read() }) else {
            return STATUS_INVALID_ARGUMENT;
        };
        let Ok(command) = serde_json::from_slice::<Command>(input) else {
            return STATUS_INVALID_ARGUMENT;
        };
        let response = match command {
            Command::Request { .. } => Response::Accepted { id: 42 },
            Command::TakeResult { .. } => Response::Pending,
            Command::Cancel { .. } | Command::ReleaseResult { .. } => Response::Ok,
            _ => return STATUS_INVALID_ARGUMENT,
        };
        // SAFETY: context points to the live test host for this callback.
        unsafe { &mut *(context as *mut FakeCatalogHost) }
            .commands
            .push(command);
        let output = serde_json::to_vec(&response).unwrap();
        // SAFETY: the output vector remains alive until the callback copies it.
        unsafe { reply.send(&output) };
        STATUS_OK
    }

    fn game(id: &str, name: &str, exe: &str) -> Game {
        Game {
            id: id.into(),
            name: name.into(),
            exe: exe.into(),
            args: String::new(),
            working_directory: String::new(),
            install_directory: String::new(),
            categories: Vec::new(),
            plugin_id: String::new(),
            plugin_name: String::new(),
            last_played: String::new(),
            installed: Some(true),
            is_installed: None,
        }
    }

    fn inventory_entry(
        source_id: &str,
        added_at_unix: Option<i64>,
        source_flags: u32,
        attached: bool,
    ) -> InventoryEntry {
        InventoryEntry {
            source_id: source_id.into(),
            added_at_unix,
            source_flags,
            attached,
        }
    }

    fn finish_inventory(
        transaction: &mut Transaction,
        entries: Vec<InventoryEntry>,
        revision: u64,
    ) {
        let total_count = entries.len();
        transaction
            .consume_inventory_page(json!({
                "ok":true,
                "collection_revision":revision,
                "total_count":total_count,
                "entries":entries,
                "next_after":null
            }))
            .unwrap();
    }

    #[test]
    fn imports_typed_playnite_launches_with_or_without_native_executable_metadata() {
        let snapshot = Snapshot {
            games: vec![
                game(
                    "a1b2c3d4-1234-5678-9abc-0123456789ab",
                    "A game",
                    r"C:\Games\game.exe",
                ),
                game("b1b2c3d4-1234-5678-9abc-0123456789ab", "No executable", ""),
                game("not-a-guid", "Invalid ID", r"C:\Games\other.exe"),
                game(
                    "c1b2c3d4-1234-5678-9abc-0123456789ab",
                    &"x".repeat(161),
                    r"C:\Games\large.exe",
                ),
            ],
            categories: vec![Category {
                id: "c".into(),
                name: "Cat".into(),
            }],
            plugins: vec![Plugin {
                id: "p".into(),
                name: "Plugin".into(),
            }],
        };
        let (batches, skipped) = prepare_entries(&snapshot.games).unwrap();
        assert_eq!(skipped, 2);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].len(), 2);
        assert_eq!(
            batches[0][0]["source_id"],
            "a1b2c3d4-1234-5678-9abc-0123456789ab"
        );
        assert_eq!(batches[0][0]["entry"]["launch"]["kind"], "playnite");
        assert_eq!(batches[0][1]["entry"]["launch"]["kind"], "playnite");
        assert_eq!(
            batches[0][1]["entry"]["launch"]["game_id"],
            "b1b2c3d4-1234-5678-9abc-0123456789ab"
        );
    }

    #[test]
    fn complete_import_finish_uses_targeted_uninstalled_cleanup_only() {
        let mut sync = CatalogSync::default();
        let snapshot = Snapshot {
            games: vec![game(
                "a1b2c3d4-1234-5678-9abc-0123456789ab",
                "Game",
                r"C:\Games\game.exe",
            )],
            categories: vec![],
            plugins: vec![],
        };
        sync.start(
            &snapshot,
            &ProductSettings {
                sync_all_installed: true,
                ..ProductSettings::default()
            },
            7,
            0,
        )
        .unwrap();
        let transaction = sync.transaction.as_ref().unwrap();
        assert_eq!(
            transaction.payload()["action"],
            "provider_snapshot_inventory"
        );
        let mut transaction = sync.transaction.take().unwrap();
        transaction.stage = Stage::Finish;
        transaction.generation = Some(1);
        transaction.revision = Some(2);
        assert_eq!(transaction.payload()["remove_stale"], false);
        assert_eq!(transaction.payload()["complete"], true);
        assert_eq!(transaction.payload()["remove_uninstalled"], true);
        assert_eq!(transaction.payload()["remove_expired"], true);
        assert_eq!(transaction.payload()["remove_selection"], true);
        assert!(transaction.remove_source_id_batches.is_empty());
    }

    #[test]
    fn process_change_cancels_unsent_and_in_flight_catalog_reconciliation() {
        let snapshot = Snapshot {
            games: vec![game(
                "a1b2c3d4-1234-5678-9abc-0123456789ab",
                "Game",
                r"C:\Games\game.exe",
            )],
            categories: vec![],
            plugins: vec![],
        };
        let settings = ProductSettings {
            sync_all_installed: true,
            ..ProductSettings::default()
        };
        let mut sync = CatalogSync::default();
        sync.start(&snapshot, &settings, 7, 0).unwrap();
        assert!(sync.busy());
        sync.cancel_after_process_change();
        assert!(!sync.busy());
        assert!(sync.take_completed_launchable_game_ids().is_none());

        sync.start(&snapshot, &settings, 7, 0).unwrap();
        sync.transaction.as_mut().unwrap().call = Call::Waiting { request: 42 };
        sync.cancel_after_process_change();
        assert!(sync.busy());
        assert!(
            sync.transaction
                .as_ref()
                .unwrap()
                .canceled_for_process_change
        );
        assert!(matches!(
            sync.transaction.as_ref().unwrap().call,
            Call::Waiting { request: 42 }
        ));
        assert!(sync.take_completed_launchable_game_ids().is_none());
    }

    #[test]
    fn operational_settings_change_keeps_in_flight_import_but_process_change_cancels_it() {
        let snapshot = Snapshot {
            games: vec![game(
                "a1b2c3d4-1234-5678-9abc-0123456789ab",
                "Game",
                r"C:\Games\game.exe",
            )],
            categories: vec![],
            plugins: vec![],
        };
        let settings = ProductSettings {
            enabled: true,
            auto_sync: false,
            ..ProductSettings::default()
        };
        let mut sync = CatalogSync::default();
        sync.epoch_barrier.epoch = Some(0);
        sync.start(&snapshot, &settings, 7, 0).unwrap();
        sync.retain_for_settings_epoch(8, true);

        let mut host = Box::new(FakeCatalogHost::default());
        let endpoint = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: (&mut *host as *mut FakeCatalogHost).cast(),
            invoke: invoke_fake_catalog_host,
        };
        // SAFETY: host remains boxed and alive through every runtime call.
        let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
        sync.advance(&runtime, 8).unwrap();
        assert!(matches!(
            host.commands.as_slice(),
            [Command::Request { .. }]
        ));
        assert!(matches!(
            sync.transaction.as_ref().unwrap().call,
            Call::Waiting { request: 42 }
        ));
        assert_eq!(sync.transaction.as_ref().unwrap().settings_epoch, 8);

        sync.cancel_after_process_change();
        sync.retain_for_settings_epoch(9, true);
        assert_eq!(sync.transaction.as_ref().unwrap().settings_epoch, 8);
        assert!(
            sync.transaction
                .as_ref()
                .unwrap()
                .canceled_for_process_change
        );
        sync.advance(&runtime, 9).unwrap();
        assert!(matches!(
            host.commands.as_slice(),
            [
                Command::Request { .. },
                Command::Cancel { request: 42 },
                Command::ReleaseResult { request: 42 }
            ]
        ));
        assert!(!sync.busy());
        assert!(sync.take_completed_launchable_game_ids().is_none());
        sync.start(&snapshot, &settings, 9, 0).unwrap();
        sync.advance(&runtime, 9).unwrap();
        assert!(matches!(
            host.commands.last(),
            Some(Command::Request { .. })
        ));
        assert!(sync.busy());
    }

    #[test]
    fn canceled_partial_import_does_not_block_or_authorize_reconnected_snapshot() {
        let old_id = "a1b2c3d4-1234-5678-9abc-0123456789ab";
        let current_id = "b1b2c3d4-1234-5678-9abc-0123456789ab";
        let settings = ProductSettings {
            sync_all_installed: true,
            ..ProductSettings::default()
        };
        let snapshot = |id| Snapshot {
            games: vec![game(id, "Game", r"C:\Games\game.exe")],
            categories: vec![],
            plugins: vec![],
        };
        let mut sync = CatalogSync::default();
        sync.epoch_barrier.epoch = Some(0);
        sync.start(&snapshot(old_id), &settings, 7, 0).unwrap();
        sync.transaction.as_mut().unwrap().call = Call::Waiting { request: 42 };
        sync.cancel_after_process_change();

        let mut host = Box::new(FakeCatalogHost::default());
        let endpoint = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: (&mut *host as *mut FakeCatalogHost).cast(),
            invoke: invoke_fake_catalog_host,
        };
        // SAFETY: host remains boxed and alive through every runtime call.
        let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
        sync.advance(&runtime, 7).unwrap();
        assert!(!sync.busy());
        assert!(sync.take_completed_launchable_game_ids().is_none());
        assert!(matches!(
            host.commands.as_slice(),
            [
                Command::Cancel { request: 42 },
                Command::ReleaseResult { request: 42 }
            ]
        ));

        sync.start(&snapshot(current_id), &settings, 8, 0).unwrap();
        let mut transaction = sync.transaction.take().unwrap();
        finish_inventory(&mut transaction, vec![], 5);
        assert_eq!(transaction.stage, Stage::Begin);
        assert!(
            !transaction
                .consume(json!({
                    "ok":true,"revision":5,"generation":2,"next_sequence":0
                }))
                .unwrap()
        );
        assert_eq!(transaction.payload()["entries"][0]["source_id"], current_id);
        assert!(
            !transaction
                .consume(json!({
                    "ok":true,"revision":6,"generation":2,"next_sequence":1
                }))
                .unwrap()
        );
        assert_eq!(transaction.stage, Stage::Finish);
        sync.finish_call(
            transaction,
            json!({
                "ok":true,"revision":6,"generation":2,"complete":true,
                "cleanup_pending":false,"remove_stale":false,
                "remove_uninstalled":true,"remove_expired":true,"remove_selection":true
            }),
        );
        assert_eq!(
            sync.take_completed_launchable_game_ids(),
            Some(std::collections::BTreeSet::from([current_id.into()]))
        );
    }

    #[test]
    fn only_explicit_uninstalled_ids_become_bounded_tombstones() {
        let installed_id = "a1b2c3d4-1234-5678-9abc-0123456789ab";
        let excluded_id = "b1b2c3d4-1234-5678-9abc-0123456789ab";
        let mut games = vec![
            game(installed_id, "Installed", r"C:\Games\installed.exe"),
            game(
                excluded_id,
                "Excluded but installed",
                r"C:\Games\excluded.exe",
            ),
        ];
        for index in 0..9 {
            let id = format!("{index:08x}-1234-5678-9abc-0123456789ab");
            let mut uninstalled = game(&id, "Uninstalled", "");
            uninstalled.installed = Some(false);
            games.push(uninstalled);
        }
        let mut invalid_uninstalled = game("not-a-guid", "Invalid ID", "");
        invalid_uninstalled.installed = Some(false);
        games.push(invalid_uninstalled);

        let snapshot = Snapshot {
            games,
            categories: vec![],
            plugins: vec![],
        };
        let settings = ProductSettings {
            sync_all_installed: true,
            exclude_games: vec![excluded_id.to_ascii_uppercase()],
            ..ProductSettings::default()
        };
        let mut sync = CatalogSync::default();
        sync.start(&snapshot, &settings, 1, 0).unwrap();
        let transaction = sync.transaction.as_mut().unwrap();
        assert!(transaction.remove_uninstalled);
        assert_eq!(transaction.entries.len(), 1);
        let inventory = (0..9)
            .map(|index| {
                inventory_entry(
                    &format!("{index:08x}-1234-5678-9abc-0123456789ab"),
                    None,
                    0,
                    true,
                )
            })
            .collect();
        finish_inventory(transaction, inventory, 4);
        assert_eq!(transaction.remove_source_id_batches.len(), 2);
        assert_eq!(
            transaction.remove_source_id_batches[0].len(),
            MAX_BATCH_ENTRIES
        );
        assert_eq!(transaction.remove_source_id_batches[1].len(), 1);

        transaction.stage = Stage::Batch;
        transaction.generation = Some(1);
        transaction.revision = Some(0);
        transaction.batch_index = 0;
        let selected_batch = transaction.payload();
        assert_eq!(selected_batch["entries"].as_array().unwrap().len(), 1);
        assert!(
            selected_batch["remove_source_ids"]
                .as_array()
                .unwrap()
                .is_empty()
        );

        transaction.batch_index = transaction.entries.len();
        let first_tombstone_batch = transaction.payload();
        assert!(
            first_tombstone_batch["entries"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            first_tombstone_batch["remove_source_ids"]
                .as_array()
                .unwrap()
                .len(),
            MAX_BATCH_ENTRIES
        );

        let mut manual_settings = settings;
        manual_settings.auto_sync = false;
        let mut manual_refresh = CatalogSync::default();
        manual_refresh
            .start(&snapshot, &manual_settings, 2, 0)
            .unwrap();
        let manual_transaction = manual_refresh.transaction.as_mut().unwrap();
        let inventory = (0..9)
            .map(|index| {
                inventory_entry(
                    &format!("{index:08x}-1234-5678-9abc-0123456789ab"),
                    None,
                    0,
                    true,
                )
            })
            .collect();
        finish_inventory(manual_transaction, inventory, 4);
        assert!(!manual_transaction.remove_uninstalled);
        assert!(manual_transaction.remove_source_id_batches.is_empty());
    }

    #[test]
    fn manual_refresh_updates_only_attached_snapshot_rows_without_changing_membership() {
        let attached_id = "a1b2c3d4-1234-5678-9abc-0123456789ab";
        let detached_id = "b1b2c3d4-1234-5678-9abc-0123456789ab";
        let absent_id = "c1b2c3d4-1234-5678-9abc-0123456789ab";
        let new_id = "d1b2c3d4-1234-5678-9abc-0123456789ab";
        let existing_source_flags =
            crate::snapshot::SOURCE_RECENT | crate::snapshot::SOURCE_CATEGORY | SOURCE_INSTALLED;
        let snapshot = Snapshot {
            games: vec![
                game(attached_id, "Updated attached", r"C:\Games\attached.exe"),
                game(detached_id, "Detached", r"C:\Games\detached.exe"),
                game(new_id, "New", r"C:\Games\new.exe"),
            ],
            categories: vec![],
            plugins: vec![],
        };
        let settings = ProductSettings {
            auto_sync: false,
            recent_games: 0,
            ..ProductSettings::default()
        };
        let mut sync = CatalogSync::default();
        sync.epoch_barrier.epoch = Some(0);
        sync.start(&snapshot, &settings, 1, 0).unwrap();
        let mut transaction = sync.transaction.take().unwrap();
        assert!(transaction.publish_launchable_game_ids);

        finish_inventory(
            &mut transaction,
            vec![
                inventory_entry(attached_id, None, existing_source_flags, true),
                inventory_entry(detached_id, None, SOURCE_INSTALLED, false),
                inventory_entry(absent_id, None, SOURCE_INSTALLED, true),
            ],
            4,
        );
        assert_eq!(transaction.entry_batch_count(), 1);
        assert!(transaction.remove_source_id_batches.is_empty());
        assert!(transaction.expire_source_id_batches.is_empty());
        assert!(transaction.selection_remove_source_id_batches.is_empty());

        assert!(
            !transaction
                .consume(json!({
                    "ok":true,
                    "revision":5,
                    "generation":2,
                    "next_sequence":0
                }))
                .unwrap()
        );
        let batch = transaction.payload();
        let entries = batch["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["source_id"], attached_id);
        assert_eq!(entries[0]["entry"]["name"], "Updated attached");
        assert_eq!(entries[0]["source_flags"], existing_source_flags);

        assert!(
            !transaction
                .consume(json!({
                    "ok":true,
                    "revision":6,
                    "generation":2,
                    "next_sequence":1
                }))
                .unwrap()
        );
        sync.finish_call(
            transaction,
            json!({
                "ok":true,
                "revision":6,
                "generation":2,
                "complete":true,
                "cleanup_pending":false,
                "remove_stale":false,
                "remove_uninstalled":false,
                "remove_expired":false,
                "remove_selection":false
            }),
        );
        assert_eq!(
            sync.take_completed_launchable_game_ids(),
            Some(std::collections::BTreeSet::from([
                attached_id.to_ascii_lowercase()
            ]))
        );
    }

    #[test]
    fn ttl_cleanup_needs_a_trusted_timestamp_and_no_play_after_import() {
        let old_id = "a1b2c3d4-1234-5678-9abc-0123456789ab";
        let played_id = "b1b2c3d4-1234-5678-9abc-0123456789ab";
        let unknown_id = "c1b2c3d4-1234-5678-9abc-0123456789ab";
        let future_id = "d1b2c3d4-1234-5678-9abc-0123456789ab";
        let now = crate::snapshot::parse_iso8601_unix("2026-09-24T00:00:00Z").unwrap();
        let snapshot = Snapshot {
            games: vec![
                game(old_id, "Old", ""),
                Game {
                    last_played: "2026-09-20T00:00:00Z".into(),
                    ..game(played_id, "Played since import", "")
                },
                game(unknown_id, "Unknown timestamp", ""),
                game(future_id, "Future import", ""),
            ],
            categories: Vec::new(),
            plugins: Vec::new(),
        };
        let settings = ProductSettings {
            autosync_require_replacement: false,
            ..ProductSettings::default()
        };
        let mut sync = CatalogSync::default();
        sync.start(&snapshot, &settings, 1, now).unwrap();
        let transaction = sync.transaction.as_mut().unwrap();
        finish_inventory(
            transaction,
            vec![
                inventory_entry(old_id, Some(now - 15 * SECONDS_PER_DAY), 0, true),
                inventory_entry(played_id, Some(now - 20 * SECONDS_PER_DAY), 0, true),
                inventory_entry(unknown_id, None, 0, true),
                inventory_entry(future_id, Some(now + SECONDS_PER_DAY), 0, true),
            ],
            3,
        );
        assert_eq!(transaction.expire_source_id_batches.concat(), vec![old_id]);
        assert!(transaction.remove_source_id_batches.is_empty());
        assert!(transaction.selection_remove_source_id_batches.is_empty());

        for settings in [
            ProductSettings {
                autosync_delete_after_days: 0,
                autosync_require_replacement: false,
                ..ProductSettings::default()
            },
            ProductSettings {
                auto_sync: false,
                autosync_require_replacement: false,
                ..ProductSettings::default()
            },
        ] {
            let mut sync = CatalogSync::default();
            sync.start(&snapshot, &settings, 2, now).unwrap();
            let transaction = sync.transaction.as_mut().unwrap();
            finish_inventory(
                transaction,
                vec![inventory_entry(
                    old_id,
                    Some(now - 15 * SECONDS_PER_DAY),
                    0,
                    true,
                )],
                4,
            );
            assert!(transaction.expire_source_id_batches.is_empty());
        }
    }

    #[test]
    fn replacement_policy_only_removes_attached_unselected_rows_when_required() {
        let selected_id = "a1b2c3d4-1234-5678-9abc-0123456789ab";
        let previous_id = "b1b2c3d4-1234-5678-9abc-0123456789ab";
        let now = crate::snapshot::parse_iso8601_unix("2026-09-24T00:00:00Z").unwrap();
        let snapshot = Snapshot {
            games: vec![Game {
                last_played: "2026-09-23T00:00:00Z".into(),
                ..game(selected_id, "New recent", "")
            }],
            categories: Vec::new(),
            plugins: Vec::new(),
        };
        let inventory = vec![inventory_entry(
            previous_id,
            Some(now - SECONDS_PER_DAY),
            crate::snapshot::SOURCE_RECENT,
            true,
        )];

        let mut required = CatalogSync::default();
        required
            .start(
                &snapshot,
                &ProductSettings {
                    autosync_require_replacement: true,
                    ..ProductSettings::default()
                },
                1,
                now,
            )
            .unwrap();
        let transaction = required.transaction.as_mut().unwrap();
        finish_inventory(transaction, inventory.clone(), 5);
        assert_eq!(
            transaction.selection_remove_source_id_batches.concat(),
            vec![previous_id]
        );

        let mut optional = CatalogSync::default();
        optional
            .start(
                &snapshot,
                &ProductSettings {
                    autosync_require_replacement: false,
                    ..ProductSettings::default()
                },
                2,
                now,
            )
            .unwrap();
        let transaction = optional.transaction.as_mut().unwrap();
        finish_inventory(transaction, inventory, 5);
        assert!(transaction.selection_remove_source_id_batches.is_empty());

        let installed_inventory = vec![inventory_entry(
            previous_id,
            Some(now - SECONDS_PER_DAY),
            SOURCE_INSTALLED,
            true,
        )];
        let mut installed_source = CatalogSync::default();
        installed_source
            .start(
                &snapshot,
                &ProductSettings {
                    autosync_require_replacement: false,
                    sync_all_installed: false,
                    ..ProductSettings::default()
                },
                3,
                now,
            )
            .unwrap();
        let transaction = installed_source.transaction.as_mut().unwrap();
        finish_inventory(transaction, installed_inventory, 5);
        assert_eq!(
            transaction.selection_remove_source_id_batches.concat(),
            vec![previous_id]
        );
    }

    #[test]
    fn disabling_sync_all_retains_unselected_rows_with_combined_source_flags() {
        let selected_id = "a1b2c3d4-1234-5678-9abc-0123456789ab";
        let previous_id = "b1c2d3e4-1234-5678-9abc-0123456789ab";
        let now = crate::snapshot::parse_iso8601_unix("2026-09-24T00:00:00Z").unwrap();
        let snapshot = Snapshot {
            games: vec![Game {
                last_played: "2026-09-23T00:00:00Z".into(),
                ..game(selected_id, "Current recent", "")
            }],
            categories: Vec::new(),
            plugins: Vec::new(),
        };
        let inventory = vec![
            inventory_entry(
                selected_id,
                Some(now - SECONDS_PER_DAY),
                crate::snapshot::SOURCE_RECENT,
                true,
            ),
            inventory_entry(
                previous_id,
                Some(now - SECONDS_PER_DAY),
                SOURCE_INSTALLED | crate::snapshot::SOURCE_RECENT,
                true,
            ),
        ];

        let mut sync = CatalogSync::default();
        sync.start(
            &snapshot,
            &ProductSettings {
                recent_games: 1,
                sync_all_installed: false,
                autosync_require_replacement: true,
                ..ProductSettings::default()
            },
            4,
            now,
        )
        .unwrap();
        let transaction = sync.transaction.as_mut().unwrap();
        finish_inventory(transaction, inventory, 5);

        assert!(transaction.selection_remove_source_id_batches.is_empty());
    }

    #[test]
    fn inventory_revision_drift_restarts_before_snapshot_begin() {
        let mut sync = CatalogSync::default();
        sync.start(
            &Snapshot {
                games: Vec::new(),
                categories: Vec::new(),
                plugins: Vec::new(),
            },
            &ProductSettings::default(),
            1,
            0,
        )
        .unwrap();
        let transaction = sync.transaction.as_mut().unwrap();
        assert_eq!(
            transaction.payload()["action"],
            "provider_snapshot_inventory"
        );
        let first_page: Vec<_> = (0..INVENTORY_PAGE_SIZE)
            .map(|index| {
                inventory_entry(
                    &format!("{index:08x}-1234-5678-9abc-0123456789ab"),
                    None,
                    0,
                    true,
                )
            })
            .collect();
        let last_source_id = "0000001f-1234-5678-9abc-0123456789ab";
        assert!(
            transaction
                .consume_inventory_page(json!({
                    "ok":true,
                    "collection_revision":5,
                    "total_count":33,
                    "entries":first_page,
                    "next_after":last_source_id
                }))
                .is_ok()
        );
        assert_eq!(transaction.payload()["expected_collection_revision"], 5);
        assert_eq!(transaction.payload()["after"], last_source_id);

        assert!(
            !transaction
                .consume(json!({
                    "ok":false,
                    "error":{"code":"revision_conflict","message":"changed"}
                }))
                .unwrap()
        );
        assert_eq!(transaction.stage, Stage::Inventory);
        assert!(transaction.inventory.is_empty());
        assert!(transaction.collection_revision.is_none());
        assert!(transaction.inventory_after.is_none());
        assert_eq!(transaction.conflicts, 1);
        assert_eq!(
            transaction.payload()["expected_collection_revision"],
            Value::Null
        );
    }

    #[test]
    fn truncated_inventory_cannot_advance_to_begin() {
        let mut sync = CatalogSync::default();
        sync.start(
            &Snapshot {
                games: Vec::new(),
                categories: Vec::new(),
                plugins: Vec::new(),
            },
            &ProductSettings::default(),
            1,
            0,
        )
        .unwrap();
        let transaction = sync.transaction.as_mut().unwrap();
        assert!(
            transaction
                .consume_inventory_page(json!({
                    "ok":true,
                    "collection_revision":5,
                    "total_count":2,
                    "entries":[inventory_entry("a1b2c3d4-1234-5678-9abc-0123456789ab",None,0,true)],
                    "next_after":null
                }))
                .is_err()
        );
        assert_eq!(transaction.stage, Stage::Inventory);
        assert!(transaction.remove_source_id_batches.is_empty());
        assert!(transaction.expire_source_id_batches.is_empty());
        assert!(transaction.selection_remove_source_id_batches.is_empty());
    }

    #[test]
    fn launch_eligibility_is_published_only_after_complete_catalog_finish() {
        let game_id = "a1b2c3d4-1234-5678-9abc-0123456789ab";
        let mut sync = CatalogSync::default();
        sync.epoch_barrier.epoch = Some(8);
        sync.start(
            &Snapshot {
                games: vec![game(game_id, "Game", r"C:\Games\game.exe")],
                categories: vec![],
                plugins: vec![],
            },
            &ProductSettings {
                sync_all_installed: true,
                ..ProductSettings::default()
            },
            7,
            0,
        )
        .unwrap();
        assert!(sync.take_completed_launchable_game_ids().is_none());

        let mut transaction = sync.transaction.take().unwrap();
        assert_eq!(transaction.provider_epoch, 8);
        transaction.stage = Stage::Begin;
        assert_eq!(transaction.payload()["provider_epoch"], 8);
        transaction.stage = Stage::Batch;
        transaction.generation = Some(1);
        transaction.revision = Some(2);
        assert_eq!(transaction.payload()["provider_epoch"], 8);
        transaction.stage = Stage::Finish;
        assert_eq!(transaction.payload()["provider_epoch"], 8);
        sync.finish_call(
            transaction,
            json!({
                "ok":true,
                "generation":1,
                "revision":3,
                "complete":true,
                "cleanup_pending":false,
                "remove_stale":false,
                "remove_uninstalled":true,
                "remove_expired":true,
                "remove_selection":true
            }),
        );
        assert_eq!(
            sync.take_completed_launchable_game_ids(),
            Some(std::collections::BTreeSet::from([game_id.into()]))
        );
    }

    #[test]
    fn selected_entries_are_prevalidated_before_begin() {
        let large = game(
            "a1b2c3d4-1234-5678-9abc-0123456789ab",
            &"x".repeat(161),
            r"C:\Games\game.exe",
        );
        assert!(provider_entry(&large).is_none());
    }

    #[test]
    fn playnite_launch_ignores_native_executable_and_argument_metadata() {
        let game = Game {
            exe: "playnite://not-a-command".into(),
            args: "malformed \" command metadata".into(),
            working_directory: "relative\u{0001}path".into(),
            install_directory: String::new(),
            ..game("a1b2c3d4-1234-5678-9abc-0123456789ab", "Game", "")
        };
        assert_eq!(
            provider_entry(&game).unwrap()["entry"]["launch"]["kind"],
            "playnite"
        );
    }
}
