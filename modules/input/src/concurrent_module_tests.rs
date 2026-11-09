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
    retired_bindings: BTreeSet<String>,
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
                if self.retired_bindings.contains(token) {
                    return Response::Result {
                        payload: json!({"status":"rejected","reason":"binding_revoked"}),
                    };
                }
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
            // A runtime without flow tracing: the module must start regardless.
            Command::TraceBind {} => Response::error("invalid_request", "invalid control message"),
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

    fn retirement_receipt(&self, binding: &str) -> Value {
        json!({
            "version":1,
            "token":binding,
            "target":target(binding),
            "revoke_operation_id":format!("revoke-{binding}"),
            "input_boot_epoch":self.module.input_boot_epoch,
        })
    }

    fn revoke(&mut self, id: u64, binding: &str) -> Value {
        self.dispatch(
            id,
            "amaterasu.session-lifecycle",
            json!({
                "action":"revoke_binding",
                "operation_id":format!("revoke-{binding}"),
                "token":binding,
                "target":target(binding),
            }),
        );
        self.host.lock().unwrap().completed[&id].clone()
    }

    fn acknowledge_retirement(&mut self, id: u64, receipt: Value) -> Value {
        self.dispatch(
            id,
            "amaterasu.session-lifecycle",
            json!({
                "action":"acknowledge_binding_retirement","receipt":receipt,
            }),
        );
        self.host.lock().unwrap().completed[&id].clone()
    }
}

#[test]
fn durable_retirement_acknowledgements_allow_more_than_128_native_context_lifetimes() {
    let mut host = Harness::new();
    let mut acknowledged = true;
    for index in 0..128 {
        let binding = format!("completed-{index}");
        let id = 10_000 * (index + 1);
        assert_eq!(host.acquire(id, &binding)["ok"], true);
        assert_eq!(host.revoke(id + 1, &binding)["status"], "retired");
        host.host
            .lock()
            .unwrap()
            .retired_bindings
            .insert(binding.clone());
        let receipt = host.retirement_receipt(&binding);
        acknowledged &= host.acknowledge_retirement(id + 2, receipt)["status"] == "acknowledged";
    }
    let next = host.acquire(2_000_000, "fresh-after-128");
    assert_eq!(
        next["ok"], true,
        "fresh binding rejected after exact retired acknowledgements: {next}"
    );
    assert!(acknowledged);
}

#[test]
fn acknowledged_retirement_fences_old_acquire_and_preserves_an_unrelated_live_context() {
    let mut host = Harness::new();
    let acquired = host.acquire(10, "old");
    assert_eq!(acquired["ok"], true);
    assert_eq!(host.acquire(20, "unrelated")["ok"], true);
    let retired = host.revoke(30, "old");
    let receipt = host.retirement_receipt("old");
    assert_eq!(retired["retirement_receipt"], receipt);
    host.host
        .lock()
        .unwrap()
        .retired_bindings
        .insert("old".into());
    assert_eq!(
        host.acknowledge_retirement(40, receipt.clone())["status"],
        "acknowledged"
    );
    assert_eq!(
        host.acknowledge_retirement(41, receipt)["status"],
        "acknowledged"
    );
    host.dispatch(
        42,
        OWNER,
        json!({
            "action":"batch","lease":acquired["lease"],"input_binding":"old",
            "operation_id":"batch-old:1","events":[model::Event::Relative {dx:1,dy:2}],
        }),
    );
    assert_eq!(host.host.lock().unwrap().completed[&42]["ok"], false);
    host.module
        .accept_request(
            43,
            "amaterasu.session-lifecycle".into(),
            SERVICE.into(),
            json!({
                "action":"revoke_binding","operation_id":"revoke-old",
                "token":"old","target":target("old"),
            }),
            5_000,
        )
        .unwrap();
    let repeated_revoke = host.host.lock().unwrap().completed[&43].clone();
    assert_eq!(repeated_revoke["ok"], false);
    assert!(repeated_revoke.get("retirement_receipt").is_none());
    assert_eq!(host.status(50, "unrelated")["status"], "bound");
    assert_eq!(host.acquire(60, "old")["ok"], false);
    assert_ne!(host.status(70, "old")["status"], "bound");
    assert_eq!(host.status(71, "unrelated")["status"], "bound");
}

