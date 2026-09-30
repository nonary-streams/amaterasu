//! Caller-scoped diagnostics with bounded, in-memory retention and no automatic collection.
mod host_logging;
mod journal;
mod logging;
mod protocol;
pub use journal::Journal;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use host_logging::HostLoggingConfigurationReader;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;
use std::path::PathBuf;
use std::time::Instant;

pub const SERVICE: &str = "amaterasu.diagnostics/1";
const MANAGEMENT_CALLER: &str = "amaterasu.management";
const HOST_CONFIGURATION_CALLER: &str = "amaterasu.host-configuration";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RefreshLoggingRequest {
    action: String,
    minimum_revision: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyHostLoggingRequest {
    action: String,
    configuration: Value,
}

pub struct DiagnosticsModule {
    runtime: RuntimeClient,
    journal: Journal,
    host_logging: HostLoggingConfigurationReader,
    bootstrap_logging: Option<logging::LoggingSettings>,
    logging_state_root: Option<PathBuf>,
    pending_product_policy: Option<ProductLoggingPolicy>,
}

#[derive(Clone)]
struct ProductLoggingPolicy {
    revision: u64,
    minimum_level: u8,
    log_path: String,
}

impl ProductLoggingPolicy {
    fn from_settings(revision: u64, settings: &logging::LoggingSettings) -> Option<Self> {
        Some(Self {
            revision,
            minimum_level: settings.minimum_level,
            log_path: settings.log_path.as_deref()?.to_string_lossy().into_owned(),
        })
    }
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
        let pending_product_policy = ProductLoggingPolicy::from_settings(0, &logging_settings);
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            journal: Journal::unready(),
            host_logging: HostLoggingConfigurationReader::default(),
            bootstrap_logging: Some(logging_settings),
            logging_state_root,
            pending_product_policy,
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
                    let product_policy =
                        settings
                            .log_path
                            .as_deref()
                            .map(|path| ProductLoggingPolicy {
                                revision,
                                minimum_level: settings.minimum_level,
                                log_path: path.to_string_lossy().into_owned(),
                            });
                    match self.journal.apply_persisted_logging(settings, revision) {
                        Ok(()) => {
                            self.host_logging.adopt_configuration(revision);
                            self.pending_product_policy = product_policy;
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

        self.journal.recover_logging_if_due(now);
        self.forward_product_logging_policy();

        match self.runtime.send(Command::Poll {})? {
            Response::Request {
                id,
                caller,
                payload,
                ..
            } => {
                let product_policy_update = product_policy_from_host_update(
                    &caller,
                    &payload,
                    self.logging_state_root.as_deref(),
                );
                let response = handle_request(
                    &mut self.journal,
                    &mut self.host_logging,
                    &caller,
                    payload,
                    now,
                    self.logging_state_root.as_deref(),
                );
                if response.get("ok").and_then(Value::as_bool) == Some(true)
                    && let Some(policy) = product_policy_update
                {
                    self.pending_product_policy = Some(policy);
                }
                // A cancellation after dispatch does not undo an accepted record/counter.
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
                    response => Err(format!("unexpected diagnostics completion: {response:?}")),
                }
            }
            Response::Pending => Ok(()),
            response => Err(format!("unexpected diagnostics poll: {response:?}")),
        }
    }
}

impl DiagnosticsModule {
    fn forward_product_logging_policy(&mut self) {
        let Some(policy) = self.pending_product_policy.as_ref() else {
            return;
        };
        if self
            .runtime
            .apply_logging_policy(policy.revision, policy.minimum_level, &policy.log_path)
            .is_ok()
        {
            self.pending_product_policy = None;
        }
    }
}

fn handle_request(
    journal: &mut Journal,
    host_logging: &mut HostLoggingConfigurationReader,
    caller: &str,
    payload: Value,
    now: Instant,
    state_root: Option<&Path>,
) -> Value {
    if payload.get("action").and_then(Value::as_str) == Some("apply_host_logging") {
        return apply_host_logging(journal, host_logging, caller, payload, state_root);
    }
    if payload.get("action").and_then(Value::as_str) != Some("refresh_logging") {
        return journal.handle(caller, payload);
    }
    if caller != MANAGEMENT_CALLER {
        return json!({"ok":false,"code":"permission_denied"});
    }
    let Ok(request) = serde_json::from_value::<RefreshLoggingRequest>(payload) else {
        return json!({"ok":false,"code":"invalid_request"});
    };
    if request.action != "refresh_logging" || request.minimum_revision == 0 {
        return json!({"ok":false,"code":"invalid_request"});
    }

    host_logging.request_refresh(now, request.minimum_revision);
    json!({
        "ok":true,
        "requested_revision":request.minimum_revision,
        "logging_configuration_revision":journal.logging_configuration_revision(),
        "ready":journal.is_ready(),
    })
}

fn apply_host_logging(
    journal: &mut Journal,
    host_logging: &mut HostLoggingConfigurationReader,
    caller: &str,
    payload: Value,
    state_root: Option<&Path>,
) -> Value {
    if caller != HOST_CONFIGURATION_CALLER {
        return json!({"ok":false,"code":"permission_denied"});
    }
    let Ok(request) = serde_json::from_value::<ApplyHostLoggingRequest>(payload) else {
        return json!({"ok":false,"code":"invalid_request"});
    };
    if request.action != "apply_host_logging" {
        return json!({"ok":false,"code":"invalid_request"});
    }
    let Ok((settings, revision)) = persisted_logging_settings(&request.configuration, state_root)
    else {
        return json!({"ok":false,"code":"invalid_configuration"});
    };
    if let Some(applied) = journal.logging_configuration_revision() {
        if revision < applied {
            return json!({"ok":false,"code":"stale_revision"});
        }
        if revision == applied {
            return if journal.persisted_settings_match(&settings) {
                if journal.logging_delivery_available() {
                    json!({"ok":true,"applied_revision":revision})
                } else {
                    json!({"ok":false,"code":"logging_unavailable"})
                }
            } else {
                json!({"ok":false,"code":"revision_conflict"})
            };
        }
    }
    if journal.apply_persisted_logging(settings, revision).is_err() {
        return json!({"ok":false,"code":"logging_unavailable"});
    }
    host_logging.adopt_configuration(revision);
    json!({"ok":true,"applied_revision":revision})
}

fn product_policy_from_host_update(
    caller: &str,
    payload: &Value,
    state_root: Option<&Path>,
) -> Option<ProductLoggingPolicy> {
    if caller != HOST_CONFIGURATION_CALLER
        || payload.get("action").and_then(Value::as_str) != Some("apply_host_logging")
    {
        return None;
    }
    let request = serde_json::from_value::<ApplyHostLoggingRequest>(payload.clone()).ok()?;
    let (settings, revision) =
        persisted_logging_settings(&request.configuration, state_root).ok()?;
    ProductLoggingPolicy::from_settings(revision, &settings)
}

fn persisted_logging_settings(
    payload: &Value,
    state_root: Option<&Path>,
) -> Result<(logging::LoggingSettings, u64), String> {
    validate_configuration_shape(payload)?;
    validate_effective_log_path(payload, state_root)?;
    let settings = logging::LoggingSettings::from_host_configuration(payload, state_root)?;
    let revision = payload
        .get("revision")
        .and_then(Value::as_u64)
        .filter(|revision| *revision > 0)
        .ok_or("host configuration revision must be positive")?;
    Ok((settings, revision))
}

fn validate_configuration_shape(payload: &Value) -> Result<(), String> {
    let values = payload
        .get("values")
        .and_then(Value::as_object)
        .ok_or("host configuration values are missing")?;
    let effective = payload
        .get("effective")
        .and_then(Value::as_object)
        .ok_or("host configuration effective values are missing")?;
    for key in [
        "file_state",
        "vibeshine_file_state",
        "credentials_file",
        "pkey",
        "cert",
        "log_path",
    ] {
        if values.get(key).and_then(Value::as_str).is_none()
            || effective.get(key).and_then(Value::as_str).is_none()
        {
            return Err(format!("host configuration omitted {key}"));
        }
    }
    let selected_level = values.get("min_log_level").and_then(Value::as_str);
    if !matches!(
        selected_level,
        Some(
            "product_default"
                | "verbose"
                | "debug"
                | "info"
                | "warning"
                | "error"
                | "fatal"
                | "none"
        )
    ) {
        return Err("host configuration minimum log level is invalid".into());
    }
    let effective_level = effective.get("min_log_level").and_then(Value::as_str);
    // The host configuration provider resolves product_default using its own
    // product channel. Diagnostics may be built at a different package version.
    let matches_selection = match selected_level {
        Some("product_default") => matches!(effective_level, Some("info" | "debug")),
        Some(selected) => effective_level == Some(selected),
        None => false,
    };
    if !matches_selection {
        return Err(
            "host configuration effective minimum log level differs from the saved policy".into(),
        );
    }
    Ok(())
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

    fn configuration(root: &Path, revision: u64, log_path: &str) -> Value {
        let effective = |path: &str| root.join(path).to_string_lossy().into_owned();
        json!({
            "ok":true,"ready":true,"version":1,"revision":revision,
            "values":{
                "file_state":"state.json","vibeshine_file_state":"legacy.json",
                "credentials_file":"","pkey":"key.pem","cert":"cert.pem",
                "log_path":log_path,"min_log_level":"warning"
            },
            "effective":{
                "file_state":effective("state.json"),
                "vibeshine_file_state":effective("legacy.json"),
                "credentials_file":effective("state.json"),
                "pkey":effective("key.pem"),"cert":effective("cert.pem"),
                "log_path":effective(log_path),"min_log_level":"warning"
            }
        })
    }

    #[test]
    fn refresh_logging_requires_management_and_reports_pending_application() {
        let now = Instant::now();
        let mut journal = Journal::unready();
        let mut reader = HostLoggingConfigurationReader::default();
        let refresh = json!({"action":"refresh_logging","minimum_revision":7});

        assert_eq!(
            handle_request(
                &mut journal,
                &mut reader,
                "untrusted",
                refresh.clone(),
                now,
                None
            ),
            json!({"ok":false,"code":"permission_denied"})
        );
        for invalid in [
            json!({"action":"refresh_logging","minimum_revision":0}),
            json!({"action":"refresh_logging","minimum_revision":7,"caller":"amaterasu.management"}),
        ] {
            assert_eq!(
                handle_request(
                    &mut journal,
                    &mut reader,
                    MANAGEMENT_CALLER,
                    invalid,
                    now,
                    None
                ),
                json!({"ok":false,"code":"invalid_request"})
            );
        }
        assert_eq!(
            handle_request(
                &mut journal,
                &mut reader,
                MANAGEMENT_CALLER,
                refresh,
                now,
                None
            ),
            json!({"ok":true,"requested_revision":7,"logging_configuration_revision":null,"ready":false})
        );
        assert!(
            reader
                .tick(now, |command| match command {
                    Command::Request { payload, .. } => {
                        assert_eq!(payload, json!({"action":"get"}));
                        Ok(Response::Accepted { id: 1 })
                    }
                    unexpected => panic!("unexpected runtime command: {unexpected:?}"),
                })
                .is_none()
        );
    }

    #[test]
    fn signed_host_apply_is_idempotent_and_preserves_policy_on_rejection() {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "amaterasu-diagnostics-signed-apply-{}-{timestamp}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        let mut journal = Journal::unready();
        let mut reader = HostLoggingConfigurationReader::default();
        let now = Instant::now();
        let apply = |revision, path| {
            json!({
                "action":"apply_host_logging",
                "configuration":configuration(&root, revision, path)
            })
        };

        assert_eq!(
            handle_request(
                &mut journal,
                &mut reader,
                MANAGEMENT_CALLER,
                apply(2, "first.log"),
                now,
                Some(&root)
            ),
            json!({"ok":false,"code":"permission_denied"})
        );
        let mut incomplete = apply(2, "first.log");
        incomplete["configuration"]["values"]
            .as_object_mut()
            .unwrap()
            .remove("file_state");
        for invalid in [apply(0, "first.log"), incomplete] {
            assert_eq!(
                handle_request(
                    &mut journal,
                    &mut reader,
                    HOST_CONFIGURATION_CALLER,
                    invalid,
                    now,
                    Some(&root)
                ),
                json!({"ok":false,"code":"invalid_configuration"})
            );
        }
        assert_eq!(
            handle_request(
                &mut journal,
                &mut reader,
                HOST_CONFIGURATION_CALLER,
                apply(2, "first.log"),
                now,
                Some(&root)
            ),
            json!({"ok":true,"applied_revision":2})
        );
        let log_directory = root.join("logs");
        let files_before_duplicate = std::fs::read_dir(&log_directory).unwrap().count();
        assert_eq!(
            handle_request(
                &mut journal,
                &mut reader,
                HOST_CONFIGURATION_CALLER,
                apply(2, "first.log"),
                now,
                Some(&root)
            ),
            json!({"ok":true,"applied_revision":2})
        );
        assert_eq!(
            std::fs::read_dir(&log_directory).unwrap().count(),
            files_before_duplicate
        );
        assert_eq!(
            handle_request(
                &mut journal,
                &mut reader,
                HOST_CONFIGURATION_CALLER,
                apply(1, "older.log"),
                now,
                Some(&root)
            ),
            json!({"ok":false,"code":"stale_revision"})
        );
        assert_eq!(
            handle_request(
                &mut journal,
                &mut reader,
                HOST_CONFIGURATION_CALLER,
                apply(2, "different.log"),
                now,
                Some(&root)
            ),
            json!({"ok":false,"code":"revision_conflict"})
        );
        let mut bad_path = apply(3, "third.log");
        bad_path["configuration"]["effective"]["log_path"] =
            json!(root.join("elsewhere.log").to_string_lossy());
        assert_eq!(
            handle_request(
                &mut journal,
                &mut reader,
                HOST_CONFIGURATION_CALLER,
                bad_path,
                now,
                Some(&root)
            ),
            json!({"ok":false,"code":"invalid_configuration"})
        );
        std::fs::write(root.join("blocked"), "not a directory").unwrap();
        assert_eq!(
            handle_request(
                &mut journal,
                &mut reader,
                HOST_CONFIGURATION_CALLER,
                apply(3, "blocked/failure.log"),
                now,
                Some(&root)
            ),
            json!({"ok":false,"code":"logging_unavailable"})
        );
        let status = journal.handle("capture", json!({"action":"status"}));
        assert_eq!(status["logging_configuration_revision"], 2);
        assert_eq!(status["logging_sink"]["state"], "healthy");

        journal.close_sink_for_test();
        assert_eq!(
            journal.handle(
                "capture",
                json!({
                    "action":"record","level":"error","target":"capture","message":"write fails"
                })
            )["sequence"],
            1
        );
        let failed = journal.handle("capture", json!({"action":"status"}));
        assert_eq!(
            failed["logging_sink"],
            json!({"state":"failed","failure":"write"})
        );
        assert_eq!(failed["logging_configuration_revision"], 2);
        assert_eq!(
            handle_request(
                &mut journal,
                &mut reader,
                HOST_CONFIGURATION_CALLER,
                apply(2, "first.log"),
                now,
                Some(&root)
            ),
            json!({"ok":false,"code":"logging_unavailable"})
        );
        assert_eq!(
            journal.handle("capture", json!({"action":"status"})),
            failed
        );
        assert_eq!(
            std::fs::read_dir(&log_directory).unwrap().count(),
            files_before_duplicate
        );

        assert_eq!(
            journal.handle(
                "capture",
                json!({
                    "action":"record","level":"error","target":"capture","message":"write recovers"
                })
            )["sequence"],
            2
        );
        assert_eq!(
            journal.handle("capture", json!({"action":"status"}))["logging_sink"]["state"],
            "failed"
        );
        journal.recover_logging_if_due(Instant::now() + std::time::Duration::from_secs(5));
        assert_eq!(
            handle_request(
                &mut journal,
                &mut reader,
                HOST_CONFIGURATION_CALLER,
                apply(2, "first.log"),
                now,
                Some(&root)
            ),
            json!({"ok":true,"applied_revision":2})
        );
        assert_eq!(
            handle_request(
                &mut journal,
                &mut reader,
                HOST_CONFIGURATION_CALLER,
                apply(3, "third.log"),
                now,
                Some(&root)
            ),
            json!({"ok":true,"applied_revision":3})
        );
        assert_eq!(
            handle_request(
                &mut journal,
                &mut reader,
                HOST_CONFIGURATION_CALLER,
                apply(2, "first.log"),
                now,
                Some(&root)
            ),
            json!({"ok":false,"code":"stale_revision"})
        );
        assert_eq!(journal.logging_configuration_revision(), Some(3));
        drop(journal);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn signed_logging_revision_recovers_after_disk_path_returns() {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "amaterasu-diagnostics-signed-recovery-{}-{timestamp}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        let mut journal = Journal::unready();
        let mut reader = HostLoggingConfigurationReader::default();
        let now = Instant::now();
        let apply = || {
            json!({
                "action":"apply_host_logging",
                "configuration":configuration(&root, 7, "capture.log")
            })
        };
        let request = |journal: &mut Journal, reader: &mut HostLoggingConfigurationReader| {
            handle_request(
                journal,
                reader,
                HOST_CONFIGURATION_CALLER,
                apply(),
                now,
                Some(&root),
            )
        };
        let record = |message| json!({"action":"record","level":"error","target":"capture","message":message});
        let status = |journal: &mut Journal| journal.handle("capture", json!({"action":"status"}));

        assert_eq!(
            request(&mut journal, &mut reader),
            json!({"ok":true,"applied_revision":7})
        );
        journal.close_sink_for_test();
        let log_directory = root.join("logs");
        let saved_logs = root.join("saved_logs");
        std::fs::rename(&log_directory, &saved_logs).unwrap();
        std::fs::write(&log_directory, b"block session reopening").unwrap();

        assert_eq!(
            journal.handle("capture", record("during outage"))["sequence"],
            1
        );
        assert_eq!(
            status(&mut journal)["logging_sink"],
            json!({"state":"failed","failure":"write"})
        );
        assert_eq!(status(&mut journal)["logging_configuration_revision"], 7);
        assert_eq!(
            request(&mut journal, &mut reader),
            json!({"ok":false,"code":"logging_unavailable"})
        );
        let retry = Instant::now() + std::time::Duration::from_secs(5);
        journal.recover_logging_if_due(retry);
        assert_eq!(
            status(&mut journal)["logging_sink"],
            json!({"state":"failed","failure":"open"})
        );
        assert_eq!(
            request(&mut journal, &mut reader),
            json!({"ok":false,"code":"logging_unavailable"})
        );

        std::fs::remove_file(&log_directory).unwrap();
        std::fs::rename(&saved_logs, &log_directory).unwrap();
        journal.recover_logging_if_due(retry + std::time::Duration::from_secs(5));
        assert_eq!(
            status(&mut journal)["logging_sink"],
            json!({"state":"healthy","failure":null})
        );
        assert_eq!(status(&mut journal)["logging_configuration_revision"], 7);
        let files_after_recovery = std::fs::read_dir(&log_directory).unwrap().count();
        assert_eq!(
            request(&mut journal, &mut reader),
            json!({"ok":true,"applied_revision":7})
        );
        assert_eq!(
            std::fs::read_dir(&log_directory).unwrap().count(),
            files_after_recovery
        );
        assert_eq!(
            journal.handle("capture", record("after recovery"))["sequence"],
            2
        );
        let contents = std::fs::read_dir(&log_directory)
            .unwrap()
            .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(contents.matches("after recovery").count(), 1);
        assert!(!contents.contains("during outage"));

        drop(journal);
        std::fs::remove_dir_all(root).unwrap();
    }

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
        let payload = configuration(&state_root, 4, "amaterasu.log");

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
        let mut payload = configuration(&state_root, 4, "amaterasu.log");
        payload["effective"]["log_path"] =
            json!(state_root.join("elsewhere.log").to_string_lossy());

        assert!(persisted_logging_settings(&payload, Some(&state_root)).is_err());
    }

    #[test]
    fn effective_level_must_match_explicit_policy_or_provider_default() {
        let root = std::env::temp_dir().canonicalize().unwrap();
        let mut payload = configuration(&root, 4, "amaterasu.log");
        payload["values"]["min_log_level"] = json!("info");
        assert!(persisted_logging_settings(&payload, Some(&root)).is_err());

        payload["values"]["min_log_level"] = json!("product_default");
        payload["effective"]["min_log_level"] = json!("info");
        assert_eq!(
            persisted_logging_settings(&payload, Some(&root))
                .unwrap()
                .0
                .minimum_level,
            2
        );
        payload["effective"]["min_log_level"] = json!("debug");
        assert_eq!(
            persisted_logging_settings(&payload, Some(&root))
                .unwrap()
                .0
                .minimum_level,
            1
        );
        payload["effective"]["min_log_level"] = json!("warning");
        assert!(persisted_logging_settings(&payload, Some(&root)).is_err());
    }
}

amaterasu_module_sdk::export_module!(DiagnosticsModule);
