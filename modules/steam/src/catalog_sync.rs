//! Reconciles successful Steam discovery snapshots into the fixed Steam catalog.
use crate::Provider;
use crate::configuration::release_terminal_child_result;
use crate::discovery::Game;
use crate::settings::ProductSettings;
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};
use serde_json::{Value, json};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CATALOG_SERVICE: &str = "amaterasu.catalog/1";
const CATALOG_TIMEOUT_MS: u64 = 30_000;
const SYNC_INTERVAL: Duration = Duration::from_secs(30);
const MAX_BATCH_ENTRIES: usize = 8;
const MAX_BATCH_BYTES: usize = 12_000;
const MAX_RETRY_TICKS: u32 = 64;
const MAX_CONFLICT_RESTARTS: u8 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Stage {
    Begin,
    Batch,
    Finish,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CallState {
    Submit,
    Waiting { request: u64 },
}

struct Transaction {
    sync_id: String,
    source_revision: u64,
    settings_epoch: u64,
    entries: Vec<Vec<Value>>,
    generation: Option<u64>,
    revision: Option<u64>,
    next_sequence: u64,
    batch_index: usize,
    conflict_restarts: u8,
    stage: Stage,
    call: CallState,
    retry_attempt: u32,
    delay_ticks: u32,
    cancel_after_call: bool,
}

impl Transaction {
    fn restart_with_new_id(&mut self) {
        self.sync_id = new_sync_id(self.source_revision);
        self.generation = None;
        self.revision = None;
        self.next_sequence = 0;
        self.batch_index = 0;
        self.stage = Stage::Begin;
        self.call = CallState::Submit;
        self.retry_attempt = 0;
        self.delay_ticks = 0;
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
                "sequence":self.next_sequence,
                "expected_revision":self.revision.expect("begin provides revision"),
                "entries":self.entries[self.batch_index],
            }),
            Stage::Finish => json!({
                "action":"provider_snapshot_finish",
                "sync_id":self.sync_id,
                "generation":self.generation.expect("begin precedes finish"),
                "source_revision":self.source_revision,
                "expected_revision":self.revision.expect("begin provides revision"),
                "next_sequence":self.next_sequence,
                "complete":true,
            }),
        }
    }

    fn operation_suffix(&self) -> &'static str {
        match self.stage {
            Stage::Begin => "begin",
            Stage::Batch => "batch",
            Stage::Finish => "finish",
        }
    }

    fn advance_success(&mut self, payload: &Value) -> Result<bool, String> {
        if payload.get("ok").and_then(Value::as_bool) != Some(true) {
            let code = payload
                .get("error")
                .and_then(|error| error.get("code"))
                .and_then(Value::as_str)
                .unwrap_or("malformed_response");
            let message = payload
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("catalog rejected provider snapshot");
            if matches!(
                code,
                "revision_conflict" | "snapshot_conflict" | "sequence_conflict"
            ) {
                if self.conflict_restarts == MAX_CONFLICT_RESTARTS {
                    return Err(format!(
                        "catalog {code} persisted after {MAX_CONFLICT_RESTARTS} snapshot restarts"
                    ));
                }
                self.conflict_restarts += 1;
                self.restart_with_new_id();
                self.delay_ticks = 1_u32
                    .checked_shl(u32::from(self.conflict_restarts))
                    .unwrap_or(MAX_RETRY_TICKS)
                    .min(MAX_RETRY_TICKS);
                return Ok(false);
            }
            return Err(format!("catalog {code}: {message}"));
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
                let next_sequence = payload
                    .get("next_sequence")
                    .and_then(Value::as_u64)
                    .ok_or("catalog begin response omitted next_sequence")?;
                if generation == 0 || next_sequence != 0 {
                    return Err(
                        "catalog begin response has an invalid generation or sequence".into(),
                    );
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
                    .next_sequence
                    .checked_add(1)
                    .ok_or("catalog snapshot sequence exhausted")?;
                if Some(generation) != self.generation
                    || revision < self.revision.unwrap_or(u64::MAX)
                    || payload.get("next_sequence").and_then(Value::as_u64) != Some(expected_next)
                {
                    return Err("catalog batch response is inconsistent".into());
                }
                self.revision = Some(revision);
                self.next_sequence = expected_next;
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
                {
                    return Err("catalog finish response is inconsistent".into());
                }
                self.revision = Some(revision);
                return Ok(true);
            }
        }
        self.call = CallState::Submit;
        self.retry_attempt = 0;
        self.delay_ticks = 0;
        Ok(false)
    }
}

