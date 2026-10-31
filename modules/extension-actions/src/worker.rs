//! CAS persistence for admitted action dispatches.
//!
//! Every transition is compared with the complete previously persisted dispatch
//! and receipt. A conflict reloads the epoch, preserving concurrent changes to
//! other receipts and collection metadata. Only a confirmed commit permits the
//! caller to perform the next action; uncertain writes remain fenced.
use crate::dispatch::{Dispatch, Stage};
use crate::service::Epoch;
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::time::Instant;

const COLLECTION: &str = "extension-actions-v1";
const STATE: &str = "amaterasu.state/1";
const MAX_QUEUED: usize = 32;
const MAX_EPOCH_BYTES: usize = 8_000;
const MAX_RETRIES: u8 = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    InvalidTransition,
    Conflict,
    Unavailable,
    RecoveryRequired,
    Capacity,
}

#[derive(Debug)]
pub struct Completion {
    pub epoch_id: String,
    pub sequence: u64,
    pub result: Result<(), Failure>,
}

struct Job {
    epoch_id: String,
    expected: Dispatch,
    replacement: Dispatch,
    expected_receipt: Value,
    receipt: Value,
    deadline: Instant,
    generation: u64,
    retries: u8,
    phase: Phase,
}

enum Phase {
    Load,
    Save {
        revision: u64,
        metadata: Value,
        epoch: Value,
    },
    Waiting {
        child: u64,
        saving: bool,
    },
    Release {
        child: u64,
        saving: bool,
        outcome: Value,
    },
}

pub struct Worker {
    queue: VecDeque<Job>,
    current: Option<Job>,
    epoch: String,
    next: u64,
}

