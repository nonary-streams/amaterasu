use super::*;
use std::net::UdpSocket;

pub(crate) fn configured() -> Controller {
    Controller::new(Settings {
        network_provider: None,
        enabled: true,
        allow_remote: false,
        allowed_principals: vec!["alice".into(), "bob".into()],
    })
}

pub(crate) fn request() -> Open {
    serde_json::from_value(json!({
        "peer_ip": "127.0.0.1",
        "connect_data": 17,
        "key_hex": "0123456789abcdef0123456789abcdef",
        "operation_id": "first"
    }))
    .unwrap()
}

#[test]
fn defaults_and_principal_policy_reject_before_allocating() {
    let mut disabled = Controller::new(Settings::default());
    assert_eq!(disabled.open("alice", request()).err(), Some("disabled"));
    let mut controller = configured();
    assert_eq!(
        controller.open("mallory", request()).err(),
        Some("forbidden")
    );
    assert!(controller.streams.is_empty());
    assert!(controller.used_keys.is_empty());
}

#[test]
fn ping_timeout_defaults_and_accepts_only_the_signed_integer_range() {
    let mut request = request();
    assert_eq!(request.ping_timeout_ms, 10_000);
    assert!(request.validate(false).is_ok());

    for value in [0, i32::MAX as i64] {
        request.ping_timeout_ms = value;
        assert!(request.validate(false).is_ok());
    }
    for value in [-1, i32::MAX as i64 + 1] {
        request.ping_timeout_ms = value;
        assert_eq!(request.validate(false), Err("invalid_ping_timeout"));
    }
}

#[test]
fn replay_is_owned_and_closed_identifiers_never_allocate_again() {
    let mut controller = configured();
    let reply = controller.open("alice", request()).unwrap();
    let id = reply.created.unwrap();
    let local = reply.payload["local"].as_str().unwrap().to_owned();
    assert!(!reply.payload.to_string().contains(&request().key_hex));
    let replay = controller.open("alice", request()).unwrap();
    assert_eq!(replay.created, None);
    assert_eq!(replay.payload["stream"], id);
    assert_eq!(controller.close("bob", id), Err("stream_not_found"));
    assert_eq!(controller.status("bob", None)["streams"], json!([]));
    assert!(
        controller
            .handle(
                "bob",
                Operation::Lookup {
                    operation_id: "first".into()
                }
            )
            .is_err()
    );
    let mut conflicting = request();
    conflicting.connect_data += 1;
    assert_eq!(
        controller.open("alice", conflicting).err(),
        Some("operation_conflict")
    );
    controller.close("alice", id).unwrap();
    assert!(UdpSocket::bind(local).is_ok());
    assert_eq!(
        controller.open("alice", request()).err(),
        Some("stream_closed")
    );
    let mut fresh_operation = request();
    fresh_operation.operation_id = Some("second".into());
    assert_eq!(
        controller.open("bob", fresh_operation).err(),
        Some("key_reuse_forbidden")
    );
}

#[test]
fn failed_socket_allocation_does_not_consume_operation_or_key() {
    let occupied = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut specification = request();
    specification.local_bind = Some(occupied.local_addr().unwrap());
    let mut controller = configured();
    assert_eq!(
        controller.open("alice", specification.clone()).err(),
        Some("udp_bind_failed")
    );
    assert!(controller.operations.is_empty());
    assert!(controller.issued_ids.is_empty());
    assert!(controller.used_keys.is_empty());
    drop(occupied);
    assert!(
        controller
            .open("alice", specification)
            .unwrap()
            .created
            .is_some()
    );
}

