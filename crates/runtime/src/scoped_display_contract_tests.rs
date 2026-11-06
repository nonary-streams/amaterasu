//! Public scoped-display replacement proof through the real broker. Protected
//! monitor claims and platform readback here are explicit test fixtures, not
//! native/hardware evidence. Read-only capture-target borrowing grants no change.
use super::Broker;
use amaterasu_contracts::{Command, Grants, Response, RuntimeConfig};
use amaterasu_module_sdk::display::{
    self, Refresh, Rotation, ScopedOperation, ScopedStatus, Target, scoped_status_from_reply,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const CONSUMER: &str = "example.scoped-consumer";
const PEER: &str = "example.peer-consumer";
const OUTSIDER: &str = "example.outsider";
const IDENTITY: &str = "trusted-monitor-identity";
const REFERENCE: &str = "retained-monitor-reference";
const MAX_SCOPES: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Scope {
    owner: String,
    identity: String,
    reference: String,
}

impl Scope {
    fn new(owner: &str, identity: &str, reference: &str) -> Self {
        Self {
            owner: owner.into(),
            identity: identity.into(),
            reference: reference.into(),
        }
    }
}

#[derive(Clone)]
struct Admission {
    scope: Scope,
    operation: String,
    mode_operation: Option<String>,
    before: Vec<Target>,
    desired: Vec<Target>,
    pending: bool,
    applied: bool,
}

trait ScopedJournal {
    fn get(&self, scope: &Scope) -> Option<Admission>;
    fn put(&mut self, admission: Admission) -> bool;
    fn remove(&mut self, scope: &Scope);
    fn len(&self) -> usize;
}

#[derive(Default)]
struct IndexedJournal(BTreeMap<Scope, Admission>);

impl ScopedJournal for IndexedJournal {
    fn get(&self, scope: &Scope) -> Option<Admission> {
        self.0.get(scope).cloned()
    }
    fn put(&mut self, admission: Admission) -> bool {
        if !self.0.contains_key(&admission.scope) && self.0.len() == MAX_SCOPES {
            return false;
        }
        self.0.insert(admission.scope.clone(), admission);
        true
    }
    fn remove(&mut self, scope: &Scope) {
        self.0.remove(scope);
    }
    fn len(&self) -> usize {
        self.0.len()
    }
}

#[derive(Default)]
struct SlottedJournal([Option<Admission>; MAX_SCOPES]);

impl ScopedJournal for SlottedJournal {
    fn get(&self, scope: &Scope) -> Option<Admission> {
        self.0
            .iter()
            .flatten()
            .find(|admission| admission.scope == *scope)
            .cloned()
    }
    fn put(&mut self, admission: Admission) -> bool {
        let position = self
            .0
            .iter()
            .position(|slot| {
                slot.as_ref()
                    .is_some_and(|entry| entry.scope == admission.scope)
            })
            .or_else(|| self.0.iter().position(Option::is_none));
        let Some(position) = position else {
            return false;
        };
        self.0[position] = Some(admission);
        true
    }
    fn remove(&mut self, scope: &Scope) {
        for slot in &mut self.0 {
            if slot
                .as_ref()
                .is_some_and(|admission| admission.scope == *scope)
            {
                *slot = None;
            }
        }
    }
    fn len(&self) -> usize {
        self.0.iter().flatten().count()
    }
}

#[derive(Clone)]
struct PreparedMode {
    operation: String,
    generation: u64,
    target: Target,
}

struct FixturePlatform {
    native: Vec<Target>,
    target: Target,
    owner_generation: u64,
    prepared: PreparedMode,
    topology_revision: String,
    readback_available: bool,
    apply_during_admission: bool,
    mutations: usize,
}

fn target(id: &str, width: u32, height: u32, x: i32) -> Target {
    Target {
        id: id.into(),
        width,
        height,
        refresh: Refresh {
            numerator: 60_000,
            denominator: 1_000,
        },
        x,
        y: 0,
        rotation: Rotation::Identity,
        hdr_enabled: Some(false),
    }
}

fn failure(code: &str) -> Value {
    json!({"ok":false,"code":code})
}

impl FixturePlatform {
    fn new(selector: &str, revision: &str, apply_during_admission: bool) -> Self {
        let owned = target(selector, 1920, 1080, 3840);
        let mut prepared_target = owned.clone();
        prepared_target.width = 2560;
        prepared_target.height = 1440;
        prepared_target.hdr_enabled = Some(true);
        Self {
            native: vec![
                target("physical-primary", 3840, 2160, 0),
                target("peer-retained-output", 1280, 720, -1280),
                owned.clone(),
            ],
            target: owned,
            owner_generation: 7,
            prepared: PreparedMode {
                operation: format!("prepared-{selector}-generation-7"),
                generation: 7,
                target: prepared_target,
            },
            topology_revision: revision.into(),
            readback_available: false,
            apply_during_admission,
            mutations: 0,
        }
    }

    fn exact_owned_claim(
        &self,
        scope: &Scope,
        requested: &Target,
        mode_operation: Option<&str>,
    ) -> bool {
        if scope != &Scope::new(CONSUMER, IDENTITY, REFERENCE) {
            return false;
        }
        let expected = if let Some(mode_operation) = mode_operation {
            if mode_operation != self.prepared.operation
                || self.prepared.generation != self.owner_generation
            {
                return false;
            }
            &self.prepared.target
        } else {
            &self.target
        };
        requested.id == expected.id
            && requested.width == expected.width
            && requested.height == expected.height
            && u64::from(requested.refresh.numerator) * u64::from(expected.refresh.denominator)
                == u64::from(expected.refresh.numerator) * u64::from(requested.refresh.denominator)
            && requested.hdr_enabled == expected.hdr_enabled
    }

    fn observe_exact(&mut self, admission: &Admission, desired: bool) {
        self.native = if desired {
            admission.desired.clone()
        } else {
            admission.before.clone()
        };
        self.readback_available = true;
    }
}

trait DisplayFixture {
    fn principal(&self) -> &'static str;
    fn journal(&self) -> &dyn ScopedJournal;
    fn platform(&self) -> &FixturePlatform;
    fn platform_mut(&mut self) -> &mut FixturePlatform;
    fn reply(&mut self, caller: &str, operation: ScopedOperation) -> Value;
}

struct IndexedDisplay {
    journal: IndexedJournal,
    platform: FixturePlatform,
}

impl Default for IndexedDisplay {
    fn default() -> Self {
        Self {
            journal: IndexedJournal::default(),
            platform: FixturePlatform::new("indexed-owned-target", "index-topology-3", true),
        }
    }
}

struct SlottedDisplay {
    journal: SlottedJournal,
    platform: FixturePlatform,
}

impl Default for SlottedDisplay {
    fn default() -> Self {
        Self {
            journal: SlottedJournal::default(),
            platform: FixturePlatform::new("slot-owned-target-b", "slot-topology-f4", false),
        }
    }
}

impl DisplayFixture for IndexedDisplay {
    fn principal(&self) -> &'static str {
        "vendor.indexed-composition"
    }
    fn journal(&self) -> &dyn ScopedJournal {
        &self.journal
    }
    fn platform(&self) -> &FixturePlatform {
        &self.platform
    }
    fn platform_mut(&mut self) -> &mut FixturePlatform {
        &mut self.platform
    }
    fn reply(&mut self, caller: &str, operation: ScopedOperation) -> Value {
        scoped_reply(&mut self.journal, &mut self.platform, caller, operation)
    }
}

