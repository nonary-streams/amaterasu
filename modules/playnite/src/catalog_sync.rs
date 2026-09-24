//! Reconciles only complete Playnite extension snapshots into the fixed owner
//! assigned to this provider by catalog startup policy.

use crate::settings::ProductSettings;
use crate::snapshot::{Game, Snapshot, executable_launch, select_games};
use amaterasu_module_sdk::{Command, Response, RuntimeClient};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const CATALOG_SERVICE: &str = "amaterasu.catalog/1";
const REQUEST_TIMEOUT_MS: u64 = 30_000;
const MAX_BATCH_ENTRIES: usize = 8;
const MAX_BATCH_BYTES: usize = 12_000;
const MAX_CONFLICT_RESTARTS: u8 = 3;
static SYNC_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Stage {
    Begin,
    Batch,
    Finish,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Call {
    Submit { operation_id: String },
    Waiting { request: u64 },
}

struct Transaction {
    sync_id: String,
    source_revision: u64,
    settings_epoch: u64,
    entries: Vec<Vec<Value>>,
    generation: Option<u64>,
    revision: Option<u64>,
    sequence: u64,
    batch_index: usize,
    stage: Stage,
    call: Call,
    conflicts: u8,
    operation_sequence: u64,
}

impl Transaction {
    fn payload(&self) -> Value {
        match self.stage {
            Stage::Begin => json!({
                "action":"provider_snapshot_begin",
                "sync_id":self.sync_id,
                "source_revision":self.source_revision
            }),
            Stage::Batch => json!({
                "action":"provider_snapshot_batch",
                "sync_id":self.sync_id,
                "generation":self.generation.expect("begin is committed first"),
                "source_revision":self.source_revision,
                "sequence":self.sequence,
                "expected_revision":self.revision.expect("begin provides revision"),
                "entries":self.entries[self.batch_index]
            }),
            Stage::Finish => json!({
                "action":"provider_snapshot_finish",
                "sync_id":self.sync_id,
                "generation":self.generation.expect("begin is committed first"),
                "source_revision":self.source_revision,
                "expected_revision":self.revision.expect("begin provides revision"),
                "next_sequence":self.sequence,
                "complete":true,
                "remove_stale":false
            }),
        }
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
        self.stage = Stage::Begin;
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
        let revision = payload
            .get("revision")
            .and_then(Value::as_u64)
            .ok_or("catalog response omitted revision")?;
        let generation = payload
            .get("generation")
            .and_then(Value::as_u64)
            .ok_or("catalog response omitted generation")?;
        match self.stage {
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
                self.stage = if self.entries.is_empty() {
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
                self.stage = if self.batch_index == self.entries.len() {
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
}

#[derive(Default)]
pub struct CatalogSync {
    transaction: Option<Transaction>,
    source_revision: u64,
    last_error: Option<String>,
    skipped_launches: u64,
}

impl CatalogSync {
    pub fn start(
        &mut self,
        snapshot: &Snapshot,
        settings: &ProductSettings,
        settings_epoch: u64,
        now_unix: i64,
    ) -> Result<usize, String> {
        if self.transaction.is_some() {
            return Err("catalog_sync_busy".into());
        }
        let selected = select_games(snapshot, settings, now_unix);
        let (entries, skipped) = prepare_entries(&selected)?;
        self.skipped_launches = skipped;
        self.source_revision = self
            .source_revision
            .checked_add(1)
            .ok_or("Playnite source revision exhausted")?;
        let mut transaction = Transaction {
            sync_id: new_sync_id(self.source_revision),
            source_revision: self.source_revision,
            settings_epoch,
            entries,
            generation: None,
            revision: None,
            sequence: 0,
            batch_index: 0,
            stage: Stage::Begin,
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
        self.last_error = None;
        Ok(selected.len().saturating_sub(skipped as usize))
    }

    pub fn busy(&self) -> bool {
        self.transaction.is_some()
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
        let Some(mut transaction) = self.transaction.take() else {
            return Ok(());
        };
        if transaction.settings_epoch != current_settings_epoch {
            if let Call::Waiting { request } = transaction.call {
                match runtime.send_ephemeral(Command::TakeResult { request }) {
                    Ok(Response::Pending) | Err(_) => {
                        transaction.call = Call::Waiting { request };
                        self.transaction = Some(transaction);
                    }
                    _ => {}
                }
            }
            self.last_error = Some("catalog import canceled after settings changed".into());
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
                        let _ = error;
                    }
                }
            }
            Call::Waiting { request } => {
                match runtime.send_ephemeral(Command::TakeResult { request }) {
                    Ok(Response::Pending) | Err(_) => {
                        transaction.call = Call::Waiting { request };
                        self.transaction = Some(transaction);
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
            Ok(true) => self.last_error = None,
            Ok(false) => self.transaction = Some(transaction),
            Err(error) => self.last_error = Some(error),
        }
    }

    fn call_failed(&mut self, mut transaction: Transaction, error: String) {
        if transaction.conflicts < MAX_CONFLICT_RESTARTS {
            transaction.conflicts += 1;
            transaction.restart();
            self.transaction = Some(transaction);
        } else {
            self.last_error = Some(error);
        }
    }
}

fn prepare_entries(games: &[Game]) -> Result<(Vec<Vec<Value>>, u64), String> {
    let mut prepared = Vec::new();
    let mut skipped = 0u64;
    for game in games {
        let Some(value) = provider_entry(game) else {
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

fn provider_entry(game: &Game) -> Option<Value> {
    if game.id.trim().is_empty()
        || game.id.len() > 96
        || game.id.chars().any(char::is_control)
        || game.name.trim().is_empty()
        || game.name.len() > 160
        || game.name.chars().any(char::is_control)
        || !paths_fit_catalog(game)
    {
        return None;
    }
    let launch = executable_launch(game)?;
    if !launch_is_catalog_safe(&launch) {
        return None;
    }
    Some(json!({
        "source_id":game.id,
        "entry":{"name":game.name,"description":"","tags":[],"launch":launch}
    }))
}

fn launch_is_catalog_safe(launch: &Value) -> bool {
    let Some(program) = launch.get("program").and_then(Value::as_str) else {
        return false;
    };
    let Some(arguments) = launch.get("arguments").and_then(Value::as_array) else {
        return false;
    };
    if !valid_catalog_path(program)
        || arguments.len() > 32
        || arguments.iter().any(|argument| {
            argument.as_str().is_none_or(|argument| {
                argument.len() > 1_024 || argument.chars().any(char::is_control)
            })
        })
    {
        return false;
    }
    launch
        .get("working_directory")
        .is_none_or(|working_directory| {
            working_directory.is_null()
                || working_directory.as_str().is_some_and(valid_catalog_path)
        })
}

fn valid_catalog_path(path: &str) -> bool {
    path.len() <= 2_048
        && !path.is_empty()
        && !path.chars().any(char::is_control)
        && (path.starts_with('/')
            || path.starts_with("\\\\")
            || (path.len() >= 3
                && path.as_bytes()[0].is_ascii_alphabetic()
                && path.as_bytes()[1] == b':'
                && matches!(path.as_bytes()[2], b'/' | b'\\')))
        && path
            .split(['/', '\\'])
            .all(|part| part != "." && part != "..")
}

fn paths_fit_catalog(game: &Game) -> bool {
    [
        Some(game.exe.as_str()),
        Some(game.working_directory.as_str()),
        Some(game.install_directory.as_str()),
    ]
    .into_iter()
    .flatten()
    .filter(|path| !path.is_empty())
    .all(|path| {
        path.len() <= 2_048
            && path
                .split(['/', '\\'])
                .all(|part| part != "." && part != "..")
    })
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

    #[test]
    fn prepares_only_native_launches_before_a_catalog_transaction() {
        let snapshot = Snapshot {
            games: vec![
                game("supported", "A game", r"C:\Games\game.exe"),
                game("not-native", "URI game", "playnite://game/1"),
                game(&"x".repeat(97), "Long ID", r"C:\Games\other.exe"),
                game("long-name", &"x".repeat(161), r"C:\Games\large.exe"),
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
        assert_eq!(skipped, 3);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0][0]["source_id"], "supported");
        assert_eq!(batches[0][0]["entry"]["launch"]["kind"], "executable");
    }

    #[test]
    fn complete_import_finish_never_deletes_prior_provider_entries() {
        let mut sync = CatalogSync::default();
        let snapshot = Snapshot {
            games: vec![game("game-1", "Game", r"C:\Games\game.exe")],
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
        assert_eq!(transaction.payload()["action"], "provider_snapshot_begin");
        let mut transaction = sync.transaction.take().unwrap();
        transaction.stage = Stage::Finish;
        transaction.generation = Some(1);
        transaction.revision = Some(2);
        assert_eq!(transaction.payload()["remove_stale"], false);
        assert_eq!(transaction.payload()["complete"], true);
    }

    #[test]
    fn selected_entries_are_prevalidated_before_begin() {
        let large = game("large", "Game", &format!(r"C:\{}", "a".repeat(3_000)));
        assert!(provider_entry(&large).is_none());
    }

    #[test]
    fn catalog_path_and_argument_limits_fail_before_snapshot_begin() {
        let control_path = game("control", "Game", "C:\\Games\\bad\nname.exe");
        let control_argument = Game {
            args: "\"bad\nargument\"".into(),
            ..game("argument", "Game", r"C:\Games\game.exe")
        };
        let oversized_working_directory = Game {
            working_directory: format!(r"C:\{}", "a".repeat(2_050)),
            ..game("working", "Game", r"C:\Games\game.exe")
        };
        assert!(provider_entry(&control_path).is_none());
        assert!(provider_entry(&control_argument).is_none());
        assert!(provider_entry(&oversized_working_directory).is_none());
    }
}
