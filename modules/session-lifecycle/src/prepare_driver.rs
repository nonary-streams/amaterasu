//! Bounded provider prepare orchestration. Resource authority is supplied by the
//! coordinator and must hold through callback settlement and status polling.
//!
//! A failed or rejected callback does not prove that its effects were absent or
//! cleaned up. Its admission remains fenced for recovery. The current protocol
//! cannot settle a known partial preparation: that requires explicit enrollment
//! of its scoped cleanup obligations. Recovery must reconcile the saved receipt,
//! not replay the preparation after a restart.
use amaterasu_extension_api::{
    self as api, ProviderOperation, ProviderRequest, ProviderResponse, ProviderStatusRequest,
    ResourcePlan,
};
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::adapter::{LifecycleSettings, MethodSettings, ParticipantSettings};
use crate::provider_dispatch::{
    DispatchJournal, DispatchRecord, DispatchState, ProviderCall, ProviderDispatcher,
};

const MAX_STATUS_POLLS: u32 = 32;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PreparePhase {
    Plan,
    Callback,
    PlanStatus,
    CallbackStatus,
    /// A validated v2 plan awaits independently acknowledged owner controls.
    ControlsPending,
    /// Fresh owner finalization plan is durably admitted; no effect was sent.
    FinalizePlanAdmitted,
    Prepared,
    CleanupSettled,
    Rejected,
    RecoveryRequired,
}

/// Everything needed for recovery; an unknown in-flight dispatch is fenced.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrepareState {
    pub provider: String,
    pub participant: ParticipantSettings,
    pub request: ProviderRequest,
    pub phase: PreparePhase,
    pub plan: Option<ResourcePlan>,
    pub admission_id: Option<String>,
    pub pending_receipt: Option<String>,
    /// Original broker-completed callback that returned the Pending receipt.
    #[serde(default)]
    pub pending_dispatch: Option<DispatchRecord>,
    pub status_polls: u32,
    pub last_sequence: u64,
    pub dispatch: Option<DispatchRecord>,
    /// Exact broker-completed callback/status evidence retained for output provenance.
    #[serde(default)]
    pub terminal_dispatch: Option<DispatchRecord>,
    pub response: Option<ProviderResponse>,
}

/// The owner must atomically commit the entire state with all cleanup obligations.
pub trait PrepareJournal {
    fn persist(&mut self, state: &PrepareState) -> ModuleResult<()>;
}

/// Implemented only by the authority owning trusted observations, grants,
/// sequence allocation, domain locks and durable admission. None of these may
/// be inferred from a provider response or a caller-provided identifier.
pub trait PrepareAuthority {
    /// Atomically allocate and persist a fresh provider-scoped sequence and unique
    /// operation ID; never reuse either for a semantically different request.
    fn allocate(&mut self, provider: &str) -> ModuleResult<(u64, String)>;
    /// Allocate a unique broker transport operation ID without advancing the
    /// outstanding provider effect sequence.
    fn allocate_poll_id(&mut self, provider: &str) -> ModuleResult<String>;
    /// Resolve installed descriptor/schema revision and exact broker approval for
    /// this provider/method. The dispatcher independently rechecks the approval.
    fn approved_call(
        &mut self,
        participant: &ParticipantSettings,
        method: &MethodSettings,
        arguments: Value,
        operation_id: String,
    ) -> ModuleResult<ProviderCall>;
    /// Validate the plan against complete fresh observation and grants, acquire
    /// all required domain rights and return a fresh admission ID. Rights must
    /// remain held until the callback and any status work is settled or fenced.
    fn admit_plan(
        &mut self,
        participant: &ParticipantSettings,
        plan: &ResourcePlan,
    ) -> ModuleResult<String>;
    /// Revalidate the exact plan under the still-held rights before each callback
    /// dispatcher tick; false or unavailable means no callback may be submitted.
    fn validate_rights(
        &mut self,
        participant: &ParticipantSettings,
        plan: &ResourcePlan,
        admission_id: &str,
    ) -> ModuleResult<()>;
    /// Whether another admitted operation still holds a mutation domain this
    /// plan needs. The driver waits for it instead of failing the operation.
    fn domains_reserved(&self, _plan: &ResourcePlan) -> bool {
        false
    }
}

/// `admit_plan` reports this when another admitted operation holds a domain.
/// It is contention between two valid operations, never a provider failure.
pub(crate) const MUTATION_DOMAIN_RESERVED: &str = "mutation domain is reserved";

/// How long one provider step waits for a reserved mutation domain. A release
/// callback normally completes well inside this; past it the contention is
/// reported like any other admission failure.
const DOMAIN_WAIT_LIMIT: std::time::Duration = std::time::Duration::from_secs(20);

/// A plan that could not be admitted because its mutation domain was reserved.
/// Nothing was admitted or sent for it, so it is discarded and planned again
/// once the domain frees; the provider's revision will have moved by then.
struct DomainWait {
    plan: ResourcePlan,
    since: std::time::Instant,
}

pub struct PrepareDriver {
    state: PrepareState,
    dispatcher: Option<ProviderDispatcher>,
    journal_failed: bool,
    allow_controls: bool,
    domain_wait: Option<DomainWait>,
    domain_wait_limit: std::time::Duration,
}

impl PrepareDriver {
    /// Caller must durably reserve the operation and matching provider sequence
    /// before constructing this driver. No plan or effect is implied by creation.
    pub fn new(state: PrepareState, settings: &LifecycleSettings) -> ModuleResult<Self> {
        Self::new_inner(state, settings, false, false, false, false)
    }

    /// Only the coordinator may call this after validating the pinned owner,
    /// current output and all durable owner-settled control results. This driver
    /// admits the fresh finalize plan and stops before any owner effect.
    #[allow(dead_code)] // The coordinator must wire the complete v2 gates first.
    pub(crate) fn new_finalize_plan(
        state: PrepareState,
        settings: &LifecycleSettings,
    ) -> ModuleResult<Self> {
        Self::new_inner(state, settings, true, false, false, false)
    }

    /// Coordinator-only v2 producer prepare with no dependency controls. The
    /// caller must first pin the installed single-owner graph and policies.
    #[allow(dead_code)] // Only enabled after the complete standalone path is wired.
    pub(crate) fn new_standalone_prepare(
        state: PrepareState,
        settings: &LifecycleSettings,
    ) -> ModuleResult<Self> {
        Self::new_inner(state, settings, false, true, false, false)
    }

