//! Reconciles complete Lutris scans into the provider's fixed catalog owner.

use crate::discovery::Snapshot;
use crate::settings::ProductSettings;
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::configuration::release_terminal_child_result;

const SERVICE: &str = "amaterasu.catalog/1";
const TIMEOUT_MS: u64 = 30_000;
const MAX_BATCH_ENTRIES: usize = 8;
const MAX_BATCH_BYTES: usize = 12_000;
const MAX_RETRY_TICKS: u32 = 64;
const MAX_RETRIES: u8 = 8;
const MAX_CONFLICT_RESTARTS: u8 = 3;
const INVENTORY_PAGE_SIZE: usize = 32;
const MAX_INVENTORY_ENTRIES: usize = 100_000;
static SYNC_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Stage {
    Inventory,
    Begin,
    Batch,
    Finish,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Call {
    Submit,
    Waiting { request: u64 },
    Delay { ticks: u32 },
}

struct Transaction {
    sync_id: String,
    source_revision: u64,
    settings: ProductSettings,
    base_batches: Vec<SnapshotBatch>,
    batches: Vec<SnapshotBatch>,
    name_exclusions: Vec<String>,
    inventory: Vec<CatalogInventoryEntry>,
    inventory_total_count: Option<usize>,
    inventory_after: Option<String>,
    collection_revision: Option<u64>,
    generation: Option<u64>,
    catalog_revision: Option<u64>,
    sequence: u64,
    batch_index: usize,
    stage: Stage,
    call: Call,
    retries: u8,
    conflicts: u8,
    operation_sequence: u64,
    operation_id: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct SnapshotBatch {
    entries: Vec<Value>,
    selection_remove_source_ids: Vec<String>,
}

impl SnapshotBatch {
    fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.selection_remove_source_ids.is_empty()
    }

    fn intent_count(&self) -> usize {
        self.entries.len() + self.selection_remove_source_ids.len()
    }
}

struct PreparedSnapshot {
    entries: Vec<Value>,
    selection_remove_source_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogInventoryEntry {
    source_id: String,
    added_at_unix: Option<i64>,
    #[serde(rename = "source_flags")]
    _source_flags: u32,
    attached: bool,
    entry_name: Option<String>,
}

impl Transaction {
    fn new(
        source_revision: u64,
        settings: ProductSettings,
        batches: Vec<SnapshotBatch>,
        name_exclusions: Vec<String>,
    ) -> Self {
        let sync_id = new_sync_id(source_revision);
        let stage = if name_exclusions.is_empty() {
            Stage::Begin
        } else {
            Stage::Inventory
        };
        let mut transaction = Self {
            sync_id,
            source_revision,
            settings,
            base_batches: batches.clone(),
            batches,
            name_exclusions,
            inventory: Vec::new(),
            inventory_total_count: None,
            inventory_after: None,
            collection_revision: None,
            generation: None,
            catalog_revision: None,
            sequence: 0,
            batch_index: 0,
            stage,
            call: Call::Submit,
            retries: 0,
            conflicts: 0,
            operation_sequence: 0,
            operation_id: String::new(),
        };
        let operation_id = transaction.next_operation_id();
        transaction.operation_id = operation_id;
        transaction
    }

    fn payload(&self) -> Value {
        match self.stage {
            Stage::Inventory => json!({
                "action":"provider_snapshot_inventory",
                "after":self.inventory_after,
                "expected_collection_revision":self.collection_revision,
                "limit":INVENTORY_PAGE_SIZE,
                "include_entry_names":true,
            }),
            Stage::Begin => json!({
                "action":"provider_snapshot_begin",
                "sync_id":self.sync_id,
                "source_revision":self.source_revision,
                "expected_collection_revision":self.collection_revision,
            }),
            Stage::Batch => json!({
                "action":"provider_snapshot_batch",
                "sync_id":self.sync_id,
                "generation":self.generation.expect("begin precedes batches"),
                "source_revision":self.source_revision,
                "sequence":self.sequence,
                "expected_revision":self.catalog_revision.expect("begin supplies revision"),
                "entries":self.batches[self.batch_index].entries,
                "remove_source_ids":[],
                "expire_source_ids":[],
                "selection_remove_source_ids":self.batches[self.batch_index].selection_remove_source_ids,
            }),
            Stage::Finish => json!({
                "action":"provider_snapshot_finish",
                "sync_id":self.sync_id,
                "generation":self.generation.expect("begin precedes finish"),
                "source_revision":self.source_revision,
                "expected_revision":self.catalog_revision.expect("begin supplies revision"),
                "next_sequence":self.sequence,
                "complete":true,
                "remove_stale":self.settings.autosync_remove_uninstalled,
                "remove_selection":true,
            }),
        }
    }

    fn next_operation_id(&mut self) -> String {
        self.operation_sequence = self.operation_sequence.saturating_add(1);
        format!(
            "lutris.catalog.{}.{}",
            self.sync_id, self.operation_sequence
        )
    }

    fn restart(&mut self) {
        self.sync_id = new_sync_id(self.source_revision);
        self.generation = None;
        self.catalog_revision = None;
        self.sequence = 0;
        self.batch_index = 0;
        self.batches.clone_from(&self.base_batches);
        self.inventory.clear();
        self.inventory_total_count = None;
        self.inventory_after = None;
        self.collection_revision = None;
        self.stage = if self.name_exclusions.is_empty() {
            Stage::Begin
        } else {
            Stage::Inventory
        };
        self.call = Call::Submit;
        self.retries = 0;
        self.operation_sequence = 0;
        let operation_id = self.next_operation_id();
        self.operation_id = operation_id;
    }

    fn consume(&mut self, response: &Value) -> Result<bool, String> {
        if response.get("ok").and_then(Value::as_bool) != Some(true) {
            let code = response
                .pointer("/error/code")
                .and_then(Value::as_str)
                .unwrap_or("catalog_error");
            let message = response
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("catalog rejected the Lutris snapshot");
            if matches!(
                code,
                "revision_conflict" | "snapshot_conflict" | "sequence_conflict"
            ) && self.conflicts < MAX_CONFLICT_RESTARTS
            {
                self.conflicts += 1;
                self.restart();
                self.call = Call::Delay {
                    ticks: 1_u32
                        .checked_shl(u32::from(self.conflicts))
                        .unwrap_or(MAX_RETRY_TICKS)
                        .min(MAX_RETRY_TICKS),
                };
                return Ok(false);
            }
            return Err(format!("{code}: {message}"));
        }
        if self.stage == Stage::Inventory {
            return self.consume_inventory_page(response);
        }
        let revision = response
            .get("revision")
            .and_then(Value::as_u64)
            .ok_or("catalog response omitted revision")?;
        let generation = response
            .get("generation")
            .and_then(Value::as_u64)
            .ok_or("catalog response omitted generation")?;
        match self.stage {
            Stage::Inventory => unreachable!("inventory responses are handled separately"),
            Stage::Begin => {
                let next_sequence = response
                    .get("next_sequence")
                    .and_then(Value::as_u64)
                    .ok_or("catalog begin response omitted next_sequence")?;
                if generation == 0 || next_sequence != 0 {
                    return Err("catalog begin response is inconsistent".into());
                }
                self.generation = Some(generation);
                self.catalog_revision = Some(revision);
                self.stage = if self.batches.is_empty() {
                    Stage::Finish
                } else {
                    Stage::Batch
                };
            }
            Stage::Batch => {
                let next_sequence = self
                    .sequence
                    .checked_add(1)
                    .ok_or("catalog snapshot sequence exhausted")?;
                if Some(generation) != self.generation
                    || revision < self.catalog_revision.unwrap_or(u64::MAX)
                    || response.get("next_sequence").and_then(Value::as_u64) != Some(next_sequence)
                {
                    return Err("catalog batch response is inconsistent".into());
                }
                self.catalog_revision = Some(revision);
                self.sequence = next_sequence;
                self.batch_index += 1;
                self.stage = if self.batch_index == self.batches.len() {
                    Stage::Finish
                } else {
                    Stage::Batch
                };
            }
            Stage::Finish => {
                if Some(generation) != self.generation
                    || revision < self.catalog_revision.unwrap_or(u64::MAX)
                    || response.get("complete").and_then(Value::as_bool) != Some(true)
                    || response.get("cleanup_pending").and_then(Value::as_bool) != Some(false)
                    || response.get("remove_stale").and_then(Value::as_bool)
                        != Some(self.settings.autosync_remove_uninstalled)
                    || response.get("remove_selection").and_then(Value::as_bool) != Some(true)
                {
                    return Err("catalog finish response is inconsistent".into());
                }
                return Ok(true);
            }
        }
        self.call = Call::Submit;
        self.retries = 0;
        let operation_id = self.next_operation_id();
        self.operation_id = operation_id;
        Ok(false)
    }

    fn consume_inventory_page(&mut self, response: &Value) -> Result<bool, String> {
        let collection_revision = response
            .get("collection_revision")
            .and_then(Value::as_u64)
            .ok_or("catalog inventory omitted collection_revision")?;
        if self
            .collection_revision
            .is_some_and(|current| current != collection_revision)
        {
            return self.restart_after_inventory_conflict();
        }
        let total_count = response
            .get("total_count")
            .and_then(Value::as_u64)
            .and_then(|count| usize::try_from(count).ok())
            .filter(|count| *count <= MAX_INVENTORY_ENTRIES)
            .ok_or("catalog inventory total_count is invalid")?;
        if self
            .inventory_total_count
            .is_some_and(|previous| previous != total_count)
        {
            return self.restart_after_inventory_conflict();
        }
        let entries = response
            .get("entries")
            .and_then(Value::as_array)
            .ok_or("catalog inventory omitted entries")?;
        if entries.len() > INVENTORY_PAGE_SIZE
            || self.inventory.len().saturating_add(entries.len()) > total_count
        {
            return Err("catalog inventory page exceeded its declared bounds".into());
        }
        let mut previous = self.inventory_after.clone().unwrap_or_default();
        for value in entries {
            let entry: CatalogInventoryEntry = serde_json::from_value(value.clone())
                .map_err(|_| "catalog inventory entry is malformed")?;
            let valid_source_id = entry
                .source_id
                .parse::<i64>()
                .ok()
                .is_some_and(|id| id > 0 && id.to_string() == entry.source_id);
            if entry.source_id.len() > 96
                || !valid_source_id
                || entry.source_id <= previous
                || entry
                    .added_at_unix
                    .is_some_and(|timestamp| !(0..=253_402_300_799).contains(&timestamp))
                || (entry.attached
                    && entry
                        .entry_name
                        .as_deref()
                        .is_none_or(|name| name.trim().is_empty() || !printable_text(name, 160)))
                || (!entry.attached && entry.entry_name.is_some())
            {
                return Err("catalog inventory entry is invalid or unsorted".into());
            }
            previous = entry.source_id.clone();
            self.inventory.push(entry);
        }
        self.collection_revision = Some(collection_revision);
        self.inventory_total_count = Some(total_count);
        match response.get("next_after") {
            Some(Value::String(next_after)) => {
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
                self.inventory_after = Some(next_after.clone());
            }
            Some(Value::Null) => {
                if self.inventory.len() != total_count {
                    return Err("catalog inventory ended before total_count".into());
                }
                self.add_name_exclusion_removals()?;
                self.inventory_after = None;
                self.stage = Stage::Begin;
            }
            _ => return Err("catalog inventory next_after is malformed".into()),
        }
        self.call = Call::Submit;
        let operation_id = self.next_operation_id();
        self.operation_id = operation_id;
        Ok(false)
    }

    fn add_name_exclusion_removals(&mut self) -> Result<(), String> {
        let already_in_snapshot = self
            .base_batches
            .iter()
            .flat_map(|batch| {
                batch
                    .entries
                    .iter()
                    .filter_map(|entry| entry.get("source_id").and_then(Value::as_str))
                    .chain(batch.selection_remove_source_ids.iter().map(String::as_str))
            })
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        let removals = self
            .inventory
            .iter()
            .filter(|entry| {
                entry.attached
                    && !already_in_snapshot.contains(&entry.source_id)
                    && entry.entry_name.as_deref().is_some_and(|name| {
                        self.name_exclusions
                            .iter()
                            .any(|excluded| excluded.eq_ignore_ascii_case(name))
                    })
            })
            .map(|entry| entry.source_id.clone())
            .collect::<Vec<_>>();
        self.batches.clone_from(&self.base_batches);
        append_selection_removals(&mut self.batches, removals)
    }

    fn restart_after_inventory_conflict(&mut self) -> Result<bool, String> {
        if self.conflicts >= MAX_CONFLICT_RESTARTS {
            return Err("catalog inventory revision kept changing".into());
        }
        self.conflicts += 1;
        self.restart();
        Ok(false)
    }
}

#[derive(Default)]
pub struct CatalogSync {
    transaction: Option<Transaction>,
    last_error: Option<String>,
    last_import_count: usize,
    skipped_steam_count: usize,
}

impl CatalogSync {
    pub fn start(
        &mut self,
        snapshot: &Snapshot,
        settings: &ProductSettings,
        steam_app_ids: &BTreeSet<u32>,
        source_revision: u64,
    ) -> Result<usize, String> {
        if self.transaction.is_some() {
            return Err("catalog_sync_busy".into());
        }
        let prepared = prepare_snapshot(snapshot, settings, steam_app_ids)?;
        let count = prepared.entries.len();
        let batches = batch_snapshot(prepared)?;
        let name_exclusions = settings
            .exclude_games
            .iter()
            .filter_map(|exclusion| match exclusion {
                crate::settings::Exclusion::Metadata(metadata) if !metadata.name.is_empty() => {
                    Some(metadata.name.clone())
                }
                _ => None,
            })
            .collect();
        self.last_import_count = count;
        self.skipped_steam_count = snapshot
            .games
            .iter()
            .filter(|game| {
                game.steam_backed()
                    && settings.include_steam
                    && game
                        .steam_app_id()
                        .is_some_and(|app_id| steam_app_ids.contains(&app_id))
            })
            .count();
        self.last_error = None;
        self.transaction = Some(Transaction::new(
            source_revision,
            settings.clone(),
            batches,
            name_exclusions,
        ));
        Ok(count)
    }

    pub fn advance(&mut self, runtime: &RuntimeClient) -> ModuleResult<()> {
        let Some(mut transaction) = self.transaction.take() else {
            return Ok(());
        };
        match transaction.call {
            Call::Delay { ticks } if ticks > 1 => {
                transaction.call = Call::Delay { ticks: ticks - 1 };
                self.transaction = Some(transaction);
                Ok(())
            }
            Call::Delay { .. } => {
                transaction.call = Call::Submit;
                self.transaction = Some(transaction);
                Ok(())
            }
            Call::Submit => self.submit(runtime, transaction),
            Call::Waiting { request } => self.take_result(runtime, transaction, request),
        }
    }

    pub fn busy(&self) -> bool {
        self.transaction.is_some()
    }

    pub fn succeeded(&self) -> bool {
        self.transaction.is_none() && self.last_error.is_none()
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn respond_for_test(&mut self, payload: Value) {
        let transaction = self
            .transaction
            .take()
            .expect("catalog response requires an in-flight transaction");
        self.consume_response(transaction, payload).unwrap();
    }

    #[cfg(test)]
    pub(crate) fn pending_payload_for_test(&self) -> Value {
        self.transaction
            .as_ref()
            .expect("catalog payload requires an in-flight transaction")
            .payload()
    }

    pub fn status(&self) -> Value {
        json!({
            "busy":self.busy(),
            "last_error":self.last_error,
            "last_import_count":self.last_import_count,
            "skipped_steam_count":self.skipped_steam_count,
        })
    }

    fn submit(
        &mut self,
        runtime: &RuntimeClient,
        mut transaction: Transaction,
    ) -> ModuleResult<()> {
        let payload = transaction.payload();
        let operation_id = transaction.operation_id.clone();
        match runtime.send(Command::Request {
            service: SERVICE.into(),
            payload,
            timeout_ms: TIMEOUT_MS,
            operation_id,
            parent: None,
        }) {
            Ok(Response::Accepted { id }) => {
                transaction.call = Call::Waiting { request: id };
                self.transaction = Some(transaction);
                Ok(())
            }
            Ok(Response::Result { payload }) => self.consume_response(transaction, payload),
            Ok(Response::Error { code, message }) => {
                self.retry_or_fail(transaction, format!("{code}: {message}"));
                Ok(())
            }
            Ok(response) => {
                self.retry_or_fail(
                    transaction,
                    format!("unexpected catalog response: {response:?}"),
                );
                Ok(())
            }
            Err(_) => {
                self.retry_or_fail(transaction, "catalog request submission failed".into());
                Ok(())
            }
        }
    }

    fn take_result(
        &mut self,
        runtime: &RuntimeClient,
        mut transaction: Transaction,
        request: u64,
    ) -> ModuleResult<()> {
        let response = match runtime.send(Command::TakeResult { request }) {
            Ok(Response::Pending) | Err(_) => {
                transaction.call = Call::Waiting { request };
                self.transaction = Some(transaction);
                return Ok(());
            }
            Ok(response) => response,
        };
        let Some(response) = release_terminal_child_result(request, response, |request| {
            runtime.send(Command::ReleaseResult { request })
        }) else {
            transaction.call = Call::Waiting { request };
            self.transaction = Some(transaction);
            return Ok(());
        };
        match response {
            Response::Result { payload } => self.consume_response(transaction, payload),
            Response::Error { code, message } => {
                self.retry_or_fail(transaction, format!("{code}: {message}"));
                Ok(())
            }
            other => {
                self.retry_or_fail(
                    transaction,
                    format!("unexpected catalog response: {other:?}"),
                );
                Ok(())
            }
        }
    }

    fn consume_response(
        &mut self,
        mut transaction: Transaction,
        payload: Value,
    ) -> ModuleResult<()> {
        match transaction.consume(&payload) {
            Ok(true) => {
                self.last_error = None;
                self.transaction = None;
            }
            Ok(false) => self.transaction = Some(transaction),
            Err(error) => {
                self.last_error = Some(error);
                self.transaction = None;
            }
        }
        Ok(())
    }

    fn retry_or_fail(&mut self, mut transaction: Transaction, error: String) {
        transaction.retries = transaction.retries.saturating_add(1);
        if transaction.retries > MAX_RETRIES {
            self.last_error = Some(error);
            self.transaction = None;
            return;
        }
        let ticks = 1_u32
            .checked_shl(u32::from(transaction.retries.min(6)))
            .unwrap_or(MAX_RETRY_TICKS)
            .min(MAX_RETRY_TICKS);
        transaction.call = Call::Delay { ticks };
        self.transaction = Some(transaction);
    }
}

#[cfg(test)]
fn prepare_entries(
    snapshot: &Snapshot,
    settings: &ProductSettings,
    steam_app_ids: &BTreeSet<u32>,
) -> Result<Vec<Value>, String> {
    Ok(prepare_snapshot(snapshot, settings, steam_app_ids)?.entries)
}

fn prepare_snapshot(
    snapshot: &Snapshot,
    settings: &ProductSettings,
    steam_app_ids: &BTreeSet<u32>,
) -> Result<PreparedSnapshot, String> {
    let executable = snapshot
        .lutris_executable
        .to_str()
        .filter(|path| Path::new(path).is_absolute())
        .ok_or_else(|| "lutris_executable_path_is_not_utf8_absolute".to_owned())?;
    let launch_arguments = |id: i64| match snapshot.launch_kind {
        crate::discovery::LaunchKind::Native => vec![format!("lutris:rungameid/{id}")],
        crate::discovery::LaunchKind::FlatpakUser => vec![
            "run".to_owned(),
            "--user".to_owned(),
            "--branch=stable".to_owned(),
            format!("--arch={}", std::env::consts::ARCH),
            "net.lutris.Lutris".to_owned(),
            format!("lutris:rungameid/{id}"),
        ],
    };
    let mut entries = Vec::new();
    let mut selection_remove_source_ids = Vec::new();
    for game in &snapshot.games {
        let source_id = game.id.to_string();
        if game.id <= 0 {
            return Err(format!("Lutris game {} has an invalid source ID", game.id));
        }
        if settings
            .exclude_games
            .iter()
            .any(|value| value.matches(game.id, &game.name))
        {
            selection_remove_source_ids.push(source_id);
            continue;
        }
        if game.steam_backed() {
            if !settings.include_steam {
                selection_remove_source_ids.push(source_id);
                continue;
            }
            let app_id = game.steam_app_id().ok_or_else(|| {
                format!("Lutris Steam entry {} has no valid Steam app ID", game.id)
            })?;
            if steam_app_ids.contains(&app_id) {
                selection_remove_source_ids.push(source_id);
                continue;
            }
        }
        let mut entry = json!({
            "name":game.name,
            "description":format!("Imported from Lutris{}", runner_description(&game.runner)),
            "tags":["Lutris"],
            "launch":{
                "kind":"executable",
                "program":executable,
                "arguments":launch_arguments(game.id),
                "working_directory":null,
                "auto_detach":true,
                "wait_all":false,
            }
        });
        if let Some(revision) = snapshot.artwork_revisions.get(&game.id) {
            entry["artwork"] = json!({
                "provider":"lutris","game_id":game.id,"revision":revision
            });
        }
        validate_catalog_entry(&entry, executable, &launch_arguments(game.id))
            .map_err(|error| format!("Lutris entry {} is not catalog-safe: {error}", game.id))?;
        entries.push(json!({
            "source_id":source_id,
            "entry":entry,
        }));
    }
    for exclusion in &settings.exclude_games {
        let excluded_id = match exclusion {
            crate::settings::Exclusion::Id(id) => Some(id),
            crate::settings::Exclusion::Metadata(metadata) if !metadata.id.is_empty() => {
                Some(&metadata.id)
            }
            crate::settings::Exclusion::Metadata(_) => None,
        };
        if let Some(source_id) = excluded_id {
            selection_remove_source_ids.push(source_id.clone());
        }
    }
    selection_remove_source_ids.sort();
    selection_remove_source_ids.dedup();
    Ok(PreparedSnapshot {
        entries,
        selection_remove_source_ids,
    })
}

fn runner_description(runner: &str) -> String {
    if runner.is_empty() {
        String::new()
    } else {
        format!(" ({runner})")
    }
}

fn validate_catalog_entry(
    entry: &Value,
    executable: &str,
    expected_arguments: &[String],
) -> Result<(), &'static str> {
    let name = entry
        .get("name")
        .and_then(Value::as_str)
        .ok_or("catalog entry has no name")?;
    let description = entry
        .get("description")
        .and_then(Value::as_str)
        .ok_or("catalog entry has no description")?;
    let tags = entry
        .get("tags")
        .and_then(Value::as_array)
        .ok_or("catalog entry has no tags")?;
    let arguments = entry
        .pointer("/launch/arguments")
        .and_then(Value::as_array)
        .ok_or("catalog entry has no launch arguments")?;
    let launch_path = entry
        .pointer("/launch/program")
        .and_then(Value::as_str)
        .ok_or("catalog entry has no launch program")?;
    if name.trim().is_empty()
        || !printable_text(name, 160)
        || !printable_text(description, 1024)
        || tags.len() > 16
        || tags.iter().any(|tag| {
            tag.as_str()
                .is_none_or(|value| value.trim().is_empty() || !printable_text(value, 64))
        })
        || !Path::new(launch_path).is_absolute()
        || launch_path != executable
        || !printable_text(launch_path, 2048)
        || arguments.len() != expected_arguments.len()
        || arguments
            .iter()
            .zip(expected_arguments)
            .any(|(actual, expected)| actual.as_str() != Some(expected))
        || arguments.iter().any(|argument| {
            argument
                .as_str()
                .is_none_or(|value| !printable_text(value, 1024))
        })
        || serde_json::to_vec(entry).map_or(true, |bytes| bytes.len() > 11_950)
    {
        return Err("catalog entry violates bounded launch metadata rules");
    }
    Ok(())
}

fn printable_text(value: &str, max_bytes: usize) -> bool {
    value.len() <= max_bytes && !value.chars().any(char::is_control)
}

fn batch_snapshot(prepared: PreparedSnapshot) -> Result<Vec<SnapshotBatch>, String> {
    let mut batches = Vec::new();
    let mut batch = SnapshotBatch::default();
    let mut source_ids = BTreeSet::new();

    for entry in prepared.entries {
        let source_id = entry
            .get("source_id")
            .and_then(Value::as_str)
            .ok_or_else(|| "Lutris catalog entry omitted its source ID".to_owned())?;
        if !source_ids.insert(source_id.to_owned()) {
            return Err("Lutris scan repeated a catalog source ID".into());
        }
        let mut candidate = batch.clone();
        candidate.entries.push(entry);
        if snapshot_batch_fits(&candidate) {
            batch = candidate;
        } else {
            if batch.is_empty() {
                return Err("Lutris catalog entry exceeds the batch payload limit".into());
            }
            batches.push(std::mem::take(&mut batch));
            batch
                .entries
                .push(candidate.entries.pop().expect("entry was just added"));
            if !snapshot_batch_fits(&batch) {
                return Err("Lutris catalog entry exceeds the batch payload limit".into());
            }
        }
    }

    for source_id in prepared.selection_remove_source_ids {
        if !source_ids.insert(source_id.clone()) {
            return Err("Lutris scan repeated a catalog source ID".into());
        }
        let mut candidate = batch.clone();
        candidate.selection_remove_source_ids.push(source_id);
        if snapshot_batch_fits(&candidate) {
            batch = candidate;
        } else {
            if batch.is_empty() {
                return Err("Lutris cleanup intent exceeds the batch payload limit".into());
            }
            batches.push(std::mem::take(&mut batch));
            batch.selection_remove_source_ids.push(
                candidate
                    .selection_remove_source_ids
                    .pop()
                    .expect("intent was just added"),
            );
            if !snapshot_batch_fits(&batch) {
                return Err("Lutris cleanup intent exceeds the batch payload limit".into());
            }
        }
    }

    if !batch.is_empty() {
        batches.push(batch);
    }
    Ok(batches)
}

fn snapshot_batch_fits(batch: &SnapshotBatch) -> bool {
    batch.intent_count() <= MAX_BATCH_ENTRIES
        && serde_json::to_vec(&json!({
            "entries":batch.entries,
            "remove_source_ids":[],
            "expire_source_ids":[],
            "selection_remove_source_ids":batch.selection_remove_source_ids,
        }))
        .is_ok_and(|bytes| bytes.len() <= MAX_BATCH_BYTES)
}

fn append_selection_removals(
    batches: &mut Vec<SnapshotBatch>,
    source_ids: impl IntoIterator<Item = String>,
) -> Result<(), String> {
    let mut seen = batches
        .iter()
        .flat_map(|batch| {
            batch
                .entries
                .iter()
                .filter_map(|entry| entry.get("source_id").and_then(Value::as_str))
                .chain(batch.selection_remove_source_ids.iter().map(String::as_str))
        })
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();

    for source_id in source_ids {
        if !seen.insert(source_id.clone()) {
            continue;
        }
        let mut candidate = batches.last().cloned().unwrap_or_default();
        candidate
            .selection_remove_source_ids
            .push(source_id.clone());
        if snapshot_batch_fits(&candidate) {
            if let Some(last) = batches.last_mut() {
                *last = candidate;
            } else {
                batches.push(candidate);
            }
            continue;
        }

        let mut batch = SnapshotBatch::default();
        batch.selection_remove_source_ids.push(source_id);
        if !snapshot_batch_fits(&batch) {
            return Err("Lutris cleanup intent exceeds the batch payload limit".into());
        }
        batches.push(batch);
    }
    Ok(())
}

fn new_sync_id(source_revision: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let sequence = SYNC_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("lutris-{source_revision}-{now:x}-{sequence:x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use crate::artwork::ArtworkStore;
    use crate::discovery::Game;
    use crate::settings::{Exclusion, ExclusionMetadata};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn game(id: i64, name: &str, runner: &str, service: &str, service_id: &str) -> Game {
        Game {
            id,
            name: name.into(),
            slug: name.to_lowercase().replace(' ', "-"),
            runner: runner.into(),
            platform: "linux".into(),
            directory: "/games/example".into(),
            config_path: "/games/example.yml".into(),
            service: service.into(),
            service_id: service_id.into(),
        }
    }

    fn snapshot(games: Vec<Game>) -> Snapshot {
        Snapshot {
            database_path: PathBuf::from("/home/user/.local/share/lutris/pga.db"),
            lutris_executable: test_lutris_path(),
            launch_kind: crate::discovery::LaunchKind::Native,
            games,
            artwork_revisions: Default::default(),
            artwork_scan_incomplete: false,
            artwork_scanned_games: 0,
        }
    }

    fn test_lutris_path() -> PathBuf {
        if cfg!(target_os = "windows") {
            std::env::temp_dir().join("lutris.exe")
        } else {
            PathBuf::from("/usr/bin/lutris")
        }
    }

    #[test]
    fn incomplete_artwork_scan_preserves_unvisited_references_only() {
        let mut source = snapshot(vec![
            game(10, "First", "wine", "", ""),
            game(11, "Second", "wine", "", ""),
        ]);
        let prior = BTreeMap::from([(10, "a".repeat(64)), (11, "b".repeat(64))]);
        source.artwork_scan_incomplete = true;
        source.artwork_scanned_games = 1;
        // The first game was visited and its missing image must clear.
        crate::preserve_unscanned_artwork(&mut source, &prior);
        let entries =
            prepare_entries(&source, &ProductSettings::default(), &BTreeSet::new()).unwrap();
        assert!(entries[0]["entry"].get("artwork").is_none());
        assert_eq!(entries[1]["entry"]["artwork"]["revision"], "b".repeat(64));

        source.artwork_scan_incomplete = false;
        source.artwork_scanned_games = source.games.len();
        source.artwork_revisions.clear();
        crate::preserve_unscanned_artwork(&mut source, &prior);
        let complete =
            prepare_entries(&source, &ProductSettings::default(), &BTreeSet::new()).unwrap();
        assert!(
            complete
                .iter()
                .all(|record| record["entry"].get("artwork").is_none())
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn artwork_digest_flows_from_discovery_to_catalog_and_signed_read() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("amaterasu-lutris-catalog-art-{nonce}"));
        let data = root.join("lutris");
        let cover = data.join("coverart/example.png");
        std::fs::create_dir_all(cover.parent().unwrap()).unwrap();
        std::fs::write(&cover, [b"\x89PNG\r\n\x1a\n".as_slice(), b"first"].concat()).unwrap();
        let mut source = snapshot(vec![game(10, "Example", "wine", "", "")]);
        source.database_path = data.join("pga.db");
        source.artwork_revisions = ArtworkStore::revisions(&source).revisions;
        let first = source.artwork_revisions[&10].clone();
        assert_eq!(first.len(), 64);
        let settings = ProductSettings::default();
        let entries = prepare_entries(&source, &settings, &BTreeSet::new()).unwrap();
        assert_eq!(
            entries[0]["entry"]["artwork"],
            json!({"provider":"lutris","game_id":10,"revision":first})
        );
        assert_eq!(entries[0]["source_id"], "10");
        let mut store = ArtworkStore::default();
        store.update(&source);
        assert_eq!(store.read(10, &first, 0).unwrap()["revision"], first);

        std::fs::write(&cover, [b"\x89PNG\r\n\x1a\n".as_slice(), b"later"].concat()).unwrap();
        source.artwork_revisions = ArtworkStore::revisions(&source).revisions;
        let second = source.artwork_revisions[&10].clone();
        assert_ne!(first, second);
        assert_eq!(
            prepare_entries(&source, &settings, &BTreeSet::new()).unwrap()[0]["entry"]["artwork"]["revision"],
            second
        );
        assert_eq!(store.read(10, &first, 0), Err("artwork_changed"));
        assert_eq!(store.read(10, &second, 0).unwrap()["revision"], second);

        let excluded = ProductSettings {
            exclude_games: vec![crate::settings::Exclusion::Id("10".into())],
            ..settings
        };
        assert!(
            prepare_entries(&source, &excluded, &BTreeSet::new())
                .unwrap()
                .is_empty()
        );
        std::fs::remove_file(cover).unwrap();
        source.artwork_revisions = ArtworkStore::revisions(&source).revisions;
        assert!(prepare_entries(&source, &ProductSettings::default(), &BTreeSet::new()).unwrap()[0]["entry"].get("artwork").is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    fn respond_to_catalog(catalog: &mut CatalogSync, payload: Value) {
        let transaction = catalog
            .transaction
            .take()
            .expect("catalog response requires an in-flight transaction");
        catalog.consume_response(transaction, payload).unwrap();
    }

    #[test]
    fn creates_native_lutris_uri_launch_entries_and_skips_steam_by_default() {
        let games = vec![
            game(7, "Example Game", "wine", "", ""),
            game(42, "Steam Copy", "steam", "steam", "9001"),
        ];
        let entries = prepare_entries(
            &snapshot(games),
            &ProductSettings::default(),
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["source_id"], "7");
        assert_eq!(
            entries[0]["entry"]["launch"]["program"],
            test_lutris_path().to_string_lossy().as_ref()
        );
        assert_eq!(
            entries[0]["entry"]["launch"]["arguments"],
            json!(["lutris:rungameid/7"])
        );
        assert_eq!(entries[0]["entry"]["launch"]["auto_detach"], true);
        assert_eq!(entries[0]["entry"]["launch"]["wait_all"], false);
    }

    #[test]
    fn flatpak_only_snapshot_projects_fixed_user_scoped_argv_without_touching_other_rows() {
        let mut source = snapshot(vec![
            game(7, "Example", "wine", "", ""),
            game(42, "Steam Copy", "steam", "steam", "9001"),
        ]);
        source.database_path =
            PathBuf::from("/home/user/.var/app/net.lutris.Lutris/data/lutris/pga.db");
        source.lutris_executable = if cfg!(target_os = "windows") {
            std::env::temp_dir().join("flatpak.exe")
        } else {
            PathBuf::from("/usr/bin/flatpak")
        };
        source.launch_kind = crate::discovery::LaunchKind::FlatpakUser;
        let entries =
            prepare_entries(&source, &ProductSettings::default(), &BTreeSet::new()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["source_id"], "7");
        assert_eq!(
            entries[0]["entry"]["launch"]["program"],
            source.lutris_executable.to_str().unwrap()
        );
        assert_eq!(
            entries[0]["entry"]["launch"]["arguments"],
            json!([
                "run",
                "--user",
                "--branch=stable",
                format!("--arch={}", std::env::consts::ARCH),
                "net.lutris.Lutris",
                "lutris:rungameid/7"
            ])
        );
        assert_eq!(entries[0]["entry"]["launch"]["auto_detach"], true);
        assert_eq!(entries[0]["entry"]["launch"]["wait_all"], false);
        let excluded = ProductSettings {
            exclude_games: vec![crate::settings::Exclusion::Id("7".into())],
            ..ProductSettings::default()
        };
        assert!(
            prepare_entries(&source, &excluded, &BTreeSet::new())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn fallback_client_reconciles_a_stable_game_only_when_unambiguous() {
        use std::time::{Instant, SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("amaterasu-lutris-client-{nonce}"));
        let normal = root.join("normal");
        let system = root.join("system");
        std::fs::create_dir_all(&normal).unwrap();
        std::fs::create_dir_all(&system).unwrap();
        let fallback = system.join("lutris");
        let second = system.join("other-lutris");
        std::fs::write(&fallback, b"client").unwrap();
        std::fs::write(&second, b"different client").unwrap();
        let verify = |candidate: &Path| {
            candidate
                .is_file()
                .then(|| candidate.canonicalize().unwrap())
        };
        let choose = |candidates: &[PathBuf]| {
            crate::discovery::select_lutris_executable(
                std::slice::from_ref(&normal),
                candidates,
                &verify,
                &verify,
            )
        };

        let selected = choose(std::slice::from_ref(&fallback)).unwrap();
        let mut source = snapshot(vec![game(7, "Example Game", "wine", "", "")]);
        source.lutris_executable = selected.clone();
        let entry = prepare_entries(&source, &ProductSettings::default(), &BTreeSet::new())
            .unwrap()
            .remove(0);
        assert_eq!(entry["source_id"], "7");
        assert_eq!(
            entry["entry"]["launch"]["program"],
            selected.to_str().unwrap()
        );
        assert_eq!(
            entry["entry"]["launch"]["arguments"],
            json!(["lutris:rungameid/7"])
        );
        assert_eq!(
            choose(std::slice::from_ref(&fallback)),
            Some(selected.clone())
        );
        assert_eq!(choose(&[fallback.clone(), second]), None);
        std::fs::remove_file(&fallback).unwrap();
        assert_eq!(choose(std::slice::from_ref(&fallback)), None);

        let disabled = ProductSettings {
            enabled: false,
            ..ProductSettings::default()
        };
        let now = Instant::now();
        assert!(!crate::scan_is_due(&disabled, true, Some(now), now));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn include_steam_keeps_only_records_missing_from_direct_steam_provider() {
        let settings = ProductSettings {
            include_steam: true,
            ..ProductSettings::default()
        };
        let games = vec![
            game(10, "Direct Steam", "steam", "steam", "9001"),
            game(11, "Lutris Steam", "steam", "steam", "9002"),
            game(12, "Native Lutris", "wine", "", ""),
        ];
        let entries = prepare_entries(&snapshot(games), &settings, &[9001].into()).unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|value| value["source_id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["11", "12"]
        );
    }

    #[test]
    fn exclusions_match_numeric_ids_or_case_insensitive_names() {
        let settings = ProductSettings {
            exclude_games: vec![Exclusion::Metadata(ExclusionMetadata {
                id: "7".into(),
                name: "EXCLUDED BY NAME".into(),
            })],
            ..ProductSettings::default()
        };
        let entries = prepare_entries(
            &snapshot(vec![
                game(7, "Different Name", "wine", "", ""),
                game(8, "Excluded By Name", "wine", "", ""),
                game(9, "Keep", "wine", "", ""),
            ]),
            &settings,
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["source_id"], "9");
    }

    #[test]
    fn include_steam_requires_a_valid_app_id_before_catalog_mutation() {
        let settings = ProductSettings {
            include_steam: true,
            ..ProductSettings::default()
        };
        assert!(
            prepare_entries(
                &snapshot(vec![game(7, "Broken Steam", "steam", "steam", "not-an-id")]),
                &settings,
                &BTreeSet::new()
            )
            .is_err()
        );
    }

    #[test]
    fn catalog_finish_honors_stale_removal_setting() {
        let captured_settings = ProductSettings {
            autosync_remove_uninstalled: false,
            ..ProductSettings::default()
        };
        let mut transaction = Transaction::new(2, captured_settings, Vec::new(), Vec::new());
        transaction.generation = Some(3);
        transaction.catalog_revision = Some(8);
        transaction.stage = Stage::Finish;
        let payload = transaction.payload();
        assert_eq!(payload["complete"], true);
        assert_eq!(payload["remove_stale"], false);
        assert_eq!(payload["remove_selection"], true);
    }

    #[test]
    fn in_flight_snapshot_finishes_once_after_disable_without_stale_cleanup() {
        let scan_start_settings = ProductSettings {
            enabled: true,
            auto_sync: false,
            autosync_remove_uninstalled: false,
            include_steam: true,
            exclude_games: Vec::new(),
        };
        let settings_after_disable = ProductSettings {
            enabled: false,
            auto_sync: true,
            autosync_remove_uninstalled: true,
            include_steam: false,
            exclude_games: vec![Exclusion::Id("10".into())],
        };
        assert!(!settings_after_disable.enabled);
        assert!(settings_after_disable.autosync_remove_uninstalled);
        assert!(!settings_after_disable.include_steam);
        assert_eq!(
            settings_after_disable.exclude_games,
            vec![Exclusion::Id("10".into())]
        );

        let mut catalog = CatalogSync::default();
        assert_eq!(
            catalog
                .start(
                    &snapshot(vec![
                        game(7, "Direct Steam copy", "steam", "steam", "9001"),
                        game(8, "Unshadowed Steam", "steam", "steam", "9002"),
                        game(10, "Native Lutris", "wine", "", ""),
                    ]),
                    &scan_start_settings,
                    &[9001].into(),
                    5,
                )
                .unwrap(),
            2
        );

        let transaction = catalog.transaction.as_ref().unwrap();
        assert_eq!(transaction.settings, scan_start_settings);
        assert_eq!(transaction.source_revision, 5);
        assert_eq!(transaction.stage, Stage::Begin);
        assert_eq!(transaction.batches.len(), 1);
        assert_eq!(
            transaction.batches[0]
                .entries
                .iter()
                .map(|entry| entry["source_id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["8", "10"]
        );
        assert_eq!(transaction.batches[0].selection_remove_source_ids, ["7"]);
        assert!(
            !transaction.batches[0]
                .selection_remove_source_ids
                .iter()
                .any(|source_id| source_id == "99" || source_id == "10")
        );

        respond_to_catalog(
            &mut catalog,
            json!({"ok":true,"revision":8,"generation":3,"next_sequence":0}),
        );
        let batch = catalog.transaction.as_ref().unwrap().payload();
        assert_eq!(batch["action"], "provider_snapshot_batch");
        assert_eq!(batch["remove_source_ids"], json!([]));
        assert_eq!(batch["expire_source_ids"], json!([]));
        assert_eq!(batch["selection_remove_source_ids"], json!(["7"]));
        respond_to_catalog(
            &mut catalog,
            json!({"ok":true,"revision":9,"generation":3,"next_sequence":1}),
        );
        let finish = catalog.transaction.as_ref().unwrap().payload();
        assert_eq!(finish["complete"], true);
        assert_eq!(finish["remove_stale"], false);
        assert_eq!(finish["remove_selection"], true);

        respond_to_catalog(
            &mut catalog,
            json!({
                "ok":true,
                "revision":10,
                "generation":3,
                "complete":true,
                "cleanup_pending":false,
                "remove_stale":false,
                "remove_selection":true,
            }),
        );
        assert!(!catalog.busy());
        assert!(catalog.succeeded());
        assert_eq!(catalog.last_error, None);
        let now = std::time::Instant::now();
        assert!(!crate::scan_is_due(
            &settings_after_disable,
            true,
            Some(now),
            now,
        ));
    }

    #[test]
    fn catalog_batches_are_bounded_to_service_contract() {
        let entries = (1..=19)
            .map(|id| json!({"source_id":id.to_string(),"entry":{"name":"Example"}}))
            .collect();
        let batches = batch_snapshot(PreparedSnapshot {
            entries,
            selection_remove_source_ids: (20..=27).map(|id| id.to_string()).collect(),
        })
        .unwrap();
        assert!(
            batches
                .iter()
                .all(|batch| batch.intent_count() <= MAX_BATCH_ENTRIES)
        );
        assert!(batches.iter().all(snapshot_batch_fits));
    }

    #[test]
    fn disabled_stale_removal_emits_selection_cleanup_but_preserves_absent_source_ids() {
        let settings = ProductSettings {
            autosync_remove_uninstalled: false,
            exclude_games: vec![Exclusion::Id("2".into())],
            ..ProductSettings::default()
        };
        let prepared = prepare_snapshot(
            &snapshot(vec![
                game(1, "Keep", "wine", "", ""),
                game(2, "Excluded", "wine", "", ""),
                game(3, "Steam filtered", "steam", "steam", "300"),
            ]),
            &settings,
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(
            prepared
                .entries
                .iter()
                .map(|entry| entry["source_id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["1"]
        );
        assert_eq!(prepared.selection_remove_source_ids, ["2", "3"]);

        // Source 88 is intentionally absent: it is not a selection change and
        // therefore remains untouched when remove_stale is disabled.
        let batches = batch_snapshot(prepared).unwrap();
        let submitted_selection_removals = batches
            .iter()
            .flat_map(|batch| batch.selection_remove_source_ids.iter().map(String::as_str))
            .collect::<Vec<_>>();
        assert_eq!(submitted_selection_removals, ["2", "3"]);
        assert!(!submitted_selection_removals.contains(&"88"));

        let mut transaction = Transaction::new(2, settings, batches, Vec::new());
        transaction.generation = Some(3);
        transaction.catalog_revision = Some(8);
        transaction.stage = Stage::Batch;
        let cleanup_batch = transaction.payload();
        assert_eq!(cleanup_batch["entries"][0]["source_id"], "1");
        assert_eq!(
            cleanup_batch["selection_remove_source_ids"],
            json!(["2", "3"])
        );

        transaction.stage = Stage::Finish;
        let finish = transaction.payload();
        assert_eq!(finish["remove_stale"], false);
        assert_eq!(finish["remove_selection"], true);
        assert!(
            transaction
                .consume(&json!({
                    "ok":true,
                    "revision":8,
                    "generation":3,
                    "complete":true,
                    "cleanup_pending":false,
                    "remove_stale":false,
                    "remove_selection":true,
                }))
                .unwrap()
        );
    }

    #[test]
    fn direct_steam_shadow_is_a_selection_removal_and_retries_are_stable() {
        let settings = ProductSettings {
            autosync_remove_uninstalled: false,
            include_steam: true,
            ..ProductSettings::default()
        };
        let prepared = prepare_snapshot(
            &snapshot(vec![
                game(4, "Direct Steam copy", "steam", "steam", "400"),
                game(5, "Lutris Steam", "steam", "steam", "500"),
            ]),
            &settings,
            &[400].into(),
        )
        .unwrap();
        assert_eq!(prepared.selection_remove_source_ids, ["4"]);
        assert_eq!(prepared.entries.len(), 1);
        assert_eq!(prepared.entries[0]["source_id"], "5");

        let batches = batch_snapshot(prepared).unwrap();
        let mut transaction = Transaction::new(7, settings, batches, Vec::new());
        transaction.generation = Some(2);
        transaction.catalog_revision = Some(9);
        transaction.stage = Stage::Batch;
        let payload_before_retry = transaction.payload();
        assert_eq!(
            payload_before_retry["selection_remove_source_ids"],
            json!(["4"])
        );
        let operation_before_retry = transaction.operation_id.clone();
        transaction.call = Call::Delay { ticks: 1 };
        assert_eq!(transaction.payload(), payload_before_retry);
        assert_eq!(transaction.operation_id, operation_before_retry);
    }

    #[test]
    fn id_exclusion_removes_a_previously_imported_row_even_if_now_absent() {
        let settings = ProductSettings {
            autosync_remove_uninstalled: false,
            exclude_games: vec![Exclusion::Id("99".into())],
            ..ProductSettings::default()
        };
        let prepared = prepare_snapshot(
            &snapshot(vec![game(1, "Present", "wine", "", "")]),
            &settings,
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(prepared.selection_remove_source_ids, ["99"]);

        let batches = batch_snapshot(prepared).unwrap();
        let submitted = batches
            .iter()
            .flat_map(|batch| batch.selection_remove_source_ids.iter().map(String::as_str))
            .collect::<Vec<_>>();
        assert_eq!(submitted, ["99"]);
    }

    #[test]
    fn paired_exclusion_name_removes_only_attached_absent_rows_at_inventory_revision() {
        let settings = ProductSettings {
            autosync_remove_uninstalled: false,
            exclude_games: vec![Exclusion::Metadata(ExclusionMetadata {
                id: "99".into(),
                name: "Legacy title".into(),
            })],
            ..ProductSettings::default()
        };
        let mut catalog = CatalogSync::default();
        catalog
            .start(
                &snapshot(vec![game(1, "Renamed title", "wine", "", "")]),
                &settings,
                &BTreeSet::new(),
                9,
            )
            .unwrap();
        let transaction = catalog.transaction.as_mut().unwrap();
        assert_eq!(
            transaction.payload()["action"],
            "provider_snapshot_inventory"
        );
        assert_eq!(transaction.payload()["include_entry_names"], true);

        assert!(!transaction
            .consume(&json!({
                "ok":true,
                "collection_revision":42,
                "total_count":3,
                "entries":[
                    {"source_id":"1","added_at_unix":1,"source_flags":0,"attached":true,"entry_name":"Legacy title"},
                    {"source_id":"2","added_at_unix":1,"source_flags":0,"attached":true,"entry_name":"LEGACY TITLE"},
                    {"source_id":"3","added_at_unix":1,"source_flags":0,"attached":false,"entry_name":null}
                ],
                "next_after":null
            }))
            .unwrap());
        assert_eq!(transaction.stage, Stage::Begin);
        let begin = transaction.payload();
        assert_eq!(begin["expected_collection_revision"], 42);

        assert!(
            !transaction
                .consume(&json!({"ok":true,"revision":8,"generation":2,"next_sequence":0}))
                .unwrap()
        );
        let batch = transaction.payload();
        assert_eq!(batch["action"], "provider_snapshot_batch");
        assert_eq!(batch["entries"][0]["source_id"], "1");
        let selection_removals = batch["selection_remove_source_ids"].as_array().unwrap();
        assert_eq!(selection_removals.len(), 2);
        assert!(selection_removals.contains(&json!("2")));
        assert!(selection_removals.contains(&json!("99")));
    }

    #[test]
    fn inventory_revision_conflict_restarts_before_applying_old_names() {
        let settings = ProductSettings {
            exclude_games: vec![Exclusion::Metadata(ExclusionMetadata {
                id: String::new(),
                name: "Legacy title".into(),
            })],
            ..ProductSettings::default()
        };
        let mut catalog = CatalogSync::default();
        catalog
            .start(
                &snapshot(vec![game(1, "Current", "wine", "", "")]),
                &settings,
                &BTreeSet::new(),
                2,
            )
            .unwrap();
        let transaction = catalog.transaction.as_mut().unwrap();
        transaction
            .consume(&json!({
                "ok":true,
                "collection_revision":7,
                "total_count":1,
                "entries":[{"source_id":"99","added_at_unix":1,"source_flags":0,"attached":true,"entry_name":"Legacy title"}],
                "next_after":null
            }))
            .unwrap();
        assert_eq!(transaction.payload()["expected_collection_revision"], 7);

        transaction
            .consume(&json!({
                "ok":false,
                "error":{"code":"revision_conflict","message":"catalog changed"}
            }))
            .unwrap();
        assert_eq!(transaction.stage, Stage::Inventory);
        assert!(transaction.inventory.is_empty());
        assert!(transaction.collection_revision.is_none());
        assert!(transaction.batches.iter().all(|batch| {
            !batch
                .selection_remove_source_ids
                .iter()
                .any(|id| id == "99")
        }));
        assert_eq!(
            transaction.payload()["action"],
            "provider_snapshot_inventory"
        );
    }
}
