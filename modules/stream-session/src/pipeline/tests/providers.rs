use super::*;
use crate::providers::ProviderServices;
use amaterasu_contracts::{Grants, RuntimeConfig};

fn alternatives() -> ProviderServices {
    ProviderServices {
        session: "vendor.sessions/1".into(),
        video: "vendor.frames/1".into(),
        audio_capture: "vendor.audio-capture/1".into(),
        audio_encoder: "vendor.audio-encoder/1".into(),
        media_transport: "vendor.media-delivery/1".into(),
        control_transport: "vendor.control/1".into(),
    }
}

#[test]
fn workflow_routes_to_selected_contracts_through_the_real_broker() {
    let providers = alternatives();
    let mut payload = prepare();
    payload.as_object_mut().unwrap().remove("action");
    let config = serde_json::from_value(payload).unwrap();
    let mut record = Record::new(42, "owner".into(), config, 30_000);
    record.providers = providers;
    let resources = [
        Resource::Session,
        Resource::Video,
        Resource::AudioCapture,
        Resource::AudioEncoder,
        Resource::Media,
        Resource::Control,
    ];
    let services = resources
        .iter()
        .map(|resource| record.service(*resource))
        .collect::<BTreeSet<_>>();
    let mut config = RuntimeConfig::default();
    config.principals.insert(
        "coordinator".into(),
        Grants {
            call: services.clone(),
            ..Default::default()
        },
    );
    config.principals.insert(
        "replacement".into(),
        Grants {
            provide: services.clone(),
            ..Default::default()
        },
    );
    let mut broker = amaterasu_runtime::Broker::new(config).unwrap();
    for service in services {
        assert!(matches!(
            broker.dispatch("replacement", Command::Register { service }),
            Response::Ok
        ));
    }
    for resource in resources {
        record.resources.insert(resource, 64);
        for (index, step) in [Step::Find(resource), Step::Close(resource)]
            .into_iter()
            .enumerate()
        {
            let job = Job::new(record.id, None, step, 5000);
            let (service, payload) = job.command(&record);
            assert_eq!(service, record.service(resource));
            let Response::Accepted { id } = broker.dispatch(
                "coordinator",
                Command::Request {
                    service: service.clone(),
                    payload,
                    timeout_ms: 5000,
                    operation_id: format!("replacement.{}.{index}", resource.name()),
                    parent: None,
                },
            ) else {
                panic!("configured replacement service was not granted");
            };
            let Response::Request {
                id: delivered,
                caller,
                service: actual,
                ..
            } = broker.dispatch("replacement", Command::Poll {})
            else {
                panic!("replacement received no request");
            };
            assert_eq!(delivered, id);
            assert_eq!(caller, "coordinator");
            assert_eq!(actual, service);
            assert!(matches!(
                broker.dispatch(
                    "replacement",
                    Command::Complete {
                        request: id,
                        payload: json!({"ok":true})
                    }
                ),
                Response::Ok
            ));
            assert!(matches!(
                broker.dispatch("coordinator", Command::TakeResult { request: id }),
                Response::Result { .. }
            ));
            assert!(matches!(
                broker.dispatch("coordinator", Command::ReleaseResult { request: id }),
                Response::Ok
            ));
        }
    }
}