    /// Release requires the coordinator's exact consumer-retirement and
    /// retained-lineage checks before this driver is constructed.
    pub(crate) fn new_standalone_release(
        state: PrepareState,
        settings: &LifecycleSettings,
    ) -> ModuleResult<Self> {
        Self::new_inner(state, settings, false, false, true, false)
    }

    /// Used only after the coordinator freezes the direct dependency graph and
    /// installed control selections; pending controls never invoke a callback.
    #[allow(dead_code)]
    pub(crate) fn new_composed(
        state: PrepareState,
        settings: &LifecycleSettings,
    ) -> ModuleResult<Self> {
        Self::new_inner(state, settings, false, false, false, true)
    }

    fn new_inner(
        state: PrepareState,
        settings: &LifecycleSettings,
        finalize_plan: bool,
        standalone_prepare: bool,
        standalone_release: bool,
        composed: bool,
    ) -> ModuleResult<Self> {
        // Regular v2 prepare/control remains fenced until all stages are wired.
        if (!finalize_plan
            && !standalone_prepare
            && !standalone_release
            && !composed
            && state.request.version != 1)
            || (standalone_prepare
                && (state.request.version != api::COMPOSITION_VERSION
                    || !state.request.control_results.is_empty()
                    || match state.request.phase {
                        api::LifecyclePhase::SessionPrepare => {
                            !state.request.dependency_inputs.is_empty()
                        }
                        api::LifecyclePhase::StreamPrepare => {
                            state.request.dependency_inputs.len() != 1
                        }
                        _ => true,
                    }
                    || !matches!(
                        state.request.operation,
                        ProviderOperation::ResourcePlan | ProviderOperation::LifecycleCallback
                    )
                    || !matches!(
                        state.phase,
                        PreparePhase::Plan
                            | PreparePhase::PlanStatus
                            | PreparePhase::Callback
                            | PreparePhase::CallbackStatus
                            | PreparePhase::Prepared
                            | PreparePhase::Rejected
                            | PreparePhase::RecoveryRequired
                    )))
            || (standalone_release
                && (state.request.version != api::COMPOSITION_VERSION
                    || state.request.dependency_inputs.len() != 1
                    || !state.request.control_results.is_empty()
                    || !matches!(
                        state.request.operation,
                        ProviderOperation::ResourcePlan | ProviderOperation::LifecycleCallback
                    )
                    || !matches!(
                        state.request.phase,
                        api::LifecyclePhase::SessionRelease | api::LifecyclePhase::StreamRelease
                    )
                    || !matches!(
                        state.phase,
                        PreparePhase::Plan
                            | PreparePhase::PlanStatus
                            | PreparePhase::Callback
                            | PreparePhase::CallbackStatus
                            | PreparePhase::CleanupSettled
                            | PreparePhase::Rejected
                            | PreparePhase::RecoveryRequired
                    )))
            || (composed
                && (state.request.version != api::COMPOSITION_VERSION
                    || !matches!(
                        state.request.operation,
                        ProviderOperation::ResourcePlan | ProviderOperation::LifecycleCallback
                    )
                    || !matches!(
                        state.phase,
                        PreparePhase::Plan
                            | PreparePhase::PlanStatus
                            | PreparePhase::ControlsPending
                            | PreparePhase::Callback
                            | PreparePhase::CallbackStatus
                            | PreparePhase::Prepared
                            | PreparePhase::CleanupSettled
                            | PreparePhase::Rejected
                            | PreparePhase::RecoveryRequired
                    )))
            || (finalize_plan
                && (state.request.version != api::COMPOSITION_VERSION
                    || state.request.operation != ProviderOperation::ResourceFinalizePlan
                    || !matches!(
                        state.phase,
                        PreparePhase::Plan
                            | PreparePhase::PlanStatus
                            | PreparePhase::FinalizePlanAdmitted
                            | PreparePhase::Rejected
                            | PreparePhase::RecoveryRequired
                    )))
        {
            return Err("provider composition is not supported".into());
        }
        if (finalize_plan || standalone_prepare || standalone_release || composed)
            && (state.participant.contract_version != api::COMPOSITION_VERSION
                || state.participant.plan_method.interface_major != api::COMPOSITION_VERSION
                || state.participant.callback_method.interface_major != api::COMPOSITION_VERSION
                || state.participant.status_method.interface_major != api::COMPOSITION_VERSION
                || state
                    .participant
                    .binding_validate_method
                    .as_ref()
                    .is_some_and(|method| method.interface_major != api::COMPOSITION_VERSION))
        {
            return Err("composition_method_version_mismatch".into());
        }
        if participant(settings, &state.provider)? != &state.participant
            || state.request.settings_revision != state.participant.settings_revision
            || serde_json::to_value(&state.request.settings).map_err(|e| e.to_string())?
                != state.participant.settings
        {
            return Err("installed participant settings changed".into());
        }
        api::validate_provider_request(&state.request)?;
        if (matches!(state.phase, PreparePhase::Plan | PreparePhase::PlanStatus)
            && state.request.operation != ProviderOperation::ResourcePlan
            && state.request.operation != ProviderOperation::ResourceFinalizePlan)
            || state.request.provider_sequence == 0
            || state.request.provider_sequence > state.last_sequence
            || state.status_polls > MAX_STATUS_POLLS
        {
            return Err("invalid prepare state".into());
        }
        let dispatcher = state
            .dispatch
            .clone()
            .map(ProviderDispatcher::new)
            .transpose()?;
        Ok(Self {
            state,
            dispatcher,
            journal_failed: false,
            allow_controls: composed,
            domain_wait: None,
            domain_wait_limit: DOMAIN_WAIT_LIMIT,
        })
    }

    #[cfg(test)]
    pub(crate) fn set_domain_wait_limit(&mut self, limit: std::time::Duration) {
        self.domain_wait_limit = limit;
    }

    /// True while this step is only waiting for another operation's domain.
    #[cfg(test)]
    pub(crate) fn waiting_for_domain(&self) -> bool {
        self.domain_wait.is_some()
    }
    pub fn state(&self) -> &PrepareState {
        &self.state
    }

