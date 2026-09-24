use super::*;

fn request() -> Response {
    Response::Request {
        id: 7,
        caller: "caller.one".into(),
        initiator: "original".into(),
        service: crate::SERVICE.into(),
        remaining_ms: 30_000,
        payload: json!({"action":"register","fields":{"enabled":{"type":"boolean","default":true}}}),
    }
}

#[test]
fn oversized_documents_fail_before_submitting_a_state_write() {
    let fields = json!({
        "first":{"type":"string","default":"x".repeat(2200)},
        "second":{"type":"string","default":"x".repeat(2200)},
        "third":{"type":"string","default":"x".repeat(2200)}
    });
    let mut pipeline = Pipeline {
        pending: Some(Pending {
            request_id: 7,
            key: "principal/caller".into(),
            deadline: Instant::now() + Duration::from_secs(30),
            state_request_id: 20,
            stage: Stage::Loading(
                serde_json::from_value(json!({"action":"register","fields":fields})).unwrap(),
            ),
            phase: PendingPhase::Waiting,
        }),
        ..Default::default()
    };
    let mut rejected = false;
    pipeline
        .tick(|command| {
            Ok(match command {
                Command::ReleaseResult { .. } => Response::Ok,
                Command::TakeResult { .. } => Response::Result {
                    payload: json!({"ok":true,"revision":0,"found":false}),
                },
                Command::Complete { payload, .. } => {
                    assert_eq!(payload["error"]["code"], "resource_exhausted");
                    rejected = true;
                    Response::Ok
                }
                Command::Poll {} => Response::Pending,
                unexpected => panic!("must not persist oversized document: {unexpected:?}"),
            })
        })
        .unwrap();
    assert!(rejected);
}

#[test]
fn registration_waits_for_persistence_and_preserves_parent_authority() {
    let mut pipeline = Pipeline::default();
    let mut stage = 0;
    let mut completed = false;
    let mut transport = |command| {
        Ok(match command {
            Command::ReleaseResult { .. } => Response::Ok,
            Command::Poll {} if stage == 0 => {
                stage = 1;
                request()
            }
            Command::Poll {} => Response::Pending,
            Command::Request {
                parent,
                service,
                payload,
                timeout_ms,
                ..
            } => {
                assert_eq!(parent, Some(7));
                assert_eq!(service, STATE_SERVICE);
                assert!((1..=30_000).contains(&timeout_ms));
                assert_eq!(payload["key"], "principal/caller.one");
                if stage == 1 {
                    assert_eq!(payload["action"], "get");
                    stage = 2;
                    Response::Accepted { id: 20 }
                } else {
                    assert_eq!(payload["action"], "set");
                    assert_eq!(payload["expected_revision"], 11);
                    assert_eq!(payload["value"]["values"]["enabled"], true);
                    stage = 4;
                    Response::Accepted { id: 21 }
                }
            }
            Command::TakeResult { request: 20 } if stage == 2 => {
                stage = 3;
                Response::Pending
            }
            Command::TakeResult { request: 20 } => Response::Result {
                payload: json!({"ok":true,"revision":11,"found":false,"value":null}),
            },
            Command::TakeResult { request: 21 } => Response::Result {
                payload: json!({"ok":true,"revision":12}),
            },
            Command::Publish { topic, payload } => {
                assert_eq!(stage, 4);
                assert_eq!(topic, crate::CHANGED_TOPIC);
                assert_eq!(payload, json!({"owner":"caller.one","revision":1}));
                Response::Ok
            }
            Command::Complete { request, payload } => {
                assert_eq!(request, 7);
                assert_eq!(stage, 4);
                assert_eq!(payload["ok"], true);
                assert_eq!(payload["revision"], 1);
                completed = true;
                Response::Ok
            }
            unexpected => panic!("unexpected command: {unexpected:?}"),
        })
    };
    for _ in 0..4 {
        pipeline.tick(&mut transport).unwrap();
    }
    assert!(completed);
}