impl DisplayFixture for SlottedDisplay {
    fn principal(&self) -> &'static str {
        "replacement.slotted-composition"
    }
    fn journal(&self) -> &dyn ScopedJournal {
        &self.journal
    }
    fn platform(&self) -> &FixturePlatform {
        &self.platform
    }
    fn platform_mut(&mut self) -> &mut FixturePlatform {
        &mut self.platform
    }
    fn reply(&mut self, caller: &str, operation: ScopedOperation) -> Value {
        scoped_reply(&mut self.journal, &mut self.platform, caller, operation)
    }
}

fn scoped_reply(
    journal: &mut dyn ScopedJournal,
    platform: &mut FixturePlatform,
    caller: &str,
    operation: ScopedOperation,
) -> Value {
    let mode_operation = match &operation {
        ScopedOperation::ScopedReconfigure {
            mode_operation_id, ..
        } => Some(mode_operation_id.clone()),
        _ => None,
    };
    match operation {
        ScopedOperation::ScopedStatus {
            identity,
            reference_id,
        } => {
            let admission = journal.get(&Scope::new(caller, &identity, &reference_id));
            json!({"ok":true,"identity":identity,"reference_id":reference_id,"operation_id":admission.as_ref().filter(|entry| entry.pending || entry.applied).map(|entry| &entry.operation),"pending":admission.is_some_and(|entry| entry.pending)})
        }
        ScopedOperation::ScopedReconcile {
            operation_id,
            identity,
            reference_id,
        } => {
            let scope = Scope::new(caller, &identity, &reference_id);
            let Some(mut admission) = journal.get(&scope) else {
                return failure("scoped_operation_not_found");
            };
            if admission.operation != operation_id {
                return failure("operation_conflict");
            }
            if !platform.readback_available {
                return json!({"ok":false,"state":"recovery_required","operation_id":operation_id,"code":"scoped_readback_uncertain"});
            }
            if platform.native == admission.desired {
                admission.pending = false;
                admission.applied = true;
                let mode_operation = admission.mode_operation.clone();
                assert!(journal.put(admission));
                json!({"ok":true,"state":"succeeded","reconciled":true,"operation_id":operation_id,"mode_operation_id":mode_operation,"target_id":platform.target.id})
            } else if admission.pending && platform.native == admission.before {
                admission.pending = false;
                admission.applied = false;
                assert!(journal.put(admission));
                json!({"ok":true,"state":"not_applied","reconciled":true,"operation_id":operation_id,"reference_retained":true,"target_id":platform.target.id})
            } else {
                json!({"ok":false,"state":"recovery_required","operation_id":operation_id,"code":"scoped_readback_changed"})
            }
        }
        ScopedOperation::ScopedRelease {
            operation_id,
            identity,
            reference_id,
            expected_topology_revision,
        } => {
            if expected_topology_revision != platform.topology_revision {
                return failure("topology_changed");
            }
            journal.remove(&Scope::new(caller, &identity, &reference_id));
            json!({"ok":true,"released":true,"native_removed":false,"target_id":platform.target.id,"operation_id":operation_id})
        }
        ScopedOperation::ScopedApply {
            operation_id,
            identity,
            reference_id,
            target,
            expected_topology_revision,
            primary,
        }
        | ScopedOperation::ScopedReconfigure {
            operation_id,
            identity,
            reference_id,
            target,
            expected_topology_revision,
            primary,
            mode_operation_id: _,
        } => {
            let scope = Scope::new(caller, &identity, &reference_id);
            if !platform.exact_owned_claim(&scope, &target, mode_operation.as_deref()) {
                return failure("target_ownership_unverified");
            }
            if primary {
                return failure("fixture_primary_unsupported");
            }
            let existing = journal.get(&scope);
            if mode_operation.is_some() && existing.is_none() {
                return failure("not_found");
            }
            if existing
                .as_ref()
                .is_some_and(|entry| entry.pending && entry.operation != operation_id)
            {
                return failure("scoped_recovery_required");
            }
            if expected_topology_revision != platform.topology_revision {
                return failure("topology_changed");
            }
            let mut desired = platform.native.clone();
            let Some(output) = desired.iter_mut().find(|output| output.id == target.id) else {
                return failure("target_unavailable");
            };
            *output = target;
            if let Some(existing) = existing
                && existing.operation == operation_id
            {
                if existing.desired != desired || existing.mode_operation != mode_operation {
                    return failure("operation_conflict");
                }
                return json!({"ok":false,"code":"scoped_recovery_required","scoped_pending":existing.pending});
            }
            let admission = Admission {
                scope,
                operation: operation_id.clone(),
                mode_operation,
                before: platform.native.clone(),
                desired: desired.clone(),
                pending: true,
                applied: false,
            };
            if !journal.put(admission) {
                return failure("resource_exhausted");
            }
            // Admission is durable before this optional mocked native effect.
            // Missing readback keeps the operation pending for either provider.
            if platform.apply_during_admission {
                platform.native = desired;
                platform.mutations += 1;
            }
            platform.readback_available = false;
            json!({"ok":false,"code":"scoped_recovery_required","scoped_pending":true,"operation_id":operation_id})
        }
    }
}

