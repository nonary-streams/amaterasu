//! Exercise completion uncertainty using the production request router.
use super::*;
use amaterasu_contracts::{Grants, RuntimeConfig};
use amaterasu_module_sdk::abi::STATUS_INTERNAL_ERROR;
use amaterasu_runtime::Broker as RealBroker;
use std::time::Duration;

struct Endpoint {
    broker: RealBroker,
    lose_completion: Option<u64>,
    lost: bool,
    blocked_completions: std::collections::BTreeSet<u64>,
    blocked_checks: std::collections::BTreeSet<u64>,
}

unsafe extern "C" fn invoke(context: *mut c_void, input: BorrowedBytes, sink: ReplySink) -> Status {
    // SAFETY: the boxed endpoint and SDK input live throughout this call.
    let endpoint = unsafe { &*context.cast::<Mutex<Endpoint>>() };
    let command: crate::Command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
    let mut endpoint = endpoint.lock().unwrap();
    if matches!(&command, crate::Command::Complete { request, .. } if endpoint.blocked_completions.contains(request))
    {
        return STATUS_INTERNAL_ERROR;
    }
    if matches!(&command, crate::Command::CheckRequest { request } if endpoint.blocked_checks.contains(request))
    {
        return STATUS_INTERNAL_ERROR;
    }
    let losing = matches!(&command, crate::Command::Complete { request, .. } if Some(*request) == endpoint.lose_completion);
    let response = endpoint.broker.dispatch("wgc", command);
    if losing && matches!(response, crate::Response::Ok) {
        endpoint.lose_completion = None;
        endpoint.lost = true;
        return STATUS_INTERNAL_ERROR;
    }
    // SAFETY: the SDK supplies the synchronous reply sink.
    unsafe { sink.send(&serde_json::to_vec(&response).unwrap()) };
    STATUS_OK
}

fn request(endpoint: &Mutex<Endpoint>, operation: &str, payload: serde_json::Value) -> u64 {
    let response = endpoint.lock().unwrap().broker.dispatch(
        "video",
        crate::Command::Request {
            service: crate::SERVICE.into(),
            payload,
            timeout_ms: 30_000,
            operation_id: operation.into(),
            parent: None,
        },
    );
    match response {
        crate::Response::Accepted { id } => id,
        other => panic!("request admission failed: {other:?}"),
    }
}

fn result(endpoint: &Mutex<Endpoint>, id: u64) -> crate::Response {
    endpoint
        .lock()
        .unwrap()
        .broker
        .dispatch("video", crate::Command::TakeResult { request: id })
}

fn drive_until(module: &mut CaptureModule, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !done() {
        module
            .tick()
            .expect("host must remain alive to retry retained work");
        assert!(Instant::now() < deadline, "capture broker did not settle");
        std::thread::yield_now();
    }
}

fn accepted_completion_with_lost_reply(release_result: bool, replay: bool) {
    let config = RuntimeConfig {
        principals: [
            (
                "wgc".into(),
                Grants {
                    provide: [crate::SERVICE.into()].into(),
                    ..Default::default()
                },
            ),
            (
                "video".into(),
                Grants {
                    call: [crate::SERVICE.into()].into(),
                    ..Default::default()
                },
            ),
        ]
        .into(),
        ..Default::default()
    };
    let mut endpoint = Box::new(Mutex::new(Endpoint {
        broker: RealBroker::new(config).unwrap(),
        lose_completion: None,
        lost: false,
        blocked_completions: Default::default(),
        blocked_checks: Default::default(),
    }));
    assert!(matches!(
        endpoint.lock().unwrap().broker.dispatch(
            "wgc",
            crate::Command::Register {
                service: crate::SERVICE.into()
            }
        ),
        crate::Response::Ok
    ));
    let opens = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    let open_count = opens.clone();
    let session_drops = drops.clone();
    let backend = NativeBackend::with_opener(move |_| {
        open_count.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(OwnedSession(session_drops.clone())))
    });
    let mut module = module_with_controller(Controller::new(backend, true));
    // SAFETY: the endpoint remains allocated through all module calls and drop.
    module.runtime = unsafe {
        RuntimeClient::from_endpoint(HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: std::ptr::from_mut(endpoint.as_mut()).cast(),
            invoke,
        })
    };
    let payload = json!({"action":"open", "output":"fixture-monitor", "operation_id":"exact-open"});
    let original = request(&endpoint, "broker-original", payload.clone());
    endpoint.lock().unwrap().lose_completion = Some(original);
    let retry = replay.then(|| request(&endpoint, "broker-retry", payload));
    drive_until(&mut module, || endpoint.lock().unwrap().lost);
    let stream = match result(&endpoint, original) {
        crate::Response::Result { payload } => payload["stream"].as_u64().unwrap(),
        other => panic!("completion was not accepted: {other:?}"),
    };
    if release_result {
        assert!(matches!(
            endpoint
                .lock()
                .unwrap()
                .broker
                .dispatch("video", crate::Command::ReleaseResult { request: original }),
            crate::Response::Ok
        ));
    }
    if let Some(retry) = retry {
        drive_until(&mut module, || {
            matches!(result(&endpoint, retry), crate::Response::Result { .. })
        });
        assert!(
            matches!(result(&endpoint, retry), crate::Response::Result { payload } if payload["stream"] == stream)
        );
    } else {
        module.tick().unwrap();
    }
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    let lookup = request(
        &endpoint,
        "broker-lookup",
        json!({"action":"lookup", "operation_id":"exact-open"}),
    );
    drive_until(&mut module, || {
        matches!(result(&endpoint, lookup), crate::Response::Result { .. })
    });
    assert!(
        matches!(result(&endpoint, lookup), crate::Response::Result { payload } if payload["stream"] == stream)
    );
    let close = request(
        &endpoint,
        "broker-close",
        json!({"action":"close", "stream":stream}),
    );
    drive_until(&mut module, || {
        matches!(result(&endpoint, close), crate::Response::Result { .. })
    });
    assert!(
        matches!(result(&endpoint, close), crate::Response::Result { payload } if payload["ok"] == true)
    );
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn accepted_but_lost_open_completion_preserves_exact_source() {
    for release_result in [false, true] {
        for replay in [false, true] {
            accepted_completion_with_lost_reply(release_result, replay);
        }
    }
}

