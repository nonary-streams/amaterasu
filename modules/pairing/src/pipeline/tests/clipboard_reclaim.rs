use super::*;
use amaterasu_contracts::{Grants, RuntimeConfig};
use amaterasu_runtime::Broker;
use amaterasu_session_lifecycle::Coordinator;

const OWNER: &str = "amaterasu.nvhttp";
const LIFECYCLE: &str = "amaterasu.session-lifecycle/1";

fn broker() -> Broker {
    let mut config = RuntimeConfig::default();
    config.principals.insert(
        "pairing".into(),
        Grants {
            provide: [SERVICE.into(), EFFECT_SERVICE.into()].into(),
            call: [LIFECYCLE.into()].into(),
            ..Default::default()
        },
    );
    config.principals.insert(
        "lifecycle".into(),
        Grants {
            provide: [LIFECYCLE.into()].into(),
            ..Default::default()
        },
    );
    for principal in [OWNER, "foreign"] {
        config.principals.insert(
            principal.into(),
            Grants {
                call: [EFFECT_SERVICE.into(), LIFECYCLE.into()].into(),
                ..Default::default()
            },
        );
    }
    let mut broker = Broker::new(config).unwrap();
    for (principal, service) in [
        ("pairing", SERVICE),
        ("pairing", EFFECT_SERVICE),
        ("lifecycle", LIFECYCLE),
    ] {
        assert!(matches!(
            broker.dispatch(
                principal,
                Command::Register {
                    service: service.into()
                }
            ),
            Response::Ok
        ));
    }
    broker
}

fn request(broker: &mut Broker, caller: &str, service: &str, payload: Value, id: &str) -> u64 {
    let Response::Accepted { id } = broker.dispatch(
        caller,
        Command::Request {
            service: service.into(),
            payload,
            timeout_ms: 30_000,
            operation_id: id.into(),
            parent: None,
        },
    ) else {
        panic!("request rejected")
    };
    id
}

fn lifecycle_tick(broker: &mut Broker, lifecycle: &mut Coordinator) {
    let Response::Request {
        id,
        caller,
        initiator,
        payload,
        ..
    } = broker.dispatch("lifecycle", Command::Poll {})
    else {
        panic!("missing lifecycle request")
    };
    assert!(matches!(
        broker.dispatch("lifecycle", Command::CheckRequest { request: id }),
        Response::Ok
    ));
    let response = lifecycle.handle(&caller, &initiator, payload);
    assert!(matches!(
        broker.dispatch(
            "lifecycle",
            Command::Complete {
                request: id,
                payload: response
            }
        ),
        Response::Ok
    ));
}

fn result(broker: &mut Broker, caller: &str, id: u64) -> Value {
    let Response::Result { payload } = broker.dispatch(caller, Command::TakeResult { request: id })
    else {
        panic!("missing result")
    };
    assert!(matches!(
        broker.dispatch(caller, Command::ReleaseResult { request: id }),
        Response::Ok
    ));
    payload
}

fn lifecycle_call(
    broker: &mut Broker,
    lifecycle: &mut Coordinator,
    payload: Value,
    id: &str,
) -> Value {
    let request = request(broker, OWNER, LIFECYCLE, payload, id);
    lifecycle_tick(broker, lifecycle);
    result(broker, OWNER, request)
}

fn pairing_call(
    broker: &mut Broker,
    pipeline: &mut Pipeline,
    caller: &str,
    payload: Value,
    id: &str,
) -> Value {
    let request = request(broker, caller, EFFECT_SERVICE, payload, id);
    pipeline
        .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
        .unwrap();
    result(broker, caller, request)
}

fn reclaim(
    broker: &mut Broker,
    lifecycle: &mut Coordinator,
    pipeline: &mut Pipeline,
    epoch: &str,
    source: &Value,
    id: &str,
) -> Value {
    let request = request(
        broker,
        OWNER,
        EFFECT_SERVICE,
        json!({"action":"reclaim_clipboard_stream","epoch":epoch,"stream":source}),
        id,
    );
    pipeline
        .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
        .unwrap();
    lifecycle_tick(broker, lifecycle);
    pipeline
        .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
        .unwrap();
    result(broker, OWNER, request)
}

fn reclaim_without_observing_reply(
    broker: &mut Broker,
    lifecycle: &mut Coordinator,
    pipeline: &mut Pipeline,
    epoch: &str,
    source: &Value,
    id: &str,
) {
    let _request = request(
        broker,
        OWNER,
        EFFECT_SERVICE,
        json!({"action":"reclaim_clipboard_stream","epoch":epoch,"stream":source}),
        id,
    );
    pipeline
        .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
        .unwrap();
    lifecycle_tick(broker, lifecycle);
    pipeline
        .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
        .unwrap();
    // Intentionally leave the completed broker result unread, as if its reply
    // were lost after pairing committed the highwater cutoff.
}