    /// At most one broker command per call. Persisted unknown operations never replay.
    pub fn advance(
        &mut self,
        settings: &LifecycleSettings,
        authority: &mut impl PrepareAuthority,
        journal: &mut impl PrepareJournal,
        send: &mut (impl FnMut(Command) -> ModuleResult<Response> + ?Sized),
    ) -> ModuleResult<()> {
        if self.journal_failed {
            return Err("prepare journal requires recovery".into());
        }
        let participant = participant(settings, &self.state.provider)?;
        if participant != &self.state.participant {
            return Err("installed participant settings changed".into());
        }
        if matches!(
            self.state.phase,
            PreparePhase::Prepared
                | PreparePhase::ControlsPending
                | PreparePhase::FinalizePlanAdmitted
                | PreparePhase::CleanupSettled
                | PreparePhase::Rejected
                | PreparePhase::RecoveryRequired
        ) {
            return Ok(());
        }
        // Once the domain is free the provider is asked to plan again. The
        // first contention time is kept so repeated contention shares a bound.
        if let Some(wait) = &self.domain_wait
            && authority.domains_reserved(&wait.plan)
        {
            if wait.since.elapsed() < self.domain_wait_limit {
                return Ok(());
            }
            return Err(MUTATION_DOMAIN_RESERVED.into());
        }
        if matches!(
            self.state.phase,
            PreparePhase::Callback | PreparePhase::CallbackStatus
        ) {
            authority.validate_rights(
                participant,
                self.state.plan.as_ref().ok_or("missing plan")?,
                self.state
                    .admission_id
                    .as_deref()
                    .ok_or("missing admission")?,
            )?;
        }
        if self.state.request.version == api::COMPOSITION_VERSION
            && self.state.phase == PreparePhase::CallbackStatus
        {
            let pending = self
                .state
                .pending_dispatch
                .as_ref()
                .ok_or("pending_callback_proof_missing")?;
            let DispatchState::Completed { payload } = &pending.state else {
                return Err("pending_callback_proof_invalid".into());
            };
            let response: ProviderResponse = serde_json::from_value(payload.clone())
                .map_err(|_| "pending_callback_proof_invalid")?;
            api::validate_provider_response(&self.state.request, &response)?;
            let ProviderResponse::Pending { receipt_id } = response else {
                return Err("pending_callback_proof_invalid".into());
            };
            let arguments = serde_json::to_value(&self.state.request).map_err(|e| e.to_string())?;
            let expected = authority.approved_call(
                participant,
                &participant.callback_method,
                arguments,
                self.state.request.operation_id.clone(),
            )?;
            if pending.call != expected || self.state.pending_receipt.as_ref() != Some(&receipt_id)
            {
                return Err("pending_callback_proof_invalid".into());
            }
        }
        if self.dispatcher.is_none() {
            let original = self.state.clone();
            let call = match self.make_call(participant, authority) {
                Ok(call) => call,
                Err(error) => {
                    self.state = original;
                    return Err(error);
                }
            };
            let mut next = self.state.clone();
            next.dispatch = Some(DispatchRecord {
                call,
                state: DispatchState::Ready,
            });
            if let Err(error) = journal.persist(&next) {
                self.journal_failed = true;
                return Err(error);
            }
            self.state = next;
            self.dispatcher = Some(ProviderDispatcher::new(
                self.state.dispatch.clone().expect("just set"),
            )?);
            return Ok(());
        }
        let dispatcher = self.dispatcher.as_mut().expect("checked");
        let mut bridge = JournalBridge {
            state: &mut self.state,
            journal,
        };
        dispatcher.advance(&mut bridge, send)?;
        let record = dispatcher.record().clone();
        if dispatcher.has_request() {
            return Ok(());
        }
        match record.state {
            DispatchState::Completed { payload } => {
                self.settle(payload, participant, authority, journal)
            }
            DispatchState::OutcomeUnknown => {
                let mut next = self.state.clone();
                next.phase = PreparePhase::RecoveryRequired;
                if let Err(error) = journal.persist(&next) {
                    self.journal_failed = true;
                    return Err(error);
                }
                self.state = next;
                self.dispatcher = None;
                Ok(())
            }
            DispatchState::Ready => Ok(()),
        }
    }

    /// Discard a plan whose domain is held by another admitted operation and
    /// return to planning. No admission exists and no effect was sent, so the
    /// state is the same as before the plan was requested.
    fn wait_for_domain(
        &mut self,
        plan: ResourcePlan,
        journal: &mut impl PrepareJournal,
    ) -> ModuleResult<()> {
        let since = self
            .domain_wait
            .as_ref()
            .map_or_else(std::time::Instant::now, |wait| wait.since);
        if since.elapsed() >= self.domain_wait_limit {
            return Err(MUTATION_DOMAIN_RESERVED.into());
        }
        let mut next = self.state.clone();
        next.phase = PreparePhase::Plan;
        next.dispatch = None;
        next.pending_receipt = None;
        next.status_polls = 0;
        if let Err(error) = journal.persist(&next) {
            self.journal_failed = true;
            return Err(error);
        }
        self.state = next;
        self.dispatcher = None;
        self.domain_wait = Some(DomainWait { plan, since });
        Ok(())
    }

    fn make_call(
        &mut self,
        participant: &ParticipantSettings,
        authority: &mut impl PrepareAuthority,
    ) -> ModuleResult<ProviderCall> {
        let is_status = matches!(
            self.state.phase,
            PreparePhase::PlanStatus | PreparePhase::CallbackStatus
        );
        let (sequence, operation_id) = if is_status {
            (
                self.state.request.provider_sequence,
                authority.allocate_poll_id(&participant.provider)?,
            )
        } else {
            let (sequence, operation_id) = authority.allocate(&participant.provider)?;
            if sequence <= self.state.last_sequence {
                return Err("invalid provider sequence".into());
            }
            (sequence, operation_id)
        };
        if operation_id.is_empty() {
            return Err("invalid provider operation ID".into());
        }
        let (method, arguments) = match self.state.phase {
            PreparePhase::Plan => {
                let mut request = self.state.request.clone();
                request.provider_sequence = sequence;
                request.operation_id = operation_id.clone();
                api::validate_provider_request(&request)?;
                self.state.request = request.clone();
                (
                    &participant.plan_method,
                    serde_json::to_value(request).map_err(|e| e.to_string())?,
                )
            }
            PreparePhase::Callback => {
                let mut request = self.state.request.clone();
                request.provider_sequence = sequence;
                request.operation_id = operation_id.clone();
                request.operation = ProviderOperation::LifecycleCallback;
                request.approved_plan = self.state.plan.clone();
                request.plan_revision = self.state.plan.as_ref().map(|p| p.revision);
                request.admission_id = self.state.admission_id.clone();
                api::validate_provider_request(&request)?;
                self.state.request = request.clone();
                (
                    &participant.callback_method,
                    serde_json::to_value(request).map_err(|e| e.to_string())?,
                )
            }
            PreparePhase::PlanStatus | PreparePhase::CallbackStatus => {
                if self.state.status_polls >= MAX_STATUS_POLLS {
                    return Err("status polling limit reached".into());
                }
                let status = ProviderStatusRequest {
                    provider_sequence: sequence,
                    operation_id: self.state.request.operation_id.clone(),
                    transition_id: self.state.request.transition_id.clone(),
                    receipt_id: self
                        .state
                        .pending_receipt
                        .clone()
                        .ok_or("missing provider receipt")?,
                };
                self.state.status_polls += 1;
                (
                    &participant.status_method,
                    serde_json::to_value(status).map_err(|e| e.to_string())?,
                )
            }
            _ => return Err("invalid prepare phase".into()),
        };
        let call = authority.approved_call(
            participant,
            method,
            arguments.clone(),
            operation_id.clone(),
        )?;
        if call.service != method.service
            || call.method != method.method
            || call.interface_major != method.interface_major
            || call.interface_minor != method.interface_minor
            || call.operation_id != operation_id
            || call.arguments != arguments
            || call.approval.get("provider").and_then(Value::as_str)
                != Some(participant.provider.as_str())
        {
            return Err("selected provider approval mismatch".into());
        }
        if !is_status {
            self.state.last_sequence = sequence;
        }
        Ok(call)
    }

