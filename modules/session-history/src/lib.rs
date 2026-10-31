//! Durable session history storage and RPC service.
mod model;
mod product_settings;
mod store;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use model::{HistorySettings, Operation};
use product_settings::{ProductSettings, UpdateCompletion};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use store::{HistoryError, HistoryStore};

const SERVICE: &str = "amaterasu.session-history/1";
const MANAGEMENT_CALLER: &str = "amaterasu.management";
const STREAM_SESSION_CALLER: &str = "amaterasu.stream-session";
const MAX_DEFERRED_SETTINGS_REQUESTS: usize = 32;

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ModuleSettings {
    database_path: Option<PathBuf>,
}

struct SessionHistoryModule {
    runtime: RuntimeClient,
    store: Option<HistoryStore>,
    product_settings: ProductSettings,
    loaded_revision: Option<u64>,
    legacy_cleanup_pending: bool,
    legacy_cleanup_retry_ticks: u8,
    deferred_settings: VecDeque<(u64, Operation)>,
}

impl ManagedModule for SessionHistoryModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        // Storage failure leaves the RPC available so management can report it and
        // streaming can continue with history degraded.
        let mut store = module_settings(&runtime)
            .and_then(database_path_from_settings)
            .ok()
            .and_then(|path| HistoryStore::open(&path, unix_time_seconds()).ok());
        let mut legacy_read_failed = false;
        let migration_candidate = match store.as_ref() {
            Some(store) => match store.legacy_settings() {
                Ok(settings) => Some(settings.unwrap_or_default()),
                Err(_) => {
                    legacy_read_failed = true;
                    None
                }
            },
            // Without readable storage, a prior local policy cannot be ruled
            // out. Wait for an existing central document instead of writing a
            // default over an uninspected migration source.
            None => None,
        };
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        if legacy_read_failed && let Some(store) = store.as_mut() {
            store.record_failure();
        }
        Ok(Self {
            runtime,
            store,
            product_settings: ProductSettings::new(migration_candidate),
            loaded_revision: None,
            legacy_cleanup_pending: false,
            legacy_cleanup_retry_ticks: 0,
            deferred_settings: VecDeque::new(),
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        match self.runtime.send(Command::Poll {})? {
            Response::Request {
                id,
                caller,
                payload,
                ..
            } => {
                match serde_json::from_value::<Operation>(payload) {
                    Ok(operation) if is_authorized(&caller, &operation) => {
                        self.accept_request(id, operation)?;
                    }
                    Ok(_) => self.complete(
                        id,
                        failure(
                            "permission_denied",
                            "caller is not authorized for this history operation",
                        ),
                    )?,
                    Err(_) => self.complete(
                        id,
                        failure(
                            "invalid_request",
                            "request does not match the history schema",
                        ),
                    )?,
                }
                Ok(())
            }
            Response::Pending => Ok(()),
            response => Err(format!(
                "unexpected session-history poll response: {response:?}"
            )),
        }?;

        let runtime = &mut self.runtime;
        self.product_settings
            .advance(&mut |command| runtime.send(command))?;
        self.apply_configuration_if_changed();
        if let Some(store) = self.store.as_mut() {
            store.prune_if_due(unix_time_seconds());
        }
        self.cleanup_legacy_settings();
        self.complete_settings_updates()?;
        self.drain_deferred_settings()?;
        Ok(())
    }
}

impl SessionHistoryModule {
    fn accept_request(&mut self, id: u64, operation: Operation) -> ModuleResult<()> {
        if matches!(
            operation,
            Operation::SettingsGet {} | Operation::SettingsUpdate { .. }
        ) && (!self.product_settings.is_ready() || self.product_settings.busy())
        {
            if self.deferred_settings.len() >= MAX_DEFERRED_SETTINGS_REQUESTS {
                return self.complete(
                    id,
                    failure(
                        "settings_busy",
                        "too many history settings requests are queued",
                    ),
                );
            }
            self.deferred_settings.push_back((id, operation));
            return Ok(());
        }

        self.handle_operation(id, operation)
    }

    fn handle_operation(&mut self, id: u64, operation: Operation) -> ModuleResult<()> {
        match operation {
            Operation::SettingsGet {} => {
                let (settings, revision) = self.product_settings.settings();
                let effective_enabled = self.store.as_ref().is_some_and(|store| store.settings().1);
                self.complete(
                    id,
                    json!({
                        "ok": true,
                        "settings": settings,
                        "revision": revision,
                        "effective_enabled": effective_enabled,
                    }),
                )
            }
            Operation::SettingsUpdate {
                settings,
                expected_revision,
            } => match self
                .product_settings
                .begin_update(id, expected_revision, settings)
            {
                Ok(()) => Ok(()),
                Err(response) => self.complete(id, response),
            },
            operation => {
                let Some(store) = self.store.as_mut() else {
                    if matches!(operation, Operation::HistoryStatus {}) {
                        return self.complete(id, unavailable_status());
                    }
                    return self.complete(
                        id,
                        failure(
                            "storage_unavailable",
                            "session history storage is unavailable",
                        ),
                    );
                };
                let response = match execute(store, operation, unix_time_seconds()) {
                    Ok(value) => value,
                    Err(error) => {
                        if error.is_database() {
                            store.record_failure();
                        }
                        error_response(error)
                    }
                };
                self.complete(id, response)
            }
        }
    }

