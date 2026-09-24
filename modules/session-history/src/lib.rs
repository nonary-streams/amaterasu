//! Durable session history storage and RPC service.
mod model;
mod store;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use model::{HistorySettings, Operation};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use store::{HistoryError, HistoryStore};

const SERVICE: &str = "amaterasu.session-history/1";
const MANAGEMENT_CALLER: &str = "amaterasu.management";
const STREAM_SESSION_CALLER: &str = "amaterasu.stream-session";

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ModuleSettings {
    database_path: Option<PathBuf>,
}

struct SessionHistoryModule {
    runtime: RuntimeClient,
    store: Option<HistoryStore>,
}

impl ManagedModule for SessionHistoryModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        // Storage failure leaves the RPC available so management can report it and
        // streaming can continue with history degraded.
        let store = module_settings(&runtime)
            .and_then(database_path_from_settings)
            .ok()
            .and_then(|path| HistoryStore::open(&path, unix_time_seconds()).ok());
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self { runtime, store })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        match self.runtime.send(Command::Poll {})? {
            Response::Request {
                id,
                caller,
                payload,
                ..
            } => {
                let response = self.handle(&caller, payload);
                match self.runtime.send(Command::Complete {
                    request: id,
                    payload: response,
                })? {
                    Response::Ok => Ok(()),
                    Response::Error { code, .. }
                        if code == "request_ended" || code == "not_found" =>
                    {
                        Ok(())
                    }
                    response => Err(format!(
                        "unexpected session-history completion response: {response:?}"
                    )),
                }
            }
            Response::Pending => Ok(()),
            response => Err(format!(
                "unexpected session-history poll response: {response:?}"
            )),
        }
    }
}

impl SessionHistoryModule {
    fn handle(&mut self, caller: &str, payload: Value) -> Value {
        let operation: Operation = match serde_json::from_value(payload) {
            Ok(operation) => operation,
            Err(_) => {
                return failure(
                    "invalid_request",
                    "request does not match the history schema",
                );
            }
        };

        if !is_authorized(caller, &operation) {
            return failure(
                "permission_denied",
                "caller is not authorized for this history operation",
            );
        }
        let Some(store) = self.store.as_mut() else {
            if matches!(&operation, Operation::HistoryStatus {}) {
                return unavailable_status();
            }
            return failure(
                "storage_unavailable",
                "session history storage is unavailable",
            );
        };

        match execute(store, operation, unix_time_seconds()) {
            Ok(value) => value,
            Err(error) => {
                if error.is_database() {
                    store.record_failure();
                }
                error_response(error)
            }
        }
    }
}

fn is_authorized(caller: &str, operation: &Operation) -> bool {
    match operation {
        Operation::SessionBegin { .. }
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
        Operation::SettingsGet {} => {
            let (settings, revision, effective_enabled) = store.settings();
            Ok(json!({
                "ok": true,
                "settings": settings,
                "revision": revision,
                "effective_enabled": effective_enabled,
            }))
        }
        Operation::SettingsUpdate {
            settings,
            expected_revision,
        } => {
            let revision = store.set_settings(settings, expected_revision, now_unix)?;
            let (settings, _, effective_enabled) = store.settings();
            Ok(json!({
                "ok": true,
                "settings": settings,
                "revision": revision,
                "effective_enabled": effective_enabled,
            }))
        }
        Operation::SessionBegin {
            metadata,
            started_at_unix,
        } => Ok(json!({
            "ok": true,
            "recorded": store.begin_session(&metadata, started_at_unix.unwrap_or(now_unix))?,
        })),
        Operation::SessionEnd {
            session_id,
            ended_at_unix,
        } => Ok(json!({
            "ok": true,
            "recorded": store.end_session(&session_id, ended_at_unix.unwrap_or(now_unix))?,
        })),
        Operation::RecordSample { sample } => Ok(json!({
            "ok": true,
            "recorded": store.record_sample(&sample)?,
        })),
        Operation::RecordEvent { event } => Ok(json!({
            "ok": true,
            "recorded": store.record_event(&event)?,
        })),
        Operation::ListSessions { limit, offset } => Ok(json!({
            "ok": true,
            "sessions": store.list_sessions(limit.unwrap_or(25), offset.unwrap_or(0))?,
        })),
        Operation::SessionDetail { session_id } => Ok(json!({
            "ok": true,
            "detail": store.session_detail(&session_id)?,
        })),
        Operation::ActiveSessions { limit } => Ok(json!({
            "ok": true,
            "sessions": store.active_sessions(limit.unwrap_or(16))?,
        })),
        Operation::HistoryStatus {} => Ok(json!({
            "ok": true,
            "status": store.status()?,
        })),
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
        HistoryError::RevisionConflict(revision) => json!({
            "ok": false,
            "code": "revision_conflict",
            "message": "history settings changed since the supplied revision",
            "revision": revision,
        }),
        HistoryError::NotFound => failure("not_found", "session history record was not found"),
        HistoryError::ActiveSession => {
            failure("active_session", "active history records cannot be deleted")
        }
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
            "enabled": HistorySettings::default().enabled,
            "effective_enabled": false,
            "degraded": true,
            "failed_operations": 0,
            "active_streams": 0,
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
