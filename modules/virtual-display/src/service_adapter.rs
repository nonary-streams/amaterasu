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

/// Whether this request changed, or may have changed, owned display state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutationEffect {
    Unchanged,
    Attempted,
}

pub struct MutationResult<A> {
    pub response: Value,
    pub admission: Option<A>,
    pub effect: MutationEffect,
    /// Confirmed release of one shared reference, after persistence succeeded.
    pub confirmed_shared_release: Option<OwnerReference>,
}

/// Manager-owned classification, independent of human-readable error codes.
/// `Rejected` guarantees no ownership or display mutation was attempted;
/// `Attempted` may have changed state even when its response is an error.
pub enum RemovalOutcome {
    Rejected(Value),
    #[cfg_attr(
        not(amaterasu_libvirtualdisplay),
        allow(
            dead_code,
            reason = "only the topology-capable Linux manager admits typed removal"
        )
    )]
    Unchanged(Value),
    #[cfg_attr(
        all(not(amaterasu_libvirtualdisplay), not(test)),
        expect(
            dead_code,
            reason = "only the topology-capable Linux manager attempts typed removal"
        )
    )]
    Attempted(Value),
}
impl RemovalOutcome {
    #[cfg(amaterasu_libvirtualdisplay)]
    pub fn into_response(self) -> Value {
        match self {
            Self::Rejected(value) | Self::Unchanged(value) | Self::Attempted(value) => value,
        }
    }
}

pub trait Backend {
    type Admission;
    fn admit(&mut self, caller: &str, operation: Operation) -> (Value, Option<Self::Admission>);
    fn completion_lost(&mut self, admission: Option<Self::Admission>);
    fn admit_with_effect(
        &mut self,
        caller: &str,
        operation: Operation,
    ) -> MutationResult<Self::Admission>;
    fn remove_owned(&mut self, caller: &str, owner: &OwnerReference) -> RemovalOutcome;
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
    fn admit_with_effect(
        &mut self,
        caller: &str,
        operation: Operation,
    ) -> MutationResult<Self::Admission> {
        self.admit_with_effect(caller, operation)
    }
    fn remove_owned(&mut self, _: &str, _: &OwnerReference) -> RemovalOutcome {
        RemovalOutcome::Rejected(crate::model::failure("unsupported_topology_domain"))
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
    fn admit_with_effect(
        &mut self,
        caller: &str,
        operation: Operation,
    ) -> MutationResult<Self::Admission> {
        self.admit_with_effect(caller, operation)
    }
    fn remove_owned(&mut self, caller: &str, owner: &OwnerReference) -> RemovalOutcome {
        self.admit_removal(caller, &owner.identity, &owner.reference_id)
    }
}

#[derive(Clone)]
struct OperationRecord {
    request: Request,
    reply: Reply,
    cleanup: bool,
    creation_fenced: bool,
    fenced_by_removal: Option<String>,
}

impl OperationRecord {
    /// Confirmed absence or removal is final for this operation's epoch.
    fn is_terminal(&self) -> bool {
        matches!(
            self.reply,
            Reply::Effect {
                state: EffectState::Completed | EffectState::Removed | EffectState::Absent,
                ..
            }
        )
    }
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
    /// Conservatively fence an older create when another owner-scoped request
    /// could replace its reference. All legacy calls pass through this hook.
    pub fn observe_mutation(
        &mut self,
        caller: &str,
        identity: &str,
        reference: Option<&str>,
        except: Option<&str>,
        removal_id: Option<&str>,
    ) -> Vec<String> {
        let mut newly_fenced = Vec::new();
        for ((principal, operation_id), record) in &mut self.operations {
            if principal != caller
                || except == Some(operation_id.as_str())
                || record.cleanup
                || record.creation_fenced
                || record.is_terminal()
            {
                continue;
            }
            if let Request::Create { owner, .. } = &record.request
                && owner.identity == identity
                && reference.is_none_or(|id| id == owner.reference_id)
            {
                record.creation_fenced = true;
                record.fenced_by_removal = removal_id.map(str::to_owned);
                newly_fenced.push(operation_id.clone());
            }
        }
        newly_fenced
    }