pub(super) struct CatalogSync {
    settings: Option<ProductSettings>,
    settings_epoch: u64,
    observed_provider_revision: u64,
    sync_requested: bool,
    next_scan: Option<Instant>,
    transaction: Option<Transaction>,
    operation_sequence: u64,
    last_error: Option<String>,
}

impl CatalogSync {
    pub(super) fn new() -> Self {
        Self {
            settings: None,
            settings_epoch: 0,
            observed_provider_revision: 0,
            sync_requested: false,
            next_scan: None,
            transaction: None,
            operation_sequence: 0,
            last_error: None,
        }
    }

    pub(super) fn tick(
        &mut self,
        runtime: &RuntimeClient,
        provider: &mut Provider,
        settings: &ProductSettings,
    ) -> ModuleResult<()> {
        let now = Instant::now();
        self.observe_settings(provider, settings, now);
        self.observe_discovery(provider, settings, now);

        if settings.enabled && settings.auto_sync {
            if self.next_scan.is_none() {
                self.next_scan = Some(now);
            }
            if self.next_scan.is_some_and(|deadline| now >= deadline)
                && !provider.scan_in_progress()
            {
                provider.start_scan();
                self.next_scan = Some(now + SYNC_INTERVAL);
            }
        }

        if self.transaction.is_none()
            && self.sync_requested
            && settings.enabled
            && settings.auto_sync
            && provider.has_complete_snapshot()
            && let Err(error) = self.start_transaction(provider, settings)
        {
            self.last_error = Some(error);
            self.sync_requested = false;
            self.next_scan = Some(now + SYNC_INTERVAL);
        }
        self.advance_transaction(runtime, now)
    }

    pub(super) fn status(&self) -> Value {
        json!({
            "enabled":self.settings.as_ref().is_some_and(|settings| settings.enabled && settings.auto_sync),
            "syncing":self.transaction.is_some(),
            "last_error":self.last_error,
        })
    }

    fn observe_settings(
        &mut self,
        provider: &mut Provider,
        settings: &ProductSettings,
        now: Instant,
    ) {
        if self.settings.as_ref() == Some(settings) {
            return;
        }
        self.settings_epoch = self.settings_epoch.saturating_add(1);
        self.settings = Some(settings.clone());
        if settings.enabled && settings.auto_sync {
            self.sync_requested = false;
            self.next_scan = Some(now + SYNC_INTERVAL);
            provider.start_scan();
        } else {
            self.sync_requested = false;
            self.next_scan = None;
        }
        if let Some(transaction) = &mut self.transaction {
            transaction.cancel_after_call = true;
        }
    }

    fn observe_discovery(&mut self, provider: &Provider, settings: &ProductSettings, now: Instant) {
        if provider.revision == self.observed_provider_revision {
            return;
        }
        self.observed_provider_revision = provider.revision;
        if let Some(transaction) = &mut self.transaction
            && transaction.source_revision != provider.revision
        {
            transaction.cancel_after_call = true;
        }
        if !settings.enabled || !settings.auto_sync {
            return;
        }
        if provider.has_complete_snapshot() {
            if let Some(message) = catalog_source_error(provider, settings) {
                self.sync_requested = false;
                self.last_error = Some(message);
            } else {
                self.sync_requested = true;
                self.last_error = None;
            }
        } else {
            self.sync_requested = false;
            self.last_error =
                Some("Steam discovery scan was incomplete; catalog was left unchanged".into());
        }
        self.next_scan.get_or_insert(now + SYNC_INTERVAL);
    }