    fn apply_configuration_if_changed(&mut self) {
        if !self.product_settings.is_ready() {
            return;
        }
        let (settings, revision) = self.product_settings.settings();
        if self.loaded_revision == Some(revision) {
            return;
        }
        if let Some(store) = self.store.as_mut() {
            match store.apply_configuration_with_revision(
                settings.clone(),
                revision,
                unix_time_seconds(),
            ) {
                Ok(()) => {
                    self.loaded_revision = Some(revision);
                    self.legacy_cleanup_pending = true;
                }
                Err(_) => store.record_failure(),
            }
        }
    }

    fn cleanup_legacy_settings(&mut self) {
        if !self.legacy_cleanup_pending || self.loaded_revision.is_none() {
            return;
        }
        if self.legacy_cleanup_retry_ticks > 0 {
            self.legacy_cleanup_retry_ticks -= 1;
            return;
        }
        let Some(store) = self.store.as_mut() else {
            self.legacy_cleanup_pending = false;
            return;
        };
        match store.discard_legacy_settings() {
            Ok(true) => self.legacy_cleanup_pending = false,
            Ok(false) => self.legacy_cleanup_retry_ticks = 30,
            Err(_) => {
                store.record_failure();
                self.legacy_cleanup_retry_ticks = 30;
            }
        }
    }

    fn complete_settings_updates(&mut self) -> ModuleResult<()> {
        while let Some(completion) = self.product_settings.take_completion() {
            match completion {
                UpdateCompletion::Applied {
                    request,
                    settings,
                    revision,
                } => {
                    let effective_enabled =
                        self.store.as_ref().is_some_and(|store| store.settings().1);
                    self.complete(
                        request,
                        json!({
                            "ok": true,
                            "settings": settings,
                            "revision": revision,
                            "effective_enabled": effective_enabled,
                        }),
                    )?;
                }
                UpdateCompletion::Failed { request, response } => {
                    self.complete(request, response)?;
                }
            }
        }
        Ok(())
    }

    fn drain_deferred_settings(&mut self) -> ModuleResult<()> {
        if !self.product_settings.is_ready() || self.product_settings.busy() {
            return Ok(());
        }
        while let Some((id, operation)) = self.deferred_settings.pop_front() {
            let is_update = matches!(operation, Operation::SettingsUpdate { .. });
            self.handle_operation(id, operation)?;
            if is_update && self.product_settings.busy() {
                break;
            }
        }
        Ok(())
    }

    fn complete(&mut self, request: u64, payload: Value) -> ModuleResult<()> {
        match self.runtime.send(Command::Complete { request, payload })? {
            Response::Ok => Ok(()),
            Response::Error { code, .. } if code == "request_ended" || code == "not_found" => {
                Ok(())
            }
            response => Err(format!(
                "unexpected session-history completion response: {response:?}"
            )),
        }
    }
}

fn is_authorized(caller: &str, operation: &Operation) -> bool {
    match operation {
        Operation::StreamActivityBegin { .. }
        | Operation::StreamActivityEnd { .. }
        | Operation::SessionBegin { .. }
        | Operation::SessionEnd { .. }
        | Operation::RecordSample { .. }
        | Operation::RecordEvent { .. } => caller == STREAM_SESSION_CALLER,
        _ => caller == MANAGEMENT_CALLER,
    }
}