#[test]
fn invalid_remote_bind_and_key_inputs_fail_before_resource_admission() {
    let mut controller = configured();
    for (field, value, expected) in [
        ("peer_ip", "192.0.2.1", "remote_disabled"),
        ("peer_ip", "::ffff:127.0.0.1", "invalid_peer"),
        ("peer_ip", "239.1.2.3", "invalid_peer"),
        ("local_bind", "0.0.0.0:0", "invalid_bind"),
        ("local_bind", "[::1]:0", "invalid_bind"),
        ("key_hex", "not a key", "invalid_key"),
        ("operation_id", "bad\nname", "invalid_operation_id"),
    ] {
        let mut payload = json!({
            "peer_ip": "127.0.0.1",
            "connect_data": 17,
            "key_hex": "0123456789abcdef0123456789abcdef"
        });
        payload[field] = value.into();
        let request = serde_json::from_value(payload).unwrap();
        assert_eq!(controller.open("alice", request).err(), Some(expected));
    }
    assert!(controller.streams.is_empty());
    assert!(controller.operations.is_empty());
    assert!(controller.used_keys.is_empty());
}

#[test]
fn remote_opt_in_does_not_admit_broadcast_multicast_mapped_or_mismatched_binds() {
    let mut controller = configured();
    controller.settings.allow_remote = true;
    for (peer, local) in [
        ("127.0.0.1", "255.255.255.255:0"),
        ("127.0.0.1", "239.1.2.3:0"),
        ("127.0.0.1", "[::1]:0"),
        ("::1", "[ff02::1]:0"),
        ("::1", "[::ffff:127.0.0.1]:0"),
        ("::1", "127.0.0.1:0"),
    ] {
        let mut specification = request();
        specification.peer_ip = peer.parse().unwrap();
        specification.local_bind = Some(local.parse().unwrap());
        assert_eq!(
            controller.open("alice", specification).err(),
            Some("invalid_bind")
        );
    }
    assert!(controller.streams.is_empty());
    assert!(controller.operations.is_empty());
    assert!(controller.issued_ids.is_empty());
    assert!(controller.used_keys.is_empty());
}