    fn start_transaction(
        &mut self,
        provider: &Provider,
        settings: &ProductSettings,
    ) -> ModuleResult<()> {
        if let Some(message) = catalog_source_error(provider, settings) {
            return Err(message);
        }
        let steam_executable = provider.steam_executable.as_deref().ok_or_else(|| {
            "Steam client executable was not found; catalog was left unchanged".to_owned()
        })?;
        let records = catalog_records(
            &provider.games,
            settings,
            steam_executable,
            unix_timestamp_now(),
        )?;
        let batches = chunk_records(records)?;
        self.transaction = Some(Transaction {
            sync_id: new_sync_id(provider.revision),
            source_revision: provider.revision,
            settings_epoch: self.settings_epoch,
            entries: batches,
            generation: None,
            revision: None,
            next_sequence: 0,
            batch_index: 0,
            conflict_restarts: 0,
            stage: Stage::Begin,
            call: CallState::Submit,
            retry_attempt: 0,
            delay_ticks: 0,
            cancel_after_call: false,
        });
        self.sync_requested = false;
        self.last_error = None;
        Ok(())
    }

    fn advance_transaction(&mut self, runtime: &RuntimeClient, now: Instant) -> ModuleResult<()> {
        let Some(transaction) = self.transaction.as_mut() else {
            return Ok(());
        };
        if transaction.cancel_after_call && transaction.call == CallState::Submit {
            self.transaction = None;
            return Ok(());
        }
        if transaction.delay_ticks > 0 {
            transaction.delay_ticks -= 1;
            return Ok(());
        }
        match transaction.call {
            CallState::Submit => {
                self.operation_sequence = self.operation_sequence.saturating_add(1);
                let operation_id = format!(
                    "steam.catalog.{}.{}.{}",
                    transaction.sync_id,
                    transaction.operation_suffix(),
                    self.operation_sequence
                );
                let command = Command::Request {
                    service: CATALOG_SERVICE.into(),
                    payload: transaction.payload(),
                    timeout_ms: CATALOG_TIMEOUT_MS,
                    operation_id,
                    parent: None,
                };
                match runtime.send(command) {
                    Ok(Response::Accepted { id }) => {
                        transaction.call = CallState::Waiting { request: id };
                    }
                    Ok(Response::Result { payload }) => {
                        self.consume_payload(payload, now);
                    }
                    Ok(Response::Error { code, message }) => {
                        self.handle_runtime_failure(&code, &message, now);
                    }
                    Ok(response) => {
                        self.handle_retry(&format!("unexpected catalog submission: {response:?}"));
                    }
                    Err(error) => self.handle_retry(&format!("catalog transport: {error}")),
                }
            }
            CallState::Waiting { request } => {
                let response = match runtime.send(Command::TakeResult { request }) {
                    Ok(Response::Pending) => return Ok(()),
                    Ok(Response::Error { code, .. }) if code == "not_found" => {
                        if transaction.cancel_after_call {
                            self.transaction = None;
                            return Ok(());
                        }
                        transaction.call = CallState::Submit;
                        self.handle_retry("catalog child result expired; replaying snapshot step");
                        return Ok(());
                    }
                    Ok(response) => response,
                    Err(error) => {
                        self.handle_retry(&format!("catalog result transport: {error}"));
                        return Ok(());
                    }
                };
                let Some(response) = release_terminal_child_result(request, response, |request| {
                    runtime.send(Command::ReleaseResult { request })
                }) else {
                    transaction.delay_ticks = 1;
                    return Ok(());
                };
                if transaction.cancel_after_call {
                    self.transaction = None;
                    return Ok(());
                }
                match response {
                    Response::Result { payload } => self.consume_payload(payload, now),
                    Response::Error { code, message } => {
                        self.handle_runtime_failure(&code, &message, now)
                    }
                    other => self.handle_retry(&format!("unexpected catalog result: {other:?}")),
                }
            }
        }
        Ok(())
    }

