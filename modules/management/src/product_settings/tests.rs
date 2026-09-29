use super::*;
use std::collections::VecDeque;

#[derive(Default)]
struct Transport {
    responses: VecDeque<ModuleResult<Response>>,
    commands: Vec<Command>,
}

impl Transport {
    fn push(&mut self, responses: impl IntoIterator<Item = Response>) {
        self.responses.extend(responses.into_iter().map(Ok));
    }

    fn send(&mut self, command: Command) -> ModuleResult<Response> {
        self.commands.push(command);
        self.responses
            .pop_front()
            .expect("unexpected transport command")
    }
}

fn stored(revision: u64, tier: Option<PeerTier>) -> Response {
    Response::Result {
        payload: json!({"ok":true,"found":true,"version":1,"revision":revision,"values":{"origin_web_ui_allowed":tier}}),
    }
}

fn load(settings: &mut ProductSettings, transport: &mut Transport, response: Response) {
    transport.push([Response::Accepted { id: 10 }]);
    settings
        .tick(Instant::now(), &mut |command| transport.send(command))
        .unwrap();
    transport.push([response, Response::Ok]);
    settings
        .tick(Instant::now(), &mut |command| transport.send(command))
        .unwrap();
}

fn update(settings: &mut ProductSettings, transport: &mut Transport, selection: Value) {
    transport.push([Response::Accepted { id: 20 }]);
    settings.receive("amaterasu.management", 7, 2000, json!({"action":"product_update","expected_revision":1,"origin_web_ui_allowed":selection}), &mut |command| transport.send(command)).unwrap();
}

#[test]
fn startup_stays_closed_until_an_absent_document_is_confirmed_and_released() {
    let mut settings = ProductSettings::default();
    let mut transport = Transport::default();
    assert_eq!(settings.maximum_peer(), None);
    assert_eq!(settings.payload()["code"], "settings_unavailable");
    load(
        &mut settings,
        &mut transport,
        Response::Result {
            payload: json!({"ok":true,"found":false,"version":null,"revision":0,"values":{}}),
        },
    );
    assert_eq!(settings.maximum_peer(), Some(PeerTier::Lan));
    assert_eq!(settings.payload()["revision"], 0);
    assert!(settings.payload()["values"]["origin_web_ui_allowed"].is_null());
    let Command::Request {
        service,
        parent,
        payload,
        ..
    } = &transport.commands[0]
    else {
        panic!()
    };
    assert_eq!(service, CONFIGURATION);
    assert_eq!(*parent, None);
    assert_eq!(payload, &json!({"action":"product_get"}));
    assert!(matches!(
        transport.commands[2],
        Command::ReleaseResult { request: 10 }
    ));
}

#[test]
fn stored_policy_restores_each_selection_on_restart() {
    for selection in [
        None,
        Some(PeerTier::Pc),
        Some(PeerTier::Lan),
        Some(PeerTier::Wan),
    ] {
        let mut settings = ProductSettings::default();
        load(
            &mut settings,
            &mut Transport::default(),
            stored(5, selection),
        );
        assert_eq!(settings.maximum_peer(), Some(selection.unwrap_or_default()));
        assert_eq!(settings.payload()["revision"], 5);
    }
}

#[test]
fn malformed_or_future_documents_keep_ingress_closed() {
    for payload in [
        json!({"ok":false}),
        json!({"ok":true,"found":true,"version":2,"revision":1,"values":{}}),
        json!({"ok":true,"found":true,"version":1,"revision":0,"values":{}}),
        json!({"ok":true,"found":true,"version":1,"revision":1,"values":{"origin_web_ui_allowed":"WAN"}}),
        json!({"ok":true,"found":true,"version":1,"revision":1,"values":{"owner":"other"}}),
        json!({"ok":true,"found":false,"version":null,"revision":1,"values":{}}),
        json!({"ok":true,"found":false,"version":1,"revision":0,"values":{}}),
        json!({"ok":true,"found":false,"version":null,"revision":0,"values":{"origin_web_ui_allowed":"wan"}}),
    ] {
        let mut settings = ProductSettings::default();
        load(
            &mut settings,
            &mut Transport::default(),
            Response::Result { payload },
        );
        assert_eq!(settings.maximum_peer(), None);
    }
}

