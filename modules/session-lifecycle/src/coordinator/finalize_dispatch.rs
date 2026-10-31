//! Durable owner finalization after a separate, freshly admitted owner plan.
use crate::prepare_authority::FinalizeContext;
use crate::provider_dispatch::{
    DispatchJournal, DispatchRecord, DispatchState, ProviderCall, ProviderDispatcher,
};
use amaterasu_extension_api::{
    ProviderStatusRequest, ResourceControlResponse, ResourceFinalizeRequest,
    validate_resource_finalize_request, validate_resource_finalize_response,
};
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAX_STATUS_POLLS: u32 = 32;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum FinalizePhase {
    Effect,
    Status {
        receipt_id: String,
    },
    Confirmed {
        result: Box<ResourceControlResponse>,
    },
    Rejected {
        reason: String,
    },
    RecoveryRequired {
        reason: String,
    },
}

/// Caller persists the exact request, context, fresh plan/admission, permits and
/// selected approvals with this state before constructing the driver.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FinalizeDispatchState {
    pub request: ResourceFinalizeRequest,
    pub context: FinalizeContext,
    pub effect_call: ProviderCall,
    pub status_selection: ProviderCall,
    pub phase: FinalizePhase,
    pub status_polls: u32,
    pub dispatch: Option<DispatchRecord>,
}
impl FinalizeDispatchState {
    pub fn new(
        request: ResourceFinalizeRequest,
        context: FinalizeContext,
        effect_call: ProviderCall,
        status_selection: ProviderCall,
    ) -> ModuleResult<Self> {
        validate_resource_finalize_request(&request)?;
        let policy = &context.policy;
        if request.output != context.output
            || request.control_results != context.control_results
            || request.target != context.target
            || request.transition_id != context.transition_id
            || effect_call.service != policy.method.service
            || effect_call.method != policy.method.method
            || effect_call.interface_major != policy.method.interface_major
            || effect_call.interface_minor != policy.method.interface_minor
            || effect_call.operation_id != request.operation_id
            || effect_call.arguments != serde_json::to_value(&request).map_err(|e| e.to_string())?
            || effect_call.approval["provider"].as_str() != Some(policy.owner_provider.as_str())
            || status_selection.service != policy.status_method.service
            || status_selection.method != policy.status_method.method
            || status_selection.interface_major != policy.status_method.interface_major
            || status_selection.interface_minor != policy.status_method.interface_minor
            || status_selection.approval["provider"].as_str()
                != Some(policy.owner_provider.as_str())
        {
            return Err("finalize_selection_mismatch".into());
        }
        ProviderDispatcher::new(DispatchRecord {
            call: effect_call.clone(),
            state: DispatchState::Ready,
        })?;
        ProviderDispatcher::new(DispatchRecord {
            call: status_selection.clone(),
            state: DispatchState::Ready,
        })?;
        Ok(Self {
            request,
            context,
            effect_call,
            status_selection,
            phase: FinalizePhase::Effect,
            status_polls: 0,
            dispatch: None,
        })
    }
}
pub(super) trait FinalizeJournal {
    fn persist(&mut self, state: &FinalizeDispatchState) -> ModuleResult<()>;
}
pub(super) struct FinalizeDispatch {
    state: FinalizeDispatchState,
    dispatcher: Option<ProviderDispatcher>,
    journal_failed: bool,
}
impl FinalizeDispatch {
    pub fn new(state: FinalizeDispatchState) -> ModuleResult<Self> {
        FinalizeDispatchState::new(
            state.request.clone(),
            state.context.clone(),
            state.effect_call.clone(),
            state.status_selection.clone(),
        )?;
        if state.status_polls > MAX_STATUS_POLLS {
            return Err("invalid_finalize_state".into());
        }
        if let Some(record) = &state.dispatch {
            let valid = match &state.phase {
                FinalizePhase::Effect => record.call == state.effect_call,
                FinalizePhase::Status { receipt_id } => {
                    status_matches(&state, &record.call, receipt_id)
                }
                FinalizePhase::RecoveryRequired { .. } => true,
                _ => false,
            };
            if !valid {
                return Err("invalid_finalize_dispatch".into());
            }
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
        })
    }
    pub fn state(&self) -> &FinalizeDispatchState {
        &self.state
    }
    /// Rights are required before submission; result/status reconciliation is
    /// permitted after revocation. Applied results require independent current
    /// schema, mode, geometry, consumer-binding and plan validation.
    pub fn advance(
        &mut self,
        journal: &mut impl FinalizeJournal,
        status_call: &mut impl FnMut(&ResourceFinalizeRequest, &str) -> ModuleResult<ProviderCall>,
        validate_rights: &mut impl FnMut(&ResourceFinalizeRequest, &FinalizeContext) -> ModuleResult<()>,
        validate_applied: &mut impl FnMut(
            &ResourceFinalizeRequest,
            &FinalizeContext,
            &ResourceControlResponse,
        ) -> ModuleResult<()>,
        send: &mut (impl FnMut(Command) -> ModuleResult<Response> + ?Sized),
    ) -> ModuleResult<()> {
        if self.journal_failed {
            return Err("finalize_journal_requires_recovery".into());
        }
        if matches!(
            self.state.phase,
            FinalizePhase::Confirmed { .. }
                | FinalizePhase::Rejected { .. }
                | FinalizePhase::RecoveryRequired { .. }
        ) {
            return Ok(());
        }
        if matches!(self.state.phase, FinalizePhase::Effect)
            && self
                .state
                .dispatch
                .as_ref()
                .is_none_or(|record| record.state == DispatchState::Ready)
        {
            validate_rights(&self.state.request, &self.state.context)?;
        }
        if self.dispatcher.is_none() {
            let mut next = self.state.clone();
            let call = match &next.phase {
                FinalizePhase::Effect => next.effect_call.clone(),
                FinalizePhase::Status { receipt_id } => {
                    if next.status_polls >= MAX_STATUS_POLLS {
                        return Err("finalize_status_limit_recovery_required".into());
                    }
                    let call = status_call(&next.request, receipt_id)?;
                    if !status_matches(&next, &call, receipt_id) {
                        return Err("finalize_status_selection_mismatch".into());
                    }
                    next.status_polls += 1;
                    call
                }
                _ => unreachable!(),
            };
            let record = DispatchRecord {
                call,
                state: DispatchState::Ready,
            };
            let dispatcher = ProviderDispatcher::new(record.clone())?;
            next.dispatch = Some(record);
            self.persist(journal, next)?;
            self.dispatcher = Some(dispatcher);
            return Ok(());
        }
        let dispatcher = self.dispatcher.as_mut().expect("checked");
        let mut bridge = Bridge {
            state: &mut self.state,
            journal,
            failed: &mut self.journal_failed,
        };
        dispatcher.advance(&mut bridge, send)?;
        if dispatcher.has_request() {
            return Ok(());
        }
        match dispatcher.record().state.clone() {
            DispatchState::Ready => Ok(()),
            DispatchState::OutcomeUnknown => {
                let mut next = self.state.clone();
                if matches!(next.phase, FinalizePhase::Status { .. }) {
                    next.dispatch = None;
                } else {
                    next.phase = FinalizePhase::RecoveryRequired {
                        reason: "finalize_outcome_unknown".into(),
                    };
                }
                self.persist(journal, next)?;
                self.dispatcher = None;
                Ok(())
            }
            DispatchState::Completed { payload } => self.settle(payload, journal, validate_applied),
        }
    }
    fn settle(
        &mut self,
        payload: Value,
        journal: &mut impl FinalizeJournal,
        validate_applied: &mut impl FnMut(
            &ResourceFinalizeRequest,
            &FinalizeContext,
            &ResourceControlResponse,
        ) -> ModuleResult<()>,
    ) -> ModuleResult<()> {
        let response: ResourceControlResponse =
            serde_json::from_value(payload).map_err(|_| "invalid_finalize_response")?;
        validate_resource_finalize_response(&self.state.request, &response)?;
        let mut next = self.state.clone();
        next.phase = match &response {
            ResourceControlResponse::Applied { .. } => {
                validate_applied(&next.request, &next.context, &response)?;
                FinalizePhase::Confirmed {
                    result: Box::new(response),
                }
            }
            ResourceControlResponse::Pending { receipt_id } => FinalizePhase::Status {
                receipt_id: receipt_id.clone(),
            },
            ResourceControlResponse::Rejected { reason }
                if matches!(next.phase, FinalizePhase::Status { .. }) =>
            {
                FinalizePhase::RecoveryRequired {
                    reason: reason.clone(),
                }
            }
            ResourceControlResponse::Rejected { reason } => FinalizePhase::Rejected {
                reason: reason.clone(),
            },
            ResourceControlResponse::RecoveryRequired { reason } => {
                FinalizePhase::RecoveryRequired {
                    reason: reason.clone(),
                }
            }
        };
        next.dispatch = None;
        self.persist(journal, next)?;
        self.dispatcher = None;
        Ok(())
    }
    fn persist(
        &mut self,
        journal: &mut impl FinalizeJournal,
        next: FinalizeDispatchState,
    ) -> ModuleResult<()> {
        if let Err(error) = journal.persist(&next) {
            self.journal_failed = true;
            return Err(error);
        }
        self.state = next;
        Ok(())
    }
}
fn status_matches(state: &FinalizeDispatchState, call: &ProviderCall, receipt_id: &str) -> bool {
    let pinned = &state.status_selection;
    let expected = ProviderStatusRequest {
        provider_sequence: state.request.provider_sequence,
        operation_id: state.request.operation_id.clone(),
        transition_id: state.request.transition_id.clone(),
        receipt_id: receipt_id.into(),
    };
    call.service == pinned.service
        && call.method == pinned.method
        && call.interface_major == pinned.interface_major
        && call.interface_minor == pinned.interface_minor
        && call.approval == pinned.approval
        && call.operation_id != state.request.operation_id
        && serde_json::to_value(expected).ok().as_ref() == Some(&call.arguments)
}
struct Bridge<'a, J> {
    state: &'a mut FinalizeDispatchState,
    journal: &'a mut J,
    failed: &'a mut bool,
}
impl<J: FinalizeJournal> DispatchJournal for Bridge<'_, J> {
    fn persist(&mut self, record: &DispatchRecord) -> ModuleResult<()> {
        let mut next = self.state.clone();
        next.dispatch = Some(record.clone());
        if let Err(error) = self.journal.persist(&next) {
            *self.failed = true;
            return Err(error);
        }
        *self.state = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::VecDeque;
    fn fixture() -> FinalizeDispatchState {
        let target = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1});
        let schema = json!({"id":"schema","major":1,"minor":0,"digest":"a".repeat(64)});
        let resource = json!({"provider":"owner","resource_id":"output","incarnation":"native","revision":1,"execution_domain":"seat","mutation_domain":"domain","mutation_domains":["domain"]});
        let output = json!({"output_id":"out","capability":"display","schema":schema,"producer":"owner","producer_service":"owner/2","origin":target,"resource":resource,"data":{},"domain_observations":[{"domain":"domain","revision":1,"borrowers":{},"complete":true}]});
        let effect_method = json!({"service":"owner/2","method":"finalize","interface_major":2,"interface_minor":0,"schema_revision":1});
        let status_method = json!({"service":"owner/2","method":"status","interface_major":2,"interface_minor":0,"schema_revision":1});
        let request:ResourceFinalizeRequest=serde_json::from_value(json!({"version":2,"transition_id":"transition","operation_id":"effect","provider_sequence":1,"target":target,"output":output,"control_results":[],"admission_id":"admission","permit_id":"permit","remaining_ms":5000})).unwrap();
        let context:FinalizeContext=serde_json::from_value(json!({"policy":{"owner_provider":"owner","owner_service":"owner/2","method":effect_method,"status_method":status_method,"output_capability":"display","output_schema":schema,"capture_provider":"capture","selector_schema":"selector","required_access":"exclusive","mutation_domains":["domain"]},"output":output,"control_results":[],"target":target,"transition_id":"transition","parent_generation":1})).unwrap();
        let call = |method: &str, args: Value, id: &str| ProviderCall {
            service: "owner/2".into(),
            method: method.into(),
            interface_major: 2,
            interface_minor: 0,
            approval: json!({"provider":"owner","method":{"service":"owner/2","method":method,"interface_major":2,"interface_minor":0}}),
            arguments: args,
            timeout_ms: 5000,
            operation_id: id.into(),
        };
        let effect = call(
            "finalize",
            serde_json::to_value(&request).unwrap(),
            "effect",
        );
        let status = call("status", json!({}), "template");
        FinalizeDispatchState::new(request, context, effect, status).unwrap()
    }
    #[derive(Default)]
    struct Journal(Vec<FinalizeDispatchState>);
    impl FinalizeJournal for Journal {
        fn persist(&mut self, state: &FinalizeDispatchState) -> ModuleResult<()> {
            self.0.push(state.clone());
            Ok(())
        }
    }
    #[test]
    fn unknown_finalize_is_never_replayed() {
        let mut job = FinalizeDispatch::new(fixture()).unwrap();
        let mut journal = Journal::default();
        let mut responses = VecDeque::from([
            Ok(Response::Result {
                payload: job.state().effect_call.approval.clone(),
            }),
            Err("lost".into()),
        ]);
        let mut count = 0;
        let mut send = |_| {
            count += 1;
            responses.pop_front().unwrap()
        };
        let mut status = |_: &ResourceFinalizeRequest, _: &str| -> ModuleResult<ProviderCall> {
            panic!("must not poll")
        };
        let mut rights = |_: &ResourceFinalizeRequest, _: &FinalizeContext| Ok(());
        let mut applied =
            |_: &ResourceFinalizeRequest, _: &FinalizeContext, _: &ResourceControlResponse| Ok(());
        job.advance(
            &mut journal,
            &mut status,
            &mut rights,
            &mut applied,
            &mut send,
        )
        .unwrap();
        job.advance(
            &mut journal,
            &mut status,
            &mut rights,
            &mut applied,
            &mut send,
        )
        .unwrap();
        assert!(
            job.advance(
                &mut journal,
                &mut status,
                &mut rights,
                &mut applied,
                &mut send
            )
            .is_err()
        );
        assert_eq!(count, 2);
        let mut resumed = FinalizeDispatch::new(journal.0.last().unwrap().clone()).unwrap();
        resumed
            .advance(
                &mut journal,
                &mut status,
                &mut rights,
                &mut applied,
                &mut |_| panic!("must not replay"),
            )
            .unwrap();
        assert!(matches!(
            resumed.state().phase,
            FinalizePhase::RecoveryRequired { .. }
        ));
    }
    #[test]
    fn output_substitution_and_missing_independent_validation_are_rejected() {
        let mut job = FinalizeDispatch::new(fixture()).unwrap();
        let mut journal = Journal::default();
        let mut wrong = job.state().request.output.clone();
        wrong.producer = "other".into();
        let mut applied =
            |_: &ResourceFinalizeRequest, _: &FinalizeContext, _: &ResourceControlResponse| Ok(());
        assert!(
            job.settle(
                json!({"status":"applied","output":wrong,"candidate":null}),
                &mut journal,
                &mut applied
            )
            .is_err()
        );
        let output = job.state().request.output.clone();
        let mut denied = |_: &ResourceFinalizeRequest,
                          _: &FinalizeContext,
                          _: &ResourceControlResponse|
         -> ModuleResult<()> { Err("constraint mismatch".into()) };
        assert!(
            job.settle(
                json!({"status":"applied","output":output,"candidate":null}),
                &mut journal,
                &mut denied
            )
            .is_err()
        );
        assert!(journal.0.is_empty());
    }
}
