//! Direct calls and trace sinks through the real endpoint ABI and the SDK.
use super::*;
use amaterasu_module_sdk::trace::Tracer;
use amaterasu_module_sdk::{DirectReply, RuntimeClient};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

const SERVICE: &str = "example/1";

struct Provider {
    endpoint: HostApi,
    entered: AtomicUsize,
}

/// Serves `echo` and declines everything else, like a provider whose other
/// operations need a later tick.
unsafe extern "C" fn serve(
    instance: *mut c_void,
    service: BorrowedBytes,
    caller: BorrowedBytes,
    request: BorrowedBytes,
    reply: ReplySink,
) -> Status {
    // SAFETY: the test registered this live provider as the instance.
    let provider = unsafe { &*instance.cast::<Provider>() };
    provider.entered.fetch_add(1, Ordering::SeqCst);
    // Provider code may use the runtime: no runtime lock is held around it.
    let settings = unsafe { provider.endpoint.call(br#"{"op":"get_settings"}"#) }.unwrap();
    assert!(matches!(
        serde_json::from_slice::<Response>(&settings).unwrap(),
        Response::Result { .. }
    ));
    // SAFETY: the runtime lends every range for this synchronous call.
    let (service, caller, request) = unsafe {
        (
            std::str::from_utf8(service.read().unwrap()).unwrap(),
            std::str::from_utf8(caller.read().unwrap()).unwrap(),
            serde_json::from_slice::<Value>(request.read().unwrap()).unwrap(),
        )
    };
    if request["action"] != "echo" {
        return STATUS_UNAVAILABLE;
    }
    let body = json!({"ok": true, "caller": caller, "service": service, "value": request["value"]});
    // SAFETY: the sink copies the bytes before this call returns.
    unsafe { reply.send(body.to_string().as_bytes()) };
    STATUS_OK
}

fn runtime_with(principals: &[(&str, Grants)]) -> *mut c_void {
    let configuration = RuntimeConfig {
        principals: principals
            .iter()
            .map(|(name, grants)| ((*name).to_owned(), grants.clone()))
            .collect(),
        ..Default::default()
    };
    let bytes = serde_json::to_vec(&configuration).unwrap();
    let mut runtime = std::ptr::null_mut();
    assert_eq!(
        unsafe { create_runtime(BorrowedBytes::new(&bytes), &mut runtime) },
        STATUS_OK
    );
    runtime
}

fn client(runtime: *mut c_void, principal: &str) -> RuntimeClient {
    // SAFETY: each test destroys its runtime only after its clients are done.
    unsafe { RuntimeClient::from_endpoint(endpoint(runtime, principal)) }
}

/// The consumer under test. It never learns which kind of provider answers:
/// it prefers a direct call and otherwise submits the same payload as an
/// ordinary request, which `serve_queued` stands in for a provider's tick.
fn deliver(consumer: &RuntimeClient, provider: &RuntimeClient, payload: Value) -> Value {
    match consumer.call_direct(SERVICE, payload.clone()).unwrap() {
        DirectReply::Completed(reply) => reply,
        DirectReply::Unavailable { .. } => {
            let Response::Accepted { id } = consumer
                .send(Command::Request {
                    service: SERVICE.into(),
                    payload,
                    timeout_ms: 1_000,
                    operation_id: "deliver-1".into(),
                    parent: None,
                })
                .unwrap()
            else {
                panic!("request was not admitted");
            };
            serve_queued(provider);
            let Response::Result { payload } = consumer
                .send_ephemeral(Command::TakeResult { request: id })
                .unwrap()
            else {
                panic!("request did not complete");
            };
            payload
        }
    }
}

fn serve_queued(provider: &RuntimeClient) {
    let Response::Request {
        id,
        caller,
        payload,
        ..
    } = provider.send(Command::Poll {}).unwrap()
    else {
        panic!("no queued request");
    };
    provider
        .require_success(Command::Complete {
            request: id,
            payload: json!({"ok": true, "caller": caller, "queued": true, "value": payload["value"]}),
        })
        .unwrap();
}

fn grants(provide: &[&str], call: &[&str]) -> Grants {
    Grants {
        provide: provide.iter().map(|name| (*name).to_owned()).collect(),
        call: call.iter().map(|name| (*name).to_owned()).collect(),
        ..Default::default()
    }
}

#[test]
fn an_unchanged_consumer_is_served_directly_or_through_the_queue() {
    let runtime = runtime_with(&[
        ("provider", grants(&[SERVICE], &[])),
        ("consumer", grants(&[], &[SERVICE])),
    ]);
    let provider = client(runtime, "provider");
    let consumer = client(runtime, "consumer");
    provider
        .require_success(Command::Register {
            service: SERVICE.into(),
        })
        .unwrap();

    // A provider built without direct support: the consumer's request is queued.
    let queued = deliver(&consumer, &provider, json!({"action": "echo", "value": 1}));
    assert_eq!(queued["queued"], true);
    assert_eq!(queued["caller"], "consumer");

    // The same service, now with the host having registered a direct handler.
    let state = Provider {
        endpoint: unsafe { endpoint(runtime, "provider") },
        entered: AtomicUsize::new(0),
    };
    // SAFETY: the export returns this library's static table.
    let direct = unsafe { *amaterasu_runtime_direct_v1() };
    let instance = std::ptr::from_ref(&state).cast_mut().cast::<c_void>();
    assert_eq!(
        unsafe { (direct.register)(runtime, BorrowedBytes::new(b"provider"), instance, serve) },
        STATUS_OK
    );
    let served = deliver(&consumer, &provider, json!({"action": "echo", "value": 2}));
    assert_eq!(
        served,
        json!({"ok": true, "caller": "consumer", "service": SERVICE, "value": 2})
    );
    assert_eq!(state.entered.load(Ordering::SeqCst), 1);

    // An operation the provider declines directly still completes when queued.
    assert_eq!(
        consumer
            .call_direct(SERVICE, json!({"action": "slow"}))
            .unwrap(),
        DirectReply::Unavailable {
            code: "direct_unavailable".into()
        }
    );
    assert_eq!(state.entered.load(Ordering::SeqCst), 2);

    assert_eq!(
        unsafe { (direct.unregister)(runtime, BorrowedBytes::new(b"provider")) },
        STATUS_OK
    );
    assert!(matches!(
        consumer
            .call_direct(SERVICE, json!({"action": "echo"}))
            .unwrap(),
        DirectReply::Unavailable { .. }
    ));
    assert_eq!(state.entered.load(Ordering::SeqCst), 2);
    unsafe { destroy_runtime(runtime) };
}

#[test]
fn a_direct_call_needs_the_call_grant_and_the_providers_own_thread() {
    let runtime = runtime_with(&[
        ("provider", grants(&[SERVICE], &[])),
        ("consumer", grants(&[], &[SERVICE])),
        ("stranger", grants(&[], &["other/1"])),
    ]);
    let provider = client(runtime, "provider");
    provider
        .require_success(Command::Register {
            service: SERVICE.into(),
        })
        .unwrap();
    let state = Provider {
        endpoint: unsafe { endpoint(runtime, "provider") },
        entered: AtomicUsize::new(0),
    };
    let direct = unsafe { *amaterasu_runtime_direct_v1() };
    let instance = std::ptr::from_ref(&state).cast_mut().cast::<c_void>();
    assert_eq!(
        unsafe { (direct.register)(runtime, BorrowedBytes::new(b"provider"), instance, serve) },
        STATUS_OK
    );
    // Only configured principals can own a handler, and only one each.
    assert_ne!(
        unsafe { (direct.register)(runtime, BorrowedBytes::new(b"provider"), instance, serve) },
        STATUS_OK
    );
    assert_ne!(
        unsafe { (direct.register)(runtime, BorrowedBytes::new(b"unknown"), instance, serve) },
        STATUS_OK
    );

    let echo = || json!({"action": "echo", "value": 3});
    let stranger = client(runtime, "stranger");
    assert_eq!(
        stranger.call_direct(SERVICE, echo()).unwrap(),
        DirectReply::Unavailable {
            code: "permission_denied".into()
        }
    );
    let consumer = client(runtime, "consumer");
    assert_eq!(
        consumer.call_direct("missing/1", echo()).unwrap(),
        DirectReply::Unavailable {
            code: "permission_denied".into()
        }
    );
    // An oversized payload is refused before the provider is entered.
    assert!(matches!(
        consumer
            .call_direct(
                SERVICE,
                json!({"action": "echo", "value": "x".repeat(20_000)})
            )
            .unwrap(),
        DirectReply::Unavailable { .. }
    ));

    // The module instance belongs to the thread that registered it.
    let consumer_endpoint = unsafe { endpoint(runtime, "consumer") };
    let context = consumer_endpoint.context as usize;
    let elsewhere = std::thread::spawn(move || {
        let endpoint = HostApi {
            context: context as *mut c_void,
            ..consumer_endpoint_template()
        };
        // SAFETY: the runtime outlives this joined thread.
        let consumer = unsafe { RuntimeClient::from_endpoint(endpoint) };
        consumer
            .call_direct(SERVICE, json!({"action": "echo", "value": 3}))
            .unwrap()
    })
    .join()
    .unwrap();
    assert_eq!(
        elsewhere,
        DirectReply::Unavailable {
            code: "direct_unavailable".into()
        }
    );
    assert_eq!(state.entered.load(Ordering::SeqCst), 0);

    assert!(matches!(
        consumer.call_direct(SERVICE, echo()).unwrap(),
        DirectReply::Completed(_)
    ));
    assert_eq!(state.entered.load(Ordering::SeqCst), 1);
    assert_eq!(
        unsafe { (direct.unregister)(runtime, BorrowedBytes::new(b"provider")) },
        STATUS_OK
    );
    unsafe { destroy_runtime(runtime) };
}

fn consumer_endpoint_template() -> HostApi {
    HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: std::ptr::null_mut(),
        invoke: invoke_command,
    }
}

#[test]
fn every_endpoint_can_trace_and_only_management_reads_the_history() {
    let runtime = runtime_with(&[
        ("amaterasu.management", Grants::default()),
        ("producer", Grants::default()),
    ]);
    let producer = client(runtime, "producer");
    let management = client(runtime, "amaterasu.management");

    let empty = management.send(Command::TraceSnapshot {}).unwrap();
    let Response::Result { payload } = empty else {
        panic!("management may read trace history");
    };
    assert_eq!(payload["collecting"], false);
    assert!(matches!(
        producer.send(Command::TraceSnapshot {}).unwrap(),
        Response::Error { code, .. } if code == "permission_denied"
    ));

    let tracer = Tracer::bind(&producer);
    assert!(tracer.is_enabled());
    // Binding again returns the same sink rather than a second one.
    assert!(matches!(
        (
            producer.send(Command::TraceBind {}).unwrap(),
            producer.send(Command::TraceBind {}).unwrap()
        ),
        (Response::TraceBound { address: first }, Response::TraceBound { address: second })
            if first == second
    ));

    let track = tracer.track("input");
    let flow = amaterasu_module_sdk::trace::flow_id(9, 1);
    tracer.mark_at(
        track,
        flow,
        tracer.stage("socket.arrived"),
        tracer.now().earlier_by(Duration::from_millis(30)),
    );
    tracer.finish(
        track,
        flow,
        tracer.stage("input.accepted"),
        Duration::from_millis(8),
    );
    tracer.record_span(
        tracer.span_kind("host.tick"),
        tracer.label("amaterasu.state"),
        Duration::from_millis(29),
    );

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let history = loop {
        let Response::Result { payload } = management.send(Command::TraceSnapshot {}).unwrap()
        else {
            panic!("management may read trace history");
        };
        if !payload["slow_flows"].as_array().unwrap().is_empty() {
            break payload;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no slow flow reported"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let slow = &history["slow_flows"][0];
    assert_eq!(slow["track"], "input");
    assert_eq!(slow["begun_by"], "producer");
    assert!(slow["total_us"].as_u64().unwrap() >= 30_000);
    assert_eq!(
        slow["slowest_step"]["during"][0]["label"],
        "amaterasu.state"
    );
    assert_eq!(history["slow_spans"][0]["span"], "host.tick");

    // Destruction joins the collector thread.
    unsafe { destroy_runtime(runtime) };
}