#[test]
fn failed_persistence_is_returned_without_success() {
    let mut pipeline = Pipeline::default();
    let mut polled = false;
    let mut failure_returned = false;
    for _ in 0..3 {
        pipeline
            .tick(|command| {
                Ok(match command {
                    Command::ReleaseResult { .. } => Response::Ok,
                    Command::Poll {} if !polled => {
                        polled = true;
                        request()
                    }
                    Command::Poll {} => Response::Pending,
                    Command::Request { payload, .. } => Response::Accepted {
                        id: if payload["action"] == "get" { 20 } else { 21 },
                    },
                    Command::TakeResult { request: 20 } => Response::Result {
                        payload: json!({"ok":true,"revision":0,"found":false}),
                    },
                    Command::TakeResult { request: 21 } => Response::Result {
                        payload: error("io_error", "disk write failed"),
                    },
                    Command::Complete { payload, .. } => {
                        assert_eq!(payload["ok"], false);
                        assert_eq!(payload["error"]["code"], "io_error");
                        failure_returned = true;
                        Response::Ok
                    }
                    unexpected => panic!("unexpected command: {unexpected:?}"),
                })
            })
            .unwrap();
    }
    assert!(failure_returned);
    assert!(pipeline.pending.is_none());
}

#[test]
fn cancellation_and_malformed_state_responses_do_not_stall_pipeline() {
    for response in [
        Response::error("cancelled", "parent ended"),
        Response::Result {
            payload: json!({"ok":true}),
        },
    ] {
        let mut pipeline = Pipeline {
            pending: Some(Pending {
                request_id: 7,
                key: "principal/caller".into(),
                deadline: Instant::now(),
                state_request_id: 20,
                stage: Stage::Loading(Operation::Get {}),
                phase: PendingPhase::Waiting,
            }),
            ..Default::default()
        };
        let mut completed = false;
        pipeline
            .tick(|command| {
                Ok(match command {
                    Command::ReleaseResult { .. } => Response::Ok,
                    Command::TakeResult { .. } => response.clone(),
                    Command::Complete { payload, .. } => {
                        assert_eq!(payload["ok"], false);
                        completed = true;
                        Response::error("request_ended", "already cancelled")
                    }
                    Command::Poll {} => Response::Pending,
                    unexpected => panic!("unexpected command: {unexpected:?}"),
                })
            })
            .unwrap();
        assert!(completed);
        assert!(pipeline.pending.is_none());
    }
}

#[test]
fn queued_work_is_bounded_while_state_is_pending() {
    let mut pipeline = Pipeline::default();
    let mut received = 0;
    for _ in 0..100 {
        pipeline
            .tick(|command| {
                Ok(match command {
                    Command::ReleaseResult { .. } => Response::Ok,
                    Command::Poll {} => {
                        received += 1;
                        request()
                    }
                    Command::Request { .. } => Response::Accepted { id: 20 },
                    Command::TakeResult { .. } => Response::Pending,
                    unexpected => panic!("unexpected command: {unexpected:?}"),
                })
            })
            .unwrap();
    }
    assert_eq!(received, MAX_QUEUED + 1);
    assert_eq!(pipeline.queued.len(), MAX_QUEUED);
}

fn registered_document() -> Document {
    serde_json::from_value(json!({"revision":4,"fields":{"title":{"type":"string","default":"default"}},"values":{"title":"private value"}})).unwrap()
}

fn waiting_save() -> Pipeline {
    Pipeline {
        pending: Some(Pending {
            request_id: 7,
            key: "principal/team/worker".into(),
            deadline: Instant::now() + Duration::from_secs(30),
            state_request_id: 21,
            stage: Stage::Saving(registered_document()),
            phase: PendingPhase::Waiting,
        }),
        ..Default::default()
    }
}