fn assert_ok(response: Response) {
    assert!(matches!(response, Response::Ok), "{response:?}");
}

fn assert_error(response: Response, code: &str) {
    assert!(
        matches!(&response, Response::Error { code: actual, .. } if actual == code),
        "{response:?}"
    );
}

struct Consumer<'a> {
    broker: Broker,
    fixture: &'a mut dyn DisplayFixture,
    sequence: u64,
}

impl<'a> Consumer<'a> {
    fn new(fixture: &'a mut dyn DisplayFixture) -> Self {
        let mut policy = RuntimeConfig::default();
        policy.principals.insert(
            fixture.principal().into(),
            Grants {
                provide: [display::SERVICE.into()].into(),
                ..Default::default()
            },
        );
        for caller in [CONSUMER, PEER] {
            policy.principals.insert(
                caller.into(),
                Grants {
                    call: [display::SERVICE.into()].into(),
                    ..Default::default()
                },
            );
        }
        policy.principals.insert(OUTSIDER.into(), Grants::default());
        let mut broker = Broker::new(policy).unwrap();
        assert_ok(broker.dispatch(
            fixture.principal(),
            Command::Register {
                service: display::SERVICE.into(),
            },
        ));
        Self {
            broker,
            fixture,
            sequence: 0,
        }
    }