    fn consume_payload(&mut self, payload: Value, now: Instant) {
        let Some(transaction) = &mut self.transaction else {
            return;
        };
        match transaction.advance_success(&payload) {
            Ok(true) => {
                self.transaction = None;
                self.last_error = None;
            }
            Ok(false) => {}
            Err(message) => self.fail_transaction(message, now),
        }
    }

    fn handle_runtime_failure(&mut self, code: &str, message: &str, now: Instant) {
        if retryable(code) {
            self.handle_retry(&format!("catalog {code}: {message}"));
        } else {
            self.fail_transaction(format!("catalog {code}: {message}"), now);
        }
    }

    fn handle_retry(&mut self, message: &str) {
        let Some(transaction) = &mut self.transaction else {
            return;
        };
        transaction.call = CallState::Submit;
        transaction.retry_attempt = transaction.retry_attempt.saturating_add(1);
        transaction.delay_ticks = 1_u32
            .checked_shl(transaction.retry_attempt.min(6))
            .unwrap_or(MAX_RETRY_TICKS)
            .min(MAX_RETRY_TICKS);
        self.last_error = Some(message.to_owned());
    }

    fn fail_transaction(&mut self, message: String, now: Instant) {
        if let Some(transaction) = self.transaction.take()
            && (transaction.settings_epoch != self.settings_epoch
                || transaction.source_revision != self.observed_provider_revision)
        {
            self.sync_requested = true;
        }
        self.last_error = Some(message);
        self.next_scan = Some(now + SYNC_INTERVAL);
    }
}

fn catalog_records(
    games: &std::collections::BTreeMap<u32, Game>,
    settings: &ProductSettings,
    steam_executable: &Path,
    now: u64,
) -> Result<Vec<Value>, String> {
    let program = steam_executable
        .to_str()
        .filter(|path| {
            Path::new(path).is_absolute()
                && path.len() <= 2_048
                && !path.chars().any(char::is_control)
                && !path
                    .split(['/', '\\'])
                    .any(|segment| segment == "." || segment == "..")
        })
        .ok_or("Steam client path is not a valid absolute UTF-8 path")?;
    let mut selected = games
        .values()
        .filter(|game| {
            game.installed
                && !settings
                    .exclude_games
                    .iter()
                    .any(|exclusion| exclusion.matches(game.app_id, &game.name))
                && is_importable(game, settings.include_tools)
        })
        .collect::<Vec<_>>();
    if !settings.sync_all_installed {
        selected.retain(|game| {
            let Some(last_played) = game.last_played.filter(|timestamp| *timestamp > 0) else {
                return false;
            };
            if last_played > now {
                return false;
            }
            let max_age_days = u64::from(settings.recent_max_age_days);
            max_age_days == 0 || now - last_played <= max_age_days * 24 * 60 * 60
        });
        selected.sort_by(|left, right| {
            right
                .last_played
                .cmp(&left.last_played)
                .then_with(|| left.app_id.cmp(&right.app_id))
        });
        selected.truncate(settings.recent_games as usize);
    }
    let mut records = Vec::new();
    for game in selected {
        let stable_id = format!("steam:{}", game.app_id);
        if game.stable_id != stable_id {
            return Err("Steam discovery returned a noncanonical source ID".into());
        }
        if game.name.trim().is_empty()
            || game.name.len() > 160
            || game.name.chars().any(char::is_control)
        {
            return Err(format!(
                "Steam game {} has a name outside catalog entry bounds",
                game.app_id
            ));
        }
        let record = json!({
            "source_id":stable_id,
            "entry":{
                "name":game.name,
                "description":"",
                "tags":["steam"],
                "launch":{
                    "kind":"executable",
                    "program":program,
                    "arguments":["-applaunch",game.app_id.to_string()],
                    "working_directory":null,
                    "auto_detach":true,
                    "wait_all":false,
                    "exit_timeout_seconds":10,
                    "detached":[],
                    "preparation":[]
                }
            }
        });
        let entry_size = serde_json::to_vec(&record)
            .map_err(|error| format!("Steam catalog record serialization failed: {error}"))?
            .len();
        let catalog_entry_size = serde_json::to_vec(&record["entry"])
            .map_err(|error| format!("Steam catalog entry serialization failed: {error}"))?
            .len();
        if entry_size > MAX_BATCH_BYTES || catalog_entry_size > 11_950 {
            return Err(format!(
                "Steam game {} exceeds catalog entry or provider batch bounds",
                game.app_id
            ));
        }
        records.push(record);
    }
    Ok(records)
}