#[test]
fn delayed_replay_reply_cannot_publish_a_stream_after_close() {
    let config = RuntimeConfig {
        principals: [
            (
                "wgc".into(),
                Grants {
                    provide: [crate::SERVICE.into()].into(),
                    ..Default::default()
                },
            ),
            (
                "video".into(),
                Grants {
                    call: [crate::SERVICE.into()].into(),
                    ..Default::default()
                },
            ),
        ]
        .into(),
        ..Default::default()
    };
    let mut endpoint = Box::new(Mutex::new(Endpoint {
        broker: RealBroker::new(config).unwrap(),
        lose_completion: None,
        lost: false,
        blocked_completions: Default::default(),
        blocked_checks: Default::default(),
    }));
    assert!(matches!(
        endpoint.lock().unwrap().broker.dispatch(
            "wgc",
            crate::Command::Register {
                service: crate::SERVICE.into()
            }
        ),
        crate::Response::Ok
    ));
    let drops = Arc::new(AtomicUsize::new(0));
    let opened_drops = drops.clone();
    let backend =
        NativeBackend::with_opener(move |_| Ok(Box::new(OwnedSession(opened_drops.clone()))));
    let mut module = module_with_controller(Controller::new(backend, true));
    // SAFETY: the endpoint remains live until all module operations complete.
    module.runtime = unsafe {
        RuntimeClient::from_endpoint(HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: std::ptr::from_mut(endpoint.as_mut()).cast(),
            invoke,
        })
    };
    let payload =
        json!({"action":"open", "output":"fixture-monitor", "operation_id":"same-operation"});
    let original = request(&endpoint, "original", payload.clone());
    drive_until(&mut module, || {
        matches!(result(&endpoint, original), crate::Response::Result { .. })
    });
    let stream = match result(&endpoint, original) {
        crate::Response::Result { payload } => payload["stream"].as_u64().unwrap(),
        other => panic!("original open did not complete: {other:?}"),
    };
    let replay = request(&endpoint, "replay", payload);
    endpoint.lock().unwrap().blocked_completions.insert(replay);
    for _ in 0..4 {
        module.tick().unwrap();
    }
    assert!(matches!(
        result(&endpoint, replay),
        crate::Response::Pending
    ));
    assert_eq!(module.pending_opens.len(), 1);
    let close = request(
        &endpoint,
        "close",
        json!({"action":"close", "stream":stream}),
    );
    drive_until(&mut module, || {
        matches!(result(&endpoint, close), crate::Response::Result { .. })
    });
    assert!(
        matches!(result(&endpoint, close), crate::Response::Result { payload } if payload["ok"] == true)
    );
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    endpoint.lock().unwrap().blocked_completions.remove(&replay);
    drive_until(&mut module, || {
        matches!(result(&endpoint, replay), crate::Response::Result { .. })
    });
    assert!(
        !matches!(result(&endpoint, replay), crate::Response::Result { payload } if payload["ok"] == true && payload["stream"] == stream),
        "retired stream must not be published by delayed replay"
    );
}