#[test]
fn change_events_wait_for_durable_ack_and_survive_cancelled_completion() {
    let mut pipeline = waiting_save();
    pipeline
        .tick(|command| {
            Ok(match command {
                Command::ReleaseResult { .. } => Response::Ok,
                Command::TakeResult { .. } | Command::Poll {} => Response::Pending,
                other => {
                    panic!("must not publish or complete before state acknowledgement: {other:?}")
                }
            })
        })
        .unwrap();
    assert!(pipeline.pending.is_some());
    let mut published = false;
    let mut completed = false;
    pipeline
        .tick(|command| {
            Ok(match command {
                Command::ReleaseResult { .. } => Response::Ok,
                Command::TakeResult { .. } => Response::Result {
                    payload: json!({"ok":true,"revision":11}),
                },
                Command::Publish { topic, payload } => {
                    assert_eq!(topic, crate::CHANGED_TOPIC);
                    assert_eq!(payload, json!({"owner":"team/worker","revision":4}));
                    published = true;
                    Response::Ok
                }
                Command::Complete { payload, .. } => {
                    assert!(published);
                    assert_eq!(payload["ok"], true);
                    completed = true;
                    // The mutation committed even if the parent expired in the meantime.
                    Response::error("request_ended", "caller cancelled")
                }
                Command::Poll {} => Response::Pending,
                other => panic!("unexpected: {other:?}"),
            })
        })
        .unwrap();
    assert!(published && completed);
    assert!(pipeline.pending.is_none());
}

#[test]
fn failed_or_cancelled_state_ack_never_publishes_change_events() {
    for response in [
        Response::error("cancelled", "parent cancelled"),
        Response::Result {
            payload: error("storage_error", "write failed"),
        },
    ] {
        let mut pipeline = waiting_save();
        let mut completed = false;
        pipeline
            .tick(|command| {
                Ok(match command {
                    Command::ReleaseResult { .. } => Response::Ok,
                    Command::TakeResult { .. } => response.clone(),
                    Command::Complete { payload, .. } => {
                        assert_eq!(payload["ok"], false);
                        completed = true;
                        Response::Ok
                    }
                    Command::Poll {} => Response::Pending,
                    other => panic!("unconfirmed persistence must not publish: {other:?}"),
                })
            })
            .unwrap();
        assert!(completed);
    }
}

#[test]
fn publication_failure_never_changes_the_persisted_success() {
    for publication in [
        Ok(Response::error("permission_denied", "no event grant")),
        Err("event transport unavailable".into()),
    ] {
        let mut pipeline = waiting_save();
        let mut completed = false;
        pipeline
            .tick(|command| match command {
                Command::TakeResult { .. } => Ok(Response::Result {
                    payload: json!({"ok":true,"revision":11}),
                }),
                Command::ReleaseResult { .. } => Ok(Response::Ok),
                Command::Publish { .. } => publication.clone(),
                Command::Complete { payload, .. } => {
                    assert_eq!(payload["ok"], true);
                    assert_eq!(payload["revision"], 4);
                    assert_eq!(payload["values"]["title"], "private value");
                    completed = true;
                    Ok(Response::Ok)
                }
                Command::Poll {} => Ok(Response::Pending),
                other => panic!("unexpected: {other:?}"),
            })
            .unwrap();
        assert!(completed);
    }
}

#[test]
fn get_and_idempotent_registration_do_not_publish_or_persist() {
    for operation in [
        Operation::Get {},
        Operation::Register {
            fields: registered_document().fields,
        },
    ] {
        let mut pipeline = Pipeline {
            pending: Some(Pending {
                request_id: 7,
                key: "principal/caller".into(),
                deadline: Instant::now() + Duration::from_secs(30),
                state_request_id: 20,
                stage: Stage::Loading(operation),
                phase: PendingPhase::Waiting,
            }),
            ..Default::default()
        };
        pipeline.tick(|command| Ok(match command {
                Command::ReleaseResult { .. } => Response::Ok,
            Command::TakeResult { .. } => Response::Result { payload: json!({"ok":true,"revision":11,"found":true,"value":registered_document()}) },
            Command::Complete { payload, .. } => { assert_eq!(payload["revision"], 4); Response::Ok },
            Command::Poll {} => Response::Pending,
            other => panic!("read must not publish or save: {other:?}"),
        })).unwrap();
        assert!(pipeline.pending.is_none());
    }
}

