//! Direct batches through the real router: same checks, no queue round trip.
use super::*;
use amaterasu_module_sdk::DirectCall;

fn pointer_batch(lease: &Value, binding: &str, sequence: u64) -> Value {
    json!({
        "action":"batch","lease":lease,"input_binding":binding,
        "operation_id":format!("batch-{binding}:{sequence}"),
        "events":[model::Event::Relative {dx:1,dy:2}],
    })
}

impl Harness {
    fn direct(&mut self, caller: &str, payload: Value) -> Option<Value> {
        self.module.call_direct(DirectCall {
            caller,
            service: SERVICE,
            payload,
        })
    }

    /// Delivers the binding's first batch through the queue, as a session
    /// whose direct call was declined would.
    fn deliver_first_queued(&mut self, id: u64, lease: &Value, binding: &str) {
        self.dispatch(id, OWNER, pointer_batch(lease, binding, 1));
        for _ in 0..3 {
            self.module.advance_activation().unwrap();
        }
        assert_eq!(self.host.lock().unwrap().completed[&id]["ok"], true);
    }
}

#[test]
fn an_active_bound_pointer_batch_is_delivered_before_the_call_returns() {
    let mut host = Harness::new();
    let acquired = host.acquire(10, "one");
    let lease = acquired["lease"].clone();

    let completed_before = host.host.lock().unwrap().completed.len();
    let reply = host
        .direct(OWNER, pointer_batch(&lease, "one", 1))
        .expect("an active pointer batch completes directly");
    assert_eq!(reply["ok"], true);
    assert_eq!(reply["accepted"], 1);
    assert_eq!(reply["lease"], lease);
    assert_eq!(reply["operation_id"], "batch-one:1");
    // No broker completion was involved.
    assert_eq!(host.host.lock().unwrap().completed.len(), completed_before);

    // The queued transport continues from the sequence the direct one reached.
    host.dispatch(30, OWNER, pointer_batch(&lease, "one", 2));
    assert_eq!(host.host.lock().unwrap().completed[&30]["ok"], true);
    assert_eq!(
        host.direct(OWNER, pointer_batch(&lease, "one", 3)).unwrap()["operation_id"],
        "batch-one:3"
    );
}

#[test]
fn a_declined_batch_leaves_its_sequence_for_the_queued_path() {
    let mut host = Harness::new();
    let lease = host.acquire(10, "one")["lease"].clone();
    host.deliver_first_queued(20, &lease, "one");

    // A replayed or skipped sequence is refused without being consumed.
    assert_eq!(host.direct(OWNER, pointer_batch(&lease, "one", 1)), None);
    assert_eq!(host.direct(OWNER, pointer_batch(&lease, "one", 5)), None);

    // Keyboard input may match a shortcut, which needs the shortcut job.
    let keyboard = json!({
        "action":"batch","lease":lease,"input_binding":"one","operation_id":"batch-one:2",
        "events":[model::Event::Key {
            key: model::Key::LinuxEvdev { code: 30 },
            state: model::KeyState::Down,
        }],
    });
    assert_eq!(host.direct(OWNER, keyboard), None);

    // Sequence 2 is still the next one for either transport.
    assert_eq!(
        host.direct(OWNER, pointer_batch(&lease, "one", 2)).unwrap()["ok"],
        true
    );
}

#[test]
fn only_the_lease_owner_reaches_the_device_and_only_for_batches() {
    let mut host = Harness::new();
    let lease = host.acquire(10, "one")["lease"].clone();
    host.deliver_first_queued(20, &lease, "one");

    assert_eq!(
        host.direct("another.module", pointer_batch(&lease, "one", 2)),
        None
    );
    assert_eq!(
        host.direct(OWNER, pointer_batch(&json!("not-the-lease"), "one", 2)),
        None
    );
    assert_eq!(host.direct(OWNER, json!({"action":"status"})), None);
    assert_eq!(
        host.module.call_direct(DirectCall {
            caller: OWNER,
            service: "other/1",
            payload: pointer_batch(&lease, "one", 2),
        }),
        None
    );
    assert_eq!(
        host.direct(OWNER, pointer_batch(&lease, "one", 2)).unwrap()["ok"],
        true
    );
}

#[test]
fn a_direct_batch_does_not_overtake_work_waiting_for_a_tick() {
    let mut host = Harness::new();
    let lease = host.acquire(10, "one")["lease"].clone();
    host.deliver_first_queued(20, &lease, "one");

    // A second binding is still validating, which fences input mutations.
    host.dispatch(
        30,
        OWNER,
        json!({"action":"acquire","operation_id":"pending","lifecycle_binding_token":"pending"}),
    );
    assert!(host.module.acquire_job.is_some());
    assert_eq!(host.direct(OWNER, pointer_batch(&lease, "one", 2)), None);

    for _ in 0..3 {
        host.module.advance_acquire().unwrap();
    }
    assert!(host.module.acquire_job.is_none());
    assert_eq!(
        host.direct(OWNER, pointer_batch(&lease, "one", 2)).unwrap()["ok"],
        true
    );
}

#[test]
fn a_revoked_binding_and_a_closed_admission_are_never_served_directly() {
    let mut host = Harness::new();
    let lease = host.acquire(10, "one")["lease"].clone();
    host.deliver_first_queued(20, &lease, "one");
    assert_eq!(host.revoke(30, "one")["status"], "retired");
    assert_eq!(host.direct(OWNER, pointer_batch(&lease, "one", 2)), None);

    let mut host = Harness::new();
    let lease = host.acquire(10, "one")["lease"].clone();
    host.deliver_first_queued(20, &lease, "one");
    host.module.prepare_shutdown(1_000).unwrap();
    let _ = host.module.quiesce().unwrap();
    assert_eq!(host.direct(OWNER, pointer_batch(&lease, "one", 2)), None);
}
