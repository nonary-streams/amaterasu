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
}

unsafe extern "C" fn invoke(context: *mut c_void, input: BorrowedBytes, sink: ReplySink) -> Status {
    // SAFETY: the boxed endpoint and SDK input live throughout this call.
    let endpoint = unsafe { &*context.cast::<Mutex<Endpoint>>() };
    let command: crate::Command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
    let mut endpoint = endpoint.lock().unwrap();
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