#[test]
fn migration_persists_one_complete_document_before_notification() {
    let operation = serde_json::from_value(json!({"action":"migrate","expected_revision":4,"fields":{"label":{"type":"string","default":"new"}},"renames":{"title":"label"}})).unwrap();
    let mut pipeline = Pipeline {
        pending: Some(Pending {
            request_id: 7,
            key: "principal/caller.one".into(),
            deadline: Instant::now() + Duration::from_secs(30),
            state_request_id: 20,
            stage: Stage::Loading(operation),
            phase: PendingPhase::Waiting,
        }),
        ..Default::default()
    };
    let mut durable = None;
    pipeline.tick(|command| Ok(match command {
                Command::ReleaseResult { .. } => Response::Ok,
        Command::TakeResult { request:20 } => Response::Result { payload:json!({"ok":true,"revision":91,"found":true,"value":registered_document()}) },
        Command::Request { payload, parent, .. } => {
            assert_eq!(parent, Some(7));
            assert_eq!(payload["action"], "set");
            assert_eq!(payload["key"], "principal/caller.one");
            assert_eq!(payload["expected_revision"], 91);
            assert_eq!(payload["value"]["revision"], 5);
            assert_eq!(payload["value"]["values"], json!({"label":"private value"}));
            durable = Some(payload["value"].clone());
            Response::Accepted { id:21 }
        }
        Command::Poll {} => Response::Pending,
        other => panic!("migration not yet durable: {other:?}"),
    })).unwrap();
    assert!(durable.is_some());
    let mut published = false;
    pipeline
        .tick(|command| {
            Ok(match command {
                Command::ReleaseResult { .. } => Response::Ok,
                Command::TakeResult { request: 21 } => Response::Result {
                    payload: json!({"ok":true,"revision":92}),
                },
                Command::Publish { payload, .. } => {
                    assert_eq!(payload, json!({"owner":"caller.one","revision":5}));
                    published = true;
                    Response::Ok
                }
                Command::Complete { payload, .. } => {
                    assert!(published);
                    assert_eq!(payload["values"], durable.as_ref().unwrap()["values"]);
                    Response::Ok
                }
                Command::Poll {} => Response::Pending,
                other => panic!("unexpected: {other:?}"),
            })
        })
        .unwrap();
    assert!(published);
}

fn product_request(caller: &str, payload: Value) -> Response {
    Response::Request {
        id: 37,
        caller: caller.into(),
        initiator: "management-user".into(),
        service: crate::SERVICE.into(),
        remaining_ms: 30_000,
        payload,
    }
}

fn product_document(revision: u64, value: &str) -> ProductDocument {
    serde_json::from_value(json!({
        "version": 2,
        "revision": revision,
        "values": {"hostname": value}
    }))
    .unwrap()
}

#[test]
fn product_storage_namespace_comes_from_runtime_caller_and_rejects_owner_selectors() {
    for caller in ["amaterasu.nvhttp", "amaterasu.launch"] {
        let mut pipeline = Pipeline::default();
        let mut received_key = None;
        pipeline
            .tick(|command| {
                Ok(match command {
                    Command::ReleaseResult { .. } => Response::Ok,
                    Command::Poll {} => product_request(caller, json!({"action":"product_get"})),
                    Command::Request {
                        payload, parent, ..
                    } => {
                        assert_eq!(parent, Some(37));
                        received_key = payload
                            .get("key")
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                        assert_eq!(payload["action"], "get");
                        Response::Accepted { id: 100 }
                    }
                    Command::TakeResult { .. } => Response::Pending,
                    other => panic!("unexpected: {other:?}"),
                })
            })
            .unwrap();
        assert_eq!(
            received_key.as_deref(),
            Some(format!("product/{caller}").as_str())
        );
    }

    for payload in [
        json!({"action":"product_get","owner":"amaterasu.launch"}),
        json!({"action":"product_update","owner":"amaterasu.launch","expected_revision":0,"version":1,"values":{}}),
        json!({"action":"product_update","expected_revision":0,"version":1,"values":[]}),
    ] {
        let mut pipeline = Pipeline::default();
        let mut rejected = false;
        pipeline
            .tick(|command| {
                Ok(match command {
                    Command::ReleaseResult { .. } => Response::Ok,
                    Command::Poll {} => product_request("amaterasu.nvhttp", payload.clone()),
                    Command::Complete { payload, .. } => {
                        assert_eq!(payload["error"]["code"], "invalid_request");
                        rejected = true;
                        Response::Ok
                    }
                    other => {
                        panic!("malformed product request must stop before storage: {other:?}")
                    }
                })
            })
            .unwrap();
        assert!(rejected);
    }
}

