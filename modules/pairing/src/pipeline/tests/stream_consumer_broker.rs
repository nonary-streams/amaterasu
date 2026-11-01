//! Consumer integration with the real pairing pipeline and runtime broker.
//! Source binding is test-only; no lifecycle authentication is claimed here.
use super::*;
use amaterasu_module_sdk::effect_authority::{
    Source, StreamScope,
    consumer::{FeedbackLease, StreamLeaseTicket, TransportRetirement},
};
use amaterasu_module_sdk::input::extensions::{Capability, Direction};
use amaterasu_module_sdk::transport::control_retirement::Receipt;

fn request_over_broker(
    pipeline: &mut Pipeline,
    broker: &mut amaterasu_runtime::Broker,
    request: &impl serde::Serialize,
    operation: &str,
) -> Value {
    let Response::Accepted { id } = broker.dispatch(
        "input",
        Command::Request {
            service: EFFECT_SERVICE.into(),
            payload: serde_json::to_value(request).unwrap(),
            timeout_ms: 30_000,
            operation_id: operation.into(),
            parent: None,
        },
    ) else {
        panic!("request rejected")
    };
    pipeline
        .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
        .unwrap();
    let Response::Result { payload } =
        broker.dispatch("input", Command::TakeResult { request: id })
    else {
        panic!("missing result")
    };
    assert!(matches!(
        broker.dispatch("input", Command::ReleaseResult { request: id }),
        Response::Ok
    ));
    payload
}

#[test]
fn scoped_consumer_exchanges_admission_and_retirement_with_actual_pairing_broker() {
    let (mut pipeline, mut harness, certificate_id) = paired_native_client();
    let schema = "org.amaterasu.feedback.haptics_pcm";
    grant_feedback_for_test(&mut harness, &mut pipeline, &certificate_id, &[schema]);
    pipeline.effect_authority = Some(policy_for_feedback(
        &[schema],
        crate::protocol::FeedbackPermission::View,
    ));
    let initial = pipeline.effect_gate.as_ref().unwrap().epoch().to_owned();
    let recovered = pipeline
        .apply_effect_request(
            "input",
            "input",
            json!({"action":"recover","epoch":initial,"owner_epoch":"owner"}),
            now(),
        )
        .unwrap();
    let epoch = recovered["epoch"].as_str().unwrap().to_owned();
    let source: Source = serde_json::from_value(json!({"version":1,"id":"ab".repeat(16),"generation":1,"host_epoch":"host","session_id":"session","stream_id":"stream","stream_generation":1,"device":"device","device_generation":1,"effect_owner_epoch":"owner"})).unwrap();
    let scope: StreamScope = serde_json::from_value(json!({"capabilities":[{"schema":"org.amaterasu.feedback.haptics_pcm","version":1,"direction":"to_client"}],"controller_slot":3,"transport_service":"amaterasu.control-transport/1","transport_stream":7,"transport_epoch":"c".repeat(32)})).unwrap();
    pipeline.apply_effect_request("nvhttp", "nvhttp", json!({"action":"bind_stream","epoch":epoch,"certificate_pem":CLIENT_CERT,"source":source,"scope":scope}), now()).unwrap();
    let mut broker = authority_broker();
    let mut admission = StreamLeaseTicket::new(
        epoch.clone(),
        source.clone(),
        "lease".into(),
        1,
        scope.clone(),
    )
    .unwrap();
    let response = request_over_broker(
        &mut pipeline,
        &mut broker,
        &admission.admit_request().unwrap(),
        "admit-lease",
    );
    for (field, value) in [
        ("controller_slot", json!(4)),
        ("transport_stream", json!(8)),
        ("transport_epoch", json!("d".repeat(32))),
        ("transport_service", json!("other.control/1")),
    ] {
        let mut changed = response.clone();
        changed["scope"][field] = value;
        assert!(admission.accept_admission(&changed).is_err());
    }
    let mut duplicated = response.clone();
    let capability = duplicated["scope"]["capabilities"][0].clone();
    duplicated["scope"]["capabilities"]
        .as_array_mut()
        .unwrap()
        .push(capability);
    assert!(admission.accept_admission(&duplicated).is_err());
    let mut wrong_source = response.clone();
    wrong_source["source"]["device_generation"] = json!(2);
    assert!(admission.accept_admission(&wrong_source).is_err());
    let mut wrong_capability = response.clone();
    wrong_capability["scope"]["capabilities"][0]["version"] = json!(2);
    assert!(admission.accept_admission(&wrong_capability).is_err());
    admission.accept_admission(&response).unwrap();
    let capability = Capability {
        schema: "org.amaterasu.feedback.haptics_pcm".into(),
        version: 1,
        direction: Direction::ToClient,
    };
    let retirement = TransportRetirement {
        stream: 7,
        operation_id: "retire".into(),
        provider_epoch: "c".repeat(32),
    };
    let mut lease = FeedbackLease::new(admission, retirement, 3, [capability.clone()].into())
        .unwrap_or_else(|_| panic!("lease"));
    for sequence in 1..=300 {
        assert_eq!(
            lease
                .enqueue(
                    &capability,
                    sequence,
                    &format!("pcm-{sequence}"),
                    &[0; 960],
                    || Ok::<_, ()>(true)
                )
                .unwrap(),
            Ok(true)
        );
    }
    assert_eq!(pipeline.effect_gate.as_ref().unwrap().in_flight(), 1);
    pipeline.effect_gate.as_mut().unwrap().fence();
    let generation = pipeline.effect_gate.as_ref().unwrap().fence_generation();
    lease.fence();
    // This test uses a synthetic receipt: the separate transport test covers the
    // actual provider receipt and ENet queue retirement.
    let receipt = Receipt {
        ok: true,
        state: "retired".into(),
        scope: "whole_control_stream".into(),
        stream: 7,
        operation_id: "retire".into(),
        provider_epoch: "c".repeat(32),
    };
    let mut lease = lease.retire(&receipt).unwrap_or_else(|_| panic!("retire"));
    let complete = lease.completion_request().unwrap();
    let completion = request_over_broker(&mut pipeline, &mut broker, &complete, "complete-lease");
    // Lost reply: repeat the exact completion over a fresh broker operation.
    let completion_again =
        request_over_broker(&mut pipeline, &mut broker, &complete, "complete-retry");
    assert_eq!(completion, completion_again);
    lease.confirm_completion(&completion_again).unwrap();
    let acknowledgement = request_over_broker(
        &mut pipeline,
        &mut broker,
        &lease.acknowledgment_request().unwrap(),
        "ack-lease",
    );
    lease.confirm_acknowledgment(&acknowledgement).unwrap();
    let drain = json!({"action":"drain","epoch":epoch,"fence_generation":generation});
    assert_eq!(
        request_over_broker(&mut pipeline, &mut broker, &drain, "drain")["ok"],
        true
    );
    assert!(pipeline.effect_gate.as_ref().unwrap().drained());
}