#[test]
#[ignore = "active uncertain broker completions can still starve a later Close"]
fn uncertain_rejection_replies_do_not_starve_later_close() {
    let config = RuntimeConfig {
        principals: [
            (
                "wgc".into(),
                Grants {
                    provide: [crate::SERVICE.into()].into(),
                    ..Default::default()
                },
            ),
            (
                "video".into(),
                Grants {
                    call: [crate::SERVICE.into()].into(),
                    ..Default::default()
                },
            ),
        ]
        .into(),
        ..Default::default()
    };
    let mut endpoint = Box::new(Mutex::new(Endpoint {
        broker: RealBroker::new(config).unwrap(),
        lose_completion: None,
        lost: false,
        blocked_completions: Default::default(),
        blocked_checks: Default::default(),
    }));
    assert!(matches!(
        endpoint.lock().unwrap().broker.dispatch(
            "wgc",
            crate::Command::Register {
                service: crate::SERVICE.into()
            }
        ),
        crate::Response::Ok
    ));
    let drops = Arc::new(AtomicUsize::new(0));
    let mut module = module_with_session(Box::new(OwnedSession(drops.clone())));
    // SAFETY: the boxed endpoint outlives all module calls.
    module.runtime = unsafe {
        RuntimeClient::from_endpoint(HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: std::ptr::from_mut(endpoint.as_mut()).cast(),
            invoke,
        })
    };
    for i in 0..crate::MAX_PENDING_REPLIES {
        let id = request(
            &endpoint,
            &format!("invalid-{i}"),
            json!({"action":"invalid"}),
        );
        endpoint.lock().unwrap().blocked_completions.insert(id);
    }
    for _ in 0..8 {
        module.tick().unwrap();
    }
    assert_eq!(module.pending_replies.len(), crate::MAX_PENDING_REPLIES);
    let close = request(
        &endpoint,
        "later-close",
        json!({"action":"close", "stream":1}),
    );
    for _ in 0..12 {
        module.tick().unwrap();
    }
    assert!(
        matches!(result(&endpoint, close), crate::Response::Result { payload } if payload["ok"] == true),
        "independently accepted Close must progress while rejection replies are uncertain"
    );
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(module.pending_replies.len(), crate::MAX_PENDING_REPLIES);
}

#[test]
fn cancelled_rejection_reclaims_capacity_when_completion_transport_stays_broken() {
    let config = RuntimeConfig {
        principals: [
            (
                "wgc".into(),
                Grants {
                    provide: [crate::SERVICE.into()].into(),
                    ..Default::default()
                },
            ),
            (
                "video".into(),
                Grants {
                    call: [crate::SERVICE.into()].into(),
                    ..Default::default()
                },
            ),
        ]
        .into(),
        ..Default::default()
    };
    let mut endpoint = Box::new(Mutex::new(Endpoint {
        broker: RealBroker::new(config).unwrap(),
        lose_completion: None,
        lost: false,
        blocked_completions: Default::default(),
        blocked_checks: Default::default(),
    }));
    assert!(matches!(
        endpoint.lock().unwrap().broker.dispatch(
            "wgc",
            crate::Command::Register {
                service: crate::SERVICE.into()
            }
        ),
        crate::Response::Ok
    ));
    let drops = Arc::new(AtomicUsize::new(0));
    let mut module = module_with_session(Box::new(OwnedSession(drops.clone())));
    // SAFETY: the boxed endpoint outlives all module calls.
    module.runtime = unsafe {
        RuntimeClient::from_endpoint(HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: std::ptr::from_mut(endpoint.as_mut()).cast(),
            invoke,
        })
    };
    let mut rejected = Vec::new();
    for i in 0..crate::MAX_PENDING_REPLIES {
        let id = request(
            &endpoint,
            &format!("cancelled-{i}"),
            json!({"action":"invalid"}),
        );
        endpoint.lock().unwrap().blocked_completions.insert(id);
        rejected.push(id);
    }
    for _ in 0..8 {
        module.tick().unwrap();
    }
    assert_eq!(module.pending_replies.len(), crate::MAX_PENDING_REPLIES);
    assert!(matches!(
        endpoint.lock().unwrap().broker.dispatch(
            "video",
            crate::Command::Cancel {
                request: rejected[0]
            }
        ),
        crate::Response::Ok
    ));
    let close = request(
        &endpoint,
        "close-after-cancel",
        json!({"action":"close", "stream":1}),
    );
    endpoint.lock().unwrap().blocked_checks.insert(rejected[0]);
    for _ in 0..3 {
        module.tick().unwrap();
    }
    assert_eq!(
        module.pending_replies.len(),
        crate::MAX_PENDING_REPLIES,
        "an unobservable request must remain owned"
    );
    assert!(matches!(result(&endpoint, close), crate::Response::Pending));
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    endpoint.lock().unwrap().blocked_checks.remove(&rejected[0]);
    for _ in 0..12 {
        module.tick().unwrap();
    }
    assert!(
        matches!(result(&endpoint, close), crate::Response::Result { payload } if payload["ok"] == true),
        "confirmed cancelled request must release its reply slot"
    );
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(module.pending_replies.len(), crate::MAX_PENDING_REPLIES - 1);
}
