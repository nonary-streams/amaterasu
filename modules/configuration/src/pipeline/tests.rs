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
        }),
        ..Default::default()
    };
    let mut rejected = false;
    pipeline
        .tick(|command| {
            Ok(match command {
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
            }),
            ..Default::default()
        };
        let mut completed = false;
        pipeline
            .tick(|command| {
                Ok(match command {
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
            }),
            ..Default::default()
        };
        pipeline.tick(|command| Ok(match command {
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
        }),
        ..Default::default()
    };
    let mut durable = None;
    pipeline.tick(|command| Ok(match command {
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