    fn mark_absent(record: &mut OperationRecord) {
        record.reply = Reply::Effect {
            state: EffectState::Absent,
            topology: None,
            observation: ModeObservation::NotObserved,
        };
        record.creation_fenced = false;
        record.fenced_by_removal = None;
    }

    // Only the removal that fenced a record may settle it. Older unrelated
    // uncertainty is not resolved by releasing a newer reference epoch.
    fn settle_fenced_creations(&mut self, caller: &str, owner: &OwnerReference, removal_id: &str) {
        for ((principal, _), record) in &mut self.operations {
            if principal == caller
                && record.creation_fenced
                && !record.cleanup
                && record.fenced_by_removal.as_deref() == Some(removal_id)
                && matches!(&record.request, Request::Create { owner: original, .. } if original == owner)
            {
                Self::mark_absent(record);
            }
        }
    }

    /// Called synchronously for a manager-confirmed shared-reference release.
    /// The IDs were fenced by this same invocation, before any later request.
    pub fn settle_legacy_shared_release(
        &mut self,
        caller: &str,
        owner: &OwnerReference,
        newly_fenced: &[String],
    ) {
        for operation_id in newly_fenced {
            if let Some(record) = self
                .operations
                .get_mut(&(caller.to_owned(), operation_id.clone()))
                && record.creation_fenced
                && record.fenced_by_removal.is_none()
                && !record.cleanup
                && matches!(&record.request, Request::Create { owner: original, .. } if original == owner)
            {
                Self::mark_absent(record);
            }
        }
    }

    pub fn observe_legacy_mutation(&mut self, caller: &str, operation: &Operation) -> Vec<String> {
        match operation {
            Operation::Create {
                identity,
                reference_id,
                ..
            }
            | Operation::Remove {
                identity,
                reference_id,
            } => self.observe_mutation(caller, identity, reference_id.as_deref(), None, None),
            // Reconfiguration can replace the output shared by all references.
            Operation::Reconfigure { identity, .. }
            | Operation::ReconfigureModeOnly { identity, .. } => {
                self.observe_mutation(caller, identity, None, None, None)
            }
            _ => Vec::new(),
        }
    }

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
            || (result["reference_released"] == true && result["reference_owned"] == false)
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

    fn creation_status(&mut self, caller: &str, request: &Request) -> Reply {
        let Request::Create {
            owner,
            expected,
            mode,
            name,
            layout,
            scale,
            ..
        } = request
        else {
            return Self::effect(EffectState::Unknown, None);
        };
        let status = self.status(caller, Some(owner));
        let topology = Self::topology(&status);
        let valid_domain = topology.as_ref().is_some_and(|current| {
            current.domain_id == expected.domain_id && current.revision >= expected.revision
        });
        let Some(monitors) = status["monitors"].as_array() else {
            return Self::effect(EffectState::Unknown, topology);
        };
        if status["ok"] != true
            || status["recovery_ready"] != true
            || !valid_domain
            || status["reference_owned"] != true
            || monitors.len() != 1
        {
            return Self::effect(EffectState::Unknown, topology);
        }
        let monitor = &monitors[0];
        let specification = &monitor["specification"];
        let expected_layout = match layout {
            Layout::Exclusive => "exclusive",
            Layout::Extended => "extended",
            _ => return Self::effect(EffectState::Unknown, topology),
        };
        if specification["identity"] != owner.identity
            || specification["name"] != *name
            || specification["width"] != mode.width
            || specification["height"] != mode.height
            || specification["refresh_millihz"] != mode.refresh_millihz
            || specification["hdr_enabled"] != mode.hdr
            || monitor["virtual_display_layout"] != expected_layout
            || scale.is_some_and(|value| monitor["dd_virtual_display_scale"] != value)
        {
            return Self::effect(EffectState::Unknown, topology);
        }
        let state = match monitor["state"].as_str() {
            Some("ready") => EffectState::Applied,
            Some("creating") => EffectState::Pending,
            Some("cleanup_failed") => EffectState::Failed,
            _ => EffectState::Unknown,
        };
        Self::effect(state, topology)
    }