#[test]
fn update_delegates_under_parent_authority_and_applies_only_confirmed_cas_values() {
    let mut settings = ProductSettings::default();
    let mut transport = Transport::default();
    load(&mut settings, &mut transport, stored(1, Some(PeerTier::Pc)));
    update(&mut settings, &mut transport, json!("wan"));
    assert_eq!(settings.maximum_peer(), Some(PeerTier::Pc));
    let Command::Request {
        parent,
        timeout_ms,
        payload,
        ..
    } = &transport.commands[3]
    else {
        panic!()
    };
    assert_eq!(*parent, Some(7));
    assert_eq!(*timeout_ms, 1900);
    assert_eq!(
        payload,
        &json!({"action":"product_update","expected_revision":1,"version":1,"values":{"origin_web_ui_allowed":"wan"}})
    );
    transport.push([
        Response::Ok,
        stored(2, Some(PeerTier::Wan)),
        Response::Ok,
        Response::Ok,
    ]);
    settings
        .tick(Instant::now(), &mut |command| transport.send(command))
        .unwrap();
    assert_eq!(settings.maximum_peer(), Some(PeerTier::Wan));
    let Command::Complete { request, payload } = transport.commands.last().unwrap() else {
        panic!()
    };
    assert_eq!(*request, 7);
    assert_eq!(payload, &settings.payload());
}

#[test]
fn omitted_selection_preserves_policy_and_null_restores_the_source_default() {
    for selection in [None, Some(Value::Null)] {
        let mut settings = ProductSettings::default();
        let mut transport = Transport::default();
        load(&mut settings, &mut transport, stored(1, Some(PeerTier::Pc)));
        let mut payload = json!({"action":"product_update","expected_revision":1});
        if let Some(selection) = selection {
            payload["origin_web_ui_allowed"] = selection;
        }
        let expected = if payload.get("origin_web_ui_allowed").is_some() {
            None
        } else {
            Some(PeerTier::Pc)
        };
        transport.push([Response::Accepted { id: 20 }]);
        settings
            .receive("amaterasu.management", 7, 2000, payload, &mut |command| {
                transport.send(command)
            })
            .unwrap();
        transport.push([
            Response::Ok,
            stored(2, expected),
            Response::Ok,
            Response::Ok,
        ]);
        settings
            .tick(Instant::now(), &mut |command| transport.send(command))
            .unwrap();
        assert_eq!(settings.maximum_peer(), Some(expected.unwrap_or_default()));
        assert_eq!(
            settings.payload()["values"]["origin_web_ui_allowed"],
            json!(expected)
        );
    }
}

#[test]
fn stale_revision_and_forged_fields_do_not_submit_a_configuration_write() {
    for payload in [
        json!({"action":"product_update","expected_revision":0,"origin_web_ui_allowed":"wan"}),
        json!({"action":"product_update","expected_revision":1,"origin_web_ui_allowed":"WAN"}),
        json!({"action":"product_update","expected_revision":1,"values":{"origin_web_ui_allowed":"wan"}}),
        json!({"action":"product_update","expected_revision":1,"origin_web_ui_allowed":"wan","owner":"other"}),
        json!({"action":"product_get","caller":"other"}),
    ] {
        let mut settings = ProductSettings::default();
        let mut transport = Transport::default();
        load(&mut settings, &mut transport, stored(1, Some(PeerTier::Pc)));
        transport.push([Response::Ok]);
        settings
            .receive("amaterasu.management", 7, 2000, payload, &mut |command| {
                transport.send(command)
            })
            .unwrap();
        assert!(matches!(
            transport.commands.last().unwrap(),
            Command::Complete { .. }
        ));
        assert_eq!(transport.commands.len(), 4);
        assert_eq!(settings.maximum_peer(), Some(PeerTier::Pc));
    }
}

#[test]
fn unconfirmed_write_closes_ingress_and_reloads_the_durable_document() {
    for response in [
        stored(1, Some(PeerTier::Wan)),
        stored(2, Some(PeerTier::Lan)),
        Response::Result {
            payload: json!({"ok":false,"error":{"code":"revision_conflict","current_revision":3}}),
        },
    ] {
        let mut settings = ProductSettings::default();
        let mut transport = Transport::default();
        load(&mut settings, &mut transport, stored(1, Some(PeerTier::Pc)));
        update(&mut settings, &mut transport, json!("wan"));
        transport.push([
            Response::Ok,
            response,
            Response::Ok,
            Response::Ok,
            Response::Accepted { id: 30 },
        ]);
        settings
            .tick(Instant::now(), &mut |command| transport.send(command))
            .unwrap();
        assert_eq!(settings.maximum_peer(), None);
        let Command::Request {
            parent, payload, ..
        } = transport.commands.last().unwrap()
        else {
            panic!()
        };
        assert_eq!(*parent, None);
        assert_eq!(payload, &json!({"action":"product_get"}));
        transport.push([stored(3, Some(PeerTier::Lan)), Response::Ok]);
        settings
            .tick(Instant::now(), &mut |command| transport.send(command))
            .unwrap();
        assert_eq!(settings.maximum_peer(), Some(PeerTier::Lan));
    }
}

