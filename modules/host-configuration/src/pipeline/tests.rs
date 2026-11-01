use super::*;
use std::collections::BTreeMap;
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

fn assert_parent_traversal_update_is_rejected_before_state_access(field: &str) {
    let mut pipeline = Pipeline::new(Ok(test_root()));
    let mut payload = json!({
        "action":"update","expected_revision":0,
        "file_state":"state.json","vibeshine_file_state":"legacy.json",
        "credentials_file":"","pkey":"key.pem","cert":"cert.pem",
        "log_path":"service.log","min_log_level":"info"
    });
    payload[field] = json!("../outside.pem");

    let mut incoming = Some(request("amaterasu.management", payload));
    let mut completed = None;
    pipeline
        .tick(|command| match command {
            Command::Poll {} => Ok(incoming.take().unwrap_or(Response::Pending)),
            Command::Complete { payload, .. } => {
                completed = Some(payload);
                Ok(Response::Ok)
            }
            Command::Request { .. }
            | Command::Cancel { .. }
            | Command::TakeResult { .. }
            | Command::ReleaseResult { .. } => {
                panic!("invalid {field} update issued a state command: {command:?}")
            }
            command => panic!("unexpected command for invalid {field} update: {command:?}"),
        })
        .unwrap();

    assert_eq!(
        completed.unwrap()["error"]["code"],
        "invalid_settings",
        "{field} parent traversal must be rejected before state access"
    );
}

#[test]
fn parent_traversal_in_pkey_is_rejected_before_state_access() {
    assert_parent_traversal_update_is_rejected_before_state_access("pkey");
}