    fn admit(&mut self, caller: &str, operation: &ScopedOperation) -> u64 {
        self.sequence += 1;
        let response = self.broker.dispatch(
            caller,
            Command::Request {
                service: display::SERVICE.into(),
                payload: operation.payload().unwrap(),
                timeout_ms: 1_000,
                operation_id: format!("scoped-broker-call-{}", self.sequence),
                parent: None,
            },
        );
        let Response::Accepted { id } = response else {
            panic!("{response:?}");
        };
        id
    }

    fn deliver(&mut self, caller: &str, request: u64, expected: &ScopedOperation) -> Value {
        let Response::Request {
            id,
            caller: owner,
            service,
            payload,
            ..
        } = self
            .broker
            .dispatch(self.fixture.principal(), Command::Poll {})
        else {
            panic!("delivery");
        };
        assert_eq!(id, request);
        assert_eq!(owner, caller);
        assert_eq!(service, display::SERVICE);
        assert_eq!(payload, expected.payload().unwrap());
        self.fixture
            .reply(&owner, serde_json::from_value(payload).unwrap())
    }

    fn call_as(&mut self, caller: &str, operation: ScopedOperation) -> Value {
        let request = self.admit(caller, &operation);
        let payload = self.deliver(caller, request, &operation);
        assert_ok(self.broker.dispatch(
            self.fixture.principal(),
            Command::Complete {
                request,
                payload: payload.clone(),
            },
        ));
        let Response::Result { payload: consumed } = self
            .broker
            .dispatch(caller, Command::TakeResult { request })
        else {
            panic!("result");
        };
        assert_eq!(consumed, payload);
        assert_ok(
            self.broker
                .dispatch(caller, Command::ReleaseResult { request }),
        );
        payload
    }

