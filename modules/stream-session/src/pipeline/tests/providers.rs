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
