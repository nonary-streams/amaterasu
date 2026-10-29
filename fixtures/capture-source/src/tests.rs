use super::*;
use amaterasu_module_sdk::abi::{AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_OK, Status};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::ffi::c_void;

struct MockRuntime {
    requests: VecDeque<Response>,
    completions: Vec<Value>,
    completion_results: VecDeque<Response>,
    valid_request: bool,
    registrations: Vec<String>,
    native_registrations: Vec<String>,
    settings: Value,
}

impl Default for MockRuntime {
    fn default() -> Self {
        Self {
            requests: VecDeque::new(),
            completions: Vec::new(),
            completion_results: VecDeque::new(),
            valid_request: true,
            registrations: Vec::new(),
            native_registrations: Vec::new(),
            settings: json!({}),
        }
    }
}

impl MockRuntime {
    fn client(&mut self) -> RuntimeClient {
        let endpoint = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: (self as *mut Self).cast(),
            invoke,
        };
        // SAFETY: each test keeps the runtime allocation alive until its module drops.
        unsafe { RuntimeClient::from_endpoint(endpoint) }
    }

    fn queue(&mut self, payload: Value) {
        self.requests.push_back(Response::Request {
            id: 1,
            caller: "amaterasu.video".into(),
            initiator: "amaterasu.nvhttp".into(),
            service: CAPTURE_SERVICE.into(),
            payload,
            remaining_ms: 5000,
        });
    }
}

unsafe extern "C" fn invoke(context: *mut c_void, input: BorrowedBytes, sink: ReplySink) -> Status {
    // SAFETY: test owns a stable runtime context and module calls it synchronously.
    let runtime = unsafe { &mut *context.cast::<MockRuntime>() };
    // SAFETY: the SDK borrows initialized command bytes for this callback.
    let command: Command = serde_json::from_slice(unsafe { input.read() }.unwrap()).unwrap();
    let response = match command {
        Command::GetSettings {} => Response::Result {
            payload: runtime.settings.clone(),
        },
        Command::Register { service } => {
            runtime.registrations.push(service);
            Response::Ok
        }
        Command::NativeRegister { service, .. } => {
            runtime.native_registrations.push(service);
            Response::Ok
        }
        Command::Poll {} => runtime.requests.pop_front().unwrap_or(Response::Pending),
        Command::CheckRequest { .. } if runtime.valid_request => Response::Ok,
        Command::CheckRequest { .. } => Response::error("request_ended", "request cancelled"),
        Command::Complete { payload, .. } => {
            runtime.completions.push(payload);
            runtime
                .completion_results
                .pop_front()
                .unwrap_or(Response::Ok)
        }
        command => panic!("unexpected fixture runtime command: {command:?}"),
    };
    let bytes = serde_json::to_vec(&response).unwrap();
    // SAFETY: SDK reply sink lives through invoke and copies bytes synchronously.
    unsafe { sink.send(&bytes) };
    STATUS_OK
}

fn open_payload(operation: &str) -> Value {
    json!({"action":"open", "output": model::OUTPUT, "operation_id":operation})
}

#[test]
fn module_registers_only_capture_and_metrics_without_encoder_capabilities() {
    let mut runtime = MockRuntime::default();
    let module = CaptureSource::start(runtime.client()).unwrap();
    assert_eq!(runtime.registrations, [CAPTURE_SERVICE, METRICS_SERVICE]);
    assert_eq!(runtime.native_registrations, [CAPTURE_SERVICE]);
    assert_eq!(
        module.controller.lock().unwrap().metrics()["native_capture_qualified"],
        false
    );
}

#[test]
fn custom_fixture_registers_only_its_own_control_and_native_identity() {
    let mut runtime = MockRuntime {
        settings: json!({"custom_service": true}),
        ..Default::default()
    };
    let mut module = CaptureSource::start(runtime.client()).unwrap();
    assert_eq!(
        runtime.registrations,
        [CUSTOM_CAPTURE_SERVICE, METRICS_SERVICE]
    );
    assert_eq!(runtime.native_registrations, [CUSTOM_CAPTURE_SERVICE]);
    runtime.queue(open_payload("custom"));
    if let Some(Response::Request { service, .. }) = runtime.requests.back_mut() {
        *service = CUSTOM_CAPTURE_SERVICE.into();
    }
    module.tick().unwrap();
    assert_eq!(runtime.completions[0]["ok"], true);
}

#[test]
fn ended_completion_rolls_back_the_open_and_preserves_immediate_caller_ownership() {
    let mut runtime = MockRuntime::default();
    let mut module = CaptureSource::start(runtime.client()).unwrap();
    runtime.queue(open_payload("uncertain-open"));
    runtime.completion_results.push_back(Response::error(
        "request_ended",
        "cancelled during completion",
    ));
    module.tick().unwrap();
    let first_id = runtime.completions[0]["stream"].as_u64().unwrap();
    let metrics = module.controller.lock().unwrap().metrics();
    assert_eq!(metrics["opened"], 1);
    assert_eq!(metrics["closed"], 1);
    assert_eq!(metrics["active"], 0);
    assert_eq!(metrics["callers"]["amaterasu.video"]["opened"], 1);
    assert!(metrics["callers"]["amaterasu.nvhttp"].is_null());
    runtime.queue(open_payload("uncertain-open"));
    module.tick().unwrap();
    assert_ne!(runtime.completions[1]["stream"].as_u64(), Some(first_id));
    assert_eq!(module.controller.lock().unwrap().metrics()["active"], 1);
    let controller = module.controller.clone();
    drop(module);
    assert_eq!(controller.lock().unwrap().metrics()["active"], 0);
    assert_eq!(controller.lock().unwrap().metrics()["closed"], 2);
}

#[test]
fn failed_retry_completion_does_not_destroy_a_previously_completed_lease() {
    let mut runtime = MockRuntime::default();
    let mut module = CaptureSource::start(runtime.client()).unwrap();
    runtime.queue(open_payload("retained"));
    module.tick().unwrap();
    runtime.queue(open_payload("retained"));
    runtime
        .completion_results
        .push_back(Response::error("not_found", "request no longer present"));
    module.tick().unwrap();
    assert_eq!(runtime.completions[0], runtime.completions[1]);
    let metrics = module.controller.lock().unwrap().metrics();
    assert_eq!(metrics["opened"], 1);
    assert_eq!(metrics["closed"], 0);
    assert_eq!(metrics["active"], 1);
}

#[test]
fn cancelled_request_allocates_nothing_and_unexpected_completion_failure_cleans_up() {
    let mut runtime = MockRuntime::default();
    let mut module = CaptureSource::start(runtime.client()).unwrap();
    runtime.valid_request = false;
    runtime.queue(open_payload("cancelled"));
    module.tick().unwrap();
    assert!(runtime.completions.is_empty());
    assert_eq!(module.controller.lock().unwrap().metrics()["opened"], 0);
    runtime.valid_request = true;
    runtime.queue(open_payload("failed"));
    runtime
        .completion_results
        .push_back(Response::error("backend_error", "completion rejected"));
    assert!(module.tick().unwrap_err().contains("backend_error"));
    assert_eq!(module.controller.lock().unwrap().metrics()["active"], 0);
    assert_eq!(module.controller.lock().unwrap().metrics()["closed"], 1);
}