    fn call(&mut self, operation: ScopedOperation) -> Value {
        self.call_as(CONSUMER, operation)
    }

    fn status(&mut self) -> ScopedStatus {
        scoped_status_from_reply(
            self.call(ScopedOperation::ScopedStatus {
                identity: IDENTITY.into(),
                reference_id: REFERENCE.into(),
            }),
            IDENTITY,
            REFERENCE,
        )
        .unwrap()
    }

    fn reconcile(&mut self, operation: &str) -> Value {
        self.call(ScopedOperation::ScopedReconcile {
            operation_id: operation.into(),
            identity: IDENTITY.into(),
            reference_id: REFERENCE.into(),
        })
    }

    fn cancel_after_admission(&mut self, operation: ScopedOperation) {
        let request = self.admit(CONSUMER, &operation);
        let payload = self.deliver(CONSUMER, request, &operation);
        assert_eq!(payload["scoped_pending"], true);
        assert_ok(self.broker.dispatch(CONSUMER, Command::Cancel { request }));
        assert_error(
            self.broker.dispatch(
                self.fixture.principal(),
                Command::Complete { request, payload },
            ),
            "request_ended",
        );
        assert_error(
            self.broker
                .dispatch(CONSUMER, Command::TakeResult { request }),
            "cancelled",
        );
        assert_ok(
            self.broker
                .dispatch(CONSUMER, Command::ReleaseResult { request }),
        );
        // Native effects may have happened. Keep the durable admission for the
        // exact status/reconcile path instead of deleting it as cancellation.
        assert_eq!(self.fixture.journal().len(), 1);
    }

    fn observe_exact(&mut self, operation: &str, desired: bool) {
        let admission = self
            .fixture
            .journal()
            .get(&Scope::new(CONSUMER, IDENTITY, REFERENCE))
            .unwrap();
        assert_eq!(admission.operation, operation);
        self.fixture
            .platform_mut()
            .observe_exact(&admission, desired);
    }
}

fn apply(operation: &str, target: &Target, revision: &str) -> ScopedOperation {
    ScopedOperation::ScopedApply {
        operation_id: operation.into(),
        identity: IDENTITY.into(),
        reference_id: REFERENCE.into(),
        target: target.clone(),
        expected_topology_revision: revision.into(),
        primary: false,
    }
}

fn reconfigure(operation: &str, receipt: &str, target: &Target, revision: &str) -> ScopedOperation {
    ScopedOperation::ScopedReconfigure {
        operation_id: operation.into(),
        mode_operation_id: receipt.into(),
        identity: IDENTITY.into(),
        reference_id: REFERENCE.into(),
        target: target.clone(),
        expected_topology_revision: revision.into(),
        primary: false,
    }
}

fn release(operation: &str, revision: &str) -> ScopedOperation {
    ScopedOperation::ScopedRelease {
        operation_id: operation.into(),
        identity: IDENTITY.into(),
        reference_id: REFERENCE.into(),
        expected_topology_revision: revision.into(),
    }
}

struct Outcome {
    selector: String,
    changed_before_native_proof: bool,
}