#[test]
fn product_update_uses_state_cas_and_returns_the_committed_document() {
    let update = json!({
        "action":"product_update",
        "expected_revision":0,
        "version":2,
        "values":{"hostname":"game-host.local"}
    });
    let mut pipeline = Pipeline::default();
    let mut stage = 0;
    pipeline
        .tick(|command| {
            Ok(match command {
                Command::ReleaseResult { .. } => Response::Ok,
                Command::Poll {} if stage == 0 => {
                    stage = 1;
                    product_request("amaterasu.nvhttp", update.clone())
                }
                Command::Poll {} => Response::Pending,
                Command::Request {
                    payload, parent, ..
                } => {
                    assert_eq!(parent, Some(37));
                    assert_eq!(payload["action"], "get");
                    assert_eq!(payload["key"], "product/amaterasu.nvhttp");
                    Response::Accepted { id: 100 }
                }
                other => panic!("unexpected before product load: {other:?}"),
            })
        })
        .unwrap();

    pipeline
        .tick(|command| {
            Ok(match command {
                Command::ReleaseResult { .. } => Response::Ok,
                Command::TakeResult { request: 100 } => Response::Result {
                    payload: json!({"ok":true,"revision":11,"found":false}),
                },
                Command::Request {
                    payload, parent, ..
                } => {
                    assert_eq!(parent, Some(37));
                    assert_eq!(payload["action"], "set");
                    assert_eq!(payload["key"], "product/amaterasu.nvhttp");
                    assert_eq!(payload["expected_revision"], 11);
                    assert_eq!(
                        payload["value"],
                        json!({
                            "version":2,
                            "revision":1,
                            "values":{"hostname":"game-host.local"}
                        })
                    );
                    Response::Accepted { id: 101 }
                }
                Command::Poll {} => Response::Pending,
                other => panic!("unexpected during product save: {other:?}"),
            })
        })
        .unwrap();

    let mut published = false;
    let mut completed = false;
    pipeline
        .tick(|command| {
            Ok(match command {
                Command::ReleaseResult { .. } => Response::Ok,
                Command::TakeResult { request: 101 } => Response::Result {
                    payload: json!({"ok":true,"revision":12}),
                },
                Command::Publish { topic, payload } => {
                    assert_eq!(topic, crate::CHANGED_TOPIC);
                    assert_eq!(
                        payload,
                        json!({
                            "owner":"amaterasu.nvhttp",
                            "revision":1,
                            "kind":"product_settings"
                        })
                    );
                    published = true;
                    Response::Ok
                }
                Command::Complete { payload, .. } => {
                    assert!(published);
                    assert_eq!(
                        payload,
                        json!({
                            "ok":true,
                            "found":true,
                            "version":2,
                            "revision":1,
                            "values":{"hostname":"game-host.local"}
                        })
                    );
                    completed = true;
                    Response::Ok
                }
                Command::Poll {} => Response::Pending,
                other => panic!("unexpected after product save: {other:?}"),
            })
        })
        .unwrap();
    assert!(published && completed);
}