#[test]
fn timeout_and_parent_cancellation_cancel_and_release_the_owned_child_before_reload() {
    for expired in [true, false] {
        let mut settings = ProductSettings::default();
        let mut transport = Transport::default();
        load(&mut settings, &mut transport, stored(1, Some(PeerTier::Pc)));
        update(&mut settings, &mut transport, json!("wan"));
        let check = if expired {
            Response::Ok
        } else {
            Response::Error {
                code: "request_ended".into(),
                message: "cancelled parent".into(),
            }
        };
        transport.push([
            check,
            Response::Ok,
            Response::Ok,
            Response::Ok,
            Response::Accepted { id: 30 },
        ]);
        let now = if expired {
            Instant::now() + Duration::from_secs(4)
        } else {
            Instant::now()
        };
        settings
            .tick(now, &mut |command| transport.send(command))
            .unwrap();
        assert!(matches!(
            transport.commands[5],
            Command::Cancel { request: 20 }
        ));
        assert!(matches!(
            transport.commands[6],
            Command::ReleaseResult { request: 20 }
        ));
        assert_eq!(settings.maximum_peer(), None);
    }
}

#[test]
fn terminal_result_is_retained_until_release_and_completion_both_succeed() {
    let mut settings = ProductSettings::default();
    let mut transport = Transport::default();
    load(&mut settings, &mut transport, stored(1, Some(PeerTier::Pc)));
    update(&mut settings, &mut transport, json!("wan"));
    transport.push([Response::Ok, stored(2, Some(PeerTier::Wan))]);
    transport
        .responses
        .push_back(Err("release transport failed".into()));
    assert!(
        settings
            .tick(Instant::now(), &mut |command| transport.send(command))
            .is_ok()
    );
    assert_eq!(settings.maximum_peer(), None);
    transport.push([Response::Ok]);
    transport
        .responses
        .push_back(Err("completion transport failed".into()));
    assert!(
        settings
            .tick(Instant::now(), &mut |command| transport.send(command))
            .is_ok()
    );
    assert_eq!(settings.maximum_peer(), None);
    transport.push([
        Response::Error {
            code: "not_found".into(),
            message: "already released".into(),
        },
        Response::Ok,
    ]);
    settings
        .tick(Instant::now(), &mut |command| transport.send(command))
        .unwrap();
    assert!(settings.pending.is_none());
    assert_eq!(settings.maximum_peer(), Some(PeerTier::Wan));
    assert_eq!(
        transport
            .commands
            .iter()
            .filter(|command| matches!(command, Command::TakeResult { request: 20 }))
            .count(),
        1
    );
}

#[test]
fn immutable_runtime_caller_is_required_for_policy_writes() {
    let mut settings = ProductSettings::default();
    let mut transport = Transport::default();
    load(&mut settings, &mut transport, stored(1, Some(PeerTier::Pc)));
    transport.push([Response::Ok]);
    settings
        .receive(
            "untrusted.module",
            7,
            2000,
            json!({"action":"product_update","expected_revision":1,"origin_web_ui_allowed":"wan"}),
            &mut |command| transport.send(command),
        )
        .unwrap();
    let Command::Complete { payload, .. } = transport.commands.last().unwrap() else {
        panic!()
    };
    assert_eq!(payload["code"], "permission_denied");
    assert_eq!(transport.commands.len(), 4);
    assert_eq!(settings.maximum_peer(), Some(PeerTier::Pc));
    transport.push([Response::Ok]);
    settings.receive("untrusted.module", 8, 2000, json!({"action":"product_update","expected_revision":1,"origin_web_ui_allowed":"wan","caller":"amaterasu.management"}), &mut |command| transport.send(command)).unwrap();
    let Command::Complete { payload, .. } = transport.commands.last().unwrap() else {
        panic!()
    };
    assert_eq!(payload["code"], "invalid_request");
}

