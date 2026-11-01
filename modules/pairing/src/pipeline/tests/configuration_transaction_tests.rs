use super::*;

fn pump_configuration_fence(
    pairing: &mut Pipeline,
    configuration: &mut amaterasu_host_configuration::test_support::TestHostConfiguration,
    state: &amaterasu_state::test_support::TestStateStore,
    broker: &mut amaterasu_runtime::Broker,
) {
    pump_configuration_fence_with_lost_finish(pairing, configuration, state, broker, &mut false);
}

fn pump_configuration_fence_with_lost_finish(
    pairing: &mut Pipeline,
    configuration: &mut amaterasu_host_configuration::test_support::TestHostConfiguration,
    state: &amaterasu_state::test_support::TestStateStore,
    broker: &mut amaterasu_runtime::Broker,
    lose_finish: &mut bool,
) {
    pump_configuration_fence_with_faults(
        pairing,
        configuration,
        state,
        broker,
        lose_finish,
        &mut false,
    );
}

fn pump_configuration_fence_with_faults(
    pairing: &mut Pipeline,
    configuration: &mut amaterasu_host_configuration::test_support::TestHostConfiguration,
    state: &amaterasu_state::test_support::TestStateStore,
    broker: &mut amaterasu_runtime::Broker,
    lose_finish: &mut bool,
    lose_begin: &mut bool,
) {
    configuration
        .tick(|command| Ok(broker.dispatch("amaterasu.host-configuration", command)))
        .unwrap();
    pairing
        .tick_at(now(), |command| {
            if let Command::Complete { request, payload } = &command
                && *lose_finish
                && payload["state"] == "released"
            {
                assert!(matches!(
                    broker.dispatch(
                        "amaterasu.host-configuration",
                        Command::Cancel { request: *request }
                    ),
                    Response::Ok
                ));
                *lose_finish = false;
            }
            if let Command::Complete { request, payload } = &command
                && *lose_begin
                && payload["state"] == "pending"
                && payload["fence_generation"].as_u64().is_some()
            {
                assert!(matches!(
                    broker.dispatch(
                        "amaterasu.host-configuration",
                        Command::Cancel { request: *request }
                    ),
                    Response::Ok
                ));
                *lose_begin = false;
            }
            Ok(broker.dispatch("amaterasu.pairing", command))
        })
        .unwrap();
    for _ in 0..16 {
        let Response::Request {
            id,
            caller,
            payload,
            ..
        } = broker.dispatch("state", Command::Poll {})
        else {
            break;
        };
        let response = state.handle(&caller, payload);
        assert!(matches!(
            broker.dispatch(
                "state",
                Command::Complete {
                    request: id,
                    payload: response
                }
            ),
            Response::Ok | Response::Error { .. }
        ));
    }
    for _ in 0..16 {
        let Response::Request { id, payload, .. } =
            broker.dispatch("diagnostics", Command::Poll {})
        else {
            break;
        };
        let response = match payload["action"].as_str() {
            Some("status") => {
                json!({"ok":true,"ready":true,"logging_configuration_revision":1,"logging_sink":{"state":"healthy","failure":null}})
            }
            Some("apply_host_logging") => {
                json!({"ok":true,"applied_revision":payload["configuration"]["revision"]})
            }
            action => panic!("unexpected diagnostics action {action:?}"),
        };
        assert!(matches!(
            broker.dispatch(
                "diagnostics",
                Command::Complete {
                    request: id,
                    payload: response
                }
            ),
            Response::Ok | Response::Error { .. }
        ));
    }
}

fn broker_input_effect(
    payload: Value,
    pairing: &mut Pipeline,
    configuration: &mut amaterasu_host_configuration::test_support::TestHostConfiguration,
    state: &amaterasu_state::test_support::TestStateStore,
    broker: &mut amaterasu_runtime::Broker,
) -> Value {
    let Response::Accepted { id } = broker.dispatch(
        "input",
        Command::Request {
            service: EFFECT_SERVICE.into(),
            payload,
            timeout_ms: 30_000,
            operation_id: format!(
                "owner-test.{}",
                amaterasu_storage::random_identifier().unwrap()
            ),
            parent: None,
        },
    ) else {
        panic!("input owner request rejected")
    };
    for _ in 0..100 {
        pump_configuration_fence(pairing, configuration, state, broker);
        if let Response::Result { payload } =
            broker.dispatch("input", Command::TakeResult { request: id })
        {
            return payload;
        }
    }
    panic!("input owner request did not complete");
}

fn broker_configuration_call(
    caller: &str,
    payload: Value,
    pairing: &mut Pipeline,
    configuration: &mut amaterasu_host_configuration::test_support::TestHostConfiguration,
    state: &amaterasu_state::test_support::TestStateStore,
    broker: &mut amaterasu_runtime::Broker,
) -> Value {
    let Response::Accepted { id } = broker.dispatch(
        caller,
        Command::Request {
            service: HOST_CONFIGURATION_SERVICE.into(),
            payload,
            timeout_ms: 30_000,
            operation_id: format!(
                "configuration-test.{}",
                amaterasu_storage::random_identifier().unwrap()
            ),
            parent: None,
        },
    ) else {
        panic!("configuration request rejected")
    };
    for _ in 0..100 {
        pump_configuration_fence(pairing, configuration, state, broker);
        if let Response::Result { payload } =
            broker.dispatch(caller, Command::TakeResult { request: id })
        {
            return payload;
        }
    }
    panic!("configuration request did not complete");
}