    fn settle(
        &mut self,
        payload: Value,
        participant: &ParticipantSettings,
        authority: &mut impl PrepareAuthority,
        journal: &mut impl PrepareJournal,
    ) -> ModuleResult<()> {
        let response: ProviderResponse =
            serde_json::from_value(payload.clone()).map_err(|_| "invalid provider response")?;
        api::validate_provider_response(&self.state.request, &response)?;
        let terminal_response = matches!(
            response,
            ProviderResponse::Planned { .. }
                | ProviderResponse::Prepared { .. }
                | ProviderResponse::Cleanup { .. }
        );
        let mut next = self.state.clone();
        match &response {
            ProviderResponse::Planned { plan }
                if matches!(next.phase, PreparePhase::Plan | PreparePhase::PlanStatus) =>
            {
                // Control proposals require coordinator-owned, schema-validated
                // execution. Version 1 has no such protocol; never admit a plan
                // whose proposed controls would otherwise be silently skipped.
                if !plan.control_proposals.is_empty() && !self.allow_controls {
                    return Err("resource control proposals are not supported".into());
                }
                let admission = match authority.admit_plan(participant, plan) {
                    Ok(admission) => admission,
                    Err(error) if error == MUTATION_DOMAIN_RESERVED => {
                        return self.wait_for_domain(plan.clone(), journal);
                    }
                    Err(error) => return Err(error),
                };
                if admission.is_empty() {
                    return Err("empty admission ID".into());
                }
                self.domain_wait = None;
                next.plan = Some(plan.clone());
                next.admission_id = Some(admission);
                next.phase = if next.request.operation == ProviderOperation::ResourceFinalizePlan {
                    PreparePhase::FinalizePlanAdmitted
                } else if !plan.control_proposals.is_empty() {
                    PreparePhase::ControlsPending
                } else {
                    PreparePhase::Callback
                };
            }
            ProviderResponse::Pending { receipt_id } => {
                if next.request.version == api::COMPOSITION_VERSION {
                    match next.phase {
                        PreparePhase::Callback => {
                            next.pending_dispatch = Some(self.state.dispatch.as_ref().filter(|record| {
                                matches!(&record.state, DispatchState::Completed { payload: stored } if stored == &payload)
                            }).cloned().ok_or("pending_callback_proof_missing")?);
                        }
                        PreparePhase::CallbackStatus
                            if next.pending_receipt.as_ref() == Some(receipt_id)
                                && next.pending_dispatch.is_some() => {}
                        _ => return Err("composition_pending_requires_recovery".into()),
                    }
                }
                next.pending_receipt = Some(receipt_id.clone());
                next.phase = match next.phase {
                    PreparePhase::Plan | PreparePhase::PlanStatus => PreparePhase::PlanStatus,
                    PreparePhase::Callback | PreparePhase::CallbackStatus => {
                        PreparePhase::CallbackStatus
                    }
                    _ => return Err("invalid pending phase".into()),
                };
            }
            ProviderResponse::Prepared { .. }
                if matches!(
                    next.phase,
                    PreparePhase::Callback | PreparePhase::CallbackStatus
                ) =>
            {
                if matches!(
                    next.request.phase,
                    api::LifecyclePhase::StreamRelease | api::LifecyclePhase::SessionRelease
                ) {
                    return Err("prepared response is invalid for release".into());
                }
                next.response = Some(response);
                next.phase = PreparePhase::Prepared;
            }
            ProviderResponse::Cleanup { outcomes, .. }
                if matches!(
                    next.phase,
                    PreparePhase::Callback | PreparePhase::CallbackStatus
                ) && matches!(
                    next.request.phase,
                    api::LifecyclePhase::StreamRelease | api::LifecyclePhase::SessionRelease
                ) =>
            {
                let complete = outcomes.len()
                    == next.plan.as_ref().ok_or("missing plan")?.obligations.len()
                    && outcomes.iter().all(|outcome| {
                        matches!(
                            outcome.state,
                            api::CleanupState::Released | api::CleanupState::DeferredRestoration
                        )
                    });
                next.response = Some(response);
                next.phase = if complete {
                    PreparePhase::CleanupSettled
                } else {
                    PreparePhase::RecoveryRequired
                };
            }
            ProviderResponse::Rejected { .. } | ProviderResponse::Failed { .. } => {
                next.response = Some(response);
                next.phase = if matches!(next.phase, PreparePhase::Plan | PreparePhase::PlanStatus)
                {
                    PreparePhase::Rejected
                } else {
                    PreparePhase::RecoveryRequired
                };
            }
            ProviderResponse::RecoveryRequired { .. } => {
                next.response = Some(response);
                next.phase = PreparePhase::RecoveryRequired;
            }
            _ => return Err("provider response not valid for prepare".into()),
        }
        if terminal_response || next.phase == PreparePhase::Rejected {
            next.terminal_dispatch = self.state.dispatch.as_ref().filter(|record| {
                matches!(&record.state, DispatchState::Completed { payload: stored } if stored == &payload)
            }).cloned();
        }
        next.dispatch = None;
        if let Err(error) = journal.persist(&next) {
            self.journal_failed = true;
            return Err(error);
        }
        self.state = next;
        self.dispatcher = None;
        Ok(())
    }
}

