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
    fail_completions: BTreeSet<u64>,
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
                if service == "amaterasu.configuration/1" {
                    assert_eq!(payload["action"], "product_get");
                } else {
                    assert_eq!(service, "amaterasu.session-lifecycle/1");
                    assert_eq!(payload["operation"], "validate_input_binding");
                }
                self.next += 1;
                self.requests.insert(self.next, payload);
                Response::Accepted { id: self.next }
            }
            Command::TakeResult { request } => {
                if self.requests[&request]["action"] == "product_get" {
                    return Response::Result {
                        payload: json!({"ok":true,"found":false,"revision":0,"values":{}}),
                    };
                }
                let token = self.requests[&request]["token"].as_str().unwrap();
                Response::Result {
                    payload: json!({"status":"succeeded","target":target(token),"geometry_revision":1,"batch_epoch":format!("batch-{token}"),"active":true}),
                }
            }
            Command::Complete { request, payload } => {
                self.completed.insert(request, payload);
                if self.fail_completions.contains(&request) {
                    Response::Error {
                        code: "transport_unavailable".into(),
                        message: "completion acknowledgement unavailable".into(),
                    }
                } else if self.lose_completions.remove(&request) {
                    Response::Error {
                        code: "request_ended".into(),
                        message: String::new(),
                    }
                } else {
                    Response::Ok
                }
            }
            Command::Poll {} => Response::Pending,
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
        let mut harness = Self {
            module: InputModule::start(runtime).unwrap(),
            host,
        };
        for _ in 0..3 {
            harness.module.tick().unwrap();
        }
        assert!(harness.module.product_settings.ready());
        harness
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

#[test]
fn shutdown_announcement_seals_new_acquisitions_without_retiring_live_consumers() {
    let mut host = Harness::new();
    assert_eq!(host.acquire(10, "one")["ok"], true);
    host.module.prepare_shutdown(20_000).unwrap();
    assert_eq!(host.status(20, "one")["status"], "bound");

    host.dispatch(
        30,
        OWNER,
        json!({"action":"acquire","operation_id":"late-acquire"}),
    );
    assert_eq!(
        host.host.lock().unwrap().completed[&30]["error"]["code"],
        "shutting_down"
    );
    assert_eq!(host.status(40, "one")["status"], "bound");
}

#[test]
fn ready_shutdown_has_retired_every_directly_owned_input_context() {
    let mut host = Harness::new();
    assert_eq!(host.acquire(10, "one")["ok"], true);
    assert_eq!(host.acquire(20, "two")["ok"], true);

    assert!(host.module.quiesce().unwrap());
    assert_ne!(host.status(30, "one")["status"], "bound");
    assert_ne!(host.status(40, "two")["status"], "bound");
}

#[test]
fn shutdown_deadline_is_never_renewed_and_pending_work_is_not_reported_retired() {
    let mut host = Harness::new();
    assert_eq!(host.acquire(10, "one")["ok"], true);
    host.dispatch(
        20,
        OWNER,
        json!({"action":"acquire","operation_id":"pending","lifecycle_binding_token":"pending"}),
    );
    assert!(host.module.acquire_job.is_some());
    host.module.prepare_shutdown(0).unwrap();
    host.module.prepare_shutdown(u64::MAX).unwrap();
    assert!(host.module.quiesce().is_err());
    // The unresolved broker job is retained, but it cannot suppress native
    // cleanup for an existing lease once the shared host budget expires.
    assert!(host.module.acquire_job.is_some());
    assert_ne!(host.status(30, "one")["status"], "bound");
    assert!(host.module.quiesce().is_err());
}

#[test]
fn admitted_binding_validation_settles_before_native_context_retirement() {
    let mut host = Harness::new();
    assert_eq!(host.acquire(10, "one")["ok"], true);
    host.dispatch(
        20,
        OWNER,
        json!({"action":"acquire","operation_id":"pending","lifecycle_binding_token":"pending"}),
    );
    assert!(!host.module.quiesce().unwrap());
    assert!(host.module.acquire_job.is_some());
    assert_eq!(host.status(30, "one")["status"], "bound");

    host.module.tick().unwrap();
    assert!(host.module.acquire_job.is_none());
    assert!(host.module.quiesce().unwrap());
    assert_ne!(host.status(40, "one")["status"], "bound");
    assert_ne!(host.status(50, "pending")["status"], "bound");
}