#[test]
fn replacement_input_provider_handles_mixed_batches_and_partial_failure() {
    use amaterasu_module_sdk::input::{
        INPUT_SERVICE, InputBatchCapabilities, batch_success, record_batch_failure,
    };

    let mut payload = prepare();
    payload.as_object_mut().unwrap().remove("action");
    let config = serde_json::from_value(payload).unwrap();
    let mut record = Record::new(42, "owner".into(), config, 30_000);
    let mut lease_reply = json!({"ok":true,"state":"active",
        "operation_id":record.input_operation(),"lease":"replacement-lease",
        "ttl_ms":record.input_ttl_ms,"expires_in_ms":record.input_ttl_ms,
        "backend":"replacement","cleanup_required":false,"controller_input":true});
    InputBatchCapabilities { mixed_events: true }
        .write_lease_response(&mut lease_reply)
        .unwrap();
    super::super::input::accept_input_lease(&mut record, &lease_reply, 0).unwrap();
    record.input_events.extend([
        json!({"kind":"relative","dx":1,"dy":2}),
        json!({"kind":"controller","number":0,"buttons":1}),
        json!({"kind":"key","key":{"code":30},"state":"up"}),
    ]);

    let mut config = RuntimeConfig::default();
    config.principals.insert(
        "coordinator".into(),
        Grants {
            call: [INPUT_SERVICE.into()].into_iter().collect(),
            ..Default::default()
        },
    );
    config.principals.insert(
        "replacement".into(),
        Grants {
            provide: [INPUT_SERVICE.into()].into_iter().collect(),
            ..Default::default()
        },
    );
    let mut broker = amaterasu_runtime::Broker::new(config).unwrap();
    assert!(matches!(
        broker.dispatch(
            "replacement",
            Command::Register {
                service: INPUT_SERVICE.into()
            }
        ),
        Response::Ok
    ));

    let exchange =
        |broker: &mut amaterasu_runtime::Broker, record: &Record, operation: &str, reply: Value| {
            let (service, payload) =
                Job::new(record.id, None, Step::InputBatch, 5000).command(record);
            let expected = payload["events"].clone();
            let Response::Accepted { id } = broker.dispatch(
                "coordinator",
                Command::Request {
                    service,
                    payload,
                    timeout_ms: 5000,
                    operation_id: operation.into(),
                    parent: None,
                },
            ) else {
                panic!("input request was not admitted")
            };
            let Response::Request {
                id: delivered,
                payload,
                ..
            } = broker.dispatch("replacement", Command::Poll {})
            else {
                panic!("replacement did not receive the batch")
            };
            assert_eq!(delivered, id);
            assert_eq!(payload["events"], expected);
            assert!(matches!(
                broker.dispatch(
                    "replacement",
                    Command::Complete {
                        request: id,
                        payload: reply
                    }
                ),
                Response::Ok
            ));
            let Response::Result { payload } =
                broker.dispatch("coordinator", Command::TakeResult { request: id })
            else {
                panic!("replacement result missing")
            };
            assert!(matches!(
                broker.dispatch("coordinator", Command::ReleaseResult { request: id }),
                Response::Ok
            ));
            payload
        };

    record.input_inflight = super::super::input::next_input_batch_size(&record);
    assert_eq!(record.input_inflight, 3);
    let mut wrong_event = json!({"ok":false,"error":{"code":"controller_unavailable"}});
    record_batch_failure(&mut wrong_event, "replacement-lease", 3, 0, true).unwrap();
    assert_eq!(
        super::super::input::accept_controller_batch_failure(&mut record, &wrong_event),
        Err("input_batch_failed")
    );
    assert_eq!(record.input_events.len(), 3);
    let mut failure =
        json!({"ok":false,"error":{"code":"controller_unavailable","message":"unavailable"}});
    record_batch_failure(&mut failure, "replacement-lease", 3, 1, true).unwrap();
    let response = exchange(&mut broker, &record, "mixed-partial", failure);
    super::super::input::accept_controller_batch_failure(&mut record, &response).unwrap();
    assert_eq!(record.input_events.len(), 1);
    assert_eq!(record.input_events[0]["kind"], "key");
    assert_eq!(record.input_events_sent, 1);

    record.input_inflight = super::super::input::next_input_batch_size(&record);
    let response = exchange(
        &mut broker,
        &record,
        "mixed-tail",
        batch_success("replacement-lease", 1).unwrap(),
    );
    super::super::input::accept_input_batch(&mut record, &response).unwrap();
    assert!(record.input_events.is_empty());
    assert_eq!(record.input_events_sent, 2);
}

