use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

fn test_root() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("amaterasu-host-config-pipeline-{nonce}"));
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

fn request(caller: &str, payload: Value) -> Response {
    Response::Request {
        id: 7,
        caller: caller.into(),
        initiator: caller.into(),
        service: crate::SERVICE.into(),
        remaining_ms: 30_000,
        payload,
    }
}

#[test]
fn update_is_rejected_for_non_management_callers_before_state_access() {
    let mut pipeline = Pipeline::new(Ok(test_root()));
    let mut incoming = Some(request(
        "consumer.module",
        json!({
            "action":"update","expected_revision":0,
            "file_state":"state.json","vibeshine_file_state":"legacy.json",
            "credentials_file":"","pkey":"key.pem","cert":"cert.pem",
            "log_path":"service.log","min_log_level":"info"
        }),
    ));
    let mut completed = None;
    pipeline
        .tick(|command| match command {
            Command::Poll {} => Ok(incoming.take().unwrap_or(Response::Pending)),
            Command::Complete { payload, .. } => {
                completed = Some(payload);
                Ok(Response::Ok)
            }
            command => panic!("unexpected command for rejected update: {command:?}"),
        })
        .unwrap();
    assert_eq!(completed.unwrap()["error"]["code"], "permission_denied");
}

#[test]
fn missing_state_key_reads_as_the_default_document_without_a_write() {
    let root = test_root();
    let mut pipeline = Pipeline::new(Ok(root.clone()));
    let mut incoming = Some(request("consumer.module", json!({"action":"get"})));
    let mut completed = None;
    let mut state_read = false;
    let mut state_write = false;

    pipeline
        .tick(|command| match command {
            Command::Poll {} => Ok(incoming.take().unwrap_or(Response::Pending)),
            Command::Request {
                service, payload, ..
            } => {
                assert_eq!(service, crate::STATE_SERVICE);
                assert_eq!(payload["action"], "get");
                assert_eq!(payload["key"], STATE_KEY);
                state_read = true;
                Ok(Response::Accepted { id: 22 })
            }
            command => panic!("unexpected first tick command: {command:?}"),
        })
        .unwrap();

    pipeline
        .tick(|command| match command {
            Command::TakeResult { request: 22 } => Ok(Response::Result {
                payload: json!({"ok":true,"revision":4,"found":false,"value":null}),
            }),
            Command::ReleaseResult { request: 22 } => Ok(Response::Ok),
            Command::Complete { payload, .. } => {
                completed = Some(payload);
                Ok(Response::Ok)
            }
            Command::Poll {} => Ok(Response::Pending),
            Command::Request { .. } => {
                state_write = true;
                panic!("a read of the default document must not write state")
            }
            command => panic!("unexpected second tick command: {command:?}"),
        })
        .unwrap();

    let response = completed.unwrap();
    assert_eq!(response["ok"], true);
    assert_eq!(response["ready"], true);
    assert_eq!(response["revision"], 0);
    assert_eq!(response["values"]["file_state"], "sunshine_state.json");
    assert_eq!(
        response["effective"]["credentials_file"],
        root.join("sunshine_state.json").to_string_lossy().as_ref()
    );
    assert!(state_read);
    assert!(!state_write);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn management_update_uses_store_revision_cas_and_returns_only_after_confirmation() {
    let root = test_root();
    let mut pipeline = Pipeline::new(Ok(root.clone()));
    let mut incoming = Some(request(
        "amaterasu.management",
        json!({
            "action":"update","expected_revision":5,
            "file_state":"state.json","vibeshine_file_state":"legacy.json",
            "credentials_file":"","pkey":"key.pem","cert":"cert.pem",
            "log_path":"service.log","min_log_level":"warning"
        }),
    ));
    let mut state_read_revision = None;
    let mut state_write_payload = None;
    let mut completed = None;

    for tick_number in 1..=3 {
        pipeline
            .tick(|command| match (tick_number, command) {
                (1, Command::Poll {}) => Ok(incoming.take().unwrap_or(Response::Pending)),
                (
                    1,
                    Command::Request {
                        service, payload, ..
                    },
                ) => {
                    assert_eq!(service, crate::STATE_SERVICE);
                    assert_eq!(payload["action"], "get");
                    Ok(Response::Accepted { id: 22 })
                }
                (2, Command::TakeResult { request: 22 }) => {
                    let stored = Document::from_settings(5, crate::settings::Settings::default());
                    Ok(Response::Result {
                        payload: json!({
                            "ok":true,"revision":12,"found":true,"value":stored
                        }),
                    })
                }
                (2, Command::ReleaseResult { request: 22 }) => Ok(Response::Ok),
                (
                    2,
                    Command::Request {
                        service, payload, ..
                    },
                ) => {
                    assert_eq!(service, crate::STATE_SERVICE);
                    assert_eq!(payload["action"], "set");
                    assert_eq!(payload["key"], STATE_KEY);
                    assert_eq!(payload["expected_revision"], 12);
                    assert_eq!(payload["value"]["revision"], 6);
                    assert_eq!(payload["value"]["min_log_level"], "warning");
                    state_read_revision = Some(12);
                    state_write_payload = Some(payload);
                    Ok(Response::Accepted { id: 23 })
                }
                (3, Command::TakeResult { request: 23 }) => Ok(Response::Result {
                    payload: json!({"ok":true,"revision":13}),
                }),
                (3, Command::ReleaseResult { request: 23 }) => Ok(Response::Ok),
                (3, Command::Complete { payload, .. }) => {
                    completed = Some(payload);
                    Ok(Response::Ok)
                }
                (_, Command::Poll {}) => Ok(Response::Pending),
                (_, command) => panic!("unexpected command in update flow: {command:?}"),
            })
            .unwrap();
    }

    let response = completed.unwrap();
    assert_eq!(state_read_revision, Some(12));
    assert_eq!(state_write_payload.unwrap()["value"]["revision"], 6);
    assert_eq!(response["ok"], true);
    assert_eq!(response["revision"], 6);
    assert_eq!(response["values"]["log_path"], "service.log");
    assert_eq!(
        response["effective"]["log_path"],
        root.join("service.log").to_string_lossy().as_ref()
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn missing_or_invalid_trusted_root_fails_readiness_closed() {
    let mut pipeline = Pipeline::new(Err("trusted root missing".into()));
    let mut incoming = Some(request("consumer.module", json!({"action":"readiness"})));
    let mut completed = None;
    pipeline
        .tick(|command| match command {
            Command::Poll {} => Ok(incoming.take().unwrap_or(Response::Pending)),
            Command::Complete { payload, .. } => {
                completed = Some(payload);
                Ok(Response::Ok)
            }
            command => panic!("unavailable readiness must not call state: {command:?}"),
        })
        .unwrap();
    assert_eq!(completed.unwrap()["ready"], false);
}

#[test]
fn absent_and_malformed_state_documents_are_distinguished() {
    assert_eq!(
        read_document(&json!({"ok":true,"revision":0,"found":false,"value":null}))
            .unwrap()
            .revision,
        0
    );
    assert!(read_document(&json!({"ok":true,"revision":0,"found":true,"value":{}})).is_err());
}

#[test]
fn settings_that_exceed_the_runtime_response_bound_are_rejected() {
    let root = test_root();
    let oversized = "x".repeat(1600);
    let absolute_path = |suffix| {
        root.join(format!("{oversized}{suffix}"))
            .to_string_lossy()
            .into_owned()
    };
    let settings = crate::settings::Settings {
        file_state: absolute_path("a"),
        vibeshine_file_state: absolute_path("b"),
        credentials_file: absolute_path("c"),
        pkey: absolute_path("d"),
        cert: absolute_path("e"),
        log_path: absolute_path("f"),
        min_log_level: crate::settings::MinLogLevel::Info,
    };
    let document = Document::from_settings(1, settings);
    assert!(document_fits(&document));
    let response = build_read_response(&document, &root).unwrap_err();
    assert_eq!(response["error"]["code"], "resource_exhausted");
    std::fs::remove_dir_all(root).unwrap();
}