#[test]
fn real_providers_hold_host_identity_write_until_owner_retires_stream() {
    use amaterasu_contracts::{Grants, RuntimeConfig};
    use amaterasu_host_configuration::test_support::TestHostConfiguration;
    use amaterasu_runtime::Broker;
    use amaterasu_state::test_support::TestStateStore;

    let root = std::env::temp_dir().join(format!(
        "amaterasu-real-config-fence-{}",
        amaterasu_storage::random_identifier().unwrap()
    ));
    fs::create_dir_all(root.join("credentials")).unwrap();
    fs::write(root.join("credentials/cacert.pem"), HOST_CERT).unwrap();
    fs::write(root.join("credentials/cakey.pem"), HOST_KEY).unwrap();
    fs::write(root.join("credentials/new-cert.pem"), HOST_CERT).unwrap();
    fs::write(root.join("credentials/new-key.pem"), HOST_KEY).unwrap();
    fs::write(root.join("credentials/third-cert.pem"), HOST_CERT).unwrap();
    fs::write(root.join("credentials/third-key.pem"), HOST_KEY).unwrap();
    let root = root.canonicalize().unwrap();
    let state = TestStateStore::open(root.join("state")).unwrap();
    let (mut pairing, harness, _, epoch, source, scope) = authorized_feedback_for_test();
    pairing
        .effect_authority
        .as_mut()
        .unwrap()
        .configuration_owner = Some("amaterasu.host-configuration".into());
    let identity = ConfiguredIdentity::load_existing(
        root.join("credentials/cacert.pem").to_str().unwrap(),
        root.join("credentials/cakey.pem").to_str().unwrap(),
        0,
        now(),
    )
    .unwrap();
    pairing.host_configuration = HostConfigurationState::Ready {
        identity: Arc::new(identity),
        refresh_at: u64::MAX,
    };
    pairing.identity_migration_checked = true;
    for revision in 0..harness.revision {
        assert_eq!(state.handle("amaterasu.pairing", json!({"action":"set","key":STATE_KEY,"value":harness.document,"expected_revision":revision}))["revision"], revision + 1);
    }
    let admitted = pairing.apply_effect_request("input", "input", json!({"action":"admit_stream_lease","epoch":epoch,"source":source,"scope":scope,"operation_id":"config-test-lease","sequence":1}), now()).unwrap();
    let mut config = RuntimeConfig::default();
    for (principal, provides, calls) in [
        (
            "amaterasu.host-configuration",
            vec![HOST_CONFIGURATION_SERVICE],
            vec![STATE_SERVICE, "amaterasu.diagnostics/1", EFFECT_SERVICE],
        ),
        (
            "amaterasu.pairing",
            vec![SERVICE, EFFECT_SERVICE],
            vec![STATE_SERVICE, HOST_CONFIGURATION_SERVICE],
        ),
        ("state", vec![STATE_SERVICE], vec![]),
        ("diagnostics", vec!["amaterasu.diagnostics/1"], vec![]),
        (
            "amaterasu.management",
            vec![],
            vec![
                HOST_CONFIGURATION_SERVICE,
                STATE_SERVICE,
                "amaterasu.diagnostics/1",
            ],
        ),
        ("input", vec![], vec![EFFECT_SERVICE]),
        ("stranger", vec![], vec![HOST_CONFIGURATION_SERVICE]),
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
        ("amaterasu.host-configuration", HOST_CONFIGURATION_SERVICE),
        ("amaterasu.pairing", SERVICE),
        ("amaterasu.pairing", EFFECT_SERVICE),
        ("state", STATE_SERVICE),
        ("diagnostics", "amaterasu.diagnostics/1"),
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
    let mut configuration = TestHostConfiguration::new(root.clone(), true);
    let Response::Accepted { id: update } = broker.dispatch("amaterasu.management", Command::Request {
        service: HOST_CONFIGURATION_SERVICE.into(), timeout_ms:30_000, operation_id:"identity-update".into(), parent:None,
        payload:json!({"action":"update","expected_revision":0,"file_state":"sunshine_state.json","vibeshine_file_state":"vibeshine_state.json","credentials_file":"","pkey":"credentials/new-key.pem","cert":"credentials/new-cert.pem","log_path":"amaterasu.log","min_log_level":"product_default"}),
    }) else { panic!("update rejected") };
    for _ in 0..100 {
        pump_configuration_fence(&mut pairing, &mut configuration, &state, &mut broker);
        if pairing.configuration_hold.is_some() {
            break;
        }
    }
    assert!(pairing.configuration_hold.is_some());
    let pending = state.handle(
        "amaterasu.host-configuration",
        json!({"action":"get","key":"global/amaterasu.host-configuration"}),
    );
    assert_eq!(pending["value"]["revision"], 0);
    assert_eq!(
        pending["value"]["transaction"]["state"]["phase"],
        "prepared"
    );
    let denied = broker_configuration_call(
        "stranger",
        json!({"action":"transaction_status"}),
        &mut pairing,
        &mut configuration,
        &state,
        &mut broker,
    );
    assert_eq!(denied["error"]["code"], "permission_denied");
    let mut stale_change = pending["value"]["transaction"]["change"].clone();
    stale_change["operation_id"] = json!("another-operation");
    let stale = broker_configuration_call(
        "amaterasu.management",
        json!({"action":"reconcile_transaction","change":stale_change}),
        &mut pairing,
        &mut configuration,
        &state,
        &mut broker,
    );
    assert_eq!(stale["error"]["code"], "configuration_change_unknown");
    assert_eq!(
        state.handle(
            "amaterasu.host-configuration",
            json!({"action":"get","key":"global/amaterasu.host-configuration"})
        )["revision"],
        pending["revision"]
    );
    assert!(matches!(
        broker.dispatch(
            "amaterasu.management",
            Command::TakeResult { request: update }
        ),
        Response::Pending
    ));
    let stale_change = pending["value"]["transaction"]["change"].clone();
    let Response::Accepted { id: delayed_status } = broker.dispatch(
        "amaterasu.pairing",
        Command::Request {
            service: HOST_CONFIGURATION_SERVICE.into(),
            timeout_ms: 30_000,
            operation_id: "delayed-pre-clear-status".into(),
            parent: None,
            payload: json!({"action":"transaction_status"}),
        },
    ) else {
        panic!("status request rejected")
    };
    let mut received_stale_status = false;
    for _ in 0..100 {
        pump_configuration_fence(&mut pairing, &mut configuration, &state, &mut broker);
        if let Response::Result { payload } = broker.dispatch(
            "amaterasu.pairing",
            Command::TakeResult {
                request: delayed_status,
            },
        ) {
            assert_eq!(payload["transaction"]["change"], stale_change);
            received_stale_status = true;
            break;
        }
    }
    assert!(received_stale_status);
    let retirement = json!({"ok":true,"service":"amaterasu.control-transport/1","stream":7,"provider_epoch":"c".repeat(32),"operation_id":"retire","scope":"whole_control_stream","state":"retired"});
    let fenced = broker_input_effect(
        json!({"action":"fence_source","epoch":epoch,"source":source,"scope":scope,"operation_id":"config-source-retirement"}),
        &mut pairing,
        &mut configuration,
        &state,
        &mut broker,
    );
    assert_eq!(fenced["state"], "fenced");
    assert_eq!(
        broker_input_effect(
            json!({"action":"complete_stream_lease","epoch":epoch,"operation_id":"config-test-lease","ticket":admitted["ticket"],"retirement":retirement}),
            &mut pairing,
            &mut configuration,
            &state,
            &mut broker
        )["ok"],
        true
    );
    assert_eq!(
        broker_input_effect(
            json!({"action":"acknowledge","epoch":epoch,"operation_id":"config-test-lease","ticket":admitted["ticket"]}),
            &mut pairing,
            &mut configuration,
            &state,
            &mut broker
        )["ok"],
        true
    );
    let generation = pairing.effect_gate.as_ref().unwrap().fence_generation();
    let premature = broker_input_effect(
        json!({"action":"drain","epoch":epoch,"fence_generation":generation}),
        &mut pairing,
        &mut configuration,
        &state,
        &mut broker,
    );
    assert_eq!(premature["code"], "source_retirement_pending");
    let retired = broker_input_effect(
        json!({"action":"complete_source_retirement","epoch":epoch,"source":source,"scope":scope,"operation_id":"config-source-retirement","retirement":retirement}),
        &mut pairing,
        &mut configuration,
        &state,
        &mut broker,
    );
    assert_eq!(retired["state"], "retired");
    assert_eq!(
        broker_input_effect(
            json!({"action":"drain","epoch":epoch,"fence_generation":generation}),
            &mut pairing,
            &mut configuration,
            &state,
            &mut broker
        )["ok"],
        true
    );
    let mut result = None;
    for _ in 0..1000 {
        pump_configuration_fence(&mut pairing, &mut configuration, &state, &mut broker);
        if let Response::Result { payload } = broker.dispatch(
            "amaterasu.management",
            Command::TakeResult { request: update },
        ) {
            result = Some(payload);
            break;
        }
    }
    assert_eq!(
        result.as_ref().map(|value| &value["ok"]),
        Some(&json!(true)),
        "{result:?}"
    );
    assert!(pairing.configuration_hold.is_none());
    let committed = state.handle(
        "amaterasu.host-configuration",
        json!({"action":"get","key":"global/amaterasu.host-configuration"}),
    );
    assert_eq!(committed["value"]["revision"], 1);
    assert_eq!(committed["value"]["cert"], "credentials/new-cert.pem");
    assert!(committed["value"].get("transaction").is_none());
    assert_eq!(
        committed["value"]["terminal_receipt"]["transaction"]["change"],
        stale_change
    );
    // Deliver the authentic pre-clear broker response after restarting pairing.
    // The retained request ID models delayed delivery across that boundary.
    let mut restarted_settings = settings();
    let mut policy = policy_for_feedback(
        &["speaker.pcm"],
        crate::protocol::FeedbackPermission::Launch,
    );
    policy.configuration_owner = Some("amaterasu.host-configuration".into());
    restarted_settings.effect_authority = Some(policy);
    pairing =
        Pipeline::with_host_configuration(restarted_settings, Arc::new(Shared::default()), true);
    pairing.configuration_status_request = Some(delayed_status);
    let restart_epoch = pairing.effect_gate.as_ref().unwrap().epoch().to_owned();
    pump_configuration_fence(&mut pairing, &mut configuration, &state, &mut broker);
    assert!(pairing.configuration_hold.is_some());
    let held = pairing
        .apply_effect_request(
            "input",
            "input",
            json!({"action":"recover","epoch":restart_epoch,"owner_epoch":"stale-status-owner"}),
            now(),
        )
        .unwrap();
    assert_eq!(held["held"], true);
    let reconciled_epoch = held["epoch"].as_str().unwrap().to_owned();
    let Response::Accepted {
        id: stale_reconcile,
    } = broker.dispatch(
        "amaterasu.management",
        Command::Request {
            service: HOST_CONFIGURATION_SERVICE.into(),
            timeout_ms: 30_000,
            operation_id: "reconcile-delayed-pre-clear-status".into(),
            parent: None,
            payload: json!({"action":"reconcile_transaction","change":stale_change}),
        },
    )
    else {
        panic!("stale status reconciliation rejected")
    };
    let mut stale_result = None;
    for _ in 0..1000 {
        pump_configuration_fence(&mut pairing, &mut configuration, &state, &mut broker);
        if let Response::Result { payload } = broker.dispatch(
            "amaterasu.management",
            Command::TakeResult {
                request: stale_reconcile,
            },
        ) {
            stale_result = Some(payload);
            break;
        }
    }
    assert_eq!(
        stale_result.as_ref().map(|value| &value["ok"]),
        Some(&json!(true)),
        "{stale_result:?}"
    );
    assert!(pairing.configuration_hold.is_none());
    let next_epoch = pairing
        .apply_effect_request(
            "input",
            "input",
            json!({"action":"recover","epoch":reconciled_epoch,"owner_epoch":"owner-2"}),
            now(),
        )
        .unwrap()["epoch"]
        .as_str()
        .unwrap()
        .to_owned();
    let next_source = feedback_source_for_test("owner-2");
    assert_eq!(pairing.apply_effect_request("nvhttp", "nvhttp", json!({"action":"bind_stream","epoch":next_epoch,"source":next_source,"scope":scope,"certificate_pem":CLIENT_CERT}), now()).unwrap()["ok"], true);
    let tuning_admitted = pairing.apply_effect_request("input", "input", json!({"action":"admit_stream_lease","epoch":next_epoch,"source":next_source,"scope":scope,"operation_id":"tuning-lease","sequence":1}), now()).unwrap();
    let snapshot = pairing.identity.get(now()).unwrap();
    let Response::Accepted { id: tuning } = broker.dispatch("amaterasu.management", Command::Request {
        service: HOST_CONFIGURATION_SERVICE.into(), timeout_ms:30_000, operation_id:"logging-only".into(), parent:None,
        payload:json!({"action":"update","expected_revision":1,"file_state":"sunshine_state.json","vibeshine_file_state":"vibeshine_state.json","credentials_file":"","pkey":"credentials/new-key.pem","cert":"credentials/new-cert.pem","log_path":"other.log","min_log_level":"info"}),
    }) else { panic!("tuning update rejected") };
    let mut tuning_result = None;
    for _ in 0..300 {
        pump_configuration_fence(&mut pairing, &mut configuration, &state, &mut broker);
        if let Response::Result { payload } = broker.dispatch(
            "amaterasu.management",
            Command::TakeResult { request: tuning },
        ) {
            tuning_result = Some(payload);
            break;
        }
    }
    assert_eq!(
        tuning_result.as_ref().map(|value| &value["ok"]),
        Some(&json!(true)),
        "{tuning_result:?}"
    );
    if let HostConfigurationState::Ready { refresh_at, .. } = &mut pairing.host_configuration {
        *refresh_at = 0;
    } else {
        panic!("pairing identity is not ready");
    }
    for _ in 0..100 {
        pump_configuration_fence(&mut pairing, &mut configuration, &state, &mut broker);
        if pairing
            .configured_identity()
            .is_some_and(|identity| identity.provider_revision == 2)
        {
            break;
        }
    }
    assert_eq!(pairing.configured_identity().unwrap().provider_revision, 2);
    assert!(pairing.identity.current(&snapshot, now()));
    assert_eq!(pairing.effect_gate.as_ref().unwrap().in_flight(), 1);
    assert!(!pairing.effect_gate.as_ref().unwrap().drain_required());
    let Response::Accepted { id: cancelled } = broker.dispatch("amaterasu.management", Command::Request {
        service: HOST_CONFIGURATION_SERVICE.into(), timeout_ms:30_000, operation_id:"cancelled-identity-update".into(), parent:None,
        payload:json!({"action":"update","expected_revision":2,"file_state":"sunshine_state.json","vibeshine_file_state":"vibeshine_state.json","credentials_file":"","pkey":"credentials/third-key.pem","cert":"credentials/third-cert.pem","log_path":"other.log","min_log_level":"info"}),
    }) else { panic!("second identity update rejected") };
    for _ in 0..100 {
        pump_configuration_fence(&mut pairing, &mut configuration, &state, &mut broker);
        if pairing.configuration_hold.is_some() {
            break;
        }
    }
    assert!(pairing.configuration_hold.is_some());
    let marker = state.handle(
        "amaterasu.host-configuration",
        json!({"action":"get","key":"global/amaterasu.host-configuration"}),
    );
    let change = marker["value"]["transaction"]["change"].clone();
    assert_eq!(change["expected_configuration_revision"], 2);
    let tuning_ticket = tuning_admitted["ticket"].clone();
    let retirement = json!({"ok":true,"service":"amaterasu.control-transport/1","stream":7,"provider_epoch":"c".repeat(32),"operation_id":"retire-tuning","scope":"whole_control_stream","state":"retired"});
    assert_eq!(pairing.apply_effect_request("input", "input", json!({"action":"fence_source","epoch":next_epoch,"source":next_source,"scope":scope,"operation_id":"tuning-source-retirement"}), now()).unwrap()["state"], "fenced");
    pairing.apply_effect_request("input", "input", json!({"action":"complete_stream_lease","epoch":next_epoch,"operation_id":"tuning-lease","ticket":tuning_ticket,"retirement":retirement}), now()).unwrap();
    pairing.apply_effect_request("input", "input", json!({"action":"acknowledge","epoch":next_epoch,"operation_id":"tuning-lease","ticket":tuning_ticket}), now()).unwrap();
    assert_eq!(pairing.apply_effect_request("input", "input", json!({"action":"complete_source_retirement","epoch":next_epoch,"source":next_source,"scope":scope,"operation_id":"tuning-source-retirement","retirement":retirement}), now()).unwrap()["state"], "retired");
    let generation = pairing.effect_gate.as_ref().unwrap().fence_generation();
    pairing
        .apply_effect_request(
            "input",
            "input",
            json!({"action":"drain","epoch":next_epoch,"fence_generation":generation}),
            now(),
        )
        .unwrap();
    let mut delayed = None;
    for _ in 0..200 {
        configuration
            .tick(|command| Ok(broker.dispatch("amaterasu.host-configuration", command)))
            .unwrap();
        pairing
            .tick_at(now(), |command| {
                Ok(broker.dispatch("amaterasu.pairing", command))
            })
            .unwrap();
        for _ in 0..16 {
            let Response::Request {
                id,
                caller,
                payload,
                ..
            } = broker.dispatch("state", Command::Poll {})
            else {
                break;
            };
            if caller == "amaterasu.host-configuration"
                && payload["action"] == "set"
                && payload["value"]["revision"] == 3
                && payload["value"]["transaction"]["state"]["phase"] == "committed"
            {
                delayed = Some((id, caller, payload));
                break;
            }
            let response = state.handle(&caller, payload);
            let _ = broker.dispatch(
                "state",
                Command::Complete {
                    request: id,
                    payload: response,
                },
            );
        }
        if delayed.is_some() {
            break;
        }
        if let Response::Request { id, payload, .. } =
            broker.dispatch("diagnostics", Command::Poll {})
        {
            let response = if payload["action"] == "status" {
                json!({"ok":true,"ready":true,"logging_configuration_revision":2,"logging_sink":{"state":"healthy","failure":null}})
            } else {
                json!({"ok":true,"applied_revision":payload["configuration"]["revision"]})
            };
            let _ = broker.dispatch(
                "diagnostics",
                Command::Complete {
                    request: id,
                    payload: response,
                },
            );
        }
    }
    let (delayed_id, delayed_caller, delayed_payload) =
        delayed.expect("state write must be delivered before cancellation");
    assert!(matches!(
        broker.dispatch(
            "amaterasu.management",
            Command::Cancel { request: cancelled }
        ),
        Response::Ok
    ));
    let Response::Accepted { id: reconcile } = broker.dispatch(
        "amaterasu.management",
        Command::Request {
            service: HOST_CONFIGURATION_SERVICE.into(),
            timeout_ms: 30_000,
            operation_id: "reconcile-cancelled".into(),
            parent: None,
            payload: json!({"action":"reconcile_transaction","change":change}),
        },
    ) else {
        panic!("reconciliation rejected")
    };
    let mut late_checked = false;
    let mut reconciled = None;
    for _ in 0..1000 {
        pump_configuration_fence(&mut pairing, &mut configuration, &state, &mut broker);
        let current = state.handle(
            "amaterasu.host-configuration",
            json!({"action":"get","key":"global/amaterasu.host-configuration"}),
        );
        if !late_checked
            && current["value"]["transaction"]["state"]["phase"] == "aborted"
            && current["value"]["transaction"]["settled"] == true
        {
            let late = state.handle(&delayed_caller, delayed_payload.clone());
            assert_eq!(late["error"]["code"], "revision_conflict");
            assert!(matches!(
                broker.dispatch(
                    "state",
                    Command::Complete {
                        request: delayed_id,
                        payload: late
                    }
                ),
                Response::Error { .. }
            ));
            late_checked = true;
        }
        if let Response::Result { payload } = broker.dispatch(
            "amaterasu.management",
            Command::TakeResult { request: reconcile },
        ) {
            reconciled = Some(payload);
            break;
        }
    }
    assert!(late_checked);
    assert_eq!(
        reconciled.as_ref().map(|value| &value["ok"]),
        Some(&json!(true)),
        "{reconciled:?}"
    );
    assert!(pairing.configuration_hold.is_none());
    let after_abort = state.handle(
        "amaterasu.host-configuration",
        json!({"action":"get","key":"global/amaterasu.host-configuration"}),
    );
    assert_eq!(after_abort["value"]["revision"], 2);
    assert_eq!(after_abort["value"]["cert"], "credentials/new-cert.pem");
    assert!(after_abort["value"].get("transaction").is_none());
    let third_epoch = pairing
        .apply_effect_request(
            "input",
            "input",
            json!({"action":"recover","epoch":next_epoch,"owner_epoch":"owner-3"}),
            now(),
        )
        .unwrap()["epoch"]
        .as_str()
        .unwrap()
        .to_owned();
    let Response::Accepted { id: lost_finish } = broker.dispatch("amaterasu.management", Command::Request {
        service: HOST_CONFIGURATION_SERVICE.into(), timeout_ms:30_000, operation_id:"lost-finish-update".into(), parent:None,
        payload:json!({"action":"update","expected_revision":2,"file_state":"sunshine_state.json","vibeshine_file_state":"vibeshine_state.json","credentials_file":"","pkey":"credentials/third-key.pem","cert":"credentials/third-cert.pem","log_path":"other.log","min_log_level":"info"}),
    }) else { panic!("lost finish update rejected") };
    for _ in 0..100 {
        pump_configuration_fence(&mut pairing, &mut configuration, &state, &mut broker);
        if pairing.configuration_hold.is_some() {
            break;
        }
    }
    assert!(pairing.configuration_hold.is_some());
    assert!(pairing.effect_gate.as_ref().unwrap().drain_required());
    let generation = pairing.effect_gate.as_ref().unwrap().fence_generation();
    pairing
        .apply_effect_request(
            "input",
            "input",
            json!({"action":"drain","epoch":third_epoch,"fence_generation":generation}),
            now(),
        )
        .unwrap();
    let mut lose_finish = true;
    let mut lost_result = None;
    for _ in 0..1000 {
        pump_configuration_fence_with_lost_finish(
            &mut pairing,
            &mut configuration,
            &state,
            &mut broker,
            &mut lose_finish,
        );
        if let Response::Result { payload } = broker.dispatch(
            "amaterasu.management",
            Command::TakeResult {
                request: lost_finish,
            },
        ) {
            lost_result = Some(payload);
            break;
        }
    }
    assert!(!lose_finish, "finish response was not dropped");
    assert_eq!(
        lost_result.as_ref().map(|value| &value["error"]["code"]),
        Some(&json!("configuration_transaction_unconfirmed")),
        "{lost_result:?}"
    );
    let marker = state.handle(
        "amaterasu.host-configuration",
        json!({"action":"get","key":"global/amaterasu.host-configuration"}),
    );
    assert_eq!(
        marker["value"]["transaction"]["state"]["phase"],
        "committed"
    );
    assert_eq!(marker["value"]["transaction"]["settled"], true);
    let Response::Accepted { id: finish_reconcile } = broker.dispatch("amaterasu.management", Command::Request {
        service: HOST_CONFIGURATION_SERVICE.into(), timeout_ms:30_000, operation_id:"reconcile-lost-finish".into(), parent:None,
        payload:json!({"action":"reconcile_transaction","change":marker["value"]["transaction"]["change"]}),
    }) else { panic!("lost finish reconciliation rejected") };
    let mut finish_result = None;
    for _ in 0..1000 {
        pump_configuration_fence(&mut pairing, &mut configuration, &state, &mut broker);
        if let Response::Result { payload } = broker.dispatch(
            "amaterasu.management",
            Command::TakeResult {
                request: finish_reconcile,
            },
        ) {
            finish_result = Some(payload);
            break;
        }
    }
    assert_eq!(
        finish_result.as_ref().map(|value| &value["ok"]),
        Some(&json!(true)),
        "{finish_result:?}"
    );
    let recovered = state.handle(
        "amaterasu.host-configuration",
        json!({"action":"get","key":"global/amaterasu.host-configuration"}),
    );
    assert_eq!(recovered["value"]["revision"], 3);
    assert!(recovered["value"].get("transaction").is_none());
    let fourth_epoch = pairing
        .apply_effect_request(
            "input",
            "input",
            json!({"action":"recover","epoch":third_epoch,"owner_epoch":"owner-4"}),
            now(),
        )
        .unwrap()["epoch"]
        .as_str()
        .unwrap()
        .to_owned();
    let Response::Accepted { id: restart_update } = broker.dispatch("amaterasu.management", Command::Request {
        service: HOST_CONFIGURATION_SERVICE.into(), timeout_ms:30_000, operation_id:"restart-during-hold".into(), parent:None,
        payload:json!({"action":"update","expected_revision":3,"file_state":"sunshine_state.json","vibeshine_file_state":"vibeshine_state.json","credentials_file":"","pkey":"credentials/cakey.pem","cert":"credentials/cacert.pem","log_path":"other.log","min_log_level":"info"}),
    }) else { panic!("restart update rejected") };
    for _ in 0..100 {
        pump_configuration_fence(&mut pairing, &mut configuration, &state, &mut broker);
        if pairing.configuration_hold.is_some() {
            break;
        }
    }
    assert!(pairing.configuration_hold.is_some());
    let pending = state.handle(
        "amaterasu.host-configuration",
        json!({"action":"get","key":"global/amaterasu.host-configuration"}),
    );
    let restart_change = pending["value"]["transaction"]["change"].clone();
    assert_eq!(restart_change["authority_epoch"], fourth_epoch);
    assert!(matches!(
        broker.dispatch(
            "amaterasu.management",
            Command::Cancel {
                request: restart_update
            }
        ),
        Response::Ok
    ));
    configuration = TestHostConfiguration::new(root.clone(), true);
    let mut restarted_settings = settings();
    let mut policy = policy_for_feedback(
        &["speaker.pcm"],
        crate::protocol::FeedbackPermission::Launch,
    );
    policy.configuration_owner = Some("amaterasu.host-configuration".into());
    restarted_settings.effect_authority = Some(policy);
    pairing =
        Pipeline::with_host_configuration(restarted_settings, Arc::new(Shared::default()), true);
    let startup_epoch = pairing.effect_gate.as_ref().unwrap().epoch().to_owned();
    assert_eq!(
        pairing.apply_effect_request(
            "input",
            "input",
            json!({"action":"recover","epoch":startup_epoch,"owner_epoch":"owner-5"}),
            now()
        ),
        Err("configuration_change_pending")
    );
    for _ in 0..200 {
        pump_configuration_fence(&mut pairing, &mut configuration, &state, &mut broker);
        if pairing.configuration_reconciled && pairing.configuration_hold.is_some() {
            break;
        }
    }
    assert!(pairing.configuration_reconciled && pairing.configuration_hold.is_some());
    let recovery = pairing
        .apply_effect_request(
            "input",
            "input",
            json!({"action":"recover","epoch":startup_epoch,"owner_epoch":"owner-5"}),
            now(),
        )
        .unwrap();
    assert_eq!(recovery["held"], true);
    let recovered_epoch = recovery["epoch"].as_str().unwrap().to_owned();
    assert_eq!(
        pairing.apply_effect_request(
            "input",
            "input",
            json!({"action":"recover","epoch":recovered_epoch,"owner_epoch":"owner-6"}),
            now()
        ),
        Err("configuration_change_pending")
    );
    let Response::Accepted {
        id: restart_reconcile,
    } = broker.dispatch(
        "amaterasu.management",
        Command::Request {
            service: HOST_CONFIGURATION_SERVICE.into(),
            timeout_ms: 30_000,
            operation_id: "reconcile-after-restart".into(),
            parent: None,
            payload: json!({"action":"reconcile_transaction","change":restart_change}),
        },
    )
    else {
        panic!("restart reconciliation rejected")
    };
    let mut restarted_result = None;
    for _ in 0..1000 {
        pump_configuration_fence(&mut pairing, &mut configuration, &state, &mut broker);
        if let Response::Result { payload } = broker.dispatch(
            "amaterasu.management",
            Command::TakeResult {
                request: restart_reconcile,
            },
        ) {
            restarted_result = Some(payload);
            break;
        }
    }
    assert_eq!(
        restarted_result.as_ref().map(|value| &value["ok"]),
        Some(&json!(true)),
        "{restarted_result:?}"
    );
    assert!(pairing.configuration_hold.is_none());
    let after_restart = state.handle(
        "amaterasu.host-configuration",
        json!({"action":"get","key":"global/amaterasu.host-configuration"}),
    );
    assert_eq!(after_restart["value"]["revision"], 3);
    assert!(after_restart["value"].get("transaction").is_none());
    let fifth_epoch = pairing
        .apply_effect_request(
            "input",
            "input",
            json!({"action":"recover","epoch":recovered_epoch,"owner_epoch":"owner-6"}),
            now(),
        )
        .unwrap()["epoch"]
        .as_str()
        .unwrap()
        .to_owned();
    let Response::Accepted { id: lost_begin_update } = broker.dispatch("amaterasu.management", Command::Request {
        service: HOST_CONFIGURATION_SERVICE.into(), timeout_ms:30_000, operation_id:"lost-begin-update".into(), parent:None,
        payload:json!({"action":"update","expected_revision":3,"file_state":"sunshine_state.json","vibeshine_file_state":"vibeshine_state.json","credentials_file":"","pkey":"credentials/cakey.pem","cert":"credentials/cacert.pem","log_path":"other.log","min_log_level":"info"}),
    }) else { panic!("lost begin update rejected") };
    let mut lose_begin = true;
    let mut begin_result = None;
    for _ in 0..300 {
        pump_configuration_fence_with_faults(
            &mut pairing,
            &mut configuration,
            &state,
            &mut broker,
            &mut false,
            &mut lose_begin,
        );
        if let Response::Result { payload } = broker.dispatch(
            "amaterasu.management",
            Command::TakeResult {
                request: lost_begin_update,
            },
        ) {
            begin_result = Some(payload);
            break;
        }
    }
    assert!(!lose_begin, "begin response was not dropped");
    assert_eq!(
        begin_result.as_ref().map(|value| &value["error"]["code"]),
        Some(&json!("configuration_transaction_unconfirmed")),
        "{begin_result:?}"
    );
    assert!(pairing.configuration_hold.is_some());
    let marker = state.handle(
        "amaterasu.host-configuration",
        json!({"action":"get","key":"global/amaterasu.host-configuration"}),
    );
    assert_eq!(marker["value"]["transaction"]["state"]["phase"], "prepared");
    let generation = pairing.effect_gate.as_ref().unwrap().fence_generation();
    pairing
        .apply_effect_request(
            "input",
            "input",
            json!({"action":"drain","epoch":fifth_epoch,"fence_generation":generation}),
            now(),
        )
        .unwrap();
    let Response::Accepted { id: begin_reconcile } = broker.dispatch("amaterasu.management", Command::Request {
        service: HOST_CONFIGURATION_SERVICE.into(), timeout_ms:30_000, operation_id:"reconcile-lost-begin".into(), parent:None,
        payload:json!({"action":"reconcile_transaction","change":marker["value"]["transaction"]["change"]}),
    }) else { panic!("lost begin reconciliation rejected") };
    let mut begin_reconciled = None;
    for _ in 0..500 {
        pump_configuration_fence(&mut pairing, &mut configuration, &state, &mut broker);
        if let Response::Result { payload } = broker.dispatch(
            "amaterasu.management",
            Command::TakeResult {
                request: begin_reconcile,
            },
        ) {
            begin_reconciled = Some(payload);
            break;
        }
    }
    assert_eq!(
        begin_reconciled.as_ref().map(|value| &value["ok"]),
        Some(&json!(true)),
        "{begin_reconciled:?}"
    );
    let final_state = state.handle(
        "amaterasu.host-configuration",
        json!({"action":"get","key":"global/amaterasu.host-configuration"}),
    );
    assert_eq!(final_state["value"]["revision"], 3);
    assert!(final_state["value"].get("transaction").is_none());
    let sixth_epoch = pairing
        .apply_effect_request(
            "input",
            "input",
            json!({"action":"recover","epoch":fifth_epoch,"owner_epoch":"owner-7"}),
            now(),
        )
        .unwrap()["epoch"]
        .as_str()
        .unwrap()
        .to_owned();
    let Response::Accepted { id: late_success_update } = broker.dispatch("amaterasu.management", Command::Request {
        service: HOST_CONFIGURATION_SERVICE.into(), timeout_ms:30_000, operation_id:"late-success-update".into(), parent:None,
        payload:json!({"action":"update","expected_revision":3,"file_state":"sunshine_state.json","vibeshine_file_state":"vibeshine_state.json","credentials_file":"","pkey":"credentials/cakey.pem","cert":"credentials/cacert.pem","log_path":"other.log","min_log_level":"info"}),
    }) else { panic!("late success update rejected") };
    for _ in 0..100 {
        pump_configuration_fence(&mut pairing, &mut configuration, &state, &mut broker);
        if pairing.configuration_hold.is_some() {
            break;
        }
    }
    assert!(pairing.configuration_hold.is_some());
    let generation = pairing.effect_gate.as_ref().unwrap().fence_generation();
    pairing
        .apply_effect_request(
            "input",
            "input",
            json!({"action":"drain","epoch":sixth_epoch,"fence_generation":generation}),
            now(),
        )
        .unwrap();
    let mut delayed_success = None;
    for _ in 0..200 {
        configuration
            .tick(|command| Ok(broker.dispatch("amaterasu.host-configuration", command)))
            .unwrap();
        pairing
            .tick_at(now(), |command| {
                Ok(broker.dispatch("amaterasu.pairing", command))
            })
            .unwrap();
        for _ in 0..16 {
            let Response::Request {
                id,
                caller,
                payload,
                ..
            } = broker.dispatch("state", Command::Poll {})
            else {
                break;
            };
            if caller == "amaterasu.host-configuration"
                && payload["action"] == "set"
                && payload["value"]["revision"] == 4
                && payload["value"]["transaction"]["state"]["phase"] == "committed"
            {
                delayed_success = Some((id, caller, payload));
                break;
            }
            let response = state.handle(&caller, payload);
            let _ = broker.dispatch(
                "state",
                Command::Complete {
                    request: id,
                    payload: response,
                },
            );
        }
        if delayed_success.is_some() {
            break;
        }
        if let Response::Request { id, payload, .. } =
            broker.dispatch("diagnostics", Command::Poll {})
        {
            let response = if payload["action"] == "status" {
                json!({"ok":true,"ready":true,"logging_configuration_revision":3,"logging_sink":{"state":"healthy","failure":null}})
            } else {
                json!({"ok":true,"applied_revision":payload["configuration"]["revision"]})
            };
            let _ = broker.dispatch(
                "diagnostics",
                Command::Complete {
                    request: id,
                    payload: response,
                },
            );
        }
    }
    let (late_id, late_caller, late_payload) =
        delayed_success.expect("late success write was not delivered");
    assert!(matches!(
        broker.dispatch(
            "amaterasu.management",
            Command::Cancel {
                request: late_success_update
            }
        ),
        Response::Ok
    ));
    let late = state.handle(&late_caller, late_payload);
    assert_eq!(late["ok"], true);
    assert!(matches!(
        broker.dispatch(
            "state",
            Command::Complete {
                request: late_id,
                payload: late
            }
        ),
        Response::Error { .. }
    ));
    let late_marker = state.handle(
        "amaterasu.host-configuration",
        json!({"action":"get","key":"global/amaterasu.host-configuration"}),
    );
    assert_eq!(late_marker["value"]["revision"], 4);
    assert_eq!(
        late_marker["value"]["transaction"]["state"]["phase"],
        "committed"
    );
    assert_eq!(late_marker["value"]["transaction"]["settled"], false);
    configuration = TestHostConfiguration::new(root.clone(), true);
    let mut restarted_settings = settings();
    let mut policy = policy_for_feedback(
        &["speaker.pcm"],
        crate::protocol::FeedbackPermission::Launch,
    );
    policy.configuration_owner = Some("amaterasu.host-configuration".into());
    restarted_settings.effect_authority = Some(policy);
    pairing =
        Pipeline::with_host_configuration(restarted_settings, Arc::new(Shared::default()), true);
    let startup_epoch = pairing.effect_gate.as_ref().unwrap().epoch().to_owned();
    assert_eq!(
        pairing.apply_effect_request(
            "input",
            "input",
            json!({"action":"recover","epoch":startup_epoch,"owner_epoch":"owner-8"}),
            now()
        ),
        Err("configuration_change_pending")
    );
    for _ in 0..200 {
        pump_configuration_fence(&mut pairing, &mut configuration, &state, &mut broker);
        if pairing.configuration_reconciled && pairing.configuration_hold.is_some() {
            break;
        }
    }
    assert!(pairing.configuration_reconciled && pairing.configuration_hold.is_some());
    let held_recovery = pairing
        .apply_effect_request(
            "input",
            "input",
            json!({"action":"recover","epoch":startup_epoch,"owner_epoch":"owner-8"}),
            now(),
        )
        .unwrap();
    assert_eq!(held_recovery["held"], true);
    let Response::Accepted { id: success_reconcile } = broker.dispatch("amaterasu.management", Command::Request {
        service: HOST_CONFIGURATION_SERVICE.into(), timeout_ms:30_000, operation_id:"reconcile-late-success".into(), parent:None,
        payload:json!({"action":"reconcile_transaction","change":late_marker["value"]["transaction"]["change"]}),
    }) else { panic!("late success reconciliation rejected") };
    let mut success_result = None;
    for _ in 0..1000 {
        pump_configuration_fence(&mut pairing, &mut configuration, &state, &mut broker);
        if let Response::Result { payload } = broker.dispatch(
            "amaterasu.management",
            Command::TakeResult {
                request: success_reconcile,
            },
        ) {
            success_result = Some(payload);
            break;
        }
    }
    assert_eq!(
        success_result.as_ref().map(|value| &value["ok"]),
        Some(&json!(true)),
        "{success_result:?}"
    );
    assert_eq!(success_result.unwrap()["logging"], "unconfirmed");
    let after_success = state.handle(
        "amaterasu.host-configuration",
        json!({"action":"get","key":"global/amaterasu.host-configuration"}),
    );
    assert_eq!(after_success["value"]["revision"], 4);
    assert!(after_success["value"].get("transaction").is_none());
    let terminal = after_success["value"]["terminal_receipt"].clone();
    let replay = broker_configuration_call(
        "amaterasu.management",
        json!({"action":"reconcile_transaction","change":stale_change}),
        &mut pairing,
        &mut configuration,
        &state,
        &mut broker,
    );
    assert_eq!(replay["error"]["code"], "configuration_change_unknown");
    assert_eq!(
        state.handle(
            "amaterasu.host-configuration",
            json!({"action":"get","key":"global/amaterasu.host-configuration"})
        )["value"]["terminal_receipt"],
        terminal
    );
    drop(state);
    fs::remove_dir_all(root).unwrap();
}