#[test]
fn parent_traversal_in_cert_is_rejected_before_state_access() {
    assert_parent_traversal_update_is_rejected_before_state_access("cert");
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

    for tick_number in 1..=5 {
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
                    assert_eq!(service, DIAGNOSTICS_SERVICE);
                    assert_eq!(payload, json!({"action":"status"}));
                    Ok(Response::Accepted { id: 23 })
                }
                (3, Command::TakeResult { request: 23 }) => Ok(Response::Result {
                    payload: json!({"ok":true,"ready":true,"logging_configuration_revision":5,"logging_sink":{"state":"healthy","failure":null}}),
                }),
                (3, Command::ReleaseResult { request: 23 }) => Ok(Response::Ok),
                (
                    3,
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
                    Ok(Response::Accepted { id: 24 })
                }
                (4, Command::TakeResult { request: 24 }) => Ok(Response::Result {
                    payload: json!({"ok":true,"revision":13}),
                }),
                (4, Command::ReleaseResult { request: 24 }) => Ok(Response::Ok),
                (
                    4,
                    Command::Request {
                        service, payload, ..
                    },
                ) => {
                    assert_eq!(service, DIAGNOSTICS_SERVICE);
                    assert_eq!(payload["action"], "apply_host_logging");
                    assert_eq!(payload["configuration"]["revision"], 6);
                    Ok(Response::Accepted { id: 25 })
                }
                (5, Command::TakeResult { request: 25 }) => Ok(Response::Result {
                    payload: json!({"ok":true,"applied_revision":6}),
                }),
                (5, Command::ReleaseResult { request: 25 }) => Ok(Response::Ok),
                (5, Command::Complete { payload, .. }) => {
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
    assert_eq!(response["logging"]["status"], "applied");
    assert_eq!(response["values"]["log_path"], "service.log");
    assert_eq!(
        response["effective"]["log_path"],
        root.join("service.log").to_string_lossy().as_ref()
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn denied_diagnostics_preflight_never_writes_configuration() {
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
    let mut completed = None;

    for tick_number in 1..=2 {
        pipeline
            .tick(|command| match (tick_number, command) {
                (1, Command::Poll {}) => Ok(incoming.take().unwrap()),
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
                (2, Command::TakeResult { request: 22 }) => Ok(Response::Result {
                    payload: json!({
                        "ok":true,"revision":12,"found":true,
                        "value":Document::from_settings(5, crate::settings::Settings::default())
                    }),
                }),
                (2, Command::ReleaseResult { request: 22 }) => Ok(Response::Ok),
                (
                    2,
                    Command::Request {
                        service, payload, ..
                    },
                ) => {
                    assert_eq!(service, DIAGNOSTICS_SERVICE);
                    assert_eq!(payload, json!({"action":"status"}));
                    Ok(Response::error("permission_denied", "grant absent"))
                }
                (2, Command::Complete { payload, .. }) => {
                    completed = Some(payload);
                    Ok(Response::Ok)
                }
                (_, Command::Poll {}) => Ok(Response::Pending),
                (_, Command::Request { service, .. }) if service == crate::STATE_SERVICE => {
                    panic!("preflight denial must not save configuration")
                }
                (_, command) => panic!("unexpected preflight command: {command:?}"),
            })
            .unwrap();
    }

    let response = completed.unwrap();
    assert_eq!(response["error"]["code"], "configuration_unavailable");
    assert_eq!(response["ready"], false);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_logging_apply_restores_previous_document_with_a_new_revision() {
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
    let mut completed = None;

    for tick_number in 1..=7 {
        pipeline
            .tick(|command| match (tick_number, command) {
                (1, Command::Poll {}) => Ok(incoming.take().unwrap()),
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
                (2, Command::TakeResult { request: 22 }) => Ok(Response::Result {
                    payload: json!({
                        "ok":true,"revision":12,"found":true,
                        "value":Document::from_settings(5, crate::settings::Settings::default())
                    }),
                }),
                (2, Command::ReleaseResult { request: 22 }) => Ok(Response::Ok),
                (
                    2,
                    Command::Request {
                        service, payload, ..
                    },
                ) => {
                    assert_eq!(service, DIAGNOSTICS_SERVICE);
                    assert_eq!(payload, json!({"action":"status"}));
                    Ok(Response::Accepted { id: 23 })
                }
                (3, Command::TakeResult { request: 23 }) => Ok(Response::Result {
                    payload: json!({"ok":true,"ready":true,"logging_configuration_revision":5,"logging_sink":{"state":"healthy","failure":null}}),
                }),
                (3, Command::ReleaseResult { request: 23 }) => Ok(Response::Ok),
                (3, Command::Request { payload, .. }) => {
                    assert_eq!(payload["action"], "set");
                    assert_eq!(payload["expected_revision"], 12);
                    assert_eq!(payload["value"]["revision"], 6);
                    Ok(Response::Accepted { id: 24 })
                }
                (4, Command::TakeResult { request: 24 }) => Ok(Response::Result {
                    payload: json!({"ok":true,"revision":13}),
                }),
                (4, Command::ReleaseResult { request: 24 }) => Ok(Response::Ok),
                (
                    4,
                    Command::Request {
                        service,
                        payload,
                        parent,
                        ..
                    },
                ) => {
                    assert_eq!(service, DIAGNOSTICS_SERVICE);
                    assert_eq!(payload["configuration"]["revision"], 6);
                    assert_eq!(parent, Some(7));
                    Ok(Response::Accepted { id: 25 })
                }
                (5, Command::TakeResult { request: 25 }) => Ok(Response::Result {
                    payload: json!({"ok":false,"error":{"code":"sink_unavailable"}}),
                }),
                (5, Command::ReleaseResult { request: 25 }) => Ok(Response::Ok),
                (
                    5,
                    Command::Request {
                        service,
                        payload,
                        parent,
                        ..
                    },
                ) => {
                    assert_eq!(service, crate::STATE_SERVICE);
                    assert_eq!(parent, None);
                    assert_eq!(payload["action"], "set");
                    assert_eq!(payload["expected_revision"], 13);
                    assert_eq!(payload["value"]["revision"], 7);
                    assert_eq!(payload["value"]["log_path"], "amaterasu.log");
                    assert_eq!(payload["value"]["min_log_level"], "product_default");
                    Ok(Response::Accepted { id: 26 })
                }
                (6, Command::TakeResult { request: 26 }) => Ok(Response::Result {
                    payload: json!({"ok":true,"revision":14}),
                }),
                (6, Command::ReleaseResult { request: 26 }) => Ok(Response::Ok),
                (
                    6,
                    Command::Request {
                        service,
                        payload,
                        parent,
                        ..
                    },
                ) => {
                    assert_eq!(service, DIAGNOSTICS_SERVICE);
                    assert_eq!(parent, Some(7));
                    assert_eq!(payload["action"], "apply_host_logging");
                    assert_eq!(payload["configuration"]["revision"], 7);
                    assert_eq!(
                        payload["configuration"]["values"]["log_path"],
                        "amaterasu.log"
                    );
                    Ok(Response::Accepted { id: 27 })
                }
                (7, Command::TakeResult { request: 27 }) => Ok(Response::Result {
                    payload: json!({"ok":true,"applied_revision":7}),
                }),
                (7, Command::ReleaseResult { request: 27 }) => Ok(Response::Ok),
                (7, Command::Complete { payload, .. }) => {
                    completed = Some(payload);
                    Ok(Response::Ok)
                }
                (_, Command::Poll {}) => Ok(Response::Pending),
                (_, command) => panic!("unexpected command in rollback flow: {command:?}"),
            })
            .unwrap();
    }

    let response = completed.unwrap();
    assert_eq!(response["error"]["code"], "logging_apply_failed");
    assert_eq!(response["logging"]["saved_revision"], 6);
    assert_eq!(response["logging"]["applied_revision"], 7);
    assert_eq!(response["logging"]["status"], "rolled_back");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn restored_state_without_confirmed_diagnostics_reapply_reports_divergence() {
    let root = test_root();
    let restored = Document::from_settings(7, crate::settings::Settings::default());
    let restored_configuration = build_read_response(&restored, &root).unwrap();
    let mut pipeline = Pipeline::new(Ok(root.clone()));
    pipeline.pending = Some(Pending {
        request_id: 7,
        deadline: Instant::now() + Duration::from_secs(30),
        state_request_id: 26,
        stage: Stage::RollingBack {
            saved_revision: 6,
            expected_state_revision: 13,
            original_deadline: Instant::now() + Duration::from_secs(30),
            restored_configuration: Some(restored_configuration),
        },
        phase: PendingPhase::Waiting,
    });
    let mut completed = None;

    for tick_number in 1..=2 {
        pipeline
            .tick(|command| match (tick_number, command) {
                (1, Command::TakeResult { request: 26 }) => Ok(Response::Result {
                    payload: json!({"ok":true,"revision":14}),
                }),
                (1, Command::ReleaseResult { request: 26 }) => Ok(Response::Ok),
                (
                    1,
                    Command::Request {
                        service, payload, ..
                    },
                ) => {
                    assert_eq!(service, DIAGNOSTICS_SERVICE);
                    assert_eq!(payload["action"], "apply_host_logging");
                    assert_eq!(payload["configuration"]["revision"], 7);
                    Ok(Response::Accepted { id: 27 })
                }
                (2, Command::TakeResult { request: 27 }) => Ok(Response::Result {
                    payload: json!({"ok":false,"code":"logging_unavailable"}),
                }),
                (2, Command::ReleaseResult { request: 27 }) => Ok(Response::Ok),
                (2, Command::Complete { payload, .. }) => {
                    completed = Some(payload);
                    Ok(Response::Ok)
                }
                (_, Command::Poll {}) => Ok(Response::Pending),
                (_, command) => panic!("unexpected compensation command: {command:?}"),
            })
            .unwrap();
    }

    let response = completed.unwrap();
    assert_eq!(response["error"]["code"], "logging_rollback_unconfirmed");
    assert_eq!(response["logging"]["restored_revision"], 7);
    assert_eq!(response["logging"]["applied_revision"], Value::Null);
    assert_eq!(
        response["logging"]["status"],
        "rollback_application_unconfirmed"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn ended_parent_retries_restored_logging_detached_without_claiming_rollback() {
    let root = test_root();
    let restored = Document::from_settings(7, crate::settings::Settings::default());
    let mut pipeline = Pipeline::new(Ok(root.clone()));
    pipeline.pending = Some(Pending {
        request_id: 7,
        deadline: Instant::now() + Duration::from_secs(30),
        state_request_id: 26,
        stage: Stage::RollingBack {
            saved_revision: 6,
            expected_state_revision: 13,
            original_deadline: Instant::now() + Duration::from_secs(30),
            restored_configuration: Some(build_read_response(&restored, &root).unwrap()),
        },
        phase: PendingPhase::Waiting,
    });
    let mut completed = None;
    let mut detached_applied = false;
    for tick_number in 1..=2 {
        pipeline
            .tick(|command| match (tick_number, command) {
                (1, Command::TakeResult { request: 26 }) => Ok(Response::Result {
                    payload: json!({"ok":true,"revision":14}),
                }),
                (1, Command::ReleaseResult { request: 26 }) => Ok(Response::Ok),
                (
                    1,
                    Command::Request {
                        service,
                        payload,
                        parent: Some(7),
                        operation_id,
                        ..
                    },
                ) => {
                    assert_eq!(service, DIAGNOSTICS_SERVICE);
                    assert_eq!(payload["configuration"]["revision"], 7);
                    assert!(operation_id.ends_with("restore-logging"));
                    Ok(Response::error(
                        "permission_denied",
                        "management parent ended",
                    ))
                }
                (
                    1,
                    Command::Request {
                        service,
                        payload,
                        parent: None,
                        operation_id,
                        ..
                    },
                ) => {
                    assert_eq!(service, DIAGNOSTICS_SERVICE);
                    assert_eq!(payload["configuration"]["revision"], 7);
                    assert!(operation_id.ends_with("restore-logging-detached"));
                    detached_applied = true;
                    Ok(Response::Accepted { id: 27 })
                }
                (2, Command::TakeResult { request: 27 }) => Ok(Response::Result {
                    payload: json!({"ok":true,"applied_revision":7}),
                }),
                (2, Command::ReleaseResult { request: 27 }) => Ok(Response::Ok),
                (2, Command::Complete { payload, .. }) => {
                    completed = Some(payload);
                    Ok(Response::Ok)
                }
                (_, Command::Poll {}) => Ok(Response::Pending),
                (_, command) => panic!("unexpected detached compensation command: {command:?}"),
            })
            .unwrap();
    }
    assert!(detached_applied);
    let response = completed.unwrap();
    assert_eq!(response["error"]["code"], "logging_rollback_unconfirmed");
    assert_eq!(response["logging"]["restored_revision"], 7);
    assert_eq!(response["logging"]["applied_revision"], Value::Null);
    assert_eq!(
        response["logging"]["status"],
        "rollback_application_unconfirmed"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn timed_out_logging_apply_reports_saved_revision_without_claiming_rollback() {
    let mut pipeline = Pipeline::new(Ok(test_root()));
    pipeline.pending = Some(Pending {
        request_id: 7,
        deadline: Instant::now() - Duration::from_secs(1),
        state_request_id: 24,
        stage: Stage::Applying {
            response: json!({"revision":6}),
            previous: Document::default(),
            saved_state_revision: 13,
        },
        phase: PendingPhase::Waiting,
    });
    let mut completed = None;
    pipeline
        .tick(|command| match command {
            Command::Cancel { request: 24 } | Command::ReleaseResult { request: 24 } => {
                Ok(Response::Ok)
            }
            Command::Complete { payload, .. } => {
                completed = Some(payload);
                Ok(Response::Ok)
            }
            Command::Poll {} => Ok(Response::Pending),
            Command::Request { .. } => panic!("ambiguous apply must not start a rollback"),
            command => panic!("unexpected command after timed-out apply: {command:?}"),
        })
        .unwrap();
    let response = completed.unwrap();
    assert_eq!(response["error"]["code"], "logging_application_unconfirmed");
    assert_eq!(response["logging"]["saved_revision"], 6);
    assert_eq!(response["logging"]["applied_revision"], Value::Null);
}

#[test]
fn accepted_state_errors_preserve_uncertain_save_and_rollback_outcomes() {
    let check = |stage: Stage, outcome: ChildOutcome, expected_code: &str| {
        let pending = Pending {
            request_id: 7,
            deadline: Instant::now() + Duration::from_secs(30),
            state_request_id: 25,
            stage,
            phase: PendingPhase::Waiting,
        };
        let mut completed = None;
        Pipeline::new(Err("unused trusted root".into()))
            .consume(pending, outcome, &mut |command| match command {
                Command::Complete { payload, .. } => {
                    completed = Some(payload);
                    Ok(Response::Ok)
                }
                command => panic!("uncertain result issued unexpected command: {command:?}"),
            })
            .unwrap();
        let response = completed.unwrap();
        assert_eq!(response["error"]["code"], expected_code);
        let expected_status = match expected_code {
            "logging_rollback_unconfirmed" => "rollback_unconfirmed",
            "logging_rollback_failed" => "rollback_failed",
            _ => "save_unconfirmed",
        };
        assert_eq!(response["logging"]["status"], expected_status);
        assert_eq!(
            response["logging"]["saved_revision"],
            if expected_code == "save_unconfirmed" {
                Value::Null
            } else {
                json!(6)
            }
        );
    };

    let saving = || Stage::Saving {
        expected_state_revision: 12,
        response: json!({"revision":6}),
        previous: Document::default(),
    };
    let rollback = || Stage::RollingBack {
        saved_revision: 6,
        expected_state_revision: 13,
        original_deadline: Instant::now() + Duration::from_secs(30),
        restored_configuration: None,
    };
    let transport_failure = || ChildOutcome::Failure {
        code: "deadline_exceeded".into(),
        message: "result expired after persistence".into(),
    };
    let storage_failure = || {
        ChildOutcome::Payload(json!({
            "ok":false,"error":{"code":"storage_error","message":"directory sync failed"}
        }))
    };

    check(saving(), transport_failure(), "save_unconfirmed");
    check(saving(), storage_failure(), "save_unconfirmed");
    check(
        rollback(),
        transport_failure(),
        "logging_rollback_unconfirmed",
    );
    check(
        rollback(),
        storage_failure(),
        "logging_rollback_unconfirmed",
    );
    check(
        rollback(),
        ChildOutcome::Payload(json!({
            "ok":false,"error":{"code":"revision_conflict","message":"CAS rejected"}
        })),
        "logging_rollback_failed",
    );
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

/// A file-backed service transport lets a new provider instance read the saved
/// document after the first instance completes a live logging update.
struct LifecycleTransport {
    state_file: PathBuf,
    incoming: Option<Response>,
    child_results: BTreeMap<u64, Value>,
    next_child_id: u64,
    completed: Option<Value>,
    applied_revision: Option<u64>,
    diagnostics_status: Value,
    events: Vec<&'static str>,
}

impl LifecycleTransport {
    fn new(state_file: PathBuf) -> Self {
        Self {
            state_file,
            incoming: None,
            child_results: BTreeMap::new(),
            next_child_id: 20,
            completed: None,
            applied_revision: None,
            diagnostics_status: json!({"ok":true,"ready":true,"logging_configuration_revision":null,"logging_sink":{"state":"healthy","failure":null}}),
            events: Vec::new(),
        }
    }

    fn stored(&self) -> (u64, Option<Document>) {
        match std::fs::read(&self.state_file) {
            Ok(bytes) => {
                let state: Value = serde_json::from_slice(&bytes).unwrap();
                (
                    state["revision"].as_u64().unwrap(),
                    Some(serde_json::from_value(state["value"].clone()).unwrap()),
                )
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (0, None),
            Err(error) => panic!("could not read test state: {error}"),
        }
    }

    fn send(&mut self, command: Command) -> TransportResult {
        match command {
            Command::Poll {} => Ok(self.incoming.take().unwrap_or(Response::Pending)),
            Command::Request {
                service, payload, ..
            } => {
                let result = if service == crate::STATE_SERVICE {
                    match payload["action"].as_str().unwrap() {
                        "get" => {
                            self.events.push("read");
                            let (revision, document) = self.stored();
                            json!({
                                "ok":true,"revision":revision,
                                "found":document.is_some(),"value":document
                            })
                        }
                        "set" => {
                            self.events.push("save");
                            let (revision, _) = self.stored();
                            assert_eq!(payload["expected_revision"], revision);
                            let document: Document =
                                serde_json::from_value(payload["value"].clone()).unwrap();
                            let next_revision = revision + 1;
                            std::fs::write(
                                &self.state_file,
                                serde_json::to_vec(&json!({
                                    "revision":next_revision,"value":document
                                }))
                                .unwrap(),
                            )
                            .unwrap();
                            json!({"ok":true,"revision":next_revision})
                        }
                        action => panic!("unexpected state action: {action}"),
                    }
                } else if service == DIAGNOSTICS_SERVICE {
                    match payload["action"].as_str().unwrap() {
                        "status" => {
                            self.events.push("preflight");
                            self.diagnostics_status.clone()
                        }
                        "apply_host_logging" => {
                            self.events.push("apply");
                            let configuration = &payload["configuration"];
                            let (_, saved) = self.stored();
                            let saved = saved.expect("logging apply follows durable save");
                            assert_eq!(configuration["revision"], saved.revision);
                            assert_eq!(configuration["values"]["log_path"], saved.log_path);
                            assert_eq!(
                                configuration["values"]["min_log_level"],
                                json!(saved.min_log_level)
                            );
                            self.applied_revision = Some(saved.revision);
                            self.diagnostics_status = json!({
                                "ok":true,"ready":true,
                                "logging_configuration_revision":saved.revision,
                                "logging_sink":{"state":"healthy","failure":null}
                            });
                            json!({"ok":true,"applied_revision":saved.revision})
                        }
                        action => panic!("unexpected diagnostics action: {action}"),
                    }
                } else {
                    panic!("unexpected service: {service}")
                };
                self.next_child_id += 1;
                self.child_results.insert(self.next_child_id, result);
                Ok(Response::Accepted {
                    id: self.next_child_id,
                })
            }
            Command::TakeResult { request } => Ok(Response::Result {
                payload: self.child_results.get(&request).unwrap().clone(),
            }),
            Command::ReleaseResult { request } => {
                assert!(self.child_results.remove(&request).is_some());
                Ok(Response::Ok)
            }
            Command::Complete { payload, .. } => {
                self.completed = Some(payload);
                Ok(Response::Ok)
            }
            command => panic!("unexpected lifecycle command: {command:?}"),
        }
    }

    fn complete(&mut self, pipeline: &mut Pipeline) -> Value {
        for _ in 0..12 {
            pipeline.tick(|command| self.send(command)).unwrap();
            if let Some(completed) = self.completed.take() {
                return completed;
            }
        }
        panic!("provider did not finish lifecycle request")
    }
}

#[test]
fn durable_update_applies_logging_then_provider_reload_reads_saved_settings() {
    let root = test_root();
    let state_file = root.join("host-configuration-test-state.json");
    let mut transport = LifecycleTransport::new(state_file.clone());
    transport.diagnostics_status = json!({
        "ok":true,"ready":false,"logging_configuration_revision":null,
        "logging_sink":{"state":"disabled","failure":null}
    });
    transport.incoming = Some(request(
        "amaterasu.management",
        json!({
            "action":"update","expected_revision":0,
            "file_state":"state.json","vibeshine_file_state":"legacy.json",
            "credentials_file":"","pkey":"key.pem","cert":"cert.pem",
            "log_path":"logs/live.log","min_log_level":"warning"
        }),
    ));
    let updated = transport.complete(&mut Pipeline::new(Ok(root.clone())));
    assert_eq!(updated["ok"], true);
    assert_eq!(updated["revision"], 1);
    assert_eq!(updated["logging"]["applied_revision"], 1);
    assert_eq!(transport.applied_revision, Some(1));
    assert_eq!(transport.diagnostics_status["ready"], true);
    assert_eq!(
        transport.diagnostics_status["logging_configuration_revision"],
        1
    );
    assert_eq!(transport.events, ["read", "preflight", "save", "apply"]);

    // A new provider instance must read the durable document, not cached settings.
    let mut restarted_transport = LifecycleTransport::new(state_file);
    restarted_transport.incoming = Some(request("consumer.module", json!({"action":"get"})));
    let reloaded = restarted_transport.complete(&mut Pipeline::new(Ok(root.clone())));
    assert_eq!(reloaded["ok"], true);
    assert_eq!(reloaded["revision"], 1);
    assert_eq!(reloaded["values"]["log_path"], "logs/live.log");
    assert_eq!(reloaded["values"]["min_log_level"], "warning");
    assert_eq!(
        reloaded["effective"]["log_path"],
        root.join("logs/live.log").to_string_lossy().as_ref()
    );
    assert_eq!(restarted_transport.events, ["read"]);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn preflight_rejects_later_unready_and_malformed_diagnostics_status() {
    let initial_status = json!({
        "ok":true,"ready":false,"logging_configuration_revision":null,
        "logging_sink":{"state":"disabled","failure":null}
    });
    let cases = [
        (1, false, initial_status.clone()),
        (0, true, initial_status.clone()),
        (
            0,
            false,
            json!({"ok":true,"ready":false,"logging_sink":{"state":"disabled","failure":null}}),
        ),
        (
            0,
            false,
            json!({"ok":true,"ready":false,"logging_configuration_revision":null}),
        ),
        (
            0,
            false,
            json!({"ok":true,"ready":false,"logging_configuration_revision":null,"logging_sink":{"state":"failed","failure":"open"}}),
        ),
        (1, false, json!({"ok":true,"ready":true})),
        (
            1,
            false,
            json!({"ok":true,"ready":true,"logging_configuration_revision":1}),
        ),
        (
            1,
            false,
            json!({"ok":true,"ready":true,"logging_configuration_revision":0,"logging_sink":{"state":"healthy","failure":null}}),
        ),
        (
            1,
            false,
            json!({"ok":true,"ready":true,"logging_configuration_revision":1,"logging_sink":{"state":"failed","failure":null}}),
        ),
    ];

    for (revision, nondefault_document, status) in cases {
        let root = test_root();
        let state_file = root.join("host-configuration-test-state.json");
        if revision != 0 || nondefault_document {
            let mut settings = crate::settings::Settings::default();
            if nondefault_document {
                settings.log_path = "old.log".into();
            }
            std::fs::write(
                &state_file,
                serde_json::to_vec(&json!({
                    "revision":revision,
                    "value":Document::from_settings(revision, settings)
                }))
                .unwrap(),
            )
            .unwrap();
        }
        let mut transport = LifecycleTransport::new(state_file);
        transport.diagnostics_status = status;
        transport.incoming = Some(request(
            "amaterasu.management",
            json!({
                "action":"update","expected_revision":revision,
                "file_state":"state.json","vibeshine_file_state":"legacy.json",
                "credentials_file":"","pkey":"key.pem","cert":"cert.pem",
                "log_path":"logs/live.log","min_log_level":"warning"
            }),
        ));
        let response = transport.complete(&mut Pipeline::new(Ok(root.clone())));
        assert_eq!(response["error"]["code"], "configuration_unavailable");
        assert_eq!(transport.stored().0, revision);
        assert_eq!(transport.events, ["read", "preflight"]);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn real_broker_reads_authoritative_configuration_while_update_waits() {
    use amaterasu_contracts::{Grants, RuntimeConfig};
    use amaterasu_runtime::Broker;
    use amaterasu_state::test_support::TestStateStore;

    let root = test_root();
    let state = TestStateStore::open(root.join("state")).unwrap();
    let mut config = RuntimeConfig::default();
    for (principal, provides, calls) in [
        (
            "amaterasu.host-configuration",
            vec![crate::SERVICE],
            vec![crate::STATE_SERVICE, DIAGNOSTICS_SERVICE],
        ),
        ("state", vec![crate::STATE_SERVICE], vec![]),
        ("diagnostics", vec![DIAGNOSTICS_SERVICE], vec![]),
        (
            "amaterasu.management",
            vec![],
            vec![crate::SERVICE, crate::STATE_SERVICE, DIAGNOSTICS_SERVICE],
        ),
        (
            "example.replacement-pairing",
            vec![],
            vec![crate::SERVICE, crate::STATE_SERVICE],
        ),
    ] {
        config.principals.insert(
            principal.into(),
            Grants {
                provide: provides.into_iter().map(str::to_owned).collect(),
                call: calls.into_iter().map(str::to_owned).collect(),
                ..Default::default()
            },
        );
    }
    let mut broker = Broker::new(config).unwrap();
    for (provider, service) in [
        ("amaterasu.host-configuration", crate::SERVICE),
        ("state", crate::STATE_SERVICE),
        ("diagnostics", DIAGNOSTICS_SERVICE),
    ] {
        assert!(matches!(
            broker.dispatch(
                provider,
                Command::Register {
                    service: service.into()
                }
            ),
            Response::Ok
        ));
    }
    let submit = |payload: Value, operation: &str| Command::Request {
        service: crate::SERVICE.into(),
        payload,
        timeout_ms: 30_000,
        operation_id: operation.into(),
        parent: None,
    };
    let Response::Accepted { id: update } = broker.dispatch(
        "amaterasu.management",
        submit(
            json!({
                "action":"update","expected_revision":0,
                "file_state":"state.json","vibeshine_file_state":"legacy.json",
                "credentials_file":"","pkey":"key.pem","cert":"cert.pem",
                "log_path":"service.log","min_log_level":"warning"
            }),
            "pending-update",
        ),
    ) else {
        panic!("update not accepted")
    };
    let mut pipeline = Pipeline::new(Ok(root.clone()));
    pipeline.enable_effect_authority("example.replacement-pairing".into());
    let tick = |pipeline: &mut Pipeline, broker: &mut Broker| {
        pipeline
            .tick(|command| Ok(broker.dispatch("amaterasu.host-configuration", command)))
            .unwrap()
    };
    let serve_state = |broker: &mut Broker| {
        let Response::Request {
            id,
            caller,
            payload,
            ..
        } = broker.dispatch("state", Command::Poll {})
        else {
            panic!("state request not delivered")
        };
        let payload = state.handle(&caller, payload);
        assert!(matches!(
            broker.dispatch(
                "state",
                Command::Complete {
                    request: id,
                    payload
                }
            ),
            Response::Ok
        ));
    };
    tick(&mut pipeline, &mut broker);
    serve_state(&mut broker);
    tick(&mut pipeline, &mut broker);
    assert!(matches!(
        broker.dispatch("diagnostics", Command::Poll {}),
        Response::Request { .. }
    ));
    let Response::Accepted { id: read } = broker.dispatch(
        "example.replacement-pairing",
        submit(json!({"action":"get"}), "concurrent-read"),
    ) else {
        panic!("read not accepted")
    };
    tick(&mut pipeline, &mut broker);
    serve_state(&mut broker);
    tick(&mut pipeline, &mut broker);
    assert!(
        matches!(broker.dispatch("example.replacement-pairing", Command::TakeResult { request: read }), Response::Result { payload } if payload["ok"] == true && payload["revision"] == 0)
    );
    assert!(matches!(
        broker.dispatch(
            "amaterasu.management",
            Command::TakeResult { request: update }
        ),
        Response::Pending
    ));
    let request = amaterasu_module_sdk::effect_authority::HostConfigurationTransactionRequest::TransactionStatus {};
    let payload = serde_json::to_value(request).unwrap();
    let Response::Accepted { id: status } = broker.dispatch(
        "example.replacement-pairing",
        submit(payload, "concurrent-status"),
    ) else {
        panic!("transaction status not accepted")
    };
    tick(&mut pipeline, &mut broker);
    serve_state(&mut broker);
    tick(&mut pipeline, &mut broker);
    let Response::Result { payload } = broker.dispatch(
        "example.replacement-pairing",
        Command::TakeResult { request: status },
    ) else {
        panic!("transaction status missing")
    };
    let consume = |payload: Value| {
        let decoded: amaterasu_module_sdk::effect_authority::HostConfigurationTransactionStatus =
            serde_json::from_value(payload).unwrap();
        assert!(decoded.valid());
        assert_eq!(decoded.configuration_revision, 0);
        assert!(decoded.transaction.is_none());
    };
    consume(payload);
    // The unchanged SDK consumer also accepts an independently implemented provider.
    let replacement =
        |request: amaterasu_module_sdk::effect_authority::HostConfigurationTransactionRequest| {
            match request {
            amaterasu_module_sdk::effect_authority::HostConfigurationTransactionRequest::TransactionStatus {} => {
                json!({"ok":true,"configuration_revision":0,"state_revision":0,"identity_fingerprint":Document::default().identity_fingerprint().unwrap(),"transaction":null,"terminal_receipt":null})
            }
            _ => panic!("unexpected replacement request"),
        }
        };
    consume(replacement(amaterasu_module_sdk::effect_authority::HostConfigurationTransactionRequest::TransactionStatus {}));
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn concurrent_read_transport_failure_preserves_pending_update() {
    let root = test_root();
    let mut pipeline = Pipeline::new(Ok(root.clone()));
    let deadline = Instant::now() + Duration::from_secs(30);
    pipeline.pending = Some(Pending {
        request_id: 7,
        deadline,
        state_request_id: 21,
        stage: Stage::Applying {
            response: json!({"revision": 1}),
            previous: Document::default(),
            saved_state_revision: 1,
        },
        phase: PendingPhase::Waiting,
    });
    pipeline.concurrent_read = Some(Pending {
        request_id: 8,
        deadline,
        state_request_id: 22,
        stage: Stage::Loading(Operation::Get {}),
        phase: PendingPhase::Waiting,
    });
    assert!(
        pipeline
            .tick(|command| match command {
                Command::TakeResult { request: 21 } => Ok(Response::Pending),
                Command::TakeResult { request: 22 } => Ok(Response::Result {
                    payload: json!({"ok":true,"revision":0,"found":false,"value":null}),
                }),
                Command::ReleaseResult { request: 22 } => Err("transport failure".into()),
                command => panic!("unexpected command: {command:?}"),
            })
            .is_err()
    );
    assert_eq!(pipeline.pending.as_ref().unwrap().request_id, 7);
    assert_eq!(pipeline.concurrent_read.as_ref().unwrap().request_id, 8);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn reconciling_terminal_receipt_does_not_clear_a_newer_active_marker() {
    let root = test_root();
    let mut pipeline = Pipeline::new(Ok(root.clone()));
    let old = ConfigurationChange {
        version: 1,
        authority_epoch: "a".repeat(32),
        operation_id: "old-operation".into(),
        expected_configuration_revision: 0,
        expected_trust_revision: 4,
    };
    let newer = ConfigurationChange {
        version: 1,
        authority_epoch: "b".repeat(32),
        operation_id: "new-operation".into(),
        expected_configuration_revision: 1,
        expected_trust_revision: 4,
    };
    let mut document = Document::from_settings(1, crate::settings::Settings::default());
    let fingerprint = document.identity_fingerprint().unwrap();
    document.terminal_receipt = Some(crate::settings::ConfigurationTerminalReceipt {
        transaction: ConfigurationTransaction {
            change: old.clone(),
            state: ConfigurationTransactionState::Committed {
                configuration_revision: 1,
            },
            expected_state_revision: 1,
            settled: true,
        },
        terminal_state_revision: 4,
        identity_fingerprint: fingerprint,
    });
    document.transaction = Some(ConfigurationTransaction {
        change: newer.clone(),
        state: ConfigurationTransactionState::Prepared {},
        expected_state_revision: 5,
        settled: false,
    });
    document.validate().unwrap();
    let mut outgoing = Vec::new();
    pipeline
        .reconcile_transaction(
            7,
            Instant::now() + Duration::from_secs(30),
            5,
            document,
            old.clone(),
            &mut |command| {
                outgoing.push(command.clone());
                Ok(match command {
                    Command::CheckRequest { .. } => Response::Ok,
                    Command::Request { .. } => Response::Accepted { id: 22 },
                    command => panic!("unexpected command: {command:?}"),
                })
            },
        )
        .unwrap();
    assert!(
        matches!(&outgoing[1], Command::Request { payload, .. } if payload["action"] == "status")
    );
    let pending = pipeline.pending.take().unwrap();
    pipeline
        .advance(pending, &mut |command| {
            Ok(match command {
                Command::TakeResult { request: 22 } => Response::Result {
                    payload: json!({"ok":true,"epoch":"c".repeat(32)}),
                },
                Command::ReleaseResult { request: 22 } | Command::CheckRequest { request: 7 } => {
                    Response::Ok
                }
                Command::Request { payload, .. } => {
                    assert_eq!(payload["change"]["operation_id"], old.operation_id);
                    Response::Accepted { id: 23 }
                }
                command => panic!("unexpected command: {command:?}"),
            })
        })
        .unwrap();
    let pending = pipeline.pending.take().unwrap();
    let mut response = None;
    pipeline
        .advance(pending, &mut |command| {
            Ok(match command {
                Command::TakeResult { request: 23 } => Response::Result {
                    payload: json!({"ok":true,"state":"released","operation_id":old.operation_id}),
                },
                Command::ReleaseResult { request: 23 } => Response::Ok,
                Command::Complete { payload, .. } => {
                    response = Some(payload);
                    Response::Ok
                }
                command => panic!("new active marker must not be written: {command:?}"),
            })
        })
        .unwrap();
    assert_eq!(response.unwrap()["ok"], true);
    assert!(pipeline.pending.is_none());
    std::fs::remove_dir_all(root).unwrap();
}