// Both implementations use this consumer unchanged. Only trusted composition
// and protected platform evidence supply target/receipt/revision values.
fn scoped_workflow(fixture: &mut dyn DisplayFixture) -> Outcome {
    let original = fixture.platform().native.clone();
    let mut requested = fixture.platform().target.clone();
    requested.x += 64;
    let revision = fixture.platform().topology_revision.clone();
    let prepared = fixture.platform().prepared.clone();
    let mut consumer = Consumer::new(fixture);
    assert_eq!(
        consumer.status(),
        ScopedStatus {
            operation_id: None,
            pending: false
        }
    );

    let mut read_only = apply("read-only-borrow-is-not-mutation", &original[0], &revision);
    if let ScopedOperation::ScopedApply {
        identity,
        reference_id,
        ..
    } = &mut read_only
    {
        *identity = "read-only-capture-target".into();
        *reference_id = "borrowed-capture-reference".into();
    }
    assert_eq!(
        consumer.call(read_only)["code"],
        "target_ownership_unverified"
    );
    assert_eq!(
        consumer.call_as(PEER, apply("peer-is-not-owner", &requested, &revision))["code"],
        "target_ownership_unverified"
    );
    let mut foreign_reference = apply("wrong-reference", &requested, &revision);
    if let ScopedOperation::ScopedApply { reference_id, .. } = &mut foreign_reference {
        *reference_id = "another-reference".into();
    }
    assert_eq!(
        consumer.call(foreign_reference)["code"],
        "target_ownership_unverified"
    );
    assert_eq!(
        consumer.call(apply("stale-topology", &requested, "unobserved-topology"))["code"],
        "topology_changed"
    );
    assert_eq!(consumer.fixture.platform().native, original);
    assert_eq!(consumer.fixture.journal().len(), 0);

    consumer.cancel_after_admission(apply("placement-lost-reply", &requested, &revision));
    let changed_before_native_proof = consumer.fixture.platform().native != original;
    let status = consumer.status();
    assert!(status.pending);
    let discovered = status.operation_id.unwrap();
    assert_eq!(discovered, "placement-lost-reply");
    let peer_status = consumer.call_as(
        PEER,
        ScopedOperation::ScopedStatus {
            identity: IDENTITY.into(),
            reference_id: REFERENCE.into(),
        },
    );
    assert_eq!(
        scoped_status_from_reply(peer_status, IDENTITY, REFERENCE)
            .unwrap()
            .operation_id,
        None
    );
    let different_scope = consumer.call(ScopedOperation::ScopedStatus {
        identity: IDENTITY.into(),
        reference_id: "different-reference".into(),
    });
    assert!(scoped_status_from_reply(different_scope, IDENTITY, REFERENCE).is_err());
    assert_eq!(
        consumer.reconcile("a-different-operation")["code"],
        "operation_conflict"
    );
    assert_eq!(
        consumer.reconcile(&discovered)["state"],
        "recovery_required"
    );
    assert!(consumer.status().pending);

    consumer.observe_exact(&discovered, true);
    consumer.fixture.platform_mut().native[0].width += 16;
    assert_eq!(
        consumer.reconcile(&discovered)["code"],
        "scoped_readback_changed"
    );
    assert!(consumer.status().pending);
    consumer.observe_exact(&discovered, true);
    let reconciled = consumer.reconcile(&discovered);
    assert_eq!(reconciled["state"], "succeeded");
    assert_eq!(reconciled["operation_id"], discovered);
    assert_eq!(
        consumer.status(),
        ScopedStatus {
            operation_id: Some(discovered.clone()),
            pending: false
        }
    );
    assert_eq!(consumer.fixture.platform().native[0..2], original[0..2]);

    // A settled admission is still not current native readiness: a later
    // readback drift must be reconciled and cannot be hidden by status.
    consumer.fixture.platform_mut().native[2].x += 1;
    assert!(!consumer.status().pending);
    assert_eq!(
        consumer.reconcile(&discovered)["code"],
        "scoped_readback_changed"
    );
    consumer.observe_exact(&discovered, true);
    assert_eq!(consumer.reconcile(&discovered)["ok"], true);

    let before_reconfigure = consumer.fixture.platform().native.clone();
    assert_eq!(
        consumer.call(reconfigure(
            "wrong-mode-receipt",
            "mode-generation-6",
            &prepared.target,
            &revision
        ))["code"],
        "target_ownership_unverified"
    );
    let mut incorrect_mode = prepared.target.clone();
    incorrect_mode.width += 1;
    assert_eq!(
        consumer.call(reconfigure(
            "wrong-mode-readback",
            &prepared.operation,
            &incorrect_mode,
            &revision
        ))["code"],
        "target_ownership_unverified"
    );
    consumer.fixture.platform_mut().prepared.generation -= 1;
    assert_eq!(
        consumer.call(reconfigure(
            "stale-prepared-generation",
            &prepared.operation,
            &prepared.target,
            &revision
        ))["code"],
        "target_ownership_unverified"
    );
    consumer.fixture.platform_mut().prepared.generation += 1;
    assert_eq!(consumer.fixture.platform().native, before_reconfigure);
    assert_eq!(
        consumer.status().operation_id.as_deref(),
        Some(discovered.as_str())
    );

    assert_eq!(
        consumer.call(reconfigure(
            "retained-mode-not-applied",
            &prepared.operation,
            &prepared.target,
            &revision
        ))["scoped_pending"],
        true
    );
    let pending = consumer.status().operation_id.unwrap();
    consumer.observe_exact(&pending, false);
    let not_applied = consumer.reconcile(&pending);
    assert_eq!(not_applied["state"], "not_applied");
    assert_eq!(not_applied["reference_retained"], true);
    assert_eq!(
        consumer.status(),
        ScopedStatus {
            operation_id: None,
            pending: false
        }
    );
    assert_eq!(consumer.fixture.journal().len(), 1);
    assert_eq!(
        consumer.call(reconfigure(
            "retained-mode-ready",
            &prepared.operation,
            &prepared.target,
            &revision
        ))["scoped_pending"],
        true
    );
    let pending = consumer.status().operation_id.unwrap();
    consumer.observe_exact(&pending, true);
    let settled = consumer.reconcile(&pending);
    assert_eq!(settled["state"], "succeeded");
    assert_eq!(settled["mode_operation_id"], prepared.operation);
    assert_eq!(consumer.fixture.platform().native[0..2], original[0..2]);

    let native_before_release = consumer.fixture.platform().native.clone();
    let mutations = consumer.fixture.platform().mutations;
    assert_eq!(
        consumer.call_as(PEER, release("peer-release-cannot-remove-owner", &revision))["native_removed"],
        false
    );
    assert_eq!(
        consumer.status().operation_id.as_deref(),
        Some(pending.as_str())
    );
    let released = consumer.call(release("release-composition-reference", &revision));
    assert_eq!(released["released"], true);
    assert_eq!(released["native_removed"], false);
    assert_eq!(consumer.fixture.platform().native, native_before_release);
    assert_eq!(consumer.fixture.platform().mutations, mutations);
    assert_eq!(consumer.fixture.journal().len(), 0);
    for _ in 0..600 {
        assert_eq!(
            consumer.status(),
            ScopedStatus {
                operation_id: None,
                pending: false
            }
        );
    }
    assert_eq!(consumer.fixture.platform().native, native_before_release);
    Outcome {
        selector: requested.id,
        changed_before_native_proof,
    }
}