#[test]
fn quiescence_rejects_previously_queued_mutations_and_keeps_cleanup_available() {
    let mut host = Harness::new();
    let lease = host.acquire(10, "one")["lease"]
        .as_str()
        .unwrap()
        .to_owned();
    host.module.queued.push_back(QueuedRequest {
        id: 20,
        caller: OWNER.into(),
        service: SERVICE.into(),
        payload: json!({"action":"acquire","operation_id":"deferred"}),
        deadline: Instant::now() + Duration::from_secs(5),
    });
    assert!(host.module.quiesce().unwrap());
    assert_eq!(
        host.host.lock().unwrap().completed[&20]["error"]["code"],
        "shutting_down"
    );
    host.dispatch(
        30,
        OWNER,
        json!({"action":"batch","lease":lease,"events":[]}),
    );
    assert_eq!(
        host.host.lock().unwrap().completed[&30]["error"]["code"],
        "shutting_down"
    );
    host.dispatch(40, "amaterasu.session-lifecycle", json!({"action":"revoke_binding","operation_id":"revoke-one","token":"one","target":target("one")}));
    assert_eq!(
        host.host.lock().unwrap().completed[&40]["status"],
        "retired"
    );
}

#[test]
fn expired_deferred_revoke_releases_its_exact_native_lease_before_reply_queue_drains() {
    let mut host = Harness::new();
    assert_eq!(host.acquire(10, "one")["ok"], true);
    assert_eq!(host.acquire(20, "two")["ok"], true);
    host.dispatch(
        30,
        OWNER,
        json!({"action":"acquire","operation_id":"pending","lifecycle_binding_token":"pending"}),
    );
    host.module.accept_request(40, "amaterasu.session-lifecycle".into(), SERVICE.into(), json!({"action":"revoke_binding","operation_id":"revoke-one","token":"one","target":target("one")}), 0).unwrap();

    host.module.controller.select_binding("one");
    assert!(!host.module.controller.has_live_state());
    assert_eq!(host.status(50, "two")["status"], "bound");
    host.module.tick().unwrap();
    assert_eq!(
        host.host.lock().unwrap().completed[&40]["error"]["code"],
        "deadline_exceeded"
    );
    host.module.controller.select_binding("one");
    assert!(!host.module.controller.has_live_state());
    assert_eq!(host.status(60, "two")["status"], "bound");
}

#[test]
fn lost_shutdown_rejection_acknowledgement_blocks_readiness_but_not_final_native_release() {
    let mut host = Harness::new();
    assert_eq!(host.acquire(10, "one")["ok"], true);
    host.module.prepare_shutdown(0).unwrap();
    host.host.lock().unwrap().fail_completions.insert(20);
    assert!(
        host.module
            .accept_request(
                20,
                OWNER.into(),
                SERVICE.into(),
                json!({"action":"acquire","operation_id":"late"}),
                5_000
            )
            .is_err()
    );
    assert!(host.module.quiesce().is_err());
    assert_ne!(host.status(30, "one")["status"], "bound");
    assert!(host.module.quiesce().is_err());

    host.host.lock().unwrap().fail_completions.remove(&20);
    assert!(host.module.quiesce().unwrap());
    assert_eq!(
        host.host.lock().unwrap().completed[&20]["error"]["code"],
        "shutting_down"
    );
}

#[test]
fn revoked_native_lease_keeps_its_retirement_reply_until_broker_acknowledgement() {
    let mut host = Harness::new();
    assert_eq!(host.acquire(10, "one")["ok"], true);
    host.host.lock().unwrap().fail_completions.insert(20);
    assert!(host.module.dispatch(QueuedRequest {
        id: 20,
        caller: "amaterasu.session-lifecycle".into(),
        service: SERVICE.into(),
        payload: json!({"action":"revoke_binding","operation_id":"revoke-one","token":"one","target":target("one")}),
        deadline: Instant::now() + Duration::from_secs(5),
    }).is_err());
    host.module.controller.select_binding("one");
    assert!(!host.module.controller.has_live_state());
    assert!(host.module.pending_revoke.is_some());
    assert!(!host.module.quiesce().unwrap());

    host.host.lock().unwrap().fail_completions.remove(&20);
    host.module.tick().unwrap();
    assert!(host.module.pending_revoke.is_none());
    assert!(host.module.quiesce().unwrap());
    assert_eq!(
        host.host.lock().unwrap().completed[&20]["status"],
        "retired"
    );
}