#[test]
fn whole_stream_retirement_discards_unsent_feedback_and_reconciles_owned_receipt() {
    use amaterasu_media::{CONTROL_RELIABLE, ControlMessage, MEDIA_CLOSED};
    use amaterasu_module_sdk::transport::control_retirement::Receipt;
    use std::io;

    let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
    receiver.set_nonblocking(true).unwrap();
    let mut controller = configured();
    let opened = controller.open("alice", request()).unwrap();
    let id = opened.created.unwrap();
    let epoch = opened.payload["retirement_epoch"]
        .as_str()
        .unwrap()
        .to_owned();
    let local = opened.payload["local"].as_str().unwrap().to_owned();
    controller
        .streams
        .get_mut(&id)
        .unwrap()
        .stage_pending_datagram_for_test(
            receiver.local_addr().unwrap(),
            b"queued-feedback".to_vec(),
        );
    controller
        .streams
        .get_mut(&id)
        .unwrap()
        .stage_reliable_enet_for_test(receiver.local_addr().unwrap(), request().connect_data);
    assert_eq!(
        controller.retire("bob", id, "revoke-1", &epoch),
        Err("stream_not_found")
    );
    assert_eq!(
        controller.retire("alice", id, "revoke-1", "wrong-epoch"),
        Err("retirement_epoch_mismatch")
    );
    assert_eq!(
        controller.retirement_status("alice", id, "revoke-1", &epoch),
        Err("retirement_unknown")
    );
    let receipt = controller.retire("alice", id, "revoke-1", &epoch).unwrap();
    let typed: Receipt = serde_json::from_value(receipt.clone()).unwrap();
    assert!(typed.matches(id, "revoke-1", &epoch));
    assert!(!typed.matches(id, "revoke-2", &epoch));
    assert_eq!(
        controller.retire("alice", id, "revoke-1", &epoch).unwrap(),
        receipt
    );
    assert_eq!(
        controller
            .retirement_status("alice", id, "revoke-1", &epoch)
            .unwrap(),
        receipt
    );
    assert_eq!(
        controller.retire("alice", id, "other-operation", &epoch),
        Err("operation_conflict")
    );
    assert_eq!(
        controller.retirement_status("bob", id, "revoke-1", &epoch),
        Err("retirement_unknown")
    );
    let message = ControlMessage {
        message_type: 1,
        channel: 0,
        delivery: CONTROL_RELIABLE,
        reserved: 0,
        data: std::ptr::null(),
        len: 0,
        sequence: 0,
    };
    // SAFETY: the zero-length message is valid and remains live during this call.
    assert_eq!(
        unsafe { controller.send("alice", id, &message) },
        MEDIA_CLOSED
    );
    controller.tick();
    let mut bytes = [0; 64];
    assert_eq!(
        receiver.recv_from(&mut bytes).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    // The bundled socket provider closes the loopback socket when its last guard drops.
    assert!(UdpSocket::bind(local).is_ok());
    let mut restarted = configured();
    restarted.retirement_epoch = Some("new-provider-epoch".into());
    assert_eq!(
        restarted.retirement_status("alice", id, "revoke-1", &epoch),
        Err("retirement_epoch_mismatch")
    );
}

#[test]
fn whole_stream_retirement_is_owner_scoped_and_reuses_no_operation_for_another_stream() {
    let mut controller = configured();
    let first = controller.open("alice", request()).unwrap();
    let first_id = first.created.unwrap();
    let epoch = first.payload["retirement_epoch"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut second_request = request();
    second_request.key_hex = "ab".repeat(16);
    second_request.operation_id = Some("second".into());
    let second = controller.open("bob", second_request).unwrap();
    let second_id = second.created.unwrap();
    assert_eq!(
        controller.retire("alice", second_id, "revoke-2", &epoch),
        Err("stream_not_found")
    );
    controller
        .retire("alice", first_id, "revoke-1", &epoch)
        .unwrap();
    let mut third_request = request();
    third_request.key_hex = "cd".repeat(16);
    third_request.operation_id = Some("third".into());
    let third_id = controller
        .open("alice", third_request)
        .unwrap()
        .created
        .unwrap();
    assert_eq!(
        controller.retire("alice", third_id, "revoke-1", &epoch),
        Err("operation_conflict")
    );
    assert!(controller.owned("alice", third_id).is_ok());
    assert!(controller.owned("bob", second_id).is_ok());
    assert_eq!(
        controller.retirement_status("bob", first_id, "revoke-1", &epoch),
        Err("retirement_unknown")
    );
    assert_eq!(
        controller
            .retire("bob", second_id, "revoke-1", &epoch)
            .unwrap()["scope"],
        "whole_control_stream"
    );
}

#[test]
fn whole_stream_retirement_waits_for_inflight_socket_handoff() {
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;

    let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
    receiver
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut controller = configured();
    let opened = controller.open("alice", request()).unwrap();
    let id = opened.created.unwrap();
    let epoch = opened.payload["retirement_epoch"]
        .as_str()
        .unwrap()
        .to_owned();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let stream = controller.streams.get_mut(&id).unwrap();
    stream
        .stage_pending_datagram_for_test(receiver.local_addr().unwrap(), b"before-retire".to_vec());
    stream.pause_local_send_for_test(entered_tx, resume_rx);
    let shared = Arc::new(Mutex::new(controller));
    let ticking = Arc::clone(&shared);
    let tick = std::thread::spawn(move || ticking.lock().unwrap().tick());
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(shared.try_lock().is_err());
    let retiring = Arc::clone(&shared);
    let (started_tx, started_rx) = mpsc::channel();
    let (retired_tx, retired_rx) = mpsc::channel();
    let retirement = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        let receipt = retiring
            .lock()
            .unwrap()
            .retire("alice", id, "revoke-race", &epoch)
            .unwrap();
        retired_tx.send(receipt).unwrap();
    });
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(
        retired_rx.try_recv().is_err(),
        "retirement must wait while send holds the controller"
    );
    resume_tx.send(()).unwrap();
    tick.join().unwrap();
    let receipt = retired_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    retirement.join().unwrap();
    assert_eq!(receipt["state"], "retired");
    let mut bytes = [0; 64];
    let (count, _) = receiver.recv_from(&mut bytes).unwrap();
    assert_eq!(&bytes[..count], b"before-retire");
    receiver.set_nonblocking(true).unwrap();
    shared.lock().unwrap().tick();
    assert_eq!(
        receiver.recv_from(&mut bytes).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn whole_stream_retirement_receipt_is_recoverable_through_the_real_broker() {
    use amaterasu_contracts::{Command, Grants, Response, RuntimeConfig, SelectedService};
    use amaterasu_module_sdk::transport::control_retirement::{Receipt, Request};
    use amaterasu_runtime::Broker;
    const SERVICE: &str = "amaterasu.control-transport/1";
    let mut controller = configured();
    let opened = controller.open("alice", request()).unwrap();
    let stream = opened.created.unwrap();
    let epoch = opened.payload["retirement_epoch"]
        .as_str()
        .unwrap()
        .to_owned();
    let config = RuntimeConfig {
        principals: [
            (
                "alice".into(),
                Grants {
                    call: [SERVICE.into()].into(),
                    ..Default::default()
                },
            ),
            (
                "bob".into(),
                Grants {
                    call: [SERVICE.into()].into(),
                    ..Default::default()
                },
            ),
            (
                "provider".into(),
                Grants {
                    provide: [SERVICE.into()].into(),
                    ..Default::default()
                },
            ),
        ]
        .into(),
        service_selections: amaterasu_contracts::ServiceSelections {
            broker: [(
                SERVICE.into(),
                Some(SelectedService {
                    provider: "provider".into(),
                    contract: SERVICE.into(),
                }),
            )]
            .into(),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut broker = Broker::new(config).unwrap();
    assert!(matches!(
        broker.dispatch(
            "provider",
            Command::Register {
                service: SERVICE.into()
            }
        ),
        Response::Ok
    ));
    let mut request_through_broker = |caller: &str, payload: serde_json::Value, broker_id: &str| {
        let id = match broker.dispatch(
            caller,
            Command::Request {
                service: SERVICE.into(),
                payload,
                timeout_ms: 5000,
                operation_id: broker_id.into(),
                parent: None,
            },
        ) {
            Response::Accepted { id } => id,
            other => panic!("unexpected admission: {other:?}"),
        };
        let (owner, payload) = match broker.dispatch("provider", Command::Poll {}) {
            Response::Request {
                id: actual,
                caller,
                payload,
                ..
            } => {
                assert_eq!(actual, id);
                (caller, payload)
            }
            other => panic!("unexpected provider delivery: {other:?}"),
        };
        let operation: Operation = serde_json::from_value(payload).unwrap();
        let reply = controller
            .handle(&owner, operation)
            .map(|reply| reply.payload)
            .unwrap_or_else(|code| json!({"ok":false,"code":code}));
        assert!(matches!(
            broker.dispatch(
                "provider",
                Command::Complete {
                    request: id,
                    payload: reply.clone()
                }
            ),
            Response::Ok
        ));
        (id, reply)
    };
    let unauthorized = serde_json::to_value(Request::Retire {
        stream,
        operation_id: "drain-1",
        provider_epoch: &epoch,
    })
    .unwrap();
    assert_eq!(
        request_through_broker("bob", unauthorized, "bob-attempt").1["code"],
        "stream_not_found"
    );
    let retirement = serde_json::to_value(Request::Retire {
        stream,
        operation_id: "drain-1",
        provider_epoch: &epoch,
    })
    .unwrap();
    // Simulate a lost client reply after successful completion: do not take it.
    let (_, first) = request_through_broker("alice", retirement, "first-attempt");
    let query = serde_json::to_value(Request::RetirementStatus {
        stream,
        operation_id: "drain-1",
        provider_epoch: &epoch,
    })
    .unwrap();
    let (id, status) = request_through_broker("alice", query, "status-attempt");
    assert_eq!(status, first);
    let receipt: Receipt = serde_json::from_value(status.clone()).unwrap();
    assert!(receipt.matches(stream, "drain-1", &epoch));
    assert!(
        matches!(broker.dispatch("alice", Command::TakeResult { request: id }), Response::Result { payload } if payload == status)
    );
}

#[test]
fn whole_stream_retirement_fails_closed_without_provider_epoch() {
    let mut controller = configured();
    let opened = controller.open("alice", request()).unwrap();
    let id = opened.created.unwrap();
    controller.retirement_epoch = None;
    assert_eq!(
        controller.retire("alice", id, "retire", "any"),
        Err("retirement_unavailable")
    );
    assert!(controller.owned("alice", id).is_ok());
}

#[path = "../../../input/src/controller/effect_authority_test.rs"]
mod effect_authority_test;

#[test]
fn aggregate_feedback_lease_drains_against_actual_control_retirement_receipt() {
    use amaterasu_media::{CONTROL_RELIABLE, ControlMessage, MEDIA_CLOSED};
    use amaterasu_module_sdk::effect_authority::{
        StreamScope,
        consumer::{FeedbackLease, TransportRetirement, feedback_capability_identity},
    };
    use amaterasu_module_sdk::input::extensions::{Capability, Direction};
    use amaterasu_module_sdk::transport::control_retirement::Receipt;
    use std::io;
    let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
    receiver.set_nonblocking(true).unwrap();
    let mut controller = configured();
    let opened = controller.open("alice", request()).unwrap();
    let stream = opened.created.unwrap();
    let provider_epoch = opened.payload["retirement_epoch"]
        .as_str()
        .unwrap()
        .to_owned();
    let capability = Capability {
        schema: "org.amaterasu.feedback.rumble".into(),
        version: 1,
        direction: Direction::ToClient,
    };
    let scope = StreamScope {
        capabilities: [feedback_capability_identity(&capability).unwrap()].into(),
        transport_service: "amaterasu.control-transport/1".into(),
        transport_stream: stream,
        transport_epoch: provider_epoch.clone(),
    };
    let (mut authority, aggregate) = effect_authority_test::Fixture::stream_admitted(scope);
    let mut lease = FeedbackLease::new(
        aggregate,
        TransportRetirement {
            stream,
            operation_id: "revoke".into(),
            provider_epoch: provider_epoch.clone(),
        },
        7,
        [capability.clone()].into(),
    )
    .unwrap_or_else(|_| panic!("lease"));
    let queued = lease
        .enqueue(&capability, 1, "feedback", b"queued", || {
            let transport = controller.streams.get_mut(&stream).unwrap();
            transport.stage_pending_datagram_for_test(
                receiver.local_addr().unwrap(),
                b"queued".to_vec(),
            );
            transport.stage_reliable_enet_for_test(
                receiver.local_addr().unwrap(),
                request().connect_data,
            );
            Ok::<_, ()>(true)
        })
        .unwrap()
        .unwrap();
    assert!(queued);
    authority.fence();
    lease.fence();
    // A lost retirement reply is reconciled by the exact provider operation.
    controller
        .retire("alice", stream, "revoke", &provider_epoch)
        .unwrap();
    let response = controller
        .retirement_status("alice", stream, "revoke", &provider_epoch)
        .unwrap();
    let receipt: Receipt = serde_json::from_value(response).unwrap();
    let mut aggregate = lease
        .retire(&receipt)
        .unwrap_or_else(|_| panic!("retirement"));
    authority.complete_stream(&mut aggregate);
    authority.drain();
    let message = ControlMessage {
        message_type: 1,
        channel: 0,
        delivery: CONTROL_RELIABLE,
        reserved: 0,
        data: std::ptr::null(),
        len: 0,
        sequence: 0,
    };
    // SAFETY: a zero-length message is valid for the synchronous call.
    assert_eq!(
        unsafe { controller.send("alice", stream, &message) },
        MEDIA_CLOSED
    );
    controller.tick();
    let mut data = [0; 64];
    assert_eq!(
        receiver.recv_from(&mut data).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}