impl Worker {
    pub fn new() -> ModuleResult<Self> {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes).map_err(|error| format!("action worker entropy: {error}"))?;
        let epoch = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        Ok(Self {
            queue: VecDeque::new(),
            current: None,
            epoch,
            next: 0,
        })
    }

    /// Queues one transition. Completion must be checked before acting on it.
    pub fn persist(
        &mut self,
        epoch_id: String,
        expected: Dispatch,
        replacement: Dispatch,
        expected_receipt: Value,
        receipt: Value,
        deadline: Instant,
    ) -> Result<(), Failure> {
        if self.queue.len() + usize::from(self.current.is_some()) >= MAX_QUEUED {
            return Err(Failure::Capacity);
        }
        if !(valid_transition(&expected, &replacement)
            || valid_cancellation(&expected, &replacement, &expected_receipt, &receipt))
            || expected.request.epoch != epoch_id
            || !valid_epoch(&epoch_id)
            || !expected_receipt.is_object()
            || !receipt.is_object()
        {
            return Err(Failure::InvalidTransition);
        }
        self.next = self.next.checked_add(1).ok_or(Failure::Capacity)?;
        self.queue.push_back(Job {
            epoch_id,
            expected,
            replacement,
            expected_receipt,
            receipt,
            deadline,
            generation: self.next,
            retries: 0,
            phase: Phase::Load,
        });
        Ok(())
    }

    /// Advances one state request without blocking on provider work.
    pub fn tick(
        &mut self,
        mut send: impl FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<Vec<Completion>> {
        let Some(mut job) = self.current.take().or_else(|| self.queue.pop_front()) else {
            return Ok(Vec::new());
        };
        if Instant::now() >= job.deadline {
            let phase = std::mem::replace(&mut job.phase, Phase::Load);
            let outcome = match phase {
                Phase::Release {
                    child,
                    saving,
                    outcome,
                } => {
                    let _ = send(Command::ReleaseResult { request: child });
                    if saving && outcome.get("ok") == Some(&Value::Bool(true)) {
                        Ok(())
                    } else {
                        Err(failure_for(&job, false))
                    }
                }
                Phase::Waiting { child, saving } => {
                    let _ = send(Command::Cancel { request: child });
                    let _ = send(Command::ReleaseResult { request: child });
                    Err(failure_for(&job, saving))
                }
                Phase::Save { .. } | Phase::Load => Err(failure_for(&job, false)),
            };
            return Ok(vec![finish(job, outcome)]);
        }
        match std::mem::replace(&mut job.phase, Phase::Load) {
            Phase::Load => {
                let payload = json!({"action":"collection_get","collection":COLLECTION,"key":format!("epoch/{}",job.epoch_id)});
                self.submit(&mut job, payload, false, &mut send)?;
            }
            Phase::Save {
                revision,
                metadata,
                epoch,
            } => {
                let payload = json!({"action":"collection_commit","collection":COLLECTION,"expected_revision":revision,"metadata":metadata,"edits":[{"key":format!("epoch/{}",job.epoch_id),"value":epoch}]});
                self.submit(&mut job, payload, true, &mut send)?;
            }
            Phase::Waiting { child, saving } => {
                match send(Command::TakeResult { request: child }) {
                    Ok(Response::Pending) | Err(_) => job.phase = Phase::Waiting { child, saving },
                    Ok(Response::Result { payload }) => {
                        job.phase = Phase::Release {
                            child,
                            saving,
                            outcome: payload,
                        }
                    }
                    Ok(Response::Error { code, message }) => {
                        job.phase = Phase::Release {
                            child,
                            saving,
                            outcome: json!({"ok":false,"error":{"code":code,"message":message}}),
                        }
                    }
                    Ok(_) => return Ok(vec![finish(job, Err(Failure::RecoveryRequired))]),
                }
            }
            Phase::Release {
                child,
                saving,
                outcome,
            } => {
                match send(Command::ReleaseResult { request: child }) {
                    Ok(Response::Ok) => (),
                    Ok(Response::Error { code, .. }) if code == "not_found" => (),
                    _ => {
                        job.phase = Phase::Release {
                            child,
                            saving,
                            outcome,
                        };
                        self.current = Some(job);
                        return Ok(Vec::new());
                    }
                }
                if saving {
                    if outcome.get("ok") == Some(&Value::Bool(true)) {
                        return Ok(vec![finish(job, Ok(()))]);
                    }
                    if outcome.pointer("/error/code").and_then(Value::as_str)
                        == Some("revision_conflict")
                        && job.retries < MAX_RETRIES
                    {
                        job.retries += 1;
                        job.phase = Phase::Load;
                    } else {
                        let failure = failure_for(&job, true);
                        return Ok(vec![finish(job, Err(failure))]);
                    }
                } else {
                    match merge(&job, &outcome) {
                        Ok((revision, metadata, epoch)) => {
                            job.phase = Phase::Save {
                                revision,
                                metadata,
                                epoch,
                            }
                        }
                        Err(error) => {
                            let error = if error == Failure::Conflict
                                && failure_for(&job, false) == Failure::RecoveryRequired
                            {
                                Failure::RecoveryRequired
                            } else {
                                error
                            };
                            return Ok(vec![finish(job, Err(error))]);
                        }
                    }
                }
            }
        }
        self.current = Some(job);
        Ok(Vec::new())
    }

    fn submit(
        &self,
        job: &mut Job,
        payload: Value,
        saving: bool,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<()> {
        let timeout_ms = job
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(30_000) as u64;
        let stage = if saving { "save" } else { "load" };
        let operation_id = format!(
            "action-worker-{}-{}-{}-{}",
            self.epoch, job.generation, job.retries, stage
        );
        let request = Command::Request {
            service: STATE.into(),
            payload,
            timeout_ms: timeout_ms.max(1),
            operation_id,
            parent: None,
        };
        match send(request) {
            Ok(Response::Accepted { id }) => job.phase = Phase::Waiting { child: id, saving },
            // An uncertain save submission may have committed; never reissue a
            // different effect or report the transition persisted.
            Err(_) if saving => {
                job.phase = Phase::Release {
                    child: 0,
                    saving: true,
                    outcome: json!({"ok":false,"error":{"code":"uncertain"}}),
                }
            }
            Err(_) => job.phase = Phase::Load,
            Ok(Response::Error { code, message }) => {
                job.phase = Phase::Release {
                    child: 0,
                    saving,
                    outcome: json!({"ok":false,"error":{"code":code,"message":message}}),
                }
            }
            Ok(_) => {
                job.phase = Phase::Release {
                    child: 0,
                    saving,
                    outcome: json!({"ok":false,"error":{"code":"invalid_response"}}),
                }
            }
        }
        Ok(())
    }
}

fn merge(job: &Job, response: &Value) -> Result<(u64, Value, Value), Failure> {
    if response.get("ok") != Some(&Value::Bool(true)) {
        return Err(failure_for(job, false));
    }
    let revision = response
        .get("revision")
        .and_then(Value::as_u64)
        .ok_or(Failure::Unavailable)?;
    let metadata = response
        .get("metadata")
        .cloned()
        .ok_or(Failure::Unavailable)?;
    let mut epoch: Epoch =
        serde_json::from_value(response.get("value").cloned().unwrap_or(Value::Null))
            .map_err(|_| Failure::Conflict)?;
    if epoch.caller != job.expected.caller || epoch.initiator != job.expected.initiator {
        return Err(Failure::Conflict);
    }
    let record = epoch
        .receipts
        .get_mut(&job.expected.request.sequence)
        .ok_or(Failure::Conflict)?;
    if record.request != job.expected.request
        || record.selected_provider != job.expected.selected_provider
        || record.registry_revision != job.expected.registry_revision
        || record.descriptor != job.expected.descriptor
        || record.dispatch.as_ref() != Some(&job.expected)
        || record.receipt != job.expected_receipt
    {
        return Err(Failure::Conflict);
    }
    record.dispatch = Some(job.replacement.clone());
    record.receipt = job.receipt.clone();
    let value = serde_json::to_value(epoch).map_err(|_| Failure::Unavailable)?;
    if serde_json::to_vec(&value).map_or(true, |bytes| bytes.len() > MAX_EPOCH_BYTES) {
        return Err(failure_for(job, false));
    }
    Ok((revision, metadata, value))
}

fn valid_cancellation(
    before: &Dispatch,
    after: &Dispatch,
    expected: &Value,
    receipt: &Value,
) -> bool {
    if before != after
        || !matches!(
            before.stage,
            Stage::AwaitingConsume { .. }
                | Stage::AwaitingTransition { .. }
                | Stage::ReadyToExecute { .. }
                | Stage::Running { .. }
                | Stage::ProviderPending { .. }
                | Stage::Completing { .. }
        )
    {
        return false;
    }
    let Some(mut expected) = expected.as_object().cloned() else {
        return false;
    };
    if expected.get("cancellation_requested") == Some(&Value::Bool(true)) {
        return false;
    }
    expected.insert("cancellation_requested".into(), Value::Bool(true));
    receipt.as_object() == Some(&expected)
}

fn valid_epoch(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn valid_transition(before: &Dispatch, after: &Dispatch) -> bool {
    if before.selected_provider.is_empty()
        || before.caller.is_empty()
        || before.initiator.is_empty()
    {
        return false;
    }
    let mut computed = before.clone();
    if before
        .proof
        .as_ref()
        .is_some_and(|proof| proof.last().result.is_none())
        && matches!(
            before.stage,
            Stage::Running { .. } | Stage::ProviderPending { .. }
        )
        && after
            .proof
            .as_ref()
            .is_some_and(|proof| proof.last().result.is_some())
        && after.proof.as_ref().map_or(0, |proof| proof.status.len())
            == before.proof.as_ref().map_or(0, |proof| proof.status.len())
    {
        let reply = after
            .proof
            .as_ref()
            .and_then(|proof| proof.last().result.clone())
            .unwrap();
        return computed.accept_reply(reply).is_ok() && computed == *after;
    }
    let result = match (&before.stage, &after.stage) {
        (Stage::Planning, Stage::AwaitingPermit { plan, .. }) => computed.accept_plan(plan.clone()),
        (Stage::AwaitingPermit { .. }, Stage::AwaitingPermit { .. })
            if before.approvals.is_none() =>
        {
            let frozen = after.approvals.as_ref().ok_or("approval_snapshot_missing");
            frozen.and_then(|frozen| {
                computed.freeze_approvals(frozen.execute.clone(), frozen.status.clone())
            })
        }
        (Stage::Planning, Stage::Cancelled) => computed.cancel_before_effect(),
        (
            Stage::AwaitingConsume { .. }
            | Stage::AwaitingTransition { .. }
            | Stage::ReadyToExecute { .. }
            | Stage::Running { .. },
            Stage::Cancelled,
        ) => computed.confirm_cancelled_before_effect(),
        (Stage::AwaitingPermit { .. }, Stage::AwaitingConsume { digest, permit, .. }) => computed
            .accept_permit(
                &before.request.operation_id,
                &before.request.target,
                digest,
                permit.clone(),
            ),
        (
            Stage::AwaitingConsume { .. },
            Stage::ReadyToExecute { digest, permit, .. }
            | Stage::AwaitingTransition { digest, permit, .. },
        ) => computed.confirm_consume(
            &before.request.operation_id,
            &before.request.target,
            digest,
            permit,
        ),
        (Stage::AwaitingTransition { .. }, Stage::ReadyToExecute { digest, .. }) => computed
            .confirm_transition(&before.request.operation_id, &before.request.target, digest),
        (Stage::ReadyToExecute { .. }, Stage::Running { .. }) => {
            computed.begin_effect().and_then(|_| {
                if let Some(proof) = &after.proof {
                    computed.arm_effect(proof.effect.approval.clone())?;
                }
                Ok(())
            })
        }
        (Stage::ProviderPending { .. }, Stage::ProviderPending { .. })
            if after.proof.as_ref().map_or(0, |proof| proof.status.len())
                > before.proof.as_ref().map_or(0, |proof| proof.status.len()) =>
        {
            let approval = after
                .proof
                .as_ref()
                .and_then(|proof| proof.status.last())
                .map(|call| call.approval.clone())
                .ok_or("status_proof_missing");
            approval.and_then(|approval| computed.arm_status(approval))
        }
        (
            Stage::Running { .. } | Stage::ProviderPending { .. },
            Stage::Completing {
                outcome,
                provider_receipt,
                ..
            },
        ) => computed.provider_result(outcome.clone(), provider_receipt.clone()),
        (
            Stage::Running { .. } | Stage::ProviderPending { .. },
            Stage::ProviderPending {
                provider_receipt, ..
            },
        ) => computed.provider_pending(provider_receipt.clone()),
        (Stage::Completing { .. }, Stage::Finished { .. }) => computed.confirm_completion(),
        (
            Stage::Planning
            | Stage::AwaitingPermit { .. }
            | Stage::AwaitingConsume { .. }
            | Stage::AwaitingTransition { .. }
            | Stage::ReadyToExecute { .. }
            | Stage::Running { .. }
            | Stage::ProviderPending { .. }
            | Stage::Completing { .. },
            Stage::RecoveryRequired,
        ) => {
            computed.stage = Stage::RecoveryRequired;
            Ok(())
        }
        _ => Err("invalid_phase"),
    };
    result.is_ok() && computed == *after
}

fn failure_for(job: &Job, unknown_save: bool) -> Failure {
    if unknown_save
        || matches!(
            job.replacement.stage,
            Stage::AwaitingConsume { .. }
                | Stage::AwaitingTransition { .. }
                | Stage::ReadyToExecute { .. }
                | Stage::Running { .. }
                | Stage::ProviderPending { .. }
                | Stage::Completing { .. }
                | Stage::RecoveryRequired
        )
        || matches!(
            job.expected.stage,
            Stage::AwaitingConsume { .. }
                | Stage::AwaitingTransition { .. }
                | Stage::ReadyToExecute { .. }
                | Stage::Running { .. }
                | Stage::ProviderPending { .. }
                | Stage::Completing { .. }
        )
    {
        Failure::RecoveryRequired
    } else {
        Failure::Unavailable
    }
}
fn finish(job: Job, result: Result<(), Failure>) -> Completion {
    Completion {
        epoch_id: job.epoch_id,
        sequence: job.expected.request.sequence,
        result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_extension_api::{
        ActionDescriptor, ActionInvoke, ActionTarget, ExecutionClass, MethodRef, TargetKind,
    };
    use std::collections::BTreeMap;
    use std::time::Duration;

    fn fixture() -> (Dispatch, Dispatch, Value) {
        let method = MethodRef {
            service: "example/1".into(),
            method: "action_execute".into(),
            interface_major: 1,
            interface_minor: 0,
            schema_revision: 1,
        };
        let descriptor = ActionDescriptor {
            id: "example/action".into(),
            label: "Action".into(),
            description: "".into(),
            execution: ExecutionClass::SessionMutation,
            arguments: Default::default(),
            allowed_targets: vec![TargetKind::Session],
            required_capabilities: vec![],
            plan: Some(method.clone()),
            execute: method,
            status: None,
        };
        let request = ActionInvoke {
            epoch: "a".repeat(32),
            sequence: 1,
            operation_id: format!("{}:1", "a".repeat(32)),
            action_id: descriptor.id.clone(),
            target: ActionTarget::Session {
                host_epoch: "h".into(),
                session_id: "s".into(),
                expected_generation: 1,
            },
            arguments: json!({}),
            expected_settings_revision: None,
            input_reservation_id: None,
        };
        let mut before = Dispatch::new(
            request,
            descriptor,
            "example".into(),
            1,
            "caller".into(),
            "initiator".into(),
        )
        .unwrap();
        before.accept_plan(json!({"plan_id":"p1","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]})).unwrap();
        let Stage::AwaitingPermit { digest, .. } = &before.stage else {
            panic!()
        };
        let digest = digest.clone();
        let target = before.request.target.clone();
        let id = before.request.operation_id.clone();
        before
            .accept_permit(&id, &target, &digest, "permit".into())
            .unwrap();
        before
            .confirm_consume(&id, &target, &digest, "permit")
            .unwrap();
        before.confirm_transition(&id, &target, &digest).unwrap();
        let mut after = before.clone();
        after.begin_effect().unwrap();
        let epoch = json!({"caller":"caller","initiator":"initiator","open_index":"index","closed":false,"receipts":{"1":{"request":before.request,"receipt":{"status":"queued"},"selected_provider":"example","registry_revision":1,"descriptor":before.descriptor,"dispatch":before}}});
        (before, after, epoch)
    }

    struct Host {
        revision: u64,
        epoch: Value,
        results: BTreeMap<u64, Value>,
        next: u64,
        conflict: bool,
        commits: usize,
        fail_save: bool,
    }
    impl Host {
        fn send(&mut self, command: Command) -> ModuleResult<Response> {
            Ok(match command {
                Command::Request { payload, .. } => {
                    if payload["action"] == "collection_commit" && self.fail_save {
                        return Err("lost response".into());
                    }
                    self.next += 1;
                    let result = if payload["action"] == "collection_get" {
                        json!({"ok":true,"revision":self.revision,"metadata":{"other":true},"value":self.epoch})
                    } else if self.conflict {
                        self.conflict = false;
                        self.revision += 1;
                        self.epoch["closed"] = json!(true);
                        json!({"ok":false,"error":{"code":"revision_conflict"}})
                    } else {
                        assert_eq!(payload["expected_revision"], self.revision);
                        assert_eq!(payload["metadata"], json!({"other":true}));
                        self.epoch = payload["edits"][0]["value"].clone();
                        self.revision += 1;
                        self.commits += 1;
                        json!({"ok":true,"revision":self.revision})
                    };
                    self.results.insert(self.next, result);
                    Response::Accepted { id: self.next }
                }
                Command::TakeResult { request } => Response::Result {
                    payload: self.results.remove(&request).unwrap(),
                },
                Command::ReleaseResult { .. } | Command::Cancel { .. } => Response::Ok,
                _ => panic!("unexpected command"),
            })
        }
    }
    fn host(epoch: Value) -> Host {
        Host {
            revision: 1,
            epoch,
            results: BTreeMap::new(),
            next: 0,
            conflict: false,
            commits: 0,
            fail_save: false,
        }
    }
    fn drive(worker: &mut Worker, host: &mut Host) -> Completion {
        for _ in 0..100 {
            let mut done = worker.tick(|command| host.send(command)).unwrap();
            if let Some(done) = done.pop() {
                return done;
            }
        }
        panic!("worker failed to finish")
    }
    #[test]
    fn conflict_reloads_and_running_must_be_persisted_before_success() {
        let (before, after, epoch) = fixture();
        let mut host = host(epoch);
        host.conflict = true;
        let mut worker = Worker::new().unwrap();
        worker
            .persist(
                before.request.epoch.clone(),
                before,
                after,
                json!({"status":"queued"}),
                json!({"status":"running"}),
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap();
        let result = drive(&mut worker, &mut host);
        assert_eq!(result.result, Ok(()));
        assert_eq!(host.commits, 1);
        assert_eq!(host.epoch["closed"], true);
        assert_eq!(
            host.epoch["receipts"]["1"]["dispatch"]["stage"]["phase"],
            "running"
        );
    }
    #[test]
    fn cancellation_or_changed_owner_prevents_stale_write() {
        let (before, after, mut epoch) = fixture();
        epoch["receipts"]["1"]["receipt"]["status"] = json!("cancelled");
        let mut host = host(epoch);
        let mut worker = Worker::new().unwrap();
        worker
            .persist(
                before.request.epoch.clone(),
                before,
                after,
                json!({"status":"queued"}),
                json!({"status":"running"}),
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(
            drive(&mut worker, &mut host).result,
            Err(Failure::RecoveryRequired)
        );
        assert_eq!(host.commits, 0);
    }
    #[test]
    fn lost_save_acknowledgement_never_grants_execution() {
        let (before, after, epoch) = fixture();
        let mut host = host(epoch);
        host.fail_save = true;
        let mut worker = Worker::new().unwrap();
        worker
            .persist(
                before.request.epoch.clone(),
                before,
                after,
                json!({"status":"queued"}),
                json!({"status":"running"}),
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(
            drive(&mut worker, &mut host).result,
            Err(Failure::RecoveryRequired)
        );
        assert_eq!(host.commits, 0);
    }
    #[test]
    fn forged_running_transition_is_rejected() {
        let (before, mut after, _) = fixture();
        let Stage::Running { permit, .. } = &mut after.stage else {
            panic!()
        };
        *permit = "other".into();
        let mut worker = Worker::new().unwrap();
        assert_eq!(
            worker.persist(
                before.request.epoch.clone(),
                before,
                after,
                json!({"status":"queued"}),
                json!({"status":"running"}),
                Instant::now() + Duration::from_secs(1)
            ),
            Err(Failure::InvalidTransition)
        );
    }
    #[test]
    fn expired_consumed_permit_stays_fenced_without_state_write() {
        let (before, after, epoch) = fixture();
        let mut host = host(epoch);
        let mut worker = Worker::new().unwrap();
        worker
            .persist(
                before.request.epoch.clone(),
                before,
                after,
                json!({"status":"queued"}),
                json!({"status":"running"}),
                Instant::now(),
            )
            .unwrap();
        let result = drive(&mut worker, &mut host);
        assert_eq!(result.result, Err(Failure::RecoveryRequired));
        assert_eq!(host.commits, 0);
    }
    #[test]
    fn pending_provider_receipts_and_restart_fence_preserve_identity() {
        let (_, mut running, _) = fixture();
        let mut pending = running.clone();
        pending.provider_pending(json!({"id":"r"})).unwrap();
        assert!(valid_transition(&running, &pending));
        let mut changed = pending.clone();
        changed
            .provider_pending(json!({"id":"r","poll":2}))
            .unwrap();
        assert!(valid_transition(&pending, &changed));
        let mut recovery = changed.clone();
        recovery.fence_after_restart();
        assert!(valid_transition(&changed, &recovery));
        running.request.operation_id = "different".into();
        assert!(!valid_transition(&running, &pending));
    }
    #[test]
    fn transition_requires_exact_consumed_permit_and_fences_uncertainty() {
        let (ready, _, _) = fixture();
        let Stage::ReadyToExecute {
            plan,
            digest,
            permit,
        } = &ready.stage
        else {
            panic!("expected ready dispatch")
        };
        let mut consumed = ready.clone();
        consumed.stage = Stage::AwaitingConsume {
            plan: plan.clone(),
            digest: digest.clone(),
            permit: permit.clone(),
        };
        let mut awaiting = consumed.clone();
        awaiting
            .confirm_consume(
                &ready.request.operation_id,
                &ready.request.target,
                digest,
                permit,
            )
            .unwrap();
        assert!(matches!(awaiting.stage, Stage::AwaitingTransition { .. }));
        assert!(valid_transition(&consumed, &awaiting));
        assert!(!valid_transition(&consumed, &ready));
        assert!(valid_transition(&awaiting, &ready));
        let mut forged = ready.clone();
        if let Stage::ReadyToExecute { permit, .. } = &mut forged.stage {
            *permit = "other".into();
        }
        assert!(!valid_transition(&awaiting, &forged));
        let mut recovery = awaiting.clone();
        recovery.stage = Stage::RecoveryRequired;
        assert!(valid_transition(&awaiting, &recovery));
    }
    #[test]
    fn cooperative_cancellation_only_sets_the_cancellation_flag() {
        let (_, running, _) = fixture();
        let old = json!({"status":"running","receipt_id":"id"});
        let requested = json!({"status":"running","receipt_id":"id","cancellation_requested":true});
        assert!(valid_cancellation(&running, &running, &old, &requested));
        assert!(!valid_cancellation(
            &running,
            &running,
            &old,
            &json!({"status":"cancelled","receipt_id":"id","cancellation_requested":true})
        ));
        assert!(!valid_cancellation(
            &running, &running, &requested, &requested
        ));
    }

    #[test]
    fn pre_effect_cancellation_requires_exact_confirmed_transition() {
        let (ready, _, _) = fixture();
        let Stage::ReadyToExecute {
            plan,
            digest,
            permit,
        } = &ready.stage
        else {
            panic!("expected ready dispatch")
        };
        let mut consume = ready.clone();
        consume.stage = Stage::AwaitingConsume {
            plan: plan.clone(),
            digest: digest.clone(),
            permit: permit.clone(),
        };
        let mut transition = consume.clone();
        transition
            .confirm_consume(
                &ready.request.operation_id,
                &ready.request.target,
                digest,
                permit,
            )
            .unwrap();
        let old = json!({"status":"running"});
        let flagged = json!({"status":"running","cancellation_requested":true});
        for before in [consume, transition, ready] {
            assert!(valid_cancellation(&before, &before, &old, &flagged));
            let mut cancelled = before.clone();
            cancelled.confirm_cancelled_before_effect().unwrap();
            assert!(valid_transition(&before, &cancelled));
            cancelled.request.operation_id = "forged".into();
            assert!(!valid_transition(&before, &cancelled));
        }
    }
    #[test]
    fn confirmed_pre_effect_cancellation_persists_only_exact_dispatch() {
        let (_, running, epoch) = fixture();
        let mut stored = epoch;
        stored["receipts"]["1"]["dispatch"] = serde_json::to_value(&running).unwrap();
        stored["receipts"]["1"]["receipt"] =
            json!({"status":"running","cancellation_requested":true});
        let mut cancelled = running.clone();
        cancelled.confirm_cancelled_before_effect().unwrap();
        assert!(valid_transition(&running, &cancelled));
        let mut forged = cancelled.clone();
        forged.request.action_id = "other/action".into();
        assert!(!valid_transition(&running, &forged));
        let mut host = host(stored);
        let mut worker = Worker::new().unwrap();
        worker
            .persist(
                running.request.epoch.clone(),
                running,
                cancelled,
                json!({"status":"running","cancellation_requested":true}),
                json!({"status":"cancelled","cancellation_requested":true}),
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(drive(&mut worker, &mut host).result, Ok(()));
        assert_eq!(
            host.epoch["receipts"]["1"]["dispatch"]["stage"]["phase"],
            "cancelled"
        );
    }
}
