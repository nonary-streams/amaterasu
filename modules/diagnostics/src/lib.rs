//! Caller-scoped diagnostics with bounded, in-memory retention and no automatic collection.
mod host_logging;
mod journal;
mod logging;
mod protocol;
pub use journal::Journal;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use host_logging::HostLoggingConfigurationReader;
use serde_json::Value;
use std::path::Path;
use std::path::PathBuf;
use std::time::Instant;

pub const SERVICE: &str = "amaterasu.diagnostics/1";

pub struct DiagnosticsModule {
    runtime: RuntimeClient,
    journal: Journal,
    host_logging: HostLoggingConfigurationReader,
    bootstrap_logging: Option<logging::LoggingSettings>,
    logging_state_root: Option<PathBuf>,
}

impl ManagedModule for DiagnosticsModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("diagnostics settings request did not return a result".into());
        };
        let logging_settings = logging::LoggingSettings::from_value(
            payload
                .get("host_logging")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({})),
        )?;
        let logging_state_root = logging_settings.state_root.clone();
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            journal: Journal::unready(),
            host_logging: HostLoggingConfigurationReader::default(),
            bootstrap_logging: Some(logging_settings),
            logging_state_root,
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        let now = Instant::now();
        if let Some(configuration) = self
            .host_logging
            .tick(now, |command| self.runtime.send(command))
        {
            match persisted_logging_settings(&configuration, self.logging_state_root.as_deref()) {
                Ok((settings, revision)) => {
                    match self.journal.apply_persisted_logging(settings, revision) {
                        Ok(()) => {
                            self.host_logging.adopt_configuration(revision);
                            self.bootstrap_logging = None;
                        }
                        Err(warning) => {
                            eprintln!(
                                "amaterasu diagnostics: persisted file logging unavailable: {warning}"
                            );
                            self.host_logging.reject_configuration(now);
                        }
                    }
                }
                Err(warning) => {
                    eprintln!("amaterasu diagnostics: persisted logging policy ignored: {warning}");
                    self.host_logging.reject_configuration(now);
                }
            }
        }

        if self.host_logging.legacy_bootstrap_authorized()
            && !self.journal.is_ready()
            && let Some(settings) = self.bootstrap_logging.take()
        {
            if let Some(warning) = self.journal.configure_logging(settings) {
                eprintln!("amaterasu diagnostics: file logging unavailable: {warning}");
            }
            self.journal.set_ready(true);
        }

        match self.runtime.send(Command::Poll {})? {
            Response::Request {
                id,
                caller,
                payload,
                ..
            } => {
                let payload = self.journal.handle(&caller, payload);
                // A cancellation after dispatch does not undo an accepted record/counter.
                match self.runtime.send(Command::Complete {
                    request: id,
                    payload,
                })? {
                    Response::Ok => Ok(()),
                    Response::Error { code, .. }
                        if code == "request_ended" || code == "not_found" =>
                    {
                        Ok(())
                    }
                    response => Err(format!("unexpected diagnostics completion: {response:?}")),
                }
            }
            Response::Pending => Ok(()),
            response => Err(format!("unexpected diagnostics poll: {response:?}")),
        }
    }
}

fn persisted_logging_settings(
    payload: &Value,
    state_root: Option<&Path>,
) -> Result<(logging::LoggingSettings, u64), String> {
    validate_effective_log_path(payload, state_root)?;
    let settings = logging::LoggingSettings::from_host_configuration(payload, state_root)?;
    let revision = payload
        .get("revision")
        .and_then(Value::as_u64)
        .filter(|revision| *revision > 0)
        .ok_or("host configuration revision must be positive")?;
    Ok((settings, revision))
}

fn validate_effective_log_path(payload: &Value, state_root: Option<&Path>) -> Result<(), String> {
    let configured = payload
        .get("values")
        .and_then(Value::as_object)
        .and_then(|values| values.get("log_path"))
        .and_then(Value::as_str)
        .ok_or("host configuration omitted configured log_path")?;
    let effective = payload
        .get("effective")
        .and_then(Value::as_object)
        .and_then(|effective| effective.get("log_path"))
        .and_then(Value::as_str)
        .ok_or("host configuration omitted effective log_path")?;
    if effective.len() > 4096 || effective.chars().any(char::is_control) {
        return Err("host configuration effective log_path is invalid".into());
    }
    let effective_path = Path::new(effective);
    if !effective_path.is_absolute() {
        return Err("host configuration effective log_path is not absolute".into());
    }

    let configured_path = Path::new(configured);
    let expected_path = if configured_path.is_absolute() {
        configured_path.to_path_buf()
    } else {
        state_root
            .ok_or("diagnostics settings are missing the trusted state root")?
            .join(configured_path)
    };
    if !same_effective_path(&expected_path, effective_path) {
        return Err(
            "host configuration effective log_path does not match the configured path".into(),
        );
    }
    Ok(())
}

fn same_effective_path(expected: &Path, effective: &Path) -> bool {
    #[cfg(windows)]
    {
        let normalize = |path: &Path| {
            let path = path.to_string_lossy().replace('/', "\\");
            path.strip_prefix(r"\\?\")
                .unwrap_or(path.as_str())
                .to_owned()
        };
        normalize(expected).eq_ignore_ascii_case(&normalize(effective))
    }
    #[cfg(not(windows))]
    {
        expected.components().collect::<PathBuf>() == effective.components().collect::<PathBuf>()
    }
}

#[cfg(test)]
mod hydration_tests {
    use super::*;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn valid_provider_configuration_enables_readiness_after_path_validation() {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state_root = std::env::temp_dir().join(format!(
            "amaterasu-diagnostics-hydration-{}-{timestamp}",
            std::process::id()
        ));
        std::fs::create_dir(&state_root).unwrap();
        let configured = "amaterasu.log";
        let payload = json!({
            "ok":true,
            "ready":true,
            "version":1,
            "revision":4,
            "values":{"log_path":configured},
            "effective":{
                "log_path":state_root.join(configured).to_string_lossy(),
                "min_log_level":"warning",
            },
        });

        let (settings, revision) = persisted_logging_settings(&payload, Some(&state_root)).unwrap();
        assert_eq!(revision, 4);
        assert_eq!(settings.minimum_level, 3);

        let mut reader = HostLoggingConfigurationReader::default();
        reader.adopt_configuration(revision);
        let mut journal = Journal::unready();
        journal.apply_persisted_logging(settings, revision).unwrap();
        assert_eq!(
            journal.handle("capture", json!({"action":"status"}))["ready"],
            true
        );
        assert_eq!(
            journal.handle("capture", json!({"action":"status"}))["logging_configuration_revision"],
            4
        );
        assert_eq!(
            journal.handle(
                "capture",
                json!({
                    "action":"record",
                    "level":"error",
                    "target":"capture",
                    "message":"hydrated"
                })
            )["sequence"],
            1
        );
        drop(journal);
        std::fs::remove_dir_all(state_root).unwrap();
    }

    #[test]
    fn mismatched_effective_path_rejects_provider_configuration() {
        let state_root = std::env::temp_dir().join("amaterasu-diagnostics-trusted-root");
        let payload = json!({
            "ok":true,
            "ready":true,
            "version":1,
            "revision":4,
            "values":{"log_path":"amaterasu.log"},
            "effective":{
                "log_path":state_root.join("elsewhere.log").to_string_lossy(),
                "min_log_level":"warning",
            },
        });

        assert!(persisted_logging_settings(&payload, Some(&state_root)).is_err());
    }
}

amaterasu_module_sdk::export_module!(DiagnosticsModule);