#[test]
fn clipboard_reclaim_uses_broker_owned_lifecycle_proof_and_rejects_delayed_work() {
    let (mut pipeline, mut harness, certificate_id) = paired_native_client();
    let grant = json!({"action":"update_client_clipboard_grants","certificate_id":certificate_id,
        "expected_revision":harness.revision,"grants":{"read":true,"write":false}});
    assert_eq!(
        harness.request(&mut pipeline, "console", grant, now())["ok"],
        true
    );
    let mut policy = policy_for_feedback(&[], crate::protocol::FeedbackPermission::View);
    policy.protocol_owner = OWNER.into();
    pipeline.effect_authority = Some(policy);
    let initial = pipeline.effect_gate.as_ref().unwrap().epoch().to_owned();
    let epoch = pipeline
        .apply_effect_request(
            "input",
            "input",
            json!({"action":"recover","epoch":initial,"owner_epoch":"stream-owner"}),
            now(),
        )
        .unwrap()["epoch"]
        .as_str()
        .unwrap()
        .to_owned();
    let directory = std::env::temp_dir().join(format!(
        "clipboard-reclaim-{}-{}",
        std::process::id(),
        now()
    ));
    std::fs::create_dir(&directory).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut lifecycle = Coordinator::open(&directory.join("journal.json")).unwrap();
    let stream = lifecycle.qualification_clipboard_stream();
    let source = serde_json::to_value(&stream).unwrap();
    let target = json!({"kind":"stream","host_epoch":stream.host_epoch,"session_id":stream.session_id,"stream_id":stream.stream_id,"generation":stream.generation});
    let operation = stream.operation_id(1).unwrap();
    let mut broker = broker();
    let reserve = lifecycle_call(
        &mut broker,
        &mut lifecycle,
        json!({"operation":"reserve_clipboard_stream","operation_id":operation,"source":source,"sequence":1}),
        "reserve",
    );
    let lifecycle_ticket = reserve["ticket"].as_str().unwrap();
    let admit = json!({"action":"admit_clipboard","epoch":epoch,"certificate_pem":CLIENT_CERT,"stream":source,"direction":"read","operation_id":operation,"sequence":1});
    let allowed = pairing_call(&mut broker, &mut pipeline, OWNER, admit.clone(), "admit");
    let ticket = allowed["ticket"].as_str().unwrap();
    assert_eq!(
        pairing_call(
            &mut broker,
            &mut pipeline,
            "foreign",
            json!({"action":"reclaim_clipboard_stream","epoch":epoch,"stream":source}),
            "foreign"
        )["code"],
        "forbidden"
    );
    assert_eq!(
        pairing_call(
            &mut broker,
            &mut pipeline,
            OWNER,
            json!({"action":"fence_clipboard_stream","epoch":epoch,"stream":source}),
            "fence"
        )["state"],
        "fenced"
    );
    assert_eq!(
        reclaim(
            &mut broker,
            &mut lifecycle,
            &mut pipeline,
            &epoch,
            &source,
            "active"
        )["code"],
        "stream_not_closed"
    );
    assert_eq!(
        lifecycle_call(
            &mut broker,
            &mut lifecycle,
            json!({"operation":"pause_stream","operation_id":"pause","target":target}),
            "pause"
        )["status"],
        "pending"
    );
    assert_eq!(
        reclaim(
            &mut broker,
            &mut lifecycle,
            &mut pipeline,
            &epoch,
            &source,
            "pending"
        )["code"],
        "stream_not_closed"
    );
    assert_eq!(
        pairing_call(
            &mut broker,
            &mut pipeline,
            OWNER,
            json!({"action":"complete_clipboard","epoch":epoch,"operation_id":operation,"ticket":ticket}),
            "complete"
        )["ok"],
        true
    );
    assert_eq!(
        pairing_call(
            &mut broker,
            &mut pipeline,
            OWNER,
            json!({"action":"acknowledge_clipboard","epoch":epoch,"operation_id":operation,"ticket":ticket}),
            "ack"
        )["ok"],
        true
    );
    assert_eq!(
        lifecycle_call(
            &mut broker,
            &mut lifecycle,
            json!({"operation":"complete_clipboard_stream","operation_id":operation,"ticket":lifecycle_ticket}),
            "lc-complete"
        )["status"],
        "completed"
    );
    assert_eq!(
        lifecycle_call(
            &mut broker,
            &mut lifecycle,
            json!({"operation":"acknowledge_clipboard_stream","operation_id":operation,"ticket":lifecycle_ticket}),
            "lc-ack"
        )["status"],
        "completed"
    );
    let canceled = request(
        &mut broker,
        OWNER,
        EFFECT_SERVICE,
        json!({"action":"reclaim_clipboard_stream","epoch":epoch,"stream":source}),
        "cancel",
    );
    pipeline
        .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
        .unwrap();
    assert!(matches!(
        broker.dispatch(OWNER, Command::Cancel { request: canceled }),
        Response::Ok
    ));
    pipeline
        .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
        .unwrap();
    assert_eq!(
        pairing_call(
            &mut broker,
            &mut pipeline,
            OWNER,
            admit.clone(),
            "still-fenced"
        )["code"],
        "admission_denied"
    );
    reclaim_without_observing_reply(
        &mut broker,
        &mut lifecycle,
        &mut pipeline,
        &epoch,
        &source,
        "reclaim-lost",
    );
    assert_eq!(
        reclaim(
            &mut broker,
            &mut lifecycle,
            &mut pipeline,
            &epoch,
            &source,
            "retry-lost-reply"
        )["state"],
        "reclaimed"
    );
    assert_eq!(
        pairing_call(&mut broker, &mut pipeline, OWNER, admit, "delayed")["code"],
        "operation_conflict"
    );
    let session_id = stream.session_id.clone();
    for sequence in 2..=(crate::effect_gate::MAX_RETIRED_SOURCES as u64 + 1) {
        let stream = lifecycle.qualification_add_clipboard_stream(
            &session_id,
            format!("qualification-{sequence}"),
            sequence,
        );
        let source = serde_json::to_value(&stream).unwrap();
        let operation = stream.operation_id(sequence).unwrap();
        let lifecycle_reservation = lifecycle_call(
            &mut broker,
            &mut lifecycle,
            json!({"operation":"reserve_clipboard_stream","operation_id":operation,"source":source,"sequence":sequence}),
            &format!("stress-reserve-{sequence}"),
        );
        let lifecycle_ticket = lifecycle_reservation["ticket"].as_str().unwrap().to_owned();
        let admission = json!({"action":"admit_clipboard","epoch":epoch,"certificate_pem":CLIENT_CERT,"stream":source,"direction":"read","operation_id":operation,"sequence":sequence});
        let permit = pairing_call(
            &mut broker,
            &mut pipeline,
            OWNER,
            admission.clone(),
            &format!("stress-admit-{sequence}"),
        );
        assert_eq!(permit["ok"], true);
        let ticket = permit["ticket"].as_str().unwrap().to_owned();
        assert_eq!(
            pairing_call(
                &mut broker,
                &mut pipeline,
                OWNER,
                json!({"action":"fence_clipboard_stream","epoch":epoch,"stream":source}),
                &format!("stress-fence-{sequence}"),
            )["state"],
            "fenced"
        );
        pairing_call(
            &mut broker,
            &mut pipeline,
            OWNER,
            json!({"action":"complete_clipboard","epoch":epoch,"operation_id":operation,"ticket":ticket}),
            &format!("stress-complete-{sequence}"),
        );
        pairing_call(
            &mut broker,
            &mut pipeline,
            OWNER,
            json!({"action":"acknowledge_clipboard","epoch":epoch,"operation_id":operation,"ticket":ticket}),
            &format!("stress-ack-{sequence}"),
        );
        lifecycle_call(
            &mut broker,
            &mut lifecycle,
            json!({"operation":"complete_clipboard_stream","operation_id":operation,"ticket":lifecycle_ticket}),
            &format!("stress-life-complete-{sequence}"),
        );
        lifecycle_call(
            &mut broker,
            &mut lifecycle,
            json!({"operation":"acknowledge_clipboard_stream","operation_id":operation,"ticket":lifecycle_ticket}),
            &format!("stress-life-ack-{sequence}"),
        );
        lifecycle.qualification_quiesce_clipboard_stream(&stream);
        assert_eq!(
            reclaim(
                &mut broker,
                &mut lifecycle,
                &mut pipeline,
                &epoch,
                &source,
                &format!("stress-reclaim-{sequence}"),
            )["state"],
            "reclaimed"
        );
        assert_eq!(
            pairing_call(
                &mut broker,
                &mut pipeline,
                OWNER,
                admission,
                &format!("stress-delayed-{sequence}"),
            )["code"],
            "operation_conflict"
        );
        lifecycle.qualification_remove_clipboard_stream(&stream);
    }
    drop(lifecycle);
    std::fs::remove_dir_all(directory).unwrap();
}