#[test]
fn scoped_display_all_operations_survive_provider_replacement() {
    let first = scoped_workflow(&mut IndexedDisplay::default());
    let replacement = scoped_workflow(&mut SlottedDisplay::default());
    assert_ne!(first.selector, replacement.selector);
    assert!(first.changed_before_native_proof);
    assert!(!replacement.changed_before_native_proof);
}

#[test]
fn scoped_display_broker_rejects_unapproved_callers_and_registration() {
    let mut fixture = IndexedDisplay::default();
    let mut consumer = Consumer::new(&mut fixture);
    let operation = ScopedOperation::ScopedStatus {
        identity: IDENTITY.into(),
        reference_id: REFERENCE.into(),
    };
    assert_error(
        consumer.broker.dispatch(
            OUTSIDER,
            Command::Register {
                service: display::SERVICE.into(),
            },
        ),
        "permission_denied",
    );
    assert_error(
        consumer.broker.dispatch(
            OUTSIDER,
            Command::Request {
                service: display::SERVICE.into(),
                payload: operation.payload().unwrap(),
                timeout_ms: 1_000,
                operation_id: "outsider-status".into(),
                parent: None,
            },
        ),
        "permission_denied",
    );
    assert_eq!(consumer.fixture.journal().len(), 0);
    assert_eq!(consumer.fixture.platform().mutations, 0);
}