fn participant<'a>(
    settings: &'a LifecycleSettings,
    provider: &str,
) -> ModuleResult<&'a ParticipantSettings> {
    settings
        .participants
        .iter()
        .find(|p| p.provider == provider)
        .ok_or_else(|| "provider is no longer installed".into())
}

struct JournalBridge<'a, J> {
    state: &'a mut PrepareState,
    journal: &'a mut J,
}
impl<J: PrepareJournal> DispatchJournal for JournalBridge<'_, J> {
    fn persist(&mut self, record: &DispatchRecord) -> ModuleResult<()> {
        let mut next = self.state.clone();
        next.dispatch = Some(record.clone());
        self.journal.persist(&next)?;
        *self.state = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> (LifecycleSettings, PrepareState) {
        let method = |name: &str| json!({"service":"provider/1","method":name,"interface_major":1,"interface_minor":0,"schema_revision":1});
        let settings: LifecycleSettings = serde_json::from_value(json!({"journal_path":"/tmp/test", "participants":[{"provider":"provider","plan_method":method("plan"),"callback_method":method("callback"),"status_method":method("status"),"binding_validate_method":method("binding"),"capabilities":[],"cleanup_methods":[],"settings_revision":"revision","settings":{}}]})).unwrap();
        let request: ProviderRequest = serde_json::from_value(json!({"provider_sequence":1,"operation":"resource_plan","version":1,"transition_id":"t","operation_id":"original","target":{"kind":"validation","host_epoch":"e","validation_id":"v","generation":1},"phase":"stream.prepare","settings_revision":"revision","settings":{},"intent":null,"reason":null,"requested_video":null,"consumer_requirements":{"execution_domain":"desktop","capture_providers":["capture"],"selector_schemas":["selector/1"]},"remaining_ms":5000,"plan_revision":null,"admission_id":null,"approved_plan":null})).unwrap();
        let state = PrepareState {
            provider: "provider".into(),
            participant: settings.participants[0].clone(),
            request,
            phase: PreparePhase::Plan,
            plan: None,
            admission_id: None,
            pending_receipt: None,
            pending_dispatch: None,
            status_polls: 0,
            last_sequence: 1,
            dispatch: None,
            terminal_dispatch: None,
            response: None,
        };
        (settings, state)
    }
    #[derive(Default)]
    struct Journal {
        states: Vec<PrepareState>,
    }
    impl PrepareJournal for Journal {
        fn persist(&mut self, state: &PrepareState) -> ModuleResult<()> {
            self.states.push(state.clone());
            Ok(())
        }
    }
    struct Authority {
        sequence: u64,
        rights: bool,
        polls: u64,
    }
    impl PrepareAuthority for Authority {
        fn allocate(&mut self, _: &str) -> ModuleResult<(u64, String)> {
            self.sequence += 1;
            Ok((self.sequence, format!("op-{}", self.sequence)))
        }
        fn allocate_poll_id(&mut self, _: &str) -> ModuleResult<String> {
            self.polls += 1;
            Ok(format!("poll-{}", self.polls))
        }
        fn approved_call(
            &mut self,
            p: &ParticipantSettings,
            m: &MethodSettings,
            arguments: Value,
            operation_id: String,
        ) -> ModuleResult<ProviderCall> {
            Ok(ProviderCall {
                service: m.service.clone(),
                method: m.method.clone(),
                interface_major: m.interface_major,
                interface_minor: m.interface_minor,
                approval: json!({"provider":p.provider,"method":{"service":m.service,"method":m.method,"interface_major":m.interface_major,"interface_minor":m.interface_minor}}),
                arguments,
                timeout_ms: 5000,
                operation_id,
            })
        }
        fn admit_plan(
            &mut self,
            _: &ParticipantSettings,
            _: &ResourcePlan,
        ) -> ModuleResult<String> {
            self.rights = true;
            Ok("admission".into())
        }
        fn validate_rights(
            &mut self,
            _: &ParticipantSettings,
            _: &ResourcePlan,
            _: &str,
        ) -> ModuleResult<()> {
            if self.rights {
                Ok(())
            } else {
                Err("no rights".into())
            }
        }
    }
    #[test]
    fn plans_then_persists_admission_before_callback_and_polls_receipt() {
        let (settings, state) = fixture();
        let mut driver = PrepareDriver::new(state, &settings).unwrap();
        let mut authority = Authority {
            sequence: 1,
            rights: false,
            polls: 0,
        };
        let mut journal = Journal::default();
        let mut current_method = String::new();
        let mut callback_seen = false;
        let mut effect = None;
        let mut status_count = 0;
        let mut poll_ids = std::collections::BTreeSet::new();
        let mut send = |command: Command| -> ModuleResult<Response> {
            match command {
                Command::QueryApiApproval {
                    service,
                    method,
                    interface_major,
                    min_minor,
                } => Ok(Response::Result {
                    payload: json!({"provider":"provider","method":{"service":service,"method":method,"interface_major":interface_major,"interface_minor":min_minor}}),
                }),
                Command::CallApi {
                    method,
                    payload,
                    operation_id,
                    ..
                } => {
                    if method == "callback" {
                        callback_seen = true;
                        effect = Some((
                            payload["provider_sequence"].clone(),
                            payload["operation_id"].clone(),
                        ));
                    }
                    if method == "status" {
                        assert!(poll_ids.insert(operation_id.clone()));
                        let (sequence, id) = effect.as_ref().unwrap();
                        assert_eq!(&payload["provider_sequence"], sequence);
                        assert_eq!(&payload["operation_id"], id);
                        assert_ne!(payload["operation_id"], operation_id);
                    }
                    current_method = method;
                    Ok(Response::Accepted { id: 5 })
                }
                Command::TakeResult { .. } => Ok(Response::Result {
                    payload: match current_method.as_str() {
                        "plan" => {
                            json!({"status":"planned","plan":{"plan_id":"plan","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]}})
                        }
                        "callback" => json!({"status":"pending","receipt_id":"receipt"}),
                        "status" => {
                            status_count += 1;
                            if status_count == 1 {
                                json!({"status":"pending","receipt_id":"receipt"})
                            } else {
                                json!({"status":"prepared","candidate":null,"obligations":[]})
                            }
                        }
                        _ => panic!("unexpected method"),
                    },
                }),
                Command::ReleaseResult { .. } => Ok(Response::Ok),
                _ => panic!("unexpected command"),
            }
        };
        for _ in 0..30 {
            driver
                .advance(&settings, &mut authority, &mut journal, &mut send)
                .unwrap();
            if driver.state.phase == PreparePhase::Prepared {
                break;
            }
        }
        assert_eq!(driver.state.phase, PreparePhase::Prepared);
        assert!(
            matches!(driver.state.terminal_dispatch.as_ref().map(|record| &record.state),
            Some(DispatchState::Completed { payload }) if payload["status"] == "prepared")
        );
        assert!(callback_seen);
        assert!(
            journal
                .states
                .iter()
                .any(|s| s.phase == PreparePhase::Callback
                    && s.plan.is_some()
                    && s.admission_id.is_some())
        );
        assert_eq!(driver.state.status_polls, 2);
        assert_eq!(authority.polls, 2);
        assert_eq!(authority.sequence, 3);
        assert_eq!(driver.state.last_sequence, 3);
    }

    /// Refuses admission while another operation holds the domain.
    struct ContendedAuthority {
        inner: Authority,
        reserved: bool,
        refused: usize,
    }
    impl PrepareAuthority for ContendedAuthority {
        fn allocate(&mut self, provider: &str) -> ModuleResult<(u64, String)> {
            self.inner.allocate(provider)
        }
        fn allocate_poll_id(&mut self, provider: &str) -> ModuleResult<String> {
            self.inner.allocate_poll_id(provider)
        }
        fn approved_call(
            &mut self,
            participant: &ParticipantSettings,
            method: &MethodSettings,
            arguments: Value,
            operation_id: String,
        ) -> ModuleResult<ProviderCall> {
            self.inner
                .approved_call(participant, method, arguments, operation_id)
        }
        fn admit_plan(
            &mut self,
            participant: &ParticipantSettings,
            plan: &ResourcePlan,
        ) -> ModuleResult<String> {
            if self.reserved {
                self.refused += 1;
                return Err(MUTATION_DOMAIN_RESERVED.into());
            }
            self.inner.admit_plan(participant, plan)
        }
        fn validate_rights(
            &mut self,
            participant: &ParticipantSettings,
            plan: &ResourcePlan,
            admission_id: &str,
        ) -> ModuleResult<()> {
            self.inner.validate_rights(participant, plan, admission_id)
        }
        fn domains_reserved(&self, _: &ResourcePlan) -> bool {
            self.reserved
        }
    }

    /// Answers every plan and callback immediately and records plan sequences.
    fn immediate_provider(
        plans: &mut Vec<u64>,
        callbacks: &mut usize,
        method_in_flight: &mut String,
        command: Command,
    ) -> ModuleResult<Response> {
        match command {
            Command::QueryApiApproval {
                service,
                method,
                interface_major,
                min_minor,
            } => Ok(Response::Result {
                payload: json!({"provider":"provider","method":{"service":service,"method":method,"interface_major":interface_major,"interface_minor":min_minor}}),
            }),
            Command::CallApi {
                method, payload, ..
            } => {
                match method.as_str() {
                    "plan" => plans.push(payload["provider_sequence"].as_u64().unwrap()),
                    "callback" => *callbacks += 1,
                    other => panic!("unexpected method {other}"),
                }
                *method_in_flight = method;
                Ok(Response::Accepted { id: 5 })
            }
            Command::TakeResult { .. } => Ok(Response::Result {
                payload: if method_in_flight == "plan" {
                    json!({"status":"planned","plan":{"plan_id":"plan","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]}})
                } else {
                    json!({"status":"prepared","candidate":null,"obligations":[]})
                },
            }),
            Command::ReleaseResult { .. } => Ok(Response::Ok),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn a_reserved_domain_is_waited_out_and_planned_again_instead_of_failing() {
        let (settings, state) = fixture();
        let mut driver = PrepareDriver::new(state, &settings).unwrap();
        let mut authority = ContendedAuthority {
            inner: Authority {
                sequence: 1,
                rights: false,
                polls: 0,
            },
            reserved: true,
            refused: 0,
        };
        let mut journal = Journal::default();
        let (mut plans, mut callbacks, mut method) = (Vec::new(), 0, String::new());
        let mut send =
            |command: Command| immediate_provider(&mut plans, &mut callbacks, &mut method, command);
        for _ in 0..16 {
            driver
                .advance(&settings, &mut authority, &mut journal, &mut send)
                .unwrap();
            if driver.waiting_for_domain() {
                break;
            }
        }
        // Contention leaves the step exactly as it was before planning.
        assert!(driver.waiting_for_domain());
        assert_eq!(driver.state().phase, PreparePhase::Plan);
        assert!(driver.state().admission_id.is_none());
        assert!(driver.state().dispatch.is_none());
        assert_eq!(authority.refused, 1);
        let persisted = journal.states.len();
        // While the domain is held nothing is sent and nothing is written.
        for _ in 0..8 {
            driver
                .advance(&settings, &mut authority, &mut journal, &mut send)
                .unwrap();
        }
        assert_eq!(journal.states.len(), persisted);
        authority.reserved = false;
        for _ in 0..32 {
            driver
                .advance(&settings, &mut authority, &mut journal, &mut send)
                .unwrap();
            if driver.state().phase == PreparePhase::Prepared {
                break;
            }
        }
        drop(send);
        assert_eq!(driver.state().phase, PreparePhase::Prepared);
        assert!(!driver.waiting_for_domain());
        // The provider planned again, so its plan reflects the freed domain.
        assert_eq!(plans.len(), 2);
        assert!(plans[1] > plans[0]);
        assert_eq!(callbacks, 1);
    }

    #[test]
    fn a_domain_reserved_past_the_wait_limit_reports_contention_with_no_effect() {
        let (settings, state) = fixture();
        let mut driver = PrepareDriver::new(state, &settings).unwrap();
        driver.set_domain_wait_limit(std::time::Duration::ZERO);
        let mut authority = ContendedAuthority {
            inner: Authority {
                sequence: 1,
                rights: false,
                polls: 0,
            },
            reserved: true,
            refused: 0,
        };
        let mut journal = Journal::default();
        let (mut plans, mut callbacks, mut method) = (Vec::new(), 0, String::new());
        let mut send =
            |command: Command| immediate_provider(&mut plans, &mut callbacks, &mut method, command);
        let mut error = None;
        for _ in 0..16 {
            if let Err(reason) = driver.advance(&settings, &mut authority, &mut journal, &mut send)
            {
                error = Some(reason);
                break;
            }
        }
        drop(send);
        assert_eq!(error.as_deref(), Some(MUTATION_DOMAIN_RESERVED));
        assert!(driver.state().admission_id.is_none());
        assert_eq!(driver.state().phase, PreparePhase::Plan);
        assert_eq!(callbacks, 0);
    }

    #[test]
    fn finalize_plan_admits_fresh_rights_without_dispatching_callback() {
        let (mut settings, mut state) = fixture();
        settings.participants[0].contract_version = api::COMPOSITION_VERSION;
        settings.participants[0].plan_method.interface_major = api::COMPOSITION_VERSION;
        settings.participants[0].callback_method.interface_major = api::COMPOSITION_VERSION;
        settings.participants[0].status_method.interface_major = api::COMPOSITION_VERSION;
        settings.participants[0]
            .binding_validate_method
            .as_mut()
            .unwrap()
            .interface_major = api::COMPOSITION_VERSION;
        state.participant = settings.participants[0].clone();
        state.request.version = api::COMPOSITION_VERSION;
        state.request.operation = ProviderOperation::ResourceFinalizePlan;
        state.request.target = serde_json::from_value(
            json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":1}),
        )
        .unwrap();
        state.request.dependency_inputs = vec![serde_json::from_value(json!({"output_id":"output","capability":"display","schema":{"id":"display/1","major":1,"minor":0,"digest":"a".repeat(64)},"producer":"provider","producer_service":"provider/2","origin":state.request.target,"resource":{"provider":"provider","resource_id":"output","incarnation":"native","revision":2,"execution_domain":"desktop","mutation_domain":null,"mutation_domains":[]},"domain_observations":[],"data":{}})).unwrap()];
        let mut driver = PrepareDriver::new_finalize_plan(state, &settings).unwrap();
        let mut authority = Authority {
            sequence: 1,
            rights: false,
            polls: 0,
        };
        let mut journal = Journal::default();
        let plan = json!({"plan_id":"final","revision":1,"resources":[{"provider":"provider","resource_id":"output","incarnation":"native","expected_revision":2,"execution_domain":"desktop","mutation_domains":[],"access":"read"}],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]});
        let mut calls = Vec::new();
        for _ in 0..12 {
            driver.advance(&settings, &mut authority, &mut journal, &mut |command| match command {
                Command::QueryApiApproval { service, method, interface_major, min_minor } => Ok(Response::Result { payload: json!({"provider":"provider","method":{"service":service,"method":method,"interface_major":interface_major,"interface_minor":min_minor}}) }),
                Command::CallApi { method, payload, .. } => {
                    calls.push(method.clone());
                    assert_eq!(method, "plan");
                    assert_eq!(payload["operation"], "resource_finalize_plan");
                    Ok(Response::Accepted { id: 1 })
                }
                Command::TakeResult { request: 1 } => Ok(Response::Result { payload: json!({"status":"planned","plan":plan}) }),
                Command::ReleaseResult { request: 1 } => Ok(Response::Ok),
                other => panic!("unexpected finalization-plan command: {other:?}"),
            }).unwrap();
            if driver.state().phase == PreparePhase::FinalizePlanAdmitted {
                break;
            }
        }
        assert_eq!(calls, ["plan"]);
        assert_eq!(driver.state().phase, PreparePhase::FinalizePlanAdmitted);
        assert!(
            matches!(driver.state().terminal_dispatch.as_ref().map(|record| &record.state),
            Some(DispatchState::Completed { payload }) if payload["status"] == "planned")
        );
        assert_eq!(driver.state().admission_id.as_deref(), Some("admission"));
        assert!(authority.rights);
        driver
            .advance(&settings, &mut authority, &mut journal, &mut |_| {
                panic!("finalize planning must not dispatch a callback")
            })
            .unwrap();
    }

    #[test]
    fn standalone_v2_requires_dependency_for_stream_prepare() {
        let (mut settings, mut state) = fixture();
        let participant = &mut settings.participants[0];
        participant.contract_version = api::COMPOSITION_VERSION;
        participant.plan_method.interface_major = api::COMPOSITION_VERSION;
        participant.callback_method.interface_major = api::COMPOSITION_VERSION;
        participant.status_method.interface_major = api::COMPOSITION_VERSION;
        participant
            .binding_validate_method
            .as_mut()
            .unwrap()
            .interface_major = api::COMPOSITION_VERSION;
        state.participant = participant.clone();
        state.request.version = api::COMPOSITION_VERSION;
        state.request.phase = api::LifecyclePhase::SessionPrepare;
        assert!(PrepareDriver::new_standalone_prepare(state.clone(), &settings).is_ok());
        state.request.phase = api::LifecyclePhase::StreamPrepare;
        assert!(PrepareDriver::new_standalone_prepare(state, &settings).is_err());
    }

    #[test]
    fn standalone_v2_release_rejects_prepare_phase() {
        let (mut settings, mut state) = fixture();
        let participant = &mut settings.participants[0];
        participant.contract_version = api::COMPOSITION_VERSION;
        participant.plan_method.interface_major = api::COMPOSITION_VERSION;
        participant.callback_method.interface_major = api::COMPOSITION_VERSION;
        participant.status_method.interface_major = api::COMPOSITION_VERSION;
        participant
            .binding_validate_method
            .as_mut()
            .unwrap()
            .interface_major = api::COMPOSITION_VERSION;
        state.participant = participant.clone();
        state.request.version = api::COMPOSITION_VERSION;
        state.request.phase = api::LifecyclePhase::StreamRelease;
        assert!(PrepareDriver::new_standalone_release(state.clone(), &settings).is_err());
        state.request.phase = api::LifecyclePhase::StreamPrepare;
        assert!(PrepareDriver::new_standalone_release(state, &settings).is_err());
    }

    #[test]
    fn recovered_version_two_request_cannot_dispatch() {
        let (settings, mut state) = fixture();
        state.request.version = 2;
        assert!(PrepareDriver::new(state, &settings).is_err());
    }

    #[test]
    fn refuses_changed_policy_and_unheld_rights() {
        let (mut settings, mut state) = fixture();
        settings.participants[0].settings_revision = "changed".into();
        assert!(PrepareDriver::new(state.clone(), &settings).is_err());
        settings.participants[0].settings_revision = "revision".into();
        state.phase = PreparePhase::Callback;
        state.plan = Some(serde_json::from_value(json!({"plan_id":"p","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]})).unwrap());
        state.admission_id = Some("admission".into());
        let mut driver = PrepareDriver::new(state, &settings).unwrap();
        let mut authority = Authority {
            sequence: 1,
            rights: false,
            polls: 0,
        };
        let mut journal = Journal::default();
        assert!(
            driver
                .advance(&settings, &mut authority, &mut journal, &mut |_| panic!(
                    "must not dispatch"
                ))
                .is_err()
        );
        assert!(journal.states.is_empty());
    }
    #[test]
    fn callback_failure_and_rejection_retain_admission_for_recovery() {
        for status in ["failed", "rejected"] {
            for phase in [PreparePhase::Callback, PreparePhase::CallbackStatus] {
                let (settings, mut state) = fixture();
                let plan: ResourcePlan = serde_json::from_value(json!({"plan_id":"p","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]})).unwrap();
                state.phase = phase;
                state.plan = Some(plan.clone());
                state.admission_id = Some("admission".into());
                state.request.operation = ProviderOperation::LifecycleCallback;
                state.request.plan_revision = Some(plan.revision);
                state.request.admission_id = state.admission_id.clone();
                state.request.approved_plan = Some(plan);
                let mut driver = PrepareDriver::new(state, &settings).unwrap();
                let mut authority = Authority {
                    sequence: 1,
                    rights: true,
                    polls: 0,
                };
                let mut journal = Journal::default();
                driver
                    .settle(
                        json!({"status":status,"reason":"partial outcome"}),
                        &settings.participants[0],
                        &mut authority,
                        &mut journal,
                    )
                    .unwrap();
                assert_eq!(driver.state.phase, PreparePhase::RecoveryRequired);
                assert_eq!(driver.state.admission_id.as_deref(), Some("admission"));
                assert!(authority.rights);
                assert_eq!(
                    journal.states.last().unwrap().phase,
                    PreparePhase::RecoveryRequired
                );
            }
        }
    }
    #[test]
    fn release_cleanup_keeps_deferred_and_unknown_outcomes_distinct() {
        for (cleanup, expected) in [
            ("released", PreparePhase::CleanupSettled),
            ("deferred_restoration", PreparePhase::CleanupSettled),
            ("pending", PreparePhase::RecoveryRequired),
            ("effect_unknown", PreparePhase::RecoveryRequired),
        ] {
            let (settings, mut state) = fixture();
            let plan: ResourcePlan = serde_json::from_value(json!({"plan_id":"p","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[{"obligation_id":"o","provider":"provider","resource_id":"r","mutation_domain":null,"depends_on":[],"cleanup_method":{"service":"provider/1","method":"cleanup","interface_major":1,"interface_minor":0,"schema_revision":1},"payload":{}}],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]})).unwrap();
            state.phase = PreparePhase::Callback;
            state.plan = Some(plan.clone());
            state.admission_id = Some("admission".into());
            state.request.phase = api::LifecyclePhase::StreamRelease;
            state.request.operation = ProviderOperation::LifecycleCallback;
            state.request.plan_revision = Some(plan.revision);
            state.request.admission_id = state.admission_id.clone();
            state.request.approved_plan = Some(plan);
            let mut driver = PrepareDriver::new(state, &settings).unwrap();
            let mut journal = Journal::default();
            let mut authority = Authority {
                sequence: 1,
                rights: true,
                polls: 0,
            };
            driver.settle(json!({"status":"cleanup","outcomes":[{"obligation_id":"o","state":cleanup,"reason":null}]}), &settings.participants[0], &mut authority, &mut journal).unwrap();
            assert_eq!(driver.state.phase, expected);
            assert!(matches!(
                driver.state.response,
                Some(ProviderResponse::Cleanup { .. })
            ));
        }
    }
    #[test]
    fn callback_result_waits_for_release_retry_before_settlement() {
        let (settings, mut state) = fixture();
        state.phase = PreparePhase::Callback;
        state.plan = Some(serde_json::from_value(json!({"plan_id":"p","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]})).unwrap());
        state.admission_id = Some("admission".into());
        let mut driver = PrepareDriver::new(state, &settings).unwrap();
        let mut authority = Authority {
            sequence: 1,
            rights: true,
            polls: 0,
        };
        let mut journal = Journal::default();
        let mut release_attempts = 0;
        let mut send = |command: Command| -> ModuleResult<Response> {
            match command {
                Command::QueryApiApproval {
                    service,
                    method,
                    interface_major,
                    min_minor,
                } => Ok(Response::Result {
                    payload: json!({"provider":"provider","method":{"service":service,"method":method,"interface_major":interface_major,"interface_minor":min_minor}}),
                }),
                Command::CallApi { .. } => Ok(Response::Accepted { id: 19 }),
                Command::TakeResult { request: 19 } => Ok(Response::Result {
                    payload: json!({"status":"prepared","candidate":null,"obligations":[]}),
                }),
                Command::ReleaseResult { request: 19 } => {
                    release_attempts += 1;
                    if release_attempts == 1 {
                        Err("release reply lost".into())
                    } else {
                        Ok(Response::Ok)
                    }
                }
                _ => panic!("unexpected command"),
            }
        };
        for _ in 0..4 {
            driver
                .advance(&settings, &mut authority, &mut journal, &mut send)
                .unwrap();
        }
        assert_eq!(driver.state.phase, PreparePhase::Callback);
        assert!(matches!(
            driver.state.dispatch.as_ref().map(|d| &d.state),
            Some(DispatchState::Completed { .. })
        ));
        assert!(
            driver
                .advance(&settings, &mut authority, &mut journal, &mut send)
                .is_err()
        );
        assert_eq!(driver.state.phase, PreparePhase::Callback);
        assert!(driver.dispatcher.as_ref().unwrap().has_request());
        assert!(
            !journal
                .states
                .iter()
                .any(|state| state.phase == PreparePhase::Prepared)
        );
        driver
            .advance(&settings, &mut authority, &mut journal, &mut send)
            .unwrap();
        assert_eq!(driver.state.phase, PreparePhase::Prepared);
        assert!(driver.dispatcher.is_none());
        assert_eq!(release_attempts, 2);
    }
}