fn is_importable(game: &Game, include_tools: bool) -> bool {
    if include_tools {
        return true;
    }
    let app_type = game
        .app_type
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(
        app_type.as_str(),
        "tool" | "config" | "dlc" | "driver" | "music" | "video"
    ) {
        return false;
    }
    const RUNTIME_APP_IDS: &[u32] = &[
        228_980, 1_070_560, 1_391_110, 1_628_350, 1_824_220, 1_493_710, 4_183_110,
    ];
    if RUNTIME_APP_IDS.contains(&game.app_id) {
        return false;
    }
    let name = game.name.to_lowercase();
    !name.starts_with("proton ")
        && !name.starts_with("steam linux runtime")
        && name != "steamworks common redistributables"
}

fn catalog_source_error(provider: &Provider, settings: &ProductSettings) -> Option<String> {
    if !settings.include_tools && !provider.appinfo_complete {
        return Some(
            "Steam appinfo metadata is missing or incomplete; catalog was left unchanged".into(),
        );
    }
    if !settings.sync_all_installed && settings.recent_games > 0 && !provider.history_complete {
        return Some(
            "Steam local play history is missing or incomplete; catalog was left unchanged".into(),
        );
    }
    None
}

fn unix_timestamp_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn chunk_records(records: Vec<Value>) -> Result<Vec<Vec<Value>>, String> {
    let mut batches = Vec::new();
    let mut batch = Vec::new();
    for record in records {
        let mut candidate = batch.clone();
        candidate.push(record.clone());
        let encoded_size = serde_json::to_vec(&candidate)
            .map_err(|error| format!("Steam batch serialization failed: {error}"))?
            .len();
        if !batch.is_empty() && (batch.len() == MAX_BATCH_ENTRIES || encoded_size > MAX_BATCH_BYTES)
        {
            batches.push(std::mem::take(&mut batch));
            batch.push(record);
        } else {
            batch = candidate;
        }
        if serde_json::to_vec(&batch)
            .map_err(|error| format!("Steam batch serialization failed: {error}"))?
            .len()
            > MAX_BATCH_BYTES
        {
            return Err("Steam catalog record exceeds the provider batch limit".into());
        }
    }
    if !batch.is_empty() {
        batches.push(batch);
    }
    Ok(batches)
}

fn new_sync_id(source_revision: u64) -> String {
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("steam-{}-{time:x}-{source_revision:x}", std::process::id())
}

