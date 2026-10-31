//! Bridge the approved display service to the existing caller-owned manager.
//! No operation is inferred from a service grant: unsupported manager features
//! are rejected before any native effect.
use crate::model::Operation;
use amaterasu_module_sdk::virtual_display_service::{
    Capabilities, EffectState, Layout, ModeObservation, OwnerReference, Provider, Reply, Request,
    TopologyRevision,
};
use serde_json::Value;
use std::collections::BTreeMap;

const MAX_OPERATIONS: usize = 128;

pub trait Backend {
    type Admission;
    fn admit(&mut self, caller: &str, operation: Operation) -> (Value, Option<Self::Admission>);
    fn completion_lost(&mut self, admission: Option<Self::Admission>);
}

#[cfg(any(not(amaterasu_libvirtualdisplay), test))]
impl<D: crate::backend::Driver> Backend for crate::manager::Manager<D> {
    type Admission = crate::manager::Admission;
    fn admit(&mut self, caller: &str, operation: Operation) -> (Value, Option<Self::Admission>) {
        self.admit(caller, operation)
    }
    fn completion_lost(&mut self, admission: Option<Self::Admission>) {
        self.completion_lost(admission);
    }
}
#[cfg(amaterasu_libvirtualdisplay)]
impl Backend for crate::linux_manager::Manager {
    type Admission = crate::linux_manager::Admission;
    fn admit(&mut self, caller: &str, operation: Operation) -> (Value, Option<Self::Admission>) {
        self.admit(caller, operation)
    }
    fn completion_lost(&mut self, admission: Option<Self::Admission>) {
        self.completion_lost(admission);
    }
}

#[derive(Clone)]
struct OperationRecord {
    request: Request,
    reply: Reply,
    cleanup: bool,
}

#[derive(Default)]
pub struct Adapter {
    operations: BTreeMap<(String, String), OperationRecord>,
}

