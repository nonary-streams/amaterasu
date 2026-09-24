//! Reconciles complete Lutris scans into the provider's fixed catalog owner.

use crate::discovery::Snapshot;
use crate::settings::ProductSettings;
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};
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
static SYNC_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Stage {
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
    settings_epoch: u64,
    remove_stale: bool,
    entries: Vec<Vec<Value>>,
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

impl Transaction {
    fn new(
        source_revision: u64,
        settings_epoch: u64,
        remove_stale: bool,
        entries: Vec<Vec<Value>>,
    ) -> Self {
        let sync_id = new_sync_id(source_revision);
        let mut transaction = Self {
            sync_id,
            source_revision,
            settings_epoch,
            remove_stale,
            entries,
            generation: None,
            catalog_revision: None,
            sequence: 0,
            batch_index: 0,
            stage: Stage::Begin,
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
            Stage::Begin => json!({
                "action":"provider_snapshot_begin",
                "sync_id":self.sync_id,
                "source_revision":self.source_revision,
            }),
            Stage::Batch => json!({
                "action":"provider_snapshot_batch",
                "sync_id":self.sync_id,
                "generation":self.generation.expect("begin precedes batches"),
                "source_revision":self.source_revision,
                "sequence":self.sequence,
                "expected_revision":self.catalog_revision.expect("begin supplies revision"),
                "entries":self.entries[self.batch_index],
            }),
            Stage::Finish => json!({
                "action":"provider_snapshot_finish",
                "sync_id":self.sync_id,
                "generation":self.generation.expect("begin precedes finish"),
                "source_revision":self.source_revision,
                "expected_revision":self.catalog_revision.expect("begin supplies revision"),
                "next_sequence":self.sequence,
                "complete":true,
                "remove_stale":self.remove_stale,
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
        self.stage = Stage::Begin;
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
        let revision = response
            .get("revision")
            .and_then(Value::as_u64)
            .ok_or("catalog response omitted revision")?;
        let generation = response
            .get("generation")
            .and_then(Value::as_u64)
            .ok_or("catalog response omitted generation")?;
        match self.stage {
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
                self.stage = if self.entries.is_empty() {
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
                self.stage = if self.batch_index == self.entries.len() {
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
                        != Some(self.remove_stale)
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
        settings_epoch: u64,
        source_revision: u64,
    ) -> Result<usize, String> {
        if self.transaction.is_some() {
            return Err("catalog_sync_busy".into());
        }
        let entries = prepare_entries(snapshot, settings, steam_app_ids)?;
        let count = entries.len();
        let batches = batch_entries(entries)?;
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
            settings_epoch,
            settings.autosync_remove_uninstalled,
            batches,
        ));
        Ok(count)
    }

    pub fn advance(&mut self, runtime: &RuntimeClient, settings_epoch: u64) -> ModuleResult<()> {
        let Some(mut transaction) = self.transaction.take() else {
            return Ok(());
        };
        if transaction.settings_epoch != settings_epoch {
            if let Call::Waiting { request } = transaction.call {
                match runtime.send(Command::TakeResult { request }) {
                    Ok(Response::Pending) | Err(_) => {
                        transaction.call = Call::Waiting { request };
                        self.transaction = Some(transaction);
                    }
                    Ok(response) => {
                        let released =
                            release_terminal_child_result(request, response, |request| {
                                runtime.send(Command::ReleaseResult { request })
                            });
                        if released.is_none() {
                            transaction.call = Call::Waiting { request };
                            self.transaction = Some(transaction);
                        }
                    }
                }
            }
            return Ok(());
        }
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

pub fn prepare_entries(
    snapshot: &Snapshot,
    settings: &ProductSettings,
    steam_app_ids: &BTreeSet<u32>,
) -> Result<Vec<Value>, String> {
    let executable = snapshot
        .lutris_executable
        .to_str()
        .filter(|path| Path::new(path).is_absolute())
        .ok_or_else(|| "lutris_executable_path_is_not_utf8_absolute".to_owned())?;
    let mut entries = Vec::new();
    for game in &snapshot.games {
        if settings
            .exclude_games
            .iter()
            .any(|value| value.matches(game.id, &game.name))
        {
            continue;
        }
        if game.steam_backed() {
            if !settings.include_steam {
                continue;
            }
            let app_id = game.steam_app_id().ok_or_else(|| {
                format!("Lutris Steam entry {} has no valid Steam app ID", game.id)
            })?;
            if steam_app_ids.contains(&app_id) {
                continue;
            }
        }
        let entry = json!({
            "name":game.name,
            "description":format!("Imported from Lutris{}", runner_description(&game.runner)),
            "tags":["Lutris"],
            "launch":{
                "kind":"executable",
                "program":executable,
                "arguments":[format!("lutris:rungameid/{}", game.id)],
                "working_directory":null,
                "auto_detach":true,
                "wait_all":false,
            }
        });
        validate_catalog_entry(&entry, executable)
            .map_err(|error| format!("Lutris entry {} is not catalog-safe: {error}", game.id))?;
        entries.push(json!({
            "source_id":game.id.to_string(),
            "entry":entry,
        }));
    }
    Ok(entries)
}

fn runner_description(runner: &str) -> String {
    if runner.is_empty() {
        String::new()
    } else {
        format!(" ({runner})")
    }
}

fn validate_catalog_entry(entry: &Value, executable: &str) -> Result<(), &'static str> {
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
        || arguments.len() != 1
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

fn batch_entries(entries: Vec<Value>) -> Result<Vec<Vec<Value>>, String> {
    let mut batches = Vec::new();
    let mut batch = Vec::new();
    for entry in entries {
        batch.push(entry);
        if batch.len() > MAX_BATCH_ENTRIES
            || serde_json::to_vec(&batch).map_or(true, |bytes| bytes.len() > MAX_BATCH_BYTES)
        {
            let last = batch.pop().expect("the batch just received an entry");
            if batch.is_empty() {
                return Err("Lutris catalog entry exceeds the batch payload limit".into());
            }
            batches.push(std::mem::take(&mut batch));
            batch.push(last);
            if serde_json::to_vec(&batch).map_or(true, |bytes| bytes.len() > MAX_BATCH_BYTES) {
                return Err("Lutris catalog entry exceeds the batch payload limit".into());
            }
        }
    }
    if !batch.is_empty() {
        batches.push(batch);
    }
    Ok(batches)
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
    use crate::discovery::Game;
    use crate::settings::{Exclusion, ExclusionMetadata};
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
            games,
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
            exclude_games: vec![
                Exclusion::Id("7".into()),
                Exclusion::Metadata(ExclusionMetadata {
                    name: "EXCLUDED BY NAME".into(),
                    ..ExclusionMetadata::default()
                }),
            ],
            ..ProductSettings::default()
        };
        let entries = prepare_entries(
            &snapshot(vec![
                game(7, "Keep?", "wine", "", ""),
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
        let mut transaction = Transaction::new(2, 1, false, Vec::new());
        transaction.generation = Some(3);
        transaction.catalog_revision = Some(8);
        transaction.stage = Stage::Finish;
        let payload = transaction.payload();
        assert_eq!(payload["complete"], true);
        assert_eq!(payload["remove_stale"], false);
    }

    #[test]
    fn catalog_batches_are_bounded_to_service_contract() {
        let entries = (1..=19)
            .map(|id| json!({"source_id":id.to_string(),"entry":{"name":"Example"}}))
            .collect();
        let batches = batch_entries(entries).unwrap();
        assert!(batches.iter().all(|batch| batch.len() <= MAX_BATCH_ENTRIES));
        assert!(
            batches
                .iter()
                .all(|batch| serde_json::to_vec(batch).unwrap().len() <= MAX_BATCH_BYTES)
        );
    }
}