#[test]
fn retirement_acknowledgement_cannot_release_live_or_mismatched_ownership() {
    let mut host = Harness::new();
    assert_eq!(host.acquire(10, "one")["ok"], true);
    let receipt = host.retirement_receipt("one");
    assert_eq!(
        host.acknowledge_retirement(20, receipt.clone())["ok"],
        false
    );
    assert_eq!(host.status(30, "one")["status"], "bound");
    assert_eq!(host.revoke(40, "one")["status"], "retired");
    for (index, field, replacement) in [
        (0, "input_boot_epoch", json!("other-provider-boot")),
        (1, "revoke_operation_id", json!("other-revocation")),
        (2, "target", target("other-target")),
        (3, "version", json!(2)),
    ] {
        let mut wrong = receipt.clone();
        wrong[field] = replacement;
        assert_eq!(host.acknowledge_retirement(50 + index, wrong)["ok"], false);
    }
    host.dispatch(
        60,
        OWNER,
        json!({"action":"acknowledge_binding_retirement","receipt":receipt}),
    );
    assert_eq!(host.host.lock().unwrap().completed[&60]["ok"], false);
    assert_eq!(
        host.acknowledge_retirement(70, receipt)["status"],
        "acknowledged"
    );
}

#[test]
fn retirement_acknowledgement_waits_for_matching_admitted_work() {
    let mut host = Harness::new();
    host.dispatch(
        10,
        OWNER,
        json!({
            "action":"acquire","operation_id":"pending-acquire",
            "lifecycle_binding_token":"pending",
        }),
    );
    assert!(host.module.acquire_job.is_some());
    assert_eq!(host.revoke(20, "pending")["status"], "retired");
    let receipt = host.retirement_receipt("pending");
    let blocked = host.acknowledge_retirement(30, receipt.clone());
    assert_eq!(blocked["error"]["code"], "binding_retirement_busy");
    assert!(host.module.acquire_job.is_some());

    host.module.advance_acquire().unwrap();
    assert!(host.module.acquire_job.is_none());
    assert_eq!(
        host.acknowledge_retirement(40, receipt)["status"],
        "acknowledged"
    );
}

#[test]
fn uncertain_retirement_ack_reply_is_retained_through_quiescence_without_replaying_release() {
    let mut host = Harness::new();
    assert_eq!(host.acquire(10, "one")["ok"], true);
    assert_eq!(host.revoke(20, "one")["status"], "retired");
    let receipt = host.retirement_receipt("one");
    host.host.lock().unwrap().fail_completions.insert(30);
    let request = QueuedRequest {
        id: 30,
        caller: "amaterasu.session-lifecycle".into(),
        service: SERVICE.into(),
        payload: json!({"action":"acknowledge_binding_retirement","receipt":receipt}),
        deadline: Instant::now() + Duration::from_secs(5),
    };
    assert!(host.module.dispatch(request).is_err());
    assert!(host.module.pending_retirement_acknowledgement.is_some());
    assert!(!host.module.controller.binding_has_live_state("one"));
    assert!(!host.module.quiesce().unwrap());
    host.host.lock().unwrap().fail_completions.remove(&30);
    assert!(host.module.quiesce().unwrap());
    assert!(host.module.pending_retirement_acknowledgement.is_none());
    assert_eq!(host.host.lock().unwrap().completed[&30]["receipt"], receipt);
    assert_eq!(
        host.acknowledge_retirement(40, receipt)["status"],
        "acknowledged"
    );
}

#[test]
fn unknown_retirement_ack_after_restart_is_metadata_only_and_remains_available_during_shutdown() {
    let mut host = Harness::new();
    assert_eq!(host.acquire(10, "unrelated")["ok"], true);
    let mut receipt = host.retirement_receipt("unknown-old-token");
    receipt["input_boot_epoch"] = json!("previous-provider-boot");
    let reply = host.acknowledge_retirement(20, receipt.clone());
    assert_eq!(reply["status"], "acknowledged");
    assert!(reply.get("retirement_receipt").is_none());
    assert_eq!(host.status(30, "unrelated")["status"], "bound");
    assert!(host.module.quiesce().unwrap());
    assert_eq!(
        host.acknowledge_retirement(40, receipt)["status"],
        "acknowledged"
    );
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

mod direct;