pub struct Invocation<'a, B: Backend> {
    adapter: &'a mut Adapter,
    backend: &'a mut B,
    admission: Option<B::Admission>,
}
impl Adapter {
    pub fn invocation<'a, B: Backend>(&'a mut self, backend: &'a mut B) -> Invocation<'a, B> {
        Invocation {
            adapter: self,
            backend,
            admission: None,
        }
    }
}
impl<B: Backend> Invocation<'_, B> {
    fn status(&mut self, caller: &str, owner: Option<&OwnerReference>) -> Value {
        self.backend
            .admit(
                caller,
                Operation::Status {
                    identity: owner.map(|o| o.identity.clone()),
                    reference_id: owner.map(|o| o.reference_id.clone()),
                },
            )
            .0
    }
    fn topology(status: &Value) -> Option<TopologyRevision> {
        let domain = status.get("mutation_domain")?;
        Some(TopologyRevision {
            domain_id: domain.get("id")?.as_str()?.into(),
            revision: domain.get("revision")?.as_u64()?,
        })
    }
    fn inspect(&mut self, caller: &str, owner: &OwnerReference) -> Reply {
        let status = self.status(caller, Some(owner));
        if status["ok"] != true {
            return failure(&status);
        }
        if status["reference_owned"] != true {
            return Reply::NotOwned;
        }
        let state = match status["monitors"][0]["state"].as_str() {
            Some("cleanup_failed" | "ownership_conflict" | "reclaim_pending") => {
                EffectState::RecoveryRequired
            }
            // Ready reports output publication, never verified mode or HDR.
            Some("ready") => EffectState::Applied,
            _ => EffectState::Pending,
        };
        Reply::Effect {
            state,
            topology: Self::topology(&status),
            observation: ModeObservation::NotObserved,
        }
    }
    fn effect(state: EffectState, topology: Option<TopologyRevision>) -> Reply {
        Reply::Effect {
            state,
            topology,
            observation: ModeObservation::NotObserved,
        }
    }

    // Only the owner-scoped manager may establish that its output has gone.
    // Missing/malformed or unreconciled status never proves cleanup settled.
    fn release_confirmed(result: &Value) -> bool {
        result["removed"] == true
            || (result["reference_released"].as_bool().is_some()
                && result["reference_owned"] == false)
    }

    fn cleanup_status(&mut self, caller: &str, owner: &OwnerReference) -> Reply {
        let status = self.status(caller, Some(owner));
        let topology = Self::topology(&status);
        if status["ok"] != true || status["recovery_ready"] != true || topology.is_none() {
            return Self::effect(EffectState::Unknown, topology);
        }
        let Some(monitors) = status["monitors"].as_array() else {
            return Self::effect(EffectState::Unknown, topology);
        };
        let state = if status["reference_owned"] != false {
            EffectState::Unknown
        } else if monitors.is_empty() {
            EffectState::Completed
        } else if monitors.len() != 1 {
            EffectState::Unknown
        } else {
            match monitors[0]["state"].as_str() {
                Some("removing") => EffectState::Pending,
                Some("cleanup_failed") => EffectState::Failed,
                _ => EffectState::Unknown,
            }
        };
        Self::effect(state, topology)
    }

    fn recover(&mut self, caller: &str, owner: &OwnerReference, operation_id: &str) -> Reply {
        let key = (caller.to_owned(), operation_id.to_owned());
        let Some(record) = self.adapter.operations.get(&key).cloned() else {
            return Self::effect(EffectState::Unknown, None);
        };
        let original = match &record.request {
            Request::Create { owner, .. } | Request::Remove { owner, .. } => owner,
            _ => return Self::effect(EffectState::Unknown, None),
        };
        if original != owner {
            return Self::effect(EffectState::Unknown, None);
        }
        let reply = match &record.reply {
            Reply::Effect {
                state: EffectState::Completed | EffectState::Removed | EffectState::Absent,
                ..
            } => record.reply.clone(),
            _ if record.cleanup => self.cleanup_status(caller, owner),
            Reply::Effect {
                state: EffectState::Pending,
                ..
            } => match self.inspect(caller, owner) {
                as_reply @ Reply::Effect {
                    state: EffectState::Pending | EffectState::Applied,
                    ..
                } => as_reply,
                _ => Self::effect(EffectState::Unknown, None),
            },
            _ => record.reply.clone(),
        };
        if let Some(saved) = self.adapter.operations.get_mut(&key) {
            saved.reply = reply.clone();
        }
        reply
    }

    fn retry_cleanup(
        &mut self,
        caller: &str,
        owner: &OwnerReference,
        operation_id: &str,
        expected: &TopologyRevision,
    ) -> Reply {
        let key = (caller.to_owned(), operation_id.to_owned());
        let Some(record) = self.adapter.operations.get(&key).cloned() else {
            return Self::effect(EffectState::Unknown, None);
        };
        let original = match &record.request {
            Request::Create { owner, .. } | Request::Remove { owner, .. } => owner,
            _ => return Self::effect(EffectState::Unknown, None),
        };
        if original != owner || !record.cleanup {
            return Reply::NotOwned;
        }
        match self.recover(caller, owner, operation_id) {
            as_reply @ Reply::Effect {
                state: EffectState::Completed,
                ..
            } => return as_reply,
            Reply::Effect {
                state: EffectState::Pending,
                ..
            } => return Reply::OperationNotSettled,
            Reply::Effect {
                state: EffectState::Failed | EffectState::Unknown,
                ..
            } => {}
            _ => return Reply::OperationNotSettled,
        }
        let status = self.status(caller, Some(owner));
        let Some(current) = Self::topology(&status) else {
            return Self::effect(EffectState::Unknown, None);
        };
        if &current != expected {
            return Reply::StaleTopology { current };
        }
        // A live reference might have been reacquired after the original
        // removal. Never consume it on the authority of an old retry record.
        if status["ok"] != true
            || status["recovery_ready"] != true
            || status["reference_owned"] != false
        {
            return Self::effect(EffectState::Unknown, Some(current));
        }
        let (result, admission) = self.backend.admit(
            caller,
            Operation::Remove {
                identity: owner.identity.clone(),
                reference_id: Some(owner.reference_id.clone()),
            },
        );
        self.admission = admission;
        let state = if result["ok"] == true && Self::release_confirmed(&result) {
            EffectState::Completed
        } else if result["ok"] == true {
            EffectState::Pending
        } else {
            EffectState::Unknown
        };
        let reply = Self::effect(state, Self::topology(&self.status(caller, None)));
        if let Some(saved) = self.adapter.operations.get_mut(&key) {
            saved.reply = reply.clone();
        }
        reply
    }

    fn mutate(
        &mut self,
        caller: &str,
        request: &Request,
        owner: &OwnerReference,
        operation_id: &str,
        expected: &TopologyRevision,
    ) -> Reply {
        let key = (caller.to_owned(), operation_id.to_owned());
        if let Some(old) = self.adapter.operations.get(&key) {
            return if old.request == *request {
                self.recover(caller, owner, operation_id)
            } else {
                Reply::Conflict
            };
        }
        if self.adapter.operations.len() >= MAX_OPERATIONS {
            return Reply::ResourceExhausted;
        }
        let status = self.status(caller, None);
        let Some(current) = Self::topology(&status) else {
            return Reply::Unavailable;
        };
        if &current != expected {
            return Reply::StaleTopology { current };
        }
        let operation = match request {
            Request::Create {
                mode,
                name,
                layout,
                scale,
                ..
            } => {
                if status["enabled"] != true || status["recovery_ready"] != true {
                    return Reply::Unavailable;
                }
                let layout = match layout {
                    Layout::Exclusive => {
                        amaterasu_module_sdk::virtual_display::VirtualDisplayLayout::Exclusive
                    }
                    Layout::Extended => {
                        amaterasu_module_sdk::virtual_display::VirtualDisplayLayout::Extended
                    }
                    _ => return Reply::Unavailable,
                };
                Operation::Create {
                    hdr_enabled: mode.hdr,
                    identity: owner.identity.clone(),
                    width: mode.width,
                    height: mode.height,
                    refresh_millihz: mode.refresh_millihz,
                    name: name.clone(),
                    virtual_display_layout: Some(layout),
                    expected_topology_revision: Some(expected.revision),
                    expected_topology_domain_id: Some(expected.domain_id.clone()),
                    reference_id: Some(owner.reference_id.clone()),
                    dd_virtual_display_scale: *scale,
                }
            }
            Request::Remove { .. } => {
                match self.inspect(caller, owner) {
                    Reply::Effect { .. } => {}
                    other => return other,
                }
                Operation::Remove {
                    identity: owner.identity.clone(),
                    reference_id: Some(owner.reference_id.clone()),
                }
            }
            _ => return Reply::Unavailable,
        };
        let (result, admission) = self.backend.admit(caller, operation);
        self.admission = admission;
        let cleanup = matches!(request, Request::Remove { .. });
        if result["ok"] != true && !cleanup {
            return failure(&result);
        }
        let state = if result["ok"] != true {
            EffectState::Unknown
        } else if cleanup && Self::release_confirmed(&result) {
            EffectState::Completed
        } else {
            EffectState::Pending
        };
        let reply = Self::effect(state, Self::topology(&self.status(caller, None)));
        self.adapter.operations.insert(
            key,
            OperationRecord {
                request: request.clone(),
                reply: reply.clone(),
                cleanup,
            },
        );
        reply
    }
}
fn failure(value: &Value) -> Reply {
    match value["code"].as_str() {
        Some("invalid_request") => Reply::InvalidRequest,
        Some("not_found" | "reference_not_owned" | "borrow_invalid") => Reply::NotOwned,
        Some("resource_exhausted" | "monitor_limit" | "reference_limit" | "busy") => {
            Reply::ResourceExhausted
        }
        Some("identity_conflict" | "reference_conflict" | "ownership_conflict") => Reply::Conflict,
        _ => Reply::Unavailable,
    }
}
impl<B: Backend> Provider for Invocation<'_, B> {
    fn execute(&mut self, caller: &str, request: &Request) -> Reply {
        match request {
            Request::Capabilities => {
                let status = self.status(caller, None);
                let topology = Self::topology(&status);
                let create = topology.is_some()
                    && status["enabled"] == true
                    && status["recovery_ready"] == true;
                Reply::Capabilities {
                    capabilities: Capabilities {
                        create,
                        reconfigure: false,
                        borrow: false,
                        recovery: true,
                        topology,
                        layouts: if create {
                            vec![Layout::Exclusive, Layout::Extended]
                        } else {
                            vec![]
                        },
                    },
                }
            }
            Request::Inspect { owner } => self.inspect(caller, owner),
            Request::Create {
                owner,
                operation_id,
                expected,
                ..
            }
            | Request::Remove {
                owner,
                operation_id,
                expected,
            } => self.mutate(caller, request, owner, operation_id, expected),
            Request::Recover {
                owner,
                operation_id,
            } => self.recover(caller, owner, operation_id),
            Request::RetryCleanup {
                owner,
                operation_id,
                expected,
            } => self.retry_cleanup(caller, owner, operation_id, expected),
            Request::ReleaseOperation {
                owner,
                operation_id,
            } => {
                let key = (caller.to_owned(), operation_id.clone());
                let owned = self.adapter.operations.get(&key).is_some_and(|record| matches!(
                    &record.request, Request::Create { owner: original, .. } | Request::Remove { owner: original, .. } if original == owner));
                if !owned {
                    return Reply::NotOwned;
                }
                match self.recover(caller, owner, operation_id) {
                    Reply::Effect {
                        state:
                            EffectState::Completed
                            | EffectState::Applied
                            | EffectState::Absent
                            | EffectState::Removed,
                        ..
                    } => {
                        self.adapter.operations.remove(&key);
                        Reply::OperationReleased
                    }
                    _ => Reply::OperationNotSettled,
                }
            }
            _ => Reply::Unavailable,
        }
    }
    fn completion_lost(&mut self, caller: &str, request: &Request, _: &Reply) {
        let had_admission = self.admission.is_some();
        self.backend.completion_lost(self.admission.take());
        if let Request::Create { operation_id, .. } = request
            && had_admission
            && let Some(record) = self
                .adapter
                .operations
                .get_mut(&(caller.into(), operation_id.clone()))
        {
            record.cleanup = true;
            record.reply = Self::effect(EffectState::Unknown, None);
        }
    }
    fn shutdown(&mut self) { /* The owning module drops the manager, which retires its workers. */
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct FakeBackend {
        state: Option<&'static str>,
        reference_owned: bool,
        revision: u64,
        partial_failure: bool,
        shared: bool,
        remove_calls: usize,
    }
    impl FakeBackend {
        fn new() -> Self {
            Self {
                state: Some("ready"),
                reference_owned: true,
                revision: 7,
                partial_failure: false,
                shared: false,
                remove_calls: 0,
            }
        }
    }
    impl Backend for FakeBackend {
        type Admission = ();
        fn admit(&mut self, _: &str, operation: Operation) -> (Value, Option<()>) {
            match operation {
                Operation::Status { .. } => (
                    json!({"ok":true, "enabled":true, "recovery_ready":true,
                    "mutation_domain":{"id":"test-domain", "revision":self.revision},
                    "reference_owned":self.reference_owned,
                    "monitors":self.state.map(|state| vec![json!({"state":state})]).unwrap_or_default()}),
                    None,
                ),
                Operation::Remove { .. } => {
                    self.remove_calls += 1;
                    self.reference_owned = false;
                    if self.shared {
                        return (
                            json!({"ok":true,"removed":false,"reference_released":true,"reference_owned":false}),
                            None,
                        );
                    }
                    if self.partial_failure {
                        self.partial_failure = false;
                        (json!({"ok":false,"code":"busy"}), None)
                    } else {
                        self.state = Some("removing");
                        self.revision += 1;
                        (json!({"ok":true,"monitor":{"state":"removing"}}), None)
                    }
                }
                _ => (json!({"ok":false,"code":"unsupported"}), None),
            }
        }
        fn completion_lost(&mut self, _: Option<()>) {}
    }
    fn owner() -> OwnerReference {
        OwnerReference {
            identity: "display".into(),
            reference_id: "stream".into(),
        }
    }
    fn topology(revision: u64) -> TopologyRevision {
        TopologyRevision {
            domain_id: "test-domain".into(),
            revision,
        }
    }
    fn remove() -> Request {
        Request::Remove {
            owner: owner(),
            operation_id: "operation".into(),
            expected: topology(7),
        }
    }
    fn recover() -> Request {
        Request::Recover {
            owner: owner(),
            operation_id: "operation".into(),
        }
    }
    fn release() -> Request {
        Request::ReleaseOperation {
            owner: owner(),
            operation_id: "operation".into(),
        }
    }
    fn run(adapter: &mut Adapter, backend: &mut FakeBackend, request: &Request) -> Reply {
        adapter.invocation(backend).execute("alice", request)
    }
    fn has_state(reply: Reply, expected: EffectState) -> bool {
        matches!(reply, Reply::Effect { state, .. } if state == expected)
    }

    #[test]
    fn partial_failure_retains_cleanup_until_retry_and_confirmed_absence() {
        let mut backend = FakeBackend::new();
        backend.partial_failure = true;
        let mut adapter = Adapter::default();
        assert!(has_state(
            run(&mut adapter, &mut backend, &remove()),
            EffectState::Unknown
        ));
        assert_eq!(backend.remove_calls, 1);
        assert_eq!(
            run(&mut adapter, &mut backend, &release()),
            Reply::OperationNotSettled
        );
        assert!(has_state(
            run(&mut adapter, &mut backend, &recover()),
            EffectState::Unknown
        ));
        let retry = Request::RetryCleanup {
            owner: owner(),
            operation_id: "operation".into(),
            expected: topology(7),
        };
        assert!(has_state(
            run(&mut adapter, &mut backend, &retry),
            EffectState::Pending
        ));
        assert_eq!(backend.remove_calls, 2);
        assert_eq!(
            run(&mut adapter, &mut backend, &release()),
            Reply::OperationNotSettled
        );
        backend.state = Some("cleanup_failed");
        assert!(has_state(
            run(&mut adapter, &mut backend, &recover()),
            EffectState::Failed
        ));
        assert_eq!(
            run(&mut adapter, &mut backend, &release()),
            Reply::OperationNotSettled
        );
        backend.reference_owned = true; // A new owner share must not be removed by an old retry.
        let current_retry = Request::RetryCleanup {
            owner: owner(),
            operation_id: "operation".into(),
            expected: topology(8),
        };
        assert!(has_state(
            run(&mut adapter, &mut backend, &current_retry),
            EffectState::Unknown
        ));
        assert_eq!(backend.remove_calls, 2);
        backend.reference_owned = false;
        assert!(has_state(
            run(&mut adapter, &mut backend, &current_retry),
            EffectState::Pending
        ));
        assert_eq!(backend.remove_calls, 3);
        backend.state = None;
        assert!(has_state(
            run(&mut adapter, &mut backend, &recover()),
            EffectState::Completed
        ));
        assert_eq!(
            run(&mut adapter, &mut backend, &release()),
            Reply::OperationReleased
        );
        assert!(has_state(
            run(&mut adapter, &mut backend, &recover()),
            EffectState::Unknown
        ));
    }

    #[test]
    fn shared_reference_release_completes_without_removing_peer_output() {
        let mut backend = FakeBackend::new();
        backend.shared = true;
        let mut adapter = Adapter::default();
        assert!(has_state(
            run(&mut adapter, &mut backend, &remove()),
            EffectState::Completed
        ));
        assert_eq!(backend.state, Some("ready"));
        assert_eq!(
            run(&mut adapter, &mut backend, &release()),
            Reply::OperationReleased
        );
    }

    #[test]
    fn full_ledger_rejects_new_work_without_discarding_pending_cleanup() {
        let mut backend = FakeBackend::new();
        let mut adapter = Adapter::default();
        for index in 0..MAX_OPERATIONS {
            let request = Request::Remove {
                owner: owner(),
                operation_id: format!("old-{index}"),
                expected: topology(7),
            };
            adapter.operations.insert(
                ("alice".into(), format!("old-{index}")),
                OperationRecord {
                    request,
                    reply: Invocation::<FakeBackend>::effect(
                        EffectState::Pending,
                        Some(topology(7)),
                    ),
                    cleanup: true,
                },
            );
        }
        assert_eq!(
            run(&mut adapter, &mut backend, &remove()),
            Reply::ResourceExhausted
        );
        assert_eq!(backend.remove_calls, 0);
        assert_eq!(adapter.operations.len(), MAX_OPERATIONS);
    }

    #[test]
    fn inconsistent_absence_never_completes_cleanup() {
        let mut backend = FakeBackend::new();
        let mut adapter = Adapter::default();
        assert!(has_state(
            run(&mut adapter, &mut backend, &remove()),
            EffectState::Pending
        ));
        backend.state = None;
        backend.reference_owned = true;
        assert!(has_state(
            run(&mut adapter, &mut backend, &recover()),
            EffectState::Unknown
        ));
        assert_eq!(
            run(&mut adapter, &mut backend, &release()),
            Reply::OperationNotSettled
        );
    }

    #[test]
    fn shutdown_does_not_claim_unknown_cleanup_is_settled() {
        let mut backend = FakeBackend::new();
        let mut adapter = Adapter::default();
        assert!(has_state(
            run(&mut adapter, &mut backend, &remove()),
            EffectState::Pending
        ));
        assert_eq!(
            run(&mut adapter, &mut backend, &release()),
            Reply::OperationNotSettled
        );
        drop(adapter);
        let mut restarted = Adapter::default();
        assert!(has_state(
            run(&mut restarted, &mut backend, &recover()),
            EffectState::Unknown
        ));
        assert_eq!(backend.state, Some("removing"));
        assert_eq!(backend.remove_calls, 1);
    }
}