#[test]
fn product_conflicts_and_malformed_stored_values_never_write() {
    let operation = serde_json::from_value(json!({
        "action":"product_update",
        "expected_revision":1,
        "version":2,
        "values":{"hostname":"new.local"}
    }))
    .unwrap();
    let mut pipeline = Pipeline {
        pending: Some(Pending {
            request_id: 37,
            key: "product/amaterasu.nvhttp".into(),
            deadline: Instant::now() + Duration::from_secs(30),
            state_request_id: 100,
            stage: Stage::Loading(operation),
            phase: PendingPhase::Waiting,
        }),
        ..Default::default()
    };
    let mut conflict = false;
    pipeline
        .tick(|command| {
            Ok(match command {
                Command::ReleaseResult { .. } => Response::Ok,
                Command::TakeResult { .. } => Response::Result {
                    payload: json!({"ok":true,"revision":11,"found":true,"value":product_document(3,"old.local")}),
                },
                Command::Complete { payload, .. } => {
                    assert_eq!(payload["error"]["code"], "revision_conflict");
                    assert_eq!(payload["error"]["current_revision"], 3);
                    conflict = true;
                    Response::Ok
                }
                Command::Poll {} => Response::Pending,
                other => panic!("conflicting product update must not write: {other:?}"),
            })
        })
        .unwrap();
    assert!(conflict);

    let mut pipeline = Pipeline {
        pending: Some(Pending {
            request_id: 38,
            key: "product/amaterasu.nvhttp".into(),
            deadline: Instant::now() + Duration::from_secs(30),
            state_request_id: 102,
            stage: Stage::Loading(Operation::ProductGet {}),
            phase: PendingPhase::Waiting,
        }),
        ..Default::default()
    };
    let mut invalid_state = false;
    pipeline
        .tick(|command| {
            Ok(match command {
                Command::ReleaseResult { .. } => Response::Ok,
                Command::TakeResult { .. } => Response::Result {
                    payload: json!({
                        "ok":true,
                        "revision":12,
                        "found":true,
                        "value":{"version":0,"revision":1,"values":{}}
                    }),
                },
                Command::Complete { payload, .. } => {
                    assert_eq!(payload["error"]["code"], "invalid_state_response");
                    invalid_state = true;
                    Response::Ok
                }
                Command::Poll {} => Response::Pending,
                other => {
                    panic!("malformed stored product document must not be returned: {other:?}")
                }
            })
        })
        .unwrap();
    assert!(invalid_state);
}

#[test]
fn oversized_product_documents_fail_before_submitting_a_state_write() {
    let operation = serde_json::from_value(json!({
        "action":"product_update",
        "expected_revision":0,
        "version":1,
        "values":{"large":"x".repeat(MAX_DOCUMENT_BYTES + 100)}
    }))
    .unwrap();
    let mut pipeline = Pipeline {
        pending: Some(Pending {
            request_id: 37,
            key: "product/amaterasu.nvhttp".into(),
            deadline: Instant::now() + Duration::from_secs(30),
            state_request_id: 100,
            stage: Stage::Loading(operation),
            phase: PendingPhase::Waiting,
        }),
        ..Default::default()
    };
    let mut rejected = false;
    pipeline
        .tick(|command| {
            Ok(match command {
                Command::ReleaseResult { .. } => Response::Ok,
                Command::TakeResult { .. } => Response::Result {
                    payload: json!({"ok":true,"revision":11,"found":false}),
                },
                Command::Complete { payload, .. } => {
                    assert_eq!(payload["error"]["code"], "resource_exhausted");
                    rejected = true;
                    Response::Ok
                }
                Command::Poll {} => Response::Pending,
                other => panic!("oversized product document must not persist: {other:?}"),
            })
        })
        .unwrap();
    assert!(rejected);
}

