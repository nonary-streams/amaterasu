use super::{speaker_pcm, steam_attributes};

fn attributes() -> steam_attributes::Attributes {
    steam_attributes::Attributes {
        transport: 1,
        valid_mask: 0xff,
        firmware_build_time: 0x0102_0304,
        bootloader_build_time: 5,
        board_revision: 6,
        capabilities: 7,
        radio_firmware_build_time: 8,
        dongle_firmware_build_time: 9,
        build_id: *b"build-id-1234567",
        unit_serial: *b"unit-serial-1234",
        board_serial: *b"board-serial-123",
    }
}
fn block() -> speaker_pcm::Block {
    speaker_pcm::Block {
        owner: 9,
        generation: 7,
        sequence: 3,
        sample_position: 480,
        flags: 0,
        samples: [0x81; speaker_pcm::SAMPLE_BYTES],
    }
}

#[test]
fn steam_attributes_have_fixed_little_endian_schema_without_slot_or_padding() {
    let source = attributes();
    let encoded = source.encode().unwrap();
    assert_eq!(encoded.len(), steam_attributes::PAYLOAD_BYTES);
    assert_eq!(&encoded[..6], &[1, 0xff, 4, 3, 2, 1]);
    assert!(steam_attributes::Attributes::decode(&encoded).unwrap() == source);
    assert!(steam_attributes::Attributes::decode(&encoded[..73]).is_err());
    let mut extra = encoded.clone();
    extra.push(0);
    assert!(steam_attributes::Attributes::decode(&extra).is_err());
    let mut unknown_transport = encoded;
    unknown_transport[0] = 3;
    assert_eq!(
        steam_attributes::Attributes::decode(&unknown_transport)
            .unwrap()
            .transport,
        3
    );
}

#[test]
fn speaker_block_checks_format_bounds_and_source_fields() {
    let source = block();
    let encoded = source.encode().unwrap();
    assert_eq!(encoded.len(), speaker_pcm::PAYLOAD_BYTES);
    assert_eq!(&encoded[0..8], &9u64.to_le_bytes());
    assert_eq!(&encoded[16..24], &480u64.to_le_bytes());
    assert!(speaker_pcm::Block::decode(&encoded).unwrap() == source);
    assert!(speaker_pcm::Block::decode(&encoded[..985]).is_err());
    let mut invalid = encoded.clone();
    invalid[0..8].fill(0);
    assert!(speaker_pcm::Block::decode(&invalid).is_err());
    let mut invalid = encoded.clone();
    invalid[8..12].fill(0);
    assert!(speaker_pcm::Block::decode(&invalid).is_err());
    let mut invalid = encoded.clone();
    invalid[24..26].copy_from_slice(&2u16.to_le_bytes());
    assert!(speaker_pcm::Block::decode(&invalid).is_err());
    let mut invalid = encoded;
    invalid[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(speaker_pcm::Block::decode(&invalid).is_err());
}

#[test]
fn sampled_capacity_is_bounded_and_expiry_frees_slots_without_consuming_rejected_sequence() {
    use super::{
        Capability, Direction, Endpoint, ExtensionAdapter, IncomingRequest, MAX_PENDING_SAMPLES,
        Message, Principal, Route, RoutePolicy, SERVICE, SubmitError,
    };
    use serde_json::json;
    use std::{
        collections::{BTreeMap, BTreeSet},
        time::{Duration, Instant},
    };
    struct Sink;
    impl Endpoint for Sink {
        fn supports(&self, _: &Capability) -> bool {
            true
        }
        fn validate(&self, _: &Message) -> Result<(), &'static str> {
            Ok(())
        }
        fn submit(&mut self, _: &Message) -> Result<(), SubmitError> {
            Ok(())
        }
    }
    let route = |name: &str| Route {
        host_epoch: "epoch".into(),
        session: "session".into(),
        source: "source".into(),
        device: name.into(),
        generation: 1,
    };
    let cap = |index: usize| Capability {
        schema: format!("example.sample-{index}"),
        version: 1,
        direction: Direction::ToClient,
    };
    let policy = |route: Route, caps: BTreeSet<Capability>| RoutePolicy {
        route,
        provider_offer: caps.clone(),
        adapter_offer: caps.clone(),
        required: BTreeSet::new(),
        sampled: caps.into_iter().map(|c| (c, 5)).collect::<BTreeMap<_, _>>(),
        terminal: BTreeMap::new(),
        to_device: vec![],
        to_client: vec![Principal {
            caller: "sender".into(),
            initiator: "sender".into(),
        }],
    };
    let first = route("first");
    let second = route("second");
    let mut adapter = ExtensionAdapter::new(
        Sink,
        vec![
            policy(first.clone(), (0..MAX_PENDING_SAMPLES).map(cap).collect()),
            policy(second.clone(), [cap(MAX_PENDING_SAMPLES)].into()),
        ],
    )
    .unwrap();
    let request = |route: Route, capability: Capability| IncomingRequest {
        id: 1,
        caller: "sender".into(),
        initiator: "sender".into(),
        service: SERVICE.into(),
        remaining_ms: 1000,
        payload: serde_json::to_value(Message {
            route,
            capability,
            sequence: 1,
            payload: vec![],
        })
        .unwrap(),
    };
    let start = Instant::now();
    for index in 0..MAX_PENDING_SAMPLES {
        assert_eq!(
            adapter.dispatch_at(&request(first.clone(), cap(index)), start)["ok"],
            true
        );
    }
    assert_eq!(adapter.pending_sample_count(), MAX_PENDING_SAMPLES);
    let overflow = request(second, cap(MAX_PENDING_SAMPLES));
    assert_eq!(
        adapter.dispatch_at(&overflow, start)["error"]["code"],
        "busy"
    );
    assert_eq!(
        adapter.dispatch_at(&overflow, start + Duration::from_millis(5)),
        json!({"ok":true,"accepted":true})
    );
    assert_eq!(adapter.pending_sample_count(), 1);
}

#[test]
fn source_observation_is_strictly_a_negotiated_label_not_a_route_grant() {
    use super::source_label::observe;
    use amaterasu_extension_api::LifecycleTarget;
    use serde_json::json;
    let target = LifecycleTarget::Stream {
        host_epoch: "epoch".into(),
        session_id: "session".into(),
        stream_id: "stream".into(),
        generation: 3,
    };
    let response = json!({"status":"succeeded", "active":true, "target":target,
        "source":{"version":1,"id":"0123456789abcdef0123456789abcdef","generation":3}});
    assert!(observe(&response, &target).unwrap().is_some());
    let mut null = response.clone();
    null["source"] = serde_json::Value::Null;
    assert!(observe(&null, &target).unwrap().is_none());
    let mut missing = response.clone();
    missing.as_object_mut().unwrap().remove("source");
    assert!(observe(&missing, &target).is_err());
    for source in [
        json!({"version":2,"id":"0123456789abcdef0123456789abcdef","generation":3}),
        json!({"version":1,"id":"0123456789ABCDEF0123456789abcdef","generation":3}),
        json!({"version":1,"id":"0123456789abcdef0123456789abcdef","generation":4}),
        json!({"version":1,"id":"0123456789abcdef0123456789abcdef","generation":3,"grant":true}),
    ] {
        let mut bad = response.clone();
        bad["source"] = source;
        assert!(observe(&bad, &target).is_err());
    }
    let mut inactive = response.clone();
    inactive["active"] = json!(false);
    assert!(observe(&inactive, &target).is_err());
    let mut forged_target = response.clone();
    forged_target["target"]["stream_id"] = json!("other");
    assert!(observe(&forged_target, &target).is_err());
}