fn execute(
    store: &mut HistoryStore,
    operation: Operation,
    now_unix: i64,
) -> Result<Value, HistoryError> {
    match operation {
        Operation::SettingsGet {} | Operation::SettingsUpdate { .. } => Err(HistoryError::Invalid(
            "history settings are handled by the configuration client",
        )),
        Operation::StreamActivityBegin { session_id } => Ok(json!({
            "ok": true,
            "reserved": store.begin_stream_activity(&session_id, now_unix)?,
        })),
        Operation::StreamActivityEnd { session_id } => Ok(json!({
            "ok": true,
            "ended": store.end_stream_activity(&session_id, now_unix)?,
        })),
        Operation::SessionBegin {
            metadata,
            started_at_unix,
        } => Ok(json!({
            "ok": true,
            "recorded": store.begin_session_at(&metadata, started_at_unix.unwrap_or(now_unix), now_unix)?,
        })),
        Operation::SessionEnd {
            session_id,
            ended_at_unix,
        } => Ok(json!({
            "ok": true,
            "recorded": store.end_session(&session_id, ended_at_unix.unwrap_or(now_unix), now_unix)?,
        })),
        Operation::RecordSample { sample } => Ok(json!({
            "ok": true,
            "recorded": store.record_sample(&sample)?,
        })),
        Operation::RecordEvent { event } => Ok(json!({
            "ok": true,
            "recorded": store.record_event(&event)?,
        })),
        Operation::ListSessions { limit, offset } => {
            let page = store.list_sessions_page(limit.unwrap_or(25), offset.unwrap_or(0))?;
            Ok(json!({"ok": true, "sessions": page.sessions, "next_offset": page.next_offset}))
        }
        Operation::SessionDetail { session_id } => Ok(json!({
            "ok": true,
            "detail": store.session_detail(&session_id)?,
        })),
        Operation::TimelineOpen {
            session_id,
            sample_limit,
        } => store.timeline_open(&session_id, sample_limit),
        Operation::TimelinePage {
            session_id,
            kind,
            cursor,
            limit,
        } => store.timeline_page(&session_id, kind, cursor, limit),
        Operation::ActiveSessions { limit, offset } => {
            let page = store.active_sessions_page(limit.unwrap_or(16), offset.unwrap_or(0))?;
            Ok(json!({"ok": true, "sessions": page.sessions, "next_offset": page.next_offset}))
        }
        Operation::HistoryStatus {} => {
            store.prune_if_due(now_unix);
            Ok(json!({
                "ok": true,
                "status": store.status()?,
            }))
        }
        Operation::DeleteSession { session_id } => {
            store.delete_session(&session_id)?;
            Ok(json!({"ok": true}))
        }
    }
}

fn error_response(error: HistoryError) -> Value {
    match error {
        HistoryError::Invalid(message) => failure("invalid_request", message),
        HistoryError::Database => failure("storage_error", "history storage operation failed"),
        HistoryError::NotFound => failure("not_found", "session history record was not found"),
        HistoryError::HistoryChanged => failure(
            "history_changed",
            "retained session history changed; open a new snapshot",
        ),
        HistoryError::ActiveSession => {
            failure("active_session", "active history records cannot be deleted")
        }
        HistoryError::RecordingDisabled => failure(
            "recording_disabled",
            "history records cannot be deleted while recording is disabled",
        ),
    }
}

fn failure(code: &str, message: &str) -> Value {
    json!({"ok": false, "code": code, "message": message})
}

fn unavailable_status() -> Value {
    json!({
        "ok": true,
        "status": {
            "available": false,
            "configuration_ready": false,
            "applied_revision": null,
            "enabled": HistorySettings::default().enabled,
            "effective_enabled": false,
            "degraded": true,
            "failed_operations": 0,
            "active_streams": 0,
            "pending_activities": 0,
            "sessions": 0,
            "samples": 0,
            "events": 0,
            "live_bytes": 0,
            "last_prune": {
                "sessions_deleted": 0,
                "live_bytes": 0,
                "size_limit_bytes": 0,
                "size_limit_unmet": false,
            },
        },
    })
}

fn module_settings(runtime: &RuntimeClient) -> Result<ModuleSettings, String> {
    let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
        return Err("history module settings were unavailable".into());
    };
    serde_json::from_value(payload).map_err(|_| "history module settings were invalid".into())
}

fn database_path_from_settings(settings: ModuleSettings) -> Result<PathBuf, String> {
    let path = match settings.database_path {
        Some(path) => path,
        None => default_database_path()?,
    };
    validate_database_path(path)
}

fn validate_database_path(path: PathBuf) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("history database path must be absolute".into());
    }
    if path.file_name().is_none() || path.parent().is_none() {
        return Err("history database path must name a file".into());
    }
    Ok(path)
}

fn default_database_path() -> Result<PathBuf, String> {
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("APPDATA"))
        .map(PathBuf::from)
        .ok_or_else(|| "user data directory is unavailable".to_owned())?;

    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "user data directory is unavailable".to_owned())?
        .join("Library")
        .join("Application Support");

    #[cfg(target_os = "linux")]
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".local").join("share"))
        })
        .ok_or_else(|| "user data directory is unavailable".to_owned())?;

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    return Err("session history storage is unsupported on this platform".into());

    validate_database_path(
        base.join("Amaterasu")
            .join("session-history")
            .join("history.db"),
    )
}

fn unix_time_seconds() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
        Err(error) => -i64::try_from(error.duration().as_secs()).unwrap_or(i64::MAX),
    }
}

amaterasu_module_sdk::export_module!(SessionHistoryModule);

#[cfg(test)]
mod tests;