#[test]
fn lost_product_write_ack_is_not_reported_as_success_and_saved_data_can_be_reloaded() {
    let committed = product_document(1, "possibly-committed.local");
    let mut pipeline = Pipeline {
        pending: Some(Pending {
            request_id: 37,
            key: "product/amaterasu.nvhttp".into(),
            deadline: Instant::now() + Duration::from_secs(30),
            state_request_id: 101,
            stage: Stage::SavingProduct(committed.clone()),
            phase: PendingPhase::Waiting,
        }),
        ..Default::default()
    };
    let mut failed = false;
    pipeline
        .tick(|command| {
            Ok(match command {
                Command::ReleaseResult { .. } => Response::Ok,
                // The state write may have committed even though its result was lost.
                Command::TakeResult { .. } => Response::error("deadline_exceeded", "ack lost"),
                Command::Complete { payload, .. } => {
                    assert_eq!(payload["ok"], false);
                    assert_eq!(payload["error"]["code"], "deadline_exceeded");
                    failed = true;
                    Response::Ok
                }
                Command::Poll {} => Response::Pending,
                other => panic!("lost acknowledgment must not publish success: {other:?}"),
            })
        })
        .unwrap();
    assert!(failed);

    let mut pipeline = Pipeline {
        pending: Some(Pending {
            request_id: 39,
            key: "product/amaterasu.nvhttp".into(),
            deadline: Instant::now() + Duration::from_secs(30),
            state_request_id: 102,
            stage: Stage::Loading(Operation::ProductGet {}),
            phase: PendingPhase::Waiting,
        }),
        ..Default::default()
    };
    let mut observed_commit = false;
    pipeline
        .tick(|command| {
            Ok(match command {
                Command::ReleaseResult { .. } => Response::Ok,
                Command::TakeResult { .. } => Response::Result {
                    payload: json!({"ok":true,"revision":12,"found":true,"value":committed}),
                },
                Command::Complete { payload, .. } => {
                    assert_eq!(payload["ok"], true);
                    assert_eq!(payload["revision"], 1);
                    assert_eq!(payload["values"]["hostname"], "possibly-committed.local");
                    observed_commit = true;
                    Response::Ok
                }
                Command::Poll {} => Response::Pending,
                other => panic!("expected product reload: {other:?}"),
            })
        })
        .unwrap();
    assert!(observed_commit);
}

#[test]
fn release_failures_retry_without_repolling_or_resubmitting_a_committed_write() {
    let committed = product_document(1, "saved.local");
    let mut pipeline = Pipeline {
        pending: Some(Pending {
            request_id: 37,
            key: "product/amaterasu.nvhttp".into(),
            deadline: Instant::now() + Duration::from_secs(30),
            state_request_id: 101,
            stage: Stage::SavingProduct(committed),
            phase: PendingPhase::Waiting,
        }),
        ..Default::default()
    };
    let mut take_count = 0;
    let mut release_count = 0;
    let mut state_writes = 0;
    let mut publishes = 0;
    let mut completions = 0;

    pipeline
        .tick(|command| {
            Ok(match command {
                Command::TakeResult { request: 101 } => {
                    take_count += 1;
                    Response::Result {
                        payload: json!({"ok":true,"revision":12}),
                    }
                }
                Command::ReleaseResult { request: 101 } => {
                    release_count += 1;
                    if release_count == 1 {
                        Response::error("temporarily_unavailable", "retry release")
                    } else {
                        Response::Ok
                    }
                }
                Command::Poll {} => Response::Pending,
                other => panic!("write acknowledgment must wait for result release: {other:?}"),
            })
        })
        .unwrap();

    assert_eq!(take_count, 1);
    assert_eq!(release_count, 1);
    assert_eq!(state_writes, 0);
    assert_eq!(publishes, 0);
    assert_eq!(completions, 0);
    assert!(pipeline.pending.is_some());

    let release_failure = pipeline.tick(|command| match command {
        Command::ReleaseResult { request: 101 } => {
            release_count += 1;
            Err("temporary release transport failure".into())
        }
        Command::Poll {} => Ok(Response::Pending),
        Command::TakeResult { .. } => panic!("release retry must not re-poll the child"),
        other => panic!("unexpected release retry command: {other:?}"),
    });
    assert_eq!(
        release_failure.unwrap_err(),
        "temporary release transport failure"
    );
    assert_eq!(take_count, 1);
    assert_eq!(release_count, 2);
    assert_eq!(publishes, 0);
    assert_eq!(completions, 0);
    assert!(pipeline.pending.is_some());

    pipeline
        .tick(|command| {
            Ok(match command {
                Command::ReleaseResult { request: 101 } => {
                    release_count += 1;
                    Response::Ok
                }
                Command::Publish { payload, .. } => {
                    assert_eq!(payload["revision"], 1);
                    publishes += 1;
                    Response::Ok
                }
                Command::Complete { payload, .. } => {
                    assert_eq!(payload["ok"], true);
                    assert_eq!(payload["revision"], 1);
                    assert_eq!(payload["values"]["hostname"], "saved.local");
                    completions += 1;
                    Response::Ok
                }
                Command::Poll {} => Response::Pending,
                Command::TakeResult { .. } => {
                    panic!("preserved result must be released without re-polling")
                }
                Command::Request { .. } => {
                    state_writes += 1;
                    panic!("committed write must not be resubmitted")
                }
                other => panic!("unexpected retry command: {other:?}"),
            })
        })
        .unwrap();

    assert_eq!(take_count, 1);
    assert_eq!(release_count, 3);
    assert_eq!(state_writes, 0);
    assert_eq!(publishes, 1);
    assert_eq!(completions, 1);
    assert!(pipeline.pending.is_none());
}