#[test]
fn legacy_input_provider_keeps_homogeneous_batches() {
    let mut payload = prepare();
    payload.as_object_mut().unwrap().remove("action");
    let config = serde_json::from_value(payload).unwrap();
    let mut record = Record::new(43, "owner".into(), config, 30_000);
    let lease_reply = json!({"ok":true,"state":"active",
        "operation_id":record.input_operation(),"lease":"legacy-lease",
        "ttl_ms":record.input_ttl_ms,"expires_in_ms":record.input_ttl_ms,
        "backend":"legacy","cleanup_required":false,"controller_input":true});
    super::super::input::accept_input_lease(&mut record, &lease_reply, 0).unwrap();
    record.input_events.extend([
        json!({"kind":"relative","dx":1,"dy":2}),
        json!({"kind":"controller","number":0,"buttons":1}),
    ]);
    assert!(!record.input_mixed_batch_supported);
    assert_eq!(super::super::input::next_input_batch_size(&record), 1);
}

#[test]
fn malformed_mixed_batch_capability_does_not_admit_a_lease() {
    let mut payload = prepare();
    payload.as_object_mut().unwrap().remove("action");
    let config = serde_json::from_value(payload).unwrap();
    let mut record = Record::new(44, "owner".into(), config, 30_000);
    let lease_reply = json!({"ok":true,"state":"active",
        "operation_id":record.input_operation(),"lease":"replacement-lease",
        "ttl_ms":record.input_ttl_ms,"expires_in_ms":record.input_ttl_ms,
        "backend":"replacement","cleanup_required":false,
        "controller_input":true,"mixed_input_batches":"true"});
    assert_eq!(
        super::super::input::accept_input_lease(&mut record, &lease_reply, 0),
        Err("invalid_input_response")
    );
    assert!(record.input_lease.is_none());
}

#[test]
fn request_payload_cannot_replace_the_startup_selected_services() {
    let mut host = Harness::new();
    let mut request = prepare();
    request["providers"] = json!(alternatives());
    assert_eq!(host.response(1, request)["code"], "invalid_request");
    assert!(host.pipeline.records.is_empty());
}

#[test]
fn each_record_pins_startup_services_for_all_later_workflow_operations() {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    let original = host.pipeline.records[&id].providers.clone();
    host.pipeline.settings.providers = alternatives();
    let record = &host.pipeline.records[&id];
    assert_eq!(record.providers, original);
    assert_eq!(
        Job::new(id, None, Step::Find(Resource::Media), 5000)
            .command(record)
            .0,
        original.media_transport
    );
}

#[test]
fn custom_native_roles_require_an_explicit_contract_acknowledgment_before_binding() {
    let mut payload = prepare();
    payload.as_object_mut().unwrap().remove("action");
    let config = serde_json::from_value(payload).unwrap();
    let mut record = Record::new(42, "owner".into(), config, 30_000);
    record.providers.control_transport = "vendor.control/1".into();
    assert_eq!(
        record.open(Resource::Control)["required_native_contract"],
        "amaterasu.control-transport/1"
    );
    for response in [
        json!({"ok":true,"stream":7}),
        json!({"ok":true,"stream":7,"native_contract":"amaterasu.audio/1"}),
    ] {
        let mut host = Harness::new();
        let mut job = Job::new(42, None, Step::Open(Resource::Control), 5000);
        assert_eq!(
            host.pipeline.opened(
                &mut record,
                &mut job,
                Resource::Control,
                &response,
                0,
                &mut |_| panic!("no further calls before contract confirmation")
            ),
            Err("native_contract_unconfirmed")
        );
        assert_eq!(
            record.resources[&Resource::Control],
            7,
            "allocated handle retained for cleanup"
        );
        assert!(record.plane.is_none());
    }
    assert_eq!(
        record.validate_native_contract(
            Resource::Control,
            &json!({"native_contract":"amaterasu.control-transport/1"})
        ),
        Ok(())
    );
    assert!(
        record
            .open(Resource::AudioEncoder)
            .get("required_native_contract")
            .is_none()
    );
}