    fn fenced_creation_status(&mut self, caller: &str, request: &Request) -> Reply {
        let Request::Create {
            owner, expected, ..
        } = request
        else {
            return Self::effect(EffectState::Unknown, None);
        };
        let status = self.status(caller, Some(owner));
        let topology = Self::topology(&status);
        let same_domain = topology.as_ref().is_some_and(|current| {
            current.domain_id == expected.domain_id && current.revision >= expected.revision
        });
        let settled = status["ok"] == true
            && status["recovery_ready"] == true
            && same_domain
            && status["reference_owned"] == false
            && status["monitors"].as_array().is_some_and(Vec::is_empty)
            && status["mutation_domain"]["pending_mutations"].as_u64() == Some(0);
        Self::effect(
            if settled {
                EffectState::Absent
            } else {
                EffectState::Unknown
            },
            topology,
        )
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
        if record.is_terminal() {
            return record.reply;
        }
        if record.creation_fenced && !record.cleanup {
            let reply = self.fenced_creation_status(caller, &record.request);
            if matches!(
                reply,
                Reply::Effect {
                    state: EffectState::Absent,
                    ..
                }
            ) && let Some(saved) = self.adapter.operations.get_mut(&key)
            {
                saved.reply = reply.clone();
                saved.creation_fenced = false;
                saved.fenced_by_removal = None;
            }
            return reply;
        }
        let reply = match &record.reply {
            _ if record.cleanup => self.cleanup_status(caller, owner),
            Reply::Effect {
                state:
                    EffectState::Pending
                    | EffectState::Unknown
                    | EffectState::Failed
                    | EffectState::Applied,
                ..
            } => self.creation_status(caller, &record.request),
            _ => record.reply.clone(),
        };
        if let Some(saved) = self.adapter.operations.get_mut(&key) {
            saved.reply = reply.clone();
        }
        if record.cleanup
            && matches!(
                reply,
                Reply::Effect {
                    state: EffectState::Completed,
                    ..
                }
            )
        {
            self.adapter
                .settle_fenced_creations(caller, owner, operation_id);
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
        let result = match self.backend.remove_owned(caller, owner) {
            RemovalOutcome::Rejected(result) => return failure(&result),
            RemovalOutcome::Unchanged(result) => result,
            RemovalOutcome::Attempted(result) => {
                self.adapter.observe_mutation(
                    caller,
                    &owner.identity,
                    Some(&owner.reference_id),
                    Some(operation_id),
                    Some(operation_id),
                );
                result
            }
        };
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
        if state == EffectState::Completed {
            self.adapter
                .settle_fenced_creations(caller, owner, operation_id);
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
        let cleanup = matches!(request, Request::Remove { .. });
        let (result, effect) = if cleanup {
            match self.backend.remove_owned(caller, owner) {
                RemovalOutcome::Rejected(result) => return failure(&result),
                RemovalOutcome::Unchanged(result) => (result, MutationEffect::Unchanged),
                RemovalOutcome::Attempted(result) => (result, MutationEffect::Attempted),
            }
        } else {
            let outcome = self.backend.admit_with_effect(caller, operation);
            self.admission = outcome.admission;
            (outcome.response, outcome.effect)
        };
        if effect == MutationEffect::Attempted {
            self.adapter.observe_mutation(
                caller,
                &owner.identity,
                Some(&owner.reference_id),
                Some(operation_id),
                cleanup.then_some(operation_id),
            );
        }
        let uncertain_create =
            !cleanup && effect == MutationEffect::Attempted && result["ok"] != true;
        if result["ok"] != true && !cleanup && !uncertain_create {
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
                creation_fenced: uncertain_create,
                fenced_by_removal: None,
            },
        );
        if cleanup && state == EffectState::Completed {
            self.adapter
                .settle_fenced_creations(caller, owner, operation_id);
        }
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
        if had_admission
            && let Request::Create {
                owner,
                operation_id,
                ..
            } = request
        {
            self.adapter.observe_mutation(
                caller,
                &owner.identity,
                Some(&owner.reference_id),
                Some(operation_id),
                None,
            );
        }
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
        shared_release_confirmed: bool,
        uncertain_create: bool,
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
                shared_release_confirmed: true,
                uncertain_create: false,
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
                    "mutation_domain":{"id":"test-domain", "revision":self.revision, "pending_mutations":0},
                    "reference_owned":self.reference_owned,
                    "monitors":self.state.map(|state| vec![json!({"state":state,"specification":{"identity":"display","name":"Display","width":1920,"height":1080,"refresh_millihz":60000,"hdr_enabled":false},"virtual_display_layout":"extended","dd_virtual_display_scale":0})]).unwrap_or_default()}),
                    None,
                ),
                Operation::Remove { .. } => {
                    self.remove_calls += 1;
                    self.reference_owned = false;
                    if self.shared {
                        return (
                            json!({"ok":true,"removed":false,"reference_released":self.shared_release_confirmed,"reference_owned":false}),
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
        fn admit_with_effect(&mut self, caller: &str, operation: Operation) -> MutationResult<()> {
            if self.uncertain_create && matches!(operation, Operation::Create { .. }) {
                return MutationResult {
                    response: json!({"ok":false,"code":"journal_unavailable"}),
                    admission: None,
                    effect: MutationEffect::Attempted,
                    confirmed_shared_release: None,
                };
            }
            let (response, admission) = self.admit(caller, operation);
            MutationResult {
                response,
                admission,
                effect: MutationEffect::Unchanged,
                confirmed_shared_release: None,
            }
        }
        fn remove_owned(&mut self, caller: &str, owner: &OwnerReference) -> RemovalOutcome {
            let (result, _) = self.admit(
                caller,
                Operation::Remove {
                    identity: owner.identity.clone(),
                    reference_id: Some(owner.reference_id.clone()),
                },
            );
            RemovalOutcome::Attempted(result)
        }
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

    fn seed_create(
        adapter: &mut Adapter,
        principal: &str,
        id: &str,
        owner: OwnerReference,
        fenced: bool,
    ) {
        let request = Request::Create {
            owner,
            operation_id: id.into(),
            expected: topology(7),
            mode: amaterasu_module_sdk::virtual_display_service::Mode {
                width: 1920,
                height: 1080,
                refresh_millihz: 60000,
                hdr: false,
            },
            acceptance: amaterasu_module_sdk::virtual_display_service::ModeAcceptance {
                max_refresh_delta_ppm: 0,
                require_hdr_match: true,
            },
            name: "Display".into(),
            layout: Layout::Extended,
            scale: Some(0),
        };
        adapter.operations.insert(
            (principal.into(), id.into()),
            OperationRecord {
                request,
                reply: Invocation::<FakeBackend>::effect(EffectState::Unknown, None),
                cleanup: false,
                creation_fenced: fenced,
                fenced_by_removal: None,
            },
        );
    }

    #[test]
    fn legacy_shared_release_settles_only_its_new_fences() {
        let mut adapter = Adapter::default();
        let peer = OwnerReference {
            identity: "display".into(),
            reference_id: "peer".into(),
        };
        seed_create(&mut adapter, "alice", "older-uncertain", owner(), true);
        seed_create(&mut adapter, "alice", "current", owner(), false);
        seed_create(&mut adapter, "alice", "peer", peer, false);
        seed_create(&mut adapter, "bob", "other-owner", owner(), false);
        let fenced = adapter.observe_legacy_mutation(
            "alice",
            &Operation::Remove {
                identity: "display".into(),
                reference_id: Some("stream".into()),
            },
        );
        assert_eq!(fenced, vec!["current".to_string()]);
        adapter.settle_legacy_shared_release("alice", &owner(), &fenced);
        assert!(adapter.operations[&("alice".into(), "older-uncertain".into())].creation_fenced);
        assert!(matches!(
            adapter.operations[&("alice".into(), "current".into())].reply,
            Reply::Effect {
                state: EffectState::Absent,
                ..
            }
        ));
        assert!(!adapter.operations[&("alice".into(), "peer".into())].creation_fenced);
        assert!(!adapter.operations[&("bob".into(), "other-owner".into())].creation_fenced);
    }

    #[test]
    fn typed_shared_release_preserves_earlier_uncertainty_and_peer() {
        let mut backend = FakeBackend::new();
        backend.shared = true;
        let mut adapter = Adapter::default();
        seed_create(&mut adapter, "alice", "older-uncertain", owner(), true);
        seed_create(&mut adapter, "alice", "current", owner(), false);
        seed_create(
            &mut adapter,
            "alice",
            "peer",
            OwnerReference {
                identity: "display".into(),
                reference_id: "peer".into(),
            },
            false,
        );
        assert!(has_state(
            run(&mut adapter, &mut backend, &remove()),
            EffectState::Completed
        ));
        assert!(adapter.operations[&("alice".into(), "older-uncertain".into())].creation_fenced);
        assert!(matches!(
            adapter.operations[&("alice".into(), "current".into())].reply,
            Reply::Effect {
                state: EffectState::Absent,
                ..
            }
        ));
        assert!(!adapter.operations[&("alice".into(), "peer".into())].creation_fenced);
    }

    #[test]
    fn typed_shared_remove_without_release_confirmation_does_not_settle() {
        let mut backend = FakeBackend::new();
        backend.shared = true;
        backend.shared_release_confirmed = false;
        let mut adapter = Adapter::default();
        seed_create(&mut adapter, "alice", "current", owner(), false);
        assert!(has_state(
            run(&mut adapter, &mut backend, &remove()),
            EffectState::Pending
        ));
        assert!(adapter.operations[&("alice".into(), "current".into())].creation_fenced);
        assert_eq!(
            run(&mut adapter, &mut backend, &release()),
            Reply::OperationNotSettled
        );
    }

    #[test]
    fn reference_mutation_fences_old_create_even_if_status_looks_ready() {
        let mut backend = FakeBackend::new();
        let mut adapter = Adapter::default();
        let request = Request::Create {
            owner: owner(),
            operation_id: "create".into(),
            expected: topology(7),
            mode: amaterasu_module_sdk::virtual_display_service::Mode {
                width: 1920,
                height: 1080,
                refresh_millihz: 60000,
                hdr: false,
            },
            acceptance: amaterasu_module_sdk::virtual_display_service::ModeAcceptance {
                max_refresh_delta_ppm: 0,
                require_hdr_match: true,
            },
            name: "Display".into(),
            layout: Layout::Extended,
            scale: Some(0),
        };
        adapter.operations.insert(
            ("alice".into(), "create".into()),
            OperationRecord {
                request,
                reply: Invocation::<FakeBackend>::effect(EffectState::Unknown, None),
                cleanup: false,
                creation_fenced: false,
                fenced_by_removal: None,
            },
        );
        let recover = Request::Recover {
            owner: owner(),
            operation_id: "create".into(),
        };
        adapter.observe_legacy_mutation(
            "bob",
            &Operation::Remove {
                identity: "display".into(),
                reference_id: Some("stream".into()),
            },
        );
        assert!(has_state(
            run(&mut adapter, &mut backend, &recover),
            EffectState::Applied
        ));
        adapter.observe_legacy_mutation(
            "alice",
            &Operation::Remove {
                identity: "display".into(),
                reference_id: Some("stream".into()),
            },
        );
        assert!(has_state(
            run(&mut adapter, &mut backend, &recover),
            EffectState::Unknown
        ));
        assert_eq!(
            run(
                &mut adapter,
                &mut backend,
                &Request::ReleaseOperation {
                    owner: owner(),
                    operation_id: "create".into()
                }
            ),
            Reply::OperationNotSettled
        );
        assert!(has_state(
            run(&mut adapter, &mut backend, &remove()),
            EffectState::Pending
        ));
        backend.state = None;
        assert!(has_state(
            run(
                &mut adapter,
                &mut backend,
                &Request::Recover {
                    owner: owner(),
                    operation_id: "operation".into()
                }
            ),
            EffectState::Completed
        ));
        assert_eq!(
            run(
                &mut adapter,
                &mut backend,
                &Request::ReleaseOperation {
                    owner: owner(),
                    operation_id: "create".into()
                }
            ),
            Reply::OperationReleased
        );
    }

    #[test]
    fn uncertain_create_fences_older_reference_and_retains_unknown_record() {
        let mut backend = FakeBackend::new();
        backend.uncertain_create = true;
        let mut adapter = Adapter::default();
        let request = Request::Create {
            owner: owner(),
            operation_id: "old-create".into(),
            expected: topology(7),
            mode: amaterasu_module_sdk::virtual_display_service::Mode {
                width: 1920,
                height: 1080,
                refresh_millihz: 60000,
                hdr: false,
            },
            acceptance: amaterasu_module_sdk::virtual_display_service::ModeAcceptance {
                max_refresh_delta_ppm: 0,
                require_hdr_match: true,
            },
            name: "Display".into(),
            layout: Layout::Extended,
            scale: Some(0),
        };
        adapter.operations.insert(
            ("alice".into(), "old-create".into()),
            OperationRecord {
                request: request.clone(),
                reply: Invocation::<FakeBackend>::effect(EffectState::Pending, None),
                cleanup: false,
                creation_fenced: false,
                fenced_by_removal: None,
            },
        );
        let mut next = request;
        if let Request::Create { operation_id, .. } = &mut next {
            *operation_id = "uncertain-create".into();
        }
        assert!(has_state(
            run(&mut adapter, &mut backend, &next),
            EffectState::Unknown
        ));
        for operation_id in ["old-create", "uncertain-create"] {
            assert!(has_state(
                run(
                    &mut adapter,
                    &mut backend,
                    &Request::Recover {
                        owner: owner(),
                        operation_id: operation_id.into()
                    }
                ),
                EffectState::Unknown
            ));
            assert_eq!(
                run(
                    &mut adapter,
                    &mut backend,
                    &Request::ReleaseOperation {
                        owner: owner(),
                        operation_id: operation_id.into()
                    }
                ),
                Reply::OperationNotSettled
            );
        }
    }

    #[test]
    fn legacy_reconfiguration_fences_every_reference_to_the_output() {
        let mut backend = FakeBackend::new();
        let mut adapter = Adapter::default();
        let request = Request::Create {
            owner: owner(),
            operation_id: "create".into(),
            expected: topology(7),
            mode: amaterasu_module_sdk::virtual_display_service::Mode {
                width: 1920,
                height: 1080,
                refresh_millihz: 60000,
                hdr: false,
            },
            acceptance: amaterasu_module_sdk::virtual_display_service::ModeAcceptance {
                max_refresh_delta_ppm: 0,
                require_hdr_match: true,
            },
            name: "Display".into(),
            layout: Layout::Extended,
            scale: Some(0),
        };
        adapter.operations.insert(
            ("alice".into(), "create".into()),
            OperationRecord {
                request,
                reply: Invocation::<FakeBackend>::effect(EffectState::Unknown, None),
                cleanup: false,
                creation_fenced: false,
                fenced_by_removal: None,
            },
        );
        adapter.observe_legacy_mutation(
            "alice",
            &Operation::Reconfigure {
                operation_id: "reconfigure".into(),
                identity: "display".into(),
                reference_id: "another-reference".into(),
                borrow_id: "borrow".into(),
                width: 1920,
                height: 1080,
                refresh_millihz: 60000,
                expected_topology_domain_id: "test-domain".into(),
                expected_topology_revision: 7,
            },
        );
        assert!(has_state(
            run(
                &mut adapter,
                &mut backend,
                &Request::Recover {
                    owner: owner(),
                    operation_id: "create".into()
                }
            ),
            EffectState::Unknown
        ));
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
                    creation_fenced: false,
                    fenced_by_removal: None,
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

#[cfg(test)]
mod invariants;
