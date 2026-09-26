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
                    payload: json!({"ok":true,"ready":true,"applied_revision":5}),
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

    for tick_number in 1..=6 {
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
                    payload: json!({"ok":true,"ready":true}),
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
                (6, Command::Complete { payload, .. }) => {
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
    assert_eq!(response["logging"]["status"], "rolled_back");
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

/// A tiny service transport with state backed by a file survives reconstruction
/// of both the provider pipeline and the transport after an update.
struct LifecycleTransport {
    state_file: PathBuf,
    incoming: Option<Response>,
    child_results: BTreeMap<u64, Value>,
    next_child_id: u64,
    completed: Option<Value>,
    applied_revision: Option<u64>,
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
                            json!({"ok":true,"ready":true,"applied_revision":self.applied_revision})
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
fn durable_update_applies_logging_and_reloads_after_provider_restart() {
    let root = test_root();
    let state_file = root.join("host-configuration-test-state.json");
    let mut transport = LifecycleTransport::new(state_file.clone());
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
    assert_eq!(transport.events, ["read", "preflight", "save", "apply"]);

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