fn retryable(code: &str) -> bool {
    matches!(
        code,
        "cancelled"
            | "deadline_exceeded"
            | "not_found"
            | "request_ended"
            | "service_unavailable"
            | "state_unavailable"
            | "unavailable"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Exclusion;

    fn game(app_id: u32, name: &str, installed: bool) -> Game {
        Game {
            stable_id: format!("steam:{app_id}"),
            app_id,
            name: name.into(),
            install_directory: "/Steam/steamapps/common/Game".into(),
            library_directory: "/Steam".into(),
            state_flags: if installed { 4 } else { 2 },
            installed,
            last_updated: 0,
            app_type: Some("game".into()),
            last_played: Some(2_000_000_000),
        }
    }

    #[test]
    fn records_are_launchable_steam_client_entries_and_apply_explicit_exclusions() {
        let games = [
            (10, game(10, "Kept", true)),
            (20, game(20, "Excluded by name", true)),
            (30, game(30, "Not installed", false)),
        ]
        .into_iter()
        .collect();
        let settings = ProductSettings {
            sync_all_installed: true,
            exclude_games: vec![Exclusion {
                id: None,
                name: Some("excluded by NAME".into()),
            }],
            ..ProductSettings::default()
        };
        let executable = std::env::current_exe().unwrap();
        let records = catalog_records(&games, &settings, &executable, 2_000_000_000).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["source_id"], "steam:10");
        assert_eq!(records[0]["entry"]["name"], "Kept");
        assert_eq!(
            records[0]["entry"]["launch"]["program"],
            executable.to_str().unwrap()
        );
        assert_eq!(
            records[0]["entry"]["launch"]["arguments"],
            json!(["-applaunch", "10"])
        );
        assert_eq!(records[0]["entry"]["tags"], json!(["steam"]));
    }

    #[test]
    fn recent_policy_filters_types_and_exclusions_before_age_and_count() {
        let game_with_metadata =
            |app_id: u32, name: &str, app_type: &str, last_played: Option<u64>| {
                let mut game = game(app_id, name, true);
                game.app_type = Some(app_type.into());
                game.last_played = last_played;
                game
            };
        let games = [
            (
                10,
                game_with_metadata(10, "Newest game", "game", Some(999_000)),
            ),
            (20, game_with_metadata(20, "Tool", "tool", Some(999_900))),
            (
                30,
                game_with_metadata(30, "Excluded game", "game", Some(999_800)),
            ),
            (
                40,
                game_with_metadata(40, "Second game", "game", Some(998_000)),
            ),
            (50, game_with_metadata(50, "Too old", "game", Some(800_000))),
            (60, game_with_metadata(60, "No history", "game", None)),
        ]
        .into_iter()
        .collect();
        let settings = ProductSettings {
            recent_games: 2,
            recent_max_age_days: 1,
            exclude_games: vec![Exclusion {
                id: Some("30".into()),
                name: None,
            }],
            ..ProductSettings::default()
        };
        let executable = std::env::current_exe().unwrap();
        let records = catalog_records(&games, &settings, &executable, 1_000_000).unwrap();
        assert_eq!(
            records
                .iter()
                .map(|record| record["source_id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["steam:10", "steam:40"]
        );
    }

    #[test]
    fn including_tools_imports_them_only_when_explicitly_enabled() {
        let mut tool = game(228_980, "Steamworks Common Redistributables", true);
        tool.app_type = Some("tool".into());
        let games = [(tool.app_id, tool)].into_iter().collect();
        let executable = std::env::current_exe().unwrap();
        assert!(
            catalog_records(
                &games,
                &ProductSettings {
                    sync_all_installed: true,
                    ..ProductSettings::default()
                },
                &executable,
                2_000_000_000,
            )
            .unwrap()
            .is_empty()
        );
        assert_eq!(
            catalog_records(
                &games,
                &ProductSettings {
                    include_tools: true,
                    sync_all_installed: true,
                    ..ProductSettings::default()
                },
                &executable,
                2_000_000_000,
            )
            .unwrap()
            .len(),
            1
        );
    }

    #[test]
    fn catalog_cleanup_requires_metadata_for_each_active_selection_policy() {
        let mut provider = Provider {
            snapshot_complete: true,
            ..Provider::default()
        };
        let recent = ProductSettings::default();
        assert!(
            catalog_source_error(&provider, &recent)
                .unwrap()
                .contains("appinfo")
        );

        provider.appinfo_complete = true;
        assert!(
            catalog_source_error(&provider, &recent)
                .unwrap()
                .contains("play history")
        );

        provider.history_complete = true;
        assert!(catalog_source_error(&provider, &recent).is_none());
        assert!(
            catalog_source_error(
                &Provider {
                    snapshot_complete: true,
                    ..Provider::default()
                },
                &ProductSettings {
                    include_tools: true,
                    sync_all_installed: true,
                    ..recent
                }
            )
            .is_none()
        );
    }

    #[test]
    fn invalid_catalog_names_and_client_paths_fail_before_a_snapshot_is_started() {
        let games = [(10, game(10, &"x".repeat(161), true))]
            .into_iter()
            .collect();
        let executable = std::env::current_exe().unwrap();
        assert!(
            catalog_records(
                &games,
                &ProductSettings {
                    sync_all_installed: true,
                    ..ProductSettings::default()
                },
                &executable,
                2_000_000_000
            )
            .is_err()
        );

        let games = [(10, game(10, "Valid name", true))].into_iter().collect();
        let long_executable = Path::new("/").join("x".repeat(2_049));
        assert!(
            catalog_records(
                &games,
                &ProductSettings {
                    sync_all_installed: true,
                    ..ProductSettings::default()
                },
                &long_executable,
                2_000_000_000
            )
            .is_err()
        );
    }

    #[test]
    fn catalog_batches_obey_count_and_encoded_size_limits() {
        let records = (1..=19)
            .map(|app_id| {
                json!({
                    "source_id":format!("steam:{app_id}"),
                    "entry":{"name":format!("Game {app_id}")}
                })
            })
            .collect();
        let batches = chunk_records(records).unwrap();
        assert_eq!(
            batches.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![8, 8, 3]
        );
        assert!(batches.iter().all(|batch| batch.len() <= MAX_BATCH_ENTRIES));
        assert!(
            batches
                .iter()
                .all(|batch| serde_json::to_vec(batch).unwrap().len() <= MAX_BATCH_BYTES)
        );
    }

    #[test]
    fn stale_or_partial_discovery_never_becomes_a_complete_catalog_plan() {
        let malformed = json!({
            "ok":true,"revision":1,"generation":2,"next_sequence":0
        });
        let mut transaction = Transaction {
            sync_id: "sync".into(),
            source_revision: 1,
            settings_epoch: 1,
            entries: Vec::new(),
            generation: None,
            revision: None,
            next_sequence: 0,
            batch_index: 0,
            conflict_restarts: 0,
            stage: Stage::Begin,
            call: CallState::Submit,
            retry_attempt: 0,
            delay_ticks: 0,
            cancel_after_call: false,
        };
        assert!(!transaction.advance_success(&malformed).unwrap());
        assert_eq!(transaction.stage, Stage::Finish);
        let incomplete_finish = json!({
            "ok":true,"revision":1,"generation":2,"complete":true,"cleanup_pending":true
        });
        assert!(transaction.advance_success(&incomplete_finish).is_err());
    }

    #[test]
    fn permanent_catalog_revision_conflicts_have_a_bounded_restart_count() {
        let conflict = json!({
            "ok":false,
            "error":{"code":"revision_conflict","message":"catalog changed"}
        });
        let mut transaction = Transaction {
            sync_id: "sync".into(),
            source_revision: 1,
            settings_epoch: 1,
            entries: Vec::new(),
            generation: None,
            revision: None,
            next_sequence: 0,
            batch_index: 0,
            conflict_restarts: 0,
            stage: Stage::Begin,
            call: CallState::Submit,
            retry_attempt: 0,
            delay_ticks: 0,
            cancel_after_call: false,
        };
        for _ in 0..MAX_CONFLICT_RESTARTS {
            assert!(!transaction.advance_success(&conflict).unwrap());
            assert_eq!(transaction.stage, Stage::Begin);
            assert!(transaction.delay_ticks > 0);
            transaction.delay_ticks = 0;
        }
        assert!(transaction.advance_success(&conflict).is_err());
    }
}