#[test]
fn product_state_results_are_released_through_high_volume_reads() {
    const REQUEST_COUNT: u64 = 600;

    let mut pipeline = Pipeline::default();
    let mut next_parent = 0_u64;
    let mut next_child = 10_000_u64;
    let mut outstanding_children = std::collections::BTreeSet::new();
    let mut completions = 0_u64;
    let mut releases = 0_u64;
    let mut ticks = 0;

    while completions < REQUEST_COUNT {
        ticks += 1;
        assert!(ticks < REQUEST_COUNT as usize * 4, "pipeline stalled");
        pipeline
            .tick(|command| {
                Ok(match command {
                    Command::Poll {} if next_parent < REQUEST_COUNT => {
                        next_parent += 1;
                        Response::Request {
                            id: next_parent,
                            caller: "amaterasu.nvhttp".into(),
                            initiator: "management-user".into(),
                            service: crate::SERVICE.into(),
                            remaining_ms: 30_000,
                            payload: json!({"action":"product_get"}),
                        }
                    }
                    Command::Poll {} => Response::Pending,
                    Command::Request { payload, .. } => {
                        assert_eq!(payload["action"], "get");
                        assert_eq!(payload["key"], "product/amaterasu.nvhttp");
                        next_child += 1;
                        let child = next_child;
                        assert!(outstanding_children.insert(child));
                        // The state broker caps retained requests at 512. Completion
                        // and release must happen before this set can approach it.
                        assert!(outstanding_children.len() <= 512);
                        Response::Accepted { id: child }
                    }
                    Command::TakeResult { request } => {
                        assert!(outstanding_children.contains(&request));
                        Response::Result {
                            payload: json!({"ok":true,"revision":0,"found":false}),
                        }
                    }
                    Command::ReleaseResult { request } => {
                        assert!(outstanding_children.remove(&request));
                        releases += 1;
                        Response::Ok
                    }
                    Command::Complete { payload, .. } => {
                        assert_eq!(payload["ok"], true);
                        assert_eq!(payload["found"], false);
                        completions += 1;
                        Response::Ok
                    }
                    other => panic!("unexpected high-volume command: {other:?}"),
                })
            })
            .unwrap();
    }

    assert_eq!(next_parent, REQUEST_COUNT);
    assert_eq!(releases, REQUEST_COUNT);
    assert!(outstanding_children.is_empty());
    assert!(pipeline.pending.is_none());
}

#[test]
fn pending_product_state_call_keeps_polling_with_bounded_backpressure() {
    let mut pipeline = Pipeline::default();
    let mut received = 0_u64;
    for _ in 0..100 {
        pipeline
            .tick(|command| {
                Ok(match command {
                    Command::Poll {} => {
                        received += 1;
                        Response::Request {
                            id: received,
                            caller: "amaterasu.nvhttp".into(),
                            initiator: "management-user".into(),
                            service: crate::SERVICE.into(),
                            remaining_ms: 30_000,
                            payload: json!({"action":"product_get"}),
                        }
                    }
                    Command::Request { .. } => Response::Accepted { id: 10_000 },
                    Command::TakeResult { request: 10_000 } => Response::Pending,
                    other => panic!("unexpected pending product command: {other:?}"),
                })
            })
            .unwrap();
    }

    assert_eq!(received, (MAX_QUEUED + 1) as u64);
    assert_eq!(pipeline.queued.len(), MAX_QUEUED);
    assert!(pipeline.pending.is_some());
}
