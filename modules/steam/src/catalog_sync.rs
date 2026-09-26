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
const ARTWORK_RETRY_INTERVAL: Duration = Duration::from_secs(5 * 60);
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
    force_sync: bool,
    settings_epoch: u64,
    source_fingerprint: SourceFingerprint,
    remove_stale: bool,
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

struct ForceSyncRequest {
    after_revision: u64,
    target_revision: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SourceFingerprint(u64);

struct FingerprintBuilder(u64);

impl FingerprintBuilder {
    fn new() -> Self {
        Self(1_469_598_103_934_665_603)
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        self.write_u64(bytes.len() as u64);
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(1_099_511_628_211);
        }
    }

    fn write_u64(&mut self, value: u64) {
        for byte in value.to_le_bytes() {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(1_099_511_628_211);
        }
    }

    fn write_bool(&mut self, value: bool) {
        self.write_u64(if value { 1 } else { 0 });
    }

    fn write_optional_string(&mut self, value: Option<&str>) {
        match value {
            Some(value) => {
                self.write_u64(1);
                self.write_bytes(value.as_bytes());
            }
            None => self.write_u64(0),
        }
    }

    fn finish(self) -> SourceFingerprint {
        SourceFingerprint(self.0)
    }
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
                "remove_stale":self.remove_stale,
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
                    || payload.get("remove_stale").and_then(Value::as_bool)
                        != Some(self.remove_stale)
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
    observed_source_fingerprint: Option<SourceFingerprint>,
    last_successful_fingerprint: Option<SourceFingerprint>,
    next_artwork_retry: Option<Instant>,
    sync_requested: bool,
    next_scan: Option<Instant>,
    force_sync_request: Option<ForceSyncRequest>,
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
            observed_source_fingerprint: None,
            last_successful_fingerprint: None,
            next_artwork_retry: None,
            sync_requested: false,
            next_scan: None,
            force_sync_request: None,
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
        self.tick_at(runtime, provider, settings, now)
    }

    pub(super) fn tick_at(
        &mut self,
        runtime: &RuntimeClient,
        provider: &mut Provider,
        settings: &ProductSettings,
        now: Instant,
    ) -> ModuleResult<()> {
        self.observe_settings(provider, settings, now);
        if provider.scan_in_progress() {
            // An explicit refresh may overlap a snapshot transaction. Stop
            // before its next catalog step, especially a stale cleanup finish.
            self.cancel_transaction_for_changed_source(None);
        }
        self.observe_discovery(provider, settings, now);

        if self.automatic_scan_due(provider, settings, now) {
            if provider.start_scan() {
                self.next_scan = None;
            } else {
                self.last_error = Some("Steam discovery could not start an automatic scan".into());
                self.schedule_next_scan(now);
            }
        }

        if self.transaction.is_none() {
            let force_sync_ready = self
                .force_sync_request
                .as_ref()
                .is_some_and(|request| request.target_revision == Some(provider.revision));
            if force_sync_ready {
                if let Err(error) = self.start_transaction(provider, settings, true) {
                    self.last_error = Some(error);
                    self.force_sync_request = None;
                    self.sync_requested = false;
                    self.schedule_next_scan(now);
                }
            } else if self.sync_requested
                && settings.enabled
                && settings.auto_sync
                && !provider.scan_in_progress()
                && provider.has_complete_snapshot()
                && let Err(error) = self.start_transaction(provider, settings, false)
            {
                self.last_error = Some(error);
                self.sync_requested = false;
                self.schedule_next_scan(now);
            }
        }
        self.advance_transaction(runtime, now)
    }

    fn automatic_scan_due(
        &self,
        provider: &Provider,
        settings: &ProductSettings,
        now: Instant,
    ) -> bool {
        settings.enabled
            && settings.auto_sync
            && self.transaction.is_none()
            && !self.sync_requested
            && self.force_sync_request.is_none()
            && !provider.scan_in_progress()
            && self.next_scan.is_some_and(|deadline| now >= deadline)
    }

    fn schedule_next_scan(&mut self, finished_at: Instant) {
        if self
            .settings
            .as_ref()
            .is_some_and(|settings| settings.enabled && settings.auto_sync)
        {
            self.next_scan = Some(finished_at + SYNC_INTERVAL);
        }
    }

    pub(super) fn status(&self) -> Value {
        json!({
            "enabled":self.settings.as_ref().is_some_and(|settings| settings.enabled && settings.auto_sync),
            "syncing":self.transaction.is_some(),
            "force_sync_pending":self.force_sync_request.is_some(),
            "last_error":self.last_error,
        })
    }

    pub(super) fn request_force_sync(
        &mut self,
        provider: &mut Provider,
        settings: &ProductSettings,
    ) -> Result<(), &'static str> {
        if !settings.enabled || !provider.enabled {
            self.force_sync_request = None;
            return Err("steam_disabled");
        }
        if self.force_sync_request.is_some() {
            return Ok(());
        }
        if !provider.scan_in_progress() && provider.revision == u64::MAX {
            return Err("revision_exhausted");
        }
        if !provider.scan_in_progress() && !provider.start_scan() {
            return Err("scan_unavailable");
        }
        self.sync_requested = false;
        self.force_sync_request = Some(ForceSyncRequest {
            after_revision: provider.revision,
            target_revision: None,
        });
        let finish_may_have_committed = self.transaction.as_ref().is_some_and(|transaction| {
            transaction.stage == Stage::Finish
                && matches!(transaction.call, CallState::Waiting { .. })
        });
        if finish_may_have_committed {
            self.last_successful_fingerprint = None;
            self.next_artwork_retry = None;
        }
        if let Some(transaction) = &mut self.transaction {
            transaction.cancel_after_call = true;
        }
        self.last_error = None;
        Ok(())
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
        self.observed_source_fingerprint = None;
        self.last_successful_fingerprint = None;
        self.next_artwork_retry = None;
        if settings.enabled && settings.auto_sync {
            self.sync_requested = false;
            self.next_scan = if provider.scan_in_progress() {
                None
            } else {
                Some(now)
            };
        } else {
            self.sync_requested = false;
            self.next_scan = None;
        }
        if !settings.enabled {
            self.force_sync_request = None;
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
        if !settings.enabled {
            self.observed_source_fingerprint = None;
            self.force_sync_request = None;
            return;
        }
        let force_snapshot_is_newer = self
            .force_sync_request
            .as_ref()
            .is_some_and(|request| provider.revision > request.after_revision);
        if force_snapshot_is_newer {
            if !provider.has_complete_snapshot() {
                self.cancel_transaction_for_changed_source(None);
                self.observed_source_fingerprint = None;
                self.force_sync_request = None;
                self.sync_requested = false;
                self.last_error =
                    Some("Steam discovery scan was incomplete; catalog was left unchanged".into());
                self.schedule_next_scan(now);
                return;
            }
            if let Some(message) = catalog_source_error(provider, settings) {
                self.cancel_transaction_for_changed_source(None);
                self.observed_source_fingerprint = None;
                self.force_sync_request = None;
                self.sync_requested = false;
                self.last_error = Some(message);
                self.schedule_next_scan(now);
                return;
            }
            if let Some(request) = &mut self.force_sync_request {
                request.target_revision = Some(provider.revision);
            }
        }
        if !settings.auto_sync {
            self.observed_source_fingerprint = self
                .force_sync_request
                .as_ref()
                .filter(|request| request.target_revision == Some(provider.revision))
                .map(|_| source_fingerprint(provider));
            if self.observed_source_fingerprint.is_some() {
                self.cancel_transaction_for_changed_source(self.observed_source_fingerprint);
            }
            return;
        }
        if !provider.has_complete_snapshot() {
            self.cancel_transaction_for_changed_source(None);
            self.observed_source_fingerprint = None;
            self.sync_requested = false;
            self.last_error =
                Some("Steam discovery scan was incomplete; catalog was left unchanged".into());
            self.schedule_next_scan(now);
            return;
        }
        if let Some(message) = catalog_source_error(provider, settings) {
            self.cancel_transaction_for_changed_source(None);
            self.observed_source_fingerprint = None;
            self.sync_requested = false;
            self.last_error = Some(message);
            self.schedule_next_scan(now);
            return;
        }

        let fingerprint = source_fingerprint(provider);
        self.observed_source_fingerprint = Some(fingerprint);
        let force_requires_new_revision = self.force_sync_request.as_ref().is_some_and(|request| {
            request.target_revision == Some(provider.revision)
                && self
                    .transaction
                    .as_ref()
                    .is_some_and(|transaction| transaction.source_revision < provider.revision)
        });
        if self.transaction.as_ref().is_some_and(|transaction| {
            transaction.settings_epoch == self.settings_epoch
                && transaction.source_fingerprint == fingerprint
        }) && !force_requires_new_revision
        {
            if let Some(transaction) = &mut self.transaction {
                transaction.cancel_after_call = false;
            }
            self.sync_requested = false;
            return;
        }
        self.cancel_transaction_for_changed_source(Some(fingerprint));

        if self.fingerprint_requires_sync(fingerprint, now) {
            self.sync_requested = true;
            self.last_error = None;
        } else {
            self.sync_requested = false;
            self.last_error = None;
            self.schedule_next_scan(now);
        }
    }

    fn cancel_transaction_for_changed_source(
        &mut self,
        current_fingerprint: Option<SourceFingerprint>,
    ) {
        let force_sync_revision = self
            .force_sync_request
            .as_ref()
            .and_then(|request| request.target_revision);
        let canceled_finish_may_have_committed =
            self.transaction.as_mut().is_some_and(|transaction| {
                if transaction.settings_epoch != self.settings_epoch
                    || Some(transaction.source_fingerprint) != current_fingerprint
                    || force_sync_revision
                        .is_some_and(|revision| revision > transaction.source_revision)
                {
                    transaction.cancel_after_call = true;
                    transaction.stage == Stage::Finish
                        && matches!(transaction.call, CallState::Waiting { .. })
                } else {
                    false
                }
            });
        if canceled_finish_may_have_committed {
            self.last_successful_fingerprint = None;
            self.next_artwork_retry = None;
        }
    }

    fn fingerprint_requires_sync(&self, fingerprint: SourceFingerprint, now: Instant) -> bool {
        self.last_successful_fingerprint != Some(fingerprint)
            || self
                .next_artwork_retry
                .is_none_or(|deadline| now >= deadline)
    }

    fn start_transaction(
        &mut self,
        provider: &Provider,
        settings: &ProductSettings,
        force_sync: bool,
    ) -> ModuleResult<()> {
        if provider.scan_in_progress() || !provider.has_complete_snapshot() {
            return Err(
                "Steam discovery has no completed snapshot; catalog was left unchanged".into(),
            );
        }
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
        let source_fingerprint = source_fingerprint(provider);
        self.transaction = Some(Transaction {
            sync_id: new_sync_id(provider.revision),
            source_revision: provider.revision,
            force_sync,
            settings_epoch: self.settings_epoch,
            source_fingerprint,
            remove_stale: should_remove_stale(settings),
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
        self.next_scan = None;
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
                let Some(transaction) = self.transaction.take() else {
                    return;
                };
                if transaction.force_sync
                    && self.force_sync_request.as_ref().is_some_and(|request| {
                        request.target_revision == Some(transaction.source_revision)
                    })
                {
                    self.force_sync_request = None;
                }
                self.last_successful_fingerprint = Some(transaction.source_fingerprint);
                self.next_artwork_retry = Some(now + ARTWORK_RETRY_INTERVAL);
                self.schedule_next_scan(now);
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
        if let Some(transaction) = self.transaction.take() {
            if transaction.force_sync
                && self.force_sync_request.as_ref().is_some_and(|request| {
                    request.target_revision == Some(transaction.source_revision)
                })
            {
                self.force_sync_request = None;
            }
            if transaction.settings_epoch != self.settings_epoch
                || Some(transaction.source_fingerprint) != self.observed_source_fingerprint
            {
                self.sync_requested = true;
            }
        }
        self.last_error = Some(message);
        self.schedule_next_scan(now);
    }
}

fn source_fingerprint(provider: &Provider) -> SourceFingerprint {
    let mut fingerprint = FingerprintBuilder::new();
    fingerprint.write_u64(provider.games.len() as u64);
    fingerprint.write_bool(provider.appinfo_complete);
    fingerprint.write_bool(provider.history_complete);
    for (map_app_id, game) in &provider.games {
        fingerprint.write_u64(u64::from(*map_app_id));
        fingerprint.write_u64(u64::from(game.app_id));
        fingerprint.write_bytes(game.stable_id.as_bytes());
        fingerprint.write_bytes(game.name.as_bytes());
        fingerprint.write_bytes(game.install_directory.as_bytes());
        fingerprint.write_bytes(game.library_directory.as_bytes());
        fingerprint.write_u64(game.state_flags);
        fingerprint.write_bool(game.installed);
        fingerprint.write_u64(game.last_updated);
        fingerprint.write_optional_string(game.app_type.as_deref());
        fingerprint.write_u64(game.playtime_minutes.unwrap_or_default());
        write_optional_u64(&mut fingerprint, game.last_played);
        fingerprint.write_optional_string(game.artwork_revision.as_deref());
        fingerprint.write_optional_string(game.artwork_source_path.as_deref());
        fingerprint.write_bool(game.artwork_is_remote);
        fingerprint.write_bytes(game.artwork_candidate_signature.as_bytes());
    }
    let steam_executable = provider
        .steam_executable
        .as_deref()
        .map(|path| path.to_string_lossy());
    fingerprint.write_optional_string(steam_executable.as_deref());
    fingerprint.finish()
}

fn write_optional_u64(fingerprint: &mut FingerprintBuilder, value: Option<u64>) {
    match value {
        Some(value) => {
            fingerprint.write_u64(1);
            fingerprint.write_u64(value);
        }
        None => fingerprint.write_u64(0),
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
        let max_age_seconds = u64::from(settings.recent_max_age_days) * 24 * 60 * 60;
        selected.retain(|game| {
            let Some(last_played) = game.last_played.filter(|timestamp| *timestamp > 0) else {
                return false;
            };
            max_age_seconds == 0 || (last_played <= now && now - last_played <= max_age_seconds)
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
        if game.app_id == 0 {
            return Err("Steam discovery returned a zero app ID".into());
        }
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
        let mut record = json!({
            "source_id":stable_id,
            "entry":{
                "name":game.name,
                "description":"",
                "tags":["steam"],
                "steam_id":game.app_id.to_string(),
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
        if !game.install_directory.is_empty() {
            if !valid_install_directory(&game.install_directory) {
                return Err(format!(
                    "Steam game {} has an invalid installation directory",
                    game.app_id
                ));
            }
            record["entry"]["steam_install_dir"] = json!(game.install_directory);
        }
        if let Some(revision) = game.artwork_revision.as_deref() {
            let parsed_revision = revision.parse::<u64>().ok();
            if parsed_revision.is_none_or(|number| number.to_string() != revision) {
                return Err(format!(
                    "Steam game {} has an invalid artwork revision",
                    game.app_id
                ));
            }
            record["entry"]["artwork"] = json!({
                "provider":"steam",
                "app_id":game.app_id,
                "revision":revision
            });
        }
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

fn valid_install_directory(value: &str) -> bool {
    if value.len() > 1_024
        || value.trim() != value
        || value.chars().any(char::is_control)
        || value
            .split(['/', '\\'])
            .any(|component| matches!(component, "." | ".."))
    {
        return false;
    }
    let components = value
        .split(['/', '\\'])
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();
    let drive_rooted = value.len() >= 3
        && value.as_bytes()[0].is_ascii_alphabetic()
        && value.as_bytes()[1] == b':'
        && matches!(value.as_bytes()[2], b'/' | b'\\');
    let unc_rooted = value.starts_with("\\\\") || value.starts_with("//");
    let posix_rooted = value.starts_with('/') && !unc_rooted;
    (drive_rooted && components.len() > 1)
        || (unc_rooted && components.len() >= 3)
        || (posix_rooted && !components.is_empty())
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
    // Match Vibeshine's C-locale bytewise lowercase behavior. Unicode case
    // conversion can change UTF-8 names into strings that match runtime names.
    let name = game.name.to_ascii_lowercase();
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

fn should_remove_stale(settings: &ProductSettings) -> bool {
    !settings.sync_all_installed || settings.autosync_remove_uninstalled
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
            playtime_minutes: None,
            artwork_revision: None,
            artwork_source_path: None,
            artwork_is_remote: false,
            artwork_local_revision: None,
            artwork_needs_remote_upgrade: false,
            artwork_candidate_signature: String::new(),
        }
    }

    fn auto_sync_settings() -> ProductSettings {
        ProductSettings {
            enabled: true,
            auto_sync: true,
            sync_all_installed: true,
            ..ProductSettings::default()
        }
    }

    fn complete_provider() -> Provider {
        Provider {
            enabled: true,
            steam_executable: Some(Path::new("/Steam/steam.exe").to_path_buf()),
            games: [(42, game(42, "Fingerprint game", true))]
                .into_iter()
                .collect(),
            revision: 1,
            snapshot_complete: true,
            appinfo_complete: true,
            history_complete: true,
            owned_history_complete: true,
            ..Provider::default()
        }
    }

    fn finish_transaction(
        fingerprint: SourceFingerprint,
        source_revision: u64,
        settings_epoch: u64,
    ) -> Transaction {
        Transaction {
            sync_id: "finish-test".into(),
            source_revision,
            force_sync: false,
            settings_epoch,
            source_fingerprint: fingerprint,
            remove_stale: true,
            entries: Vec::new(),
            generation: Some(2),
            revision: Some(8),
            next_sequence: 0,
            batch_index: 0,
            conflict_restarts: 0,
            stage: Stage::Finish,
            call: CallState::Submit,
            retry_attempt: 0,
            delay_ticks: 0,
            cancel_after_call: false,
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
        let recent = ProductSettings {
            sync_all_installed: false,
            recent_games: 10,
            recent_max_age_days: 30,
            ..settings
        };
        let recent_records = catalog_records(&games, &recent, &executable, 2_000_000_000).unwrap();
        assert_eq!(recent_records.len(), 1);
        assert_eq!(recent_records[0]["source_id"], "steam:10");
    }

    #[test]
    fn catalog_records_carry_only_the_opaque_artwork_reference() {
        let mut artwork_game = game(42, "Artwork game", true);
        artwork_game.artwork_revision = Some("123456".into());
        artwork_game.artwork_source_path = Some("C:/private/Steam/cache/42.png".into());
        let games = [(42, artwork_game)].into_iter().collect();
        let settings = ProductSettings {
            sync_all_installed: true,
            ..ProductSettings::default()
        };
        let executable = std::env::current_exe().unwrap();
        let records = catalog_records(&games, &settings, &executable, 2_000_000_000).unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["source_id"], "steam:42");
        assert_eq!(records[0]["entry"]["steam_id"], "42");
        assert_eq!(
            records[0]["entry"]["steam_install_dir"],
            "/Steam/steamapps/common/Game"
        );
        assert_eq!(
            records[0]["entry"]["artwork"],
            json!({"provider":"steam","app_id":42,"revision":"123456"})
        );
        assert!(
            serde_json::to_string(&records[0])
                .unwrap()
                .find("C:/private/Steam/cache/42.png")
                .is_none()
        );

        let mut invalid = game(42, "Invalid artwork game", true);
        invalid.artwork_revision = Some("revision".into());
        let invalid_games = [(42, invalid)].into_iter().collect();
        assert!(catalog_records(&invalid_games, &settings, &executable, 2_000_000_000).is_err());

        let mut missing_install_dir = game(43, "No install directory", true);
        missing_install_dir.install_directory.clear();
        let records = catalog_records(
            &[(43, missing_install_dir)].into_iter().collect(),
            &settings,
            &executable,
            2_000_000_000,
        )
        .unwrap();
        assert_eq!(records[0]["entry"]["steam_id"], "43");
        assert!(records[0]["entry"].get("steam_install_dir").is_none());

        for invalid_path in ["/", "\\foo", "/games/../outside"] {
            let mut invalid_game = game(44, "Invalid install directory", true);
            invalid_game.install_directory = invalid_path.into();
            let invalid_games = [(44, invalid_game)].into_iter().collect();
            assert!(
                catalog_records(&invalid_games, &settings, &executable, 2_000_000_000).is_err(),
                "accepted install directory {invalid_path:?}"
            );
        }
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
    fn recent_max_age_cutoff_includes_game_played_exactly_at_boundary() {
        let now = 1_000_000;
        let max_age_seconds = 24 * 60 * 60;
        let mut cutoff_game = game(70, "At age cutoff", true);
        cutoff_game.last_played = Some(now - max_age_seconds);
        let games = [(70, cutoff_game)].into_iter().collect();
        let settings = ProductSettings {
            recent_games: 1,
            recent_max_age_days: 1,
            ..ProductSettings::default()
        };
        let executable = std::env::current_exe().unwrap();

        let records = catalog_records(&games, &settings, &executable, now).unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["source_id"], "steam:70");
    }

    #[test]
    fn zero_recent_max_age_days_does_not_filter_future_timestamps() {
        let mut future_game = game(70, "Future timestamp", true);
        future_game.last_played = Some(1_000_001);
        let games = [(70, future_game)].into_iter().collect();
        let settings = ProductSettings {
            recent_games: 1,
            recent_max_age_days: 0,
            ..ProductSettings::default()
        };
        let executable = std::env::current_exe().unwrap();

        let records = catalog_records(&games, &settings, &executable, 1_000_000).unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["source_id"], "steam:70");

        let age_limited_settings = ProductSettings {
            recent_max_age_days: 1,
            ..settings
        };
        let age_limited_records =
            catalog_records(&games, &age_limited_settings, &executable, 1_000_000).unwrap();

        assert!(age_limited_records.is_empty());
    }

    #[test]
    fn source_fingerprint_tracks_represented_game_artwork_and_client_fields() {
        let mut provider = complete_provider();
        let original = source_fingerprint(&provider);
        assert_eq!(source_fingerprint(&provider), original);

        provider.games.get_mut(&42).unwrap().name = "Renamed game".into();
        assert_ne!(source_fingerprint(&provider), original);
        provider.games.get_mut(&42).unwrap().name = "Fingerprint game".into();

        provider.games.get_mut(&42).unwrap().artwork_revision = Some("124".into());
        assert_ne!(source_fingerprint(&provider), original);
        provider.games.get_mut(&42).unwrap().artwork_revision = None;
        provider.games.get_mut(&42).unwrap().artwork_source_path =
            Some("/Steam/art/portrait.jpg".into());
        assert_ne!(source_fingerprint(&provider), original);
        provider.games.get_mut(&42).unwrap().artwork_source_path = None;

        provider
            .games
            .get_mut(&42)
            .unwrap()
            .artwork_candidate_signature = "candidate-set-a".into();
        let candidate_signature_fingerprint = source_fingerprint(&provider);
        provider
            .games
            .get_mut(&42)
            .unwrap()
            .artwork_candidate_signature = "candidate-set-b".into();
        assert_ne!(
            source_fingerprint(&provider),
            candidate_signature_fingerprint
        );
        provider
            .games
            .get_mut(&42)
            .unwrap()
            .artwork_candidate_signature = String::new();

        provider.steam_executable = Some(Path::new("/Steam/updated-steam.exe").to_path_buf());
        assert_ne!(source_fingerprint(&provider), original);
    }

    #[test]
    fn unchanged_fingerprint_skips_until_the_artwork_retry_deadline() {
        let now = Instant::now();
        let fingerprint = SourceFingerprint(7);
        let mut sync = CatalogSync::new();
        sync.settings = Some(auto_sync_settings());
        sync.last_successful_fingerprint = Some(fingerprint);
        sync.next_artwork_retry = Some(now + ARTWORK_RETRY_INTERVAL);

        assert!(!sync.fingerprint_requires_sync(fingerprint, now));
        assert!(!sync.fingerprint_requires_sync(
            fingerprint,
            now + ARTWORK_RETRY_INTERVAL - Duration::from_nanos(1)
        ));
        assert!(sync.fingerprint_requires_sync(fingerprint, now + ARTWORK_RETRY_INTERVAL));
    }

    #[test]
    fn settings_epoch_change_invalidates_cached_fingerprint_and_retry() {
        let now = Instant::now();
        let mut provider = complete_provider();
        let original_settings = auto_sync_settings();
        let fingerprint = source_fingerprint(&provider);
        let mut sync = CatalogSync::new();
        sync.settings = Some(original_settings.clone());
        sync.settings_epoch = 4;
        sync.observed_provider_revision = provider.revision;
        sync.observed_source_fingerprint = Some(fingerprint);
        sync.last_successful_fingerprint = Some(fingerprint);
        sync.next_artwork_retry = Some(now + ARTWORK_RETRY_INTERVAL);

        let updated_settings = ProductSettings {
            recent_games: original_settings.recent_games + 1,
            ..original_settings
        };
        sync.observe_settings(&mut provider, &updated_settings, now);

        assert_eq!(sync.settings_epoch, 5);
        assert!(sync.observed_source_fingerprint.is_none());
        assert!(sync.last_successful_fingerprint.is_none());
        assert!(sync.next_artwork_retry.is_none());
        assert_eq!(sync.next_scan, Some(now));
    }

    #[test]
    fn a_new_revision_with_the_same_fingerprint_keeps_the_active_transaction() {
        let now = Instant::now();
        let settings = auto_sync_settings();
        let mut provider = complete_provider();
        let fingerprint = source_fingerprint(&provider);
        let mut sync = CatalogSync::new();
        sync.settings = Some(settings.clone());
        sync.settings_epoch = 1;
        sync.observed_provider_revision = provider.revision;
        let mut transaction = finish_transaction(fingerprint, provider.revision, 1);
        transaction.cancel_after_call = true;
        sync.transaction = Some(transaction);

        provider.revision += 1;
        sync.observe_discovery(&provider, &settings, now);

        assert!(!sync.transaction.as_ref().unwrap().cancel_after_call);
        assert!(!sync.sync_requested);

        provider.games.get_mut(&42).unwrap().name = "Changed game".into();
        provider.revision += 1;
        sync.observe_discovery(&provider, &settings, now);
        assert!(sync.transaction.as_ref().unwrap().cancel_after_call);
        assert!(sync.sync_requested);
    }

    #[test]
    fn validated_finish_caches_fingerprint_and_schedules_from_completion() {
        let finished_at = Instant::now();
        let settings = auto_sync_settings();
        let fingerprint = SourceFingerprint(11);
        let mut sync = CatalogSync::new();
        sync.settings = Some(settings);
        sync.settings_epoch = 1;
        sync.observed_provider_revision = 1;
        sync.observed_source_fingerprint = Some(fingerprint);
        sync.transaction = Some(finish_transaction(fingerprint, 1, 1));

        sync.consume_payload(
            json!({
                "ok":true,"revision":8,"generation":2,"complete":true,
                "cleanup_pending":false,"remove_stale":true
            }),
            finished_at,
        );

        assert_eq!(sync.last_successful_fingerprint, Some(fingerprint));
        assert_eq!(
            sync.next_artwork_retry,
            Some(finished_at + ARTWORK_RETRY_INTERVAL)
        );
        assert_eq!(sync.next_scan, Some(finished_at + SYNC_INTERVAL));
        assert!(sync.transaction.is_none());
    }

    #[test]
    fn failed_finish_does_not_cache_fingerprint_and_retries_after_the_interval() {
        let failed_at = Instant::now();
        let settings = auto_sync_settings();
        let fingerprint = SourceFingerprint(13);
        let mut sync = CatalogSync::new();
        sync.settings = Some(settings);
        sync.settings_epoch = 1;
        sync.observed_provider_revision = 1;
        sync.observed_source_fingerprint = Some(fingerprint);
        sync.transaction = Some(finish_transaction(fingerprint, 1, 1));

        sync.consume_payload(
            json!({
                "ok":true,"revision":8,"generation":2,"complete":true,
                "cleanup_pending":true,"remove_stale":true
            }),
            failed_at,
        );

        assert!(sync.last_successful_fingerprint.is_none());
        assert!(sync.next_artwork_retry.is_none());
        assert_eq!(sync.next_scan, Some(failed_at + SYNC_INTERVAL));
        assert!(sync.fingerprint_requires_sync(fingerprint, failed_at + SYNC_INTERVAL));
        assert!(sync.transaction.is_none());
    }

    #[test]
    fn periodic_scans_wait_for_reconciliation_and_never_overlap_an_active_scan() {
        let now = Instant::now();
        let settings = auto_sync_settings();
        let mut provider = complete_provider();
        let mut sync = CatalogSync::new();
        sync.settings = Some(settings.clone());
        sync.next_scan = Some(now + SYNC_INTERVAL);

        assert!(!sync.automatic_scan_due(&provider, &settings, now));
        assert!(!sync.automatic_scan_due(
            &provider,
            &settings,
            now + SYNC_INTERVAL - Duration::from_nanos(1)
        ));
        let deadline = now + SYNC_INTERVAL;
        assert!(sync.automatic_scan_due(&provider, &settings, deadline));
        sync.transaction = Some(finish_transaction(SourceFingerprint(1), 1, 1));
        assert!(!sync.automatic_scan_due(&provider, &settings, deadline));
        sync.transaction = None;
        provider.scan = Some(crate::discovery::Scan::new(&[]));
        assert!(!sync.automatic_scan_due(&provider, &settings, deadline));
    }

    #[test]
    fn catalog_plan_requires_a_completed_current_discovery_snapshot() {
        let mut provider = complete_provider();
        let settings = ProductSettings {
            include_tools: true,
            ..auto_sync_settings()
        };
        let mut sync = CatalogSync::new();
        provider.scan = Some(crate::discovery::Scan::new(&[]));
        assert!(sync.start_transaction(&provider, &settings, false).is_err());
        assert!(sync.transaction.is_none());

        provider.scan = None;
        provider.snapshot_complete = false;
        provider.games.clear();
        provider.games.insert(43, game(43, "Installed", true));
        assert!(sync.start_transaction(&provider, &settings, false).is_err());
        assert!(sync.transaction.is_none());

        provider.snapshot_complete = true;
        provider.steam_executable = Some(std::env::current_exe().unwrap());
        sync.start_transaction(&provider, &settings, false).unwrap();
        let records = &sync.transaction.as_ref().unwrap().entries;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0][0]["source_id"], "steam:43");
    }

    #[test]
    fn paired_exclusions_match_by_id_or_name_during_catalog_selection() {
        let games = [
            (10, game(10, "Excluded by ID", true)),
            (20, game(20, "Excluded by name", true)),
            (30, game(30, "Kept", true)),
        ]
        .into_iter()
        .collect();
        let settings = ProductSettings {
            sync_all_installed: true,
            exclude_games: vec![Exclusion {
                id: Some("10".into()),
                name: Some("EXCLUDED BY NAME".into()),
            }],
            ..ProductSettings::default()
        };
        let records = catalog_records(
            &games,
            &settings,
            &std::env::current_exe().unwrap(),
            2_000_000_000,
        )
        .unwrap();

        assert_eq!(
            records
                .iter()
                .map(|record| record["source_id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["steam:30"]
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
    fn runtime_name_fallback_uses_ascii_bytewise_case_conversion() {
        let ascii_runtime_name = game(42, "Steamworks Common Redistributables", true);
        let non_ascii_runtime_name = game(43, "Steamwor\u{212A}s Common Redistributables", true);
        let uppercase_tool_type = Game {
            app_type: Some("TOOL".into()),
            ..game(44, "Uppercase tool type", true)
        };

        assert!(!is_importable(&ascii_runtime_name, false));
        assert!(is_importable(&non_ascii_runtime_name, false));
        assert!(!is_importable(&uppercase_tool_type, false));
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
    fn all_installed_can_import_with_only_manifests_when_tool_filter_is_disabled() {
        let executable = std::env::current_exe().unwrap();
        let provider = Provider {
            steam_executable: Some(executable.clone()),
            games: [(42, game(42, "New install", true))].into_iter().collect(),
            snapshot_complete: true,
            revision: 1,
            ..Provider::default()
        };
        let settings = ProductSettings {
            include_tools: true,
            sync_all_installed: true,
            autosync_remove_uninstalled: true,
            ..ProductSettings::default()
        };
        assert!(!provider.appinfo_complete && !provider.history_complete);
        assert!(catalog_source_error(&provider, &settings).is_none());
        let records =
            catalog_records(&provider.games, &settings, &executable, 2_000_000_000).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0]["entry"]["launch"]["arguments"],
            json!(["-applaunch", "42"])
        );

        let filtered = ProductSettings {
            include_tools: false,
            ..settings
        };
        let with_incomplete_uninstalled_candidate = Provider {
            appinfo_complete: true,
            owned_history_complete: false,
            history_complete: true,
            ..provider
        };
        assert!(should_remove_stale(&filtered));
        assert!(catalog_source_error(&with_incomplete_uninstalled_candidate, &filtered).is_none());
        assert_eq!(
            catalog_records(
                &with_incomplete_uninstalled_candidate.games,
                &filtered,
                &executable,
                2_000_000_000,
            )
            .unwrap()
            .len(),
            1
        );
    }

    #[test]
    fn cleanup_policy_forces_removal_in_recent_mode_and_respects_sync_all_setting() {
        assert!(should_remove_stale(&ProductSettings::default()));
        assert!(should_remove_stale(&ProductSettings {
            sync_all_installed: false,
            autosync_remove_uninstalled: false,
            ..ProductSettings::default()
        }));
        assert!(should_remove_stale(&ProductSettings {
            sync_all_installed: true,
            autosync_remove_uninstalled: true,
            ..ProductSettings::default()
        }));
        assert!(!should_remove_stale(&ProductSettings {
            sync_all_installed: true,
            autosync_remove_uninstalled: false,
            ..ProductSettings::default()
        }));

        let transaction = Transaction {
            sync_id: "test-sync".into(),
            source_revision: 4,
            force_sync: false,
            settings_epoch: 1,
            source_fingerprint: SourceFingerprint(1),
            remove_stale: false,
            entries: Vec::new(),
            generation: Some(2),
            revision: Some(8),
            next_sequence: 0,
            batch_index: 0,
            conflict_restarts: 0,
            stage: Stage::Finish,
            call: CallState::Submit,
            retry_attempt: 0,
            delay_ticks: 0,
            cancel_after_call: false,
        };
        let request = transaction.payload();
        assert_eq!(request["remove_stale"], false);
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
            force_sync: false,
            settings_epoch: 1,
            source_fingerprint: SourceFingerprint(1),
            remove_stale: true,
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
            "ok":true,"revision":1,"generation":2,"complete":true,"cleanup_pending":true,"remove_stale":true
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
            force_sync: false,
            settings_epoch: 1,
            source_fingerprint: SourceFingerprint(1),
            remove_stale: true,
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