#[test]
fn ambiguous_admission_ends_parent_then_replays_exact_request_and_cleans_owned_child() {
    for ambiguous in [
        Err("lost admission reply".into()),
        Ok(Response::Pending),
        Ok(Response::Ok),
    ] {
        let mut settings = ProductSettings::default();
        let mut transport = Transport::default();
        load(
            &mut settings,
            &mut transport,
            stored(1, Some(PeerTier::Wan)),
        );
        transport.responses.push_back(ambiguous);
        settings.receive("amaterasu.management", 7, 2000, json!({"action":"product_update","expected_revision":1,"origin_web_ui_allowed":"pc"}), &mut |command| transport.send(command)).unwrap();
        let original = serde_json::to_value(&transport.commands[3]).unwrap();
        assert_eq!(settings.maximum_peer(), None);
        transport.push([
            Response::Ok,
            Response::Accepted { id: 20 },
            Response::Ok,
            Response::Ok,
            Response::Ok,
            Response::Accepted { id: 30 },
        ]);
        settings
            .tick(Instant::now(), &mut |command| transport.send(command))
            .unwrap();
        assert!(matches!(
            transport.commands[4],
            Command::Complete { request: 7, .. }
        ));
        assert_eq!(
            serde_json::to_value(&transport.commands[5]).unwrap(),
            original
        );
        assert!(matches!(
            transport.commands[6],
            Command::Cancel { request: 20 }
        ));
        assert!(matches!(
            transport.commands[7],
            Command::ReleaseResult { request: 20 }
        ));
        assert_eq!(settings.maximum_peer(), None);
        transport.push([stored(2, Some(PeerTier::Pc)), Response::Ok]);
        settings
            .tick(Instant::now(), &mut |command| transport.send(command))
            .unwrap();
        assert_eq!(settings.maximum_peer(), Some(PeerTier::Pc));
    }
}

#[test]
fn repeated_ambiguous_admission_retains_operation_until_explicit_rejection_after_parent_end() {
    let mut settings = ProductSettings::default();
    let mut transport = Transport::default();
    load(
        &mut settings,
        &mut transport,
        stored(1, Some(PeerTier::Wan)),
    );
    transport
        .responses
        .push_back(Err("admission reply lost".into()));
    settings
        .receive(
            "amaterasu.management",
            7,
            2000,
            json!({"action":"product_update","expected_revision":1,"origin_web_ui_allowed":"pc"}),
            &mut |command| transport.send(command),
        )
        .unwrap();
    transport.push([Response::Ok, Response::Pending]);
    settings
        .tick(Instant::now(), &mut |command| transport.send(command))
        .unwrap();
    assert!(settings.pending.is_some());
    assert_eq!(settings.maximum_peer(), None);
    transport.push([Response::Error {
        code: "permission_denied".into(),
        message: "ended parent cannot create a request".into(),
    }]);
    settings
        .tick(Instant::now(), &mut |command| transport.send(command))
        .unwrap();
    assert!(settings.pending.is_none());
    assert_eq!(settings.maximum_peer(), None);
    for command in [
        &transport.commands[3],
        &transport.commands[5],
        &transport.commands[6],
    ] {
        assert_eq!(
            serde_json::to_value(command).unwrap(),
            serde_json::to_value(&transport.commands[3]).unwrap()
        );
    }
}

#[test]
fn take_result_transport_failure_closes_ingress_and_keeps_the_owned_child() {
    let mut settings = ProductSettings::default();
    let mut transport = Transport::default();
    load(
        &mut settings,
        &mut transport,
        stored(1, Some(PeerTier::Wan)),
    );
    update(&mut settings, &mut transport, json!("pc"));
    transport.push([Response::Ok]);
    transport
        .responses
        .push_back(Err("lost result reply".into()));
    settings
        .tick(Instant::now(), &mut |command| transport.send(command))
        .unwrap();
    assert_eq!(settings.maximum_peer(), None);
    assert_eq!(settings.pending.as_ref().unwrap().child, Some(20));
    transport.push([
        Response::Ok,
        stored(2, Some(PeerTier::Pc)),
        Response::Ok,
        Response::Ok,
    ]);
    settings
        .tick(Instant::now(), &mut |command| transport.send(command))
        .unwrap();
    assert_eq!(settings.maximum_peer(), Some(PeerTier::Pc));
}

#[test]
fn unsuccessful_release_reply_keeps_ingress_closed_and_retries_the_captured_result() {
    let mut settings = ProductSettings::default();
    let mut transport = Transport::default();
    load(
        &mut settings,
        &mut transport,
        stored(1, Some(PeerTier::Wan)),
    );
    update(&mut settings, &mut transport, json!("pc"));
    transport.push([
        Response::Ok,
        stored(2, Some(PeerTier::Pc)),
        Response::Error {
            code: "request_pending".into(),
            message: "release not settled".into(),
        },
    ]);
    settings
        .tick(Instant::now(), &mut |command| transport.send(command))
        .unwrap();
    assert_eq!(settings.maximum_peer(), None);
    assert!(settings.pending.is_some());
    transport.push([Response::Ok, Response::Ok]);
    settings
        .tick(Instant::now(), &mut |command| transport.send(command))
        .unwrap();
    assert_eq!(settings.maximum_peer(), Some(PeerTier::Pc));
}
