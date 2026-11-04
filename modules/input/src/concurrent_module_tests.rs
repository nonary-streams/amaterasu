//! Exercise the real module router with a non-injecting qualification backend.
use super::*;
use amaterasu_module_sdk::abi;
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::c_void,
    sync::Mutex,
};

const OWNER: &str = "amaterasu.stream-session";
#[derive(Default)]
struct Host {
    next: u64,
    requests: BTreeMap<u64, Value>,
    completed: BTreeMap<u64, Value>,
    lose_completions: BTreeSet<u64>,
}
impl Host {
    fn call(&mut self, command: Command) -> Response {
        match command {
            Command::GetSettings {} => Response::Result {
                payload: json!({"allow_injection":true,"linux_companion_socket_path":"/nonexistent/input-qualification.sock"}),
            },
            Command::Register { .. }
            | Command::CheckRequest { .. }
            | Command::ReleaseResult { .. } => Response::Ok,
            Command::Request {
                service, payload, ..
            } => {
                assert_eq!(service, "amaterasu.session-lifecycle/1");
                assert_eq!(payload["operation"], "validate_input_binding");
                self.next += 1;
                self.requests.insert(self.next, payload);
                Response::Accepted { id: self.next }
            }
            Command::TakeResult { request } => {
                let token = self.requests[&request]["token"].as_str().unwrap();
                Response::Result {
                    payload: json!({"status":"succeeded","target":target(token),"geometry_revision":1,"batch_epoch":format!("batch-{token}"),"active":true}),
                }
            }
            Command::Complete { request, payload } => {
                self.completed.insert(request, payload);
                if self.lose_completions.remove(&request) {
                    Response::Error {
                        code: "request_ended".into(),
                        message: String::new(),
                    }
                } else {
                    Response::Ok
                }
            }
            other => panic!("unexpected module request {other:?}"),
        }
    }
}
fn target(binding: &str) -> Value {
    json!({"kind":"stream","host_epoch":"host","session_id":"application","stream_id":binding,"generation":1})
}
unsafe extern "C" fn invoke(
    context: *mut c_void,
    bytes: abi::BorrowedBytes,
    reply: abi::ReplySink,
) -> abi::Status {
    // SAFETY: Harness retains a boxed synchronized host until after its module drops.
    let host = unsafe { &*context.cast::<Mutex<Host>>() };
    // SAFETY: input is borrowed for the synchronous call and reply copies its bytes.
    let command: Command = serde_json::from_slice(unsafe { bytes.read() }.unwrap()).unwrap();
    let result = host.lock().unwrap().call(command);
    let result = serde_json::to_vec(&result).unwrap();
    unsafe { reply.send(&result) };
    abi::STATUS_OK
}
struct Harness {
    // Field order keeps the endpoint alive through module/device teardown.
    module: InputModule,
    host: Box<Mutex<Host>>,
}
impl Harness {
    fn new() -> Self {
        let mut host = Box::new(Mutex::new(Host::default()));
        let endpoint = abi::HostApi {
            header: abi::AbiHeader::of::<abi::HostApi>(),
            context: (&mut *host as *mut Mutex<Host>).cast(),
            invoke,
        };
        // SAFETY: boxed synchronized host stays alive until after module destruction.
        let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
        Self {
            module: InputModule::start(runtime).unwrap(),
            host,
        }
    }
    fn dispatch(&mut self, id: u64, caller: &str, payload: Value) {
        self.module
            .dispatch(QueuedRequest {
                id,
                caller: caller.into(),
                service: SERVICE.into(),
                payload,
                deadline: Instant::now() + Duration::from_secs(5),
            })
            .unwrap();
    }
    fn acquire(&mut self, id: u64, binding: &str) -> Value {
        self.dispatch(id, OWNER, json!({"action":"acquire","operation_id":format!("acquire-{binding}"),"lifecycle_binding_token":binding}));
        // Simulate unrelated read-only status interleaving with authority validation.
        self.dispatch(id + 1000, OWNER, json!({"action":"status"}));
        for _ in 0..3 {
            self.module.advance_acquire().unwrap();
        }
        self.host.lock().unwrap().completed[&id].clone()
    }
    fn status(&mut self, id: u64, binding: &str) -> Value {
        self.dispatch(id, "amaterasu.session-lifecycle", json!({"action":"binding_status","operation_id":format!("status-{binding}"),"token":binding,"target":target(binding)}));
        self.host.lock().unwrap().completed[&id].clone()
    }
}

#[test]
fn lost_acquisition_reply_and_binding_revoke_preserve_other_streams() {
    let mut host = Harness::new();
    assert_eq!(host.acquire(10, "one")["ok"], true);
    host.host.lock().unwrap().lose_completions.insert(20);
    assert_eq!(host.acquire(20, "lost")["ok"], true);
    assert_eq!(host.status(30, "one")["status"], "bound");
    assert_ne!(host.status(31, "lost")["status"], "bound");
    assert_eq!(host.acquire(40, "two")["ok"], true);
    host.dispatch(
        41,
        OWNER,
        json!({"action":"acquire","operation_id":"unbound-legacy"}),
    );
    assert_eq!(
        host.host.lock().unwrap().completed[&41]["error"]["code"],
        "input_busy"
    );
    host.dispatch(50, "amaterasu.session-lifecycle", json!({"action":"revoke_binding","operation_id":"revoke-one","token":"one","target":target("one")}));
    assert_eq!(
        host.host.lock().unwrap().completed[&50]["status"],
        "retired"
    );
    assert_ne!(host.status(60, "one")["status"], "bound");
    assert_eq!(host.status(61, "two")["status"], "bound");
}
