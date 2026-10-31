//! Durable, single-owner resource-control dispatch. The caller owns admission,
//! current domain rights and persistence of this entire state.
use amaterasu_extension_api::{
    ConfirmedControl, LifecycleTarget, ProviderStatusRequest, ResourceControlRequest,
    ResourceControlResponse, ResourceOutput,
};
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider_dispatch::{
    DispatchJournal, DispatchRecord, DispatchState, ProviderCall, ProviderDispatcher,
};

const MAX_STATUS_POLLS: u32 = 32;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlPhase {
    Effect,
    Status {
        receipt_id: String,
    },
    /// The owner reported Applied; this is provisional until settlement is
    /// durably recorded and the owner acknowledges its independent CAS.
    #[serde(alias = "confirmed")]
    Applied {
        result: Box<ConfirmedControl>,
    },
    Rejected {
        reason: String,
    },
    RecoveryRequired {
        reason: String,
    },
}

/// Persist before constructing a dispatcher. The effect call and permit are
/// immutable; the owner and both method approvals come from installed policy.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlDispatchState {
    pub request: ResourceControlRequest,
    pub dependency: ResourceOutput,
    pub effect_call: ProviderCall,
    pub status_selection: ProviderCall,
    pub phase: ControlPhase,
    pub status_polls: u32,
    pub dispatch: Option<DispatchRecord>,
    /// Exact owner Pending response that established the status receipt.
    #[serde(default)]
    pub pending_proof: Option<DispatchRecord>,
}

impl ControlDispatchState {
    pub fn new(
        request: ResourceControlRequest,
        dependency: ResourceOutput,
        effect_call: ProviderCall,
        status_selection: ProviderCall,
    ) -> ModuleResult<Self> {
        let proposal = &request.proposal;
        if request.version != 2
            || request.provider_sequence == 0
            || request.transition_id.is_empty()
            || request.operation_id.is_empty()
            || request.admission_id.is_empty()
            || request.permit_id.is_empty()
            || request.remaining_ms == 0
            || proposal.target != request.target
            || proposal.output_id != dependency.output_id
            || proposal.owner_provider != dependency.producer
            || proposal.owner_service != dependency.producer_service
            || proposal.resource != dependency.resource
            || proposal.method.service != proposal.owner_service
            || effect_call.service != proposal.method.service
            || effect_call.method != proposal.method.method
            || effect_call.interface_major != proposal.method.interface_major
            || effect_call.interface_minor != proposal.method.interface_minor
            || effect_call.operation_id != request.operation_id
            || effect_call.arguments != serde_json::to_value(&request).map_err(|e| e.to_string())?
            || effect_call.approval["provider"].as_str() != Some(proposal.owner_provider.as_str())
            || status_selection.approval["provider"].as_str()
                != Some(proposal.owner_provider.as_str())
        {
            return Err("resource_control_selection_mismatch".into());
        }
        // The dispatcher validates the whole approval, method and wire limits.
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
            dependency,
            effect_call,
            status_selection,
            phase: ControlPhase::Effect,
            status_polls: 0,
            dispatch: None,
            pending_proof: None,
        })
    }
}

pub trait ControlJournal {
    fn persist(&mut self, state: &ControlDispatchState) -> ModuleResult<()>;
}

pub struct ControlDispatch {
    state: ControlDispatchState,
    dispatcher: Option<ProviderDispatcher>,
    journal_failed: bool,
}

impl ControlDispatch {
    pub fn new(state: ControlDispatchState) -> ModuleResult<Self> {
        // Revalidate immutable selection even when reading persisted state.
        ControlDispatchState::new(
            state.request.clone(),
            state.dependency.clone(),
            state.effect_call.clone(),
            state.status_selection.clone(),
        )?;
        if state.status_polls > MAX_STATUS_POLLS {
            return Err("invalid_control_state".into());
        }
        if let ControlPhase::Status { receipt_id } = &state.phase
            && !valid_pending_proof(&state, receipt_id)
        {
            return Err("control_pending_receipt_unproven".into());
        }
        if let Some(record) = &state.dispatch {
            let valid = match &state.phase {
                ControlPhase::Effect => record.call == state.effect_call,
                ControlPhase::Status { receipt_id } => {
                    let call = &record.call;
                    let pinned = &state.status_selection;
                    let expected = ProviderStatusRequest {
                        provider_sequence: state.request.provider_sequence,
                        operation_id: state.request.operation_id.clone(),
                        transition_id: state.request.transition_id.clone(),
                        receipt_id: receipt_id.clone(),
                    };
                    call.service == pinned.service
                        && call.method == pinned.method
                        && call.interface_major == pinned.interface_major
                        && call.interface_minor == pinned.interface_minor
                        && call.approval == pinned.approval
                        && call.operation_id != state.request.operation_id
                        && serde_json::to_value(expected).ok().as_ref() == Some(&call.arguments)
                }
                ControlPhase::RecoveryRequired { .. } => true,
                ControlPhase::Applied { result } => {
                    let exact_call = record.call == state.effect_call;
                    let status: Option<ProviderStatusRequest> =
                        serde_json::from_value(record.call.arguments.clone()).ok();
                    let status_call = record.call.service == state.status_selection.service
                        && record.call.method == state.status_selection.method
                        && record.call.interface_major == state.status_selection.interface_major
                        && record.call.interface_minor == state.status_selection.interface_minor
                        && record.call.approval == state.status_selection.approval
                        && status.as_ref().is_some_and(|status| {
                            status.provider_sequence == state.request.provider_sequence
                                && status.operation_id == state.request.operation_id
                                && status.transition_id == state.request.transition_id
                                && !status.receipt_id.is_empty()
                        });
                    let expected = ResourceControlResponse::Applied {
                        output: Box::new(result.output.clone()),
                        candidate: result.candidate.clone().map(Box::new),
                    };
                    // Status completion requires independently retained Pending
                    // receipt proof, which older journals do not provide.
                    (exact_call
                        || (status_call
                            && status.as_ref().is_some_and(|status| {
                                valid_pending_proof(&state, &status.receipt_id)
                            })))
                        && matches!(&record.state, DispatchState::Completed { payload } if serde_json::from_value::<ResourceControlResponse>(payload.clone()).ok().as_ref() == Some(&expected))
                }
                _ => false,
            };
            if !valid {
                return Err("invalid_control_dispatch".into());
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

    pub fn state(&self) -> &ControlDispatchState {
        &self.state
    }

    /// `status_call` allocates a durable unique transport ID and returns the
    /// installed status selection. `validate_applied` checks current admitted
    /// rights and all plan-specific output, geometry, mode and binding constraints.
    pub fn advance(
        &mut self,
        journal: &mut impl ControlJournal,
        status_call: &mut impl FnMut(&ResourceControlRequest, &str) -> ModuleResult<ProviderCall>,
        validate_rights: &mut impl FnMut(&ResourceControlRequest, &ResourceOutput) -> ModuleResult<()>,
        validate_applied: &mut impl FnMut(
            &ResourceControlRequest,
            &ResourceOutput,
            &ConfirmedControl,
        ) -> ModuleResult<()>,
        send: &mut (impl FnMut(Command) -> ModuleResult<Response> + ?Sized),
    ) -> ModuleResult<()> {
        if self.journal_failed {
            return Err("control_journal_requires_recovery".into());
        }
        if matches!(
            self.state.phase,
            ControlPhase::Applied { .. }
                | ControlPhase::Rejected { .. }
                | ControlPhase::RecoveryRequired { .. }
        ) {
            return Ok(());
        }
        // Fresh rights gate only a new effect. Once armed, result collection and
        // read-only status reconciliation must remain possible after revocation.
        if matches!(self.state.phase, ControlPhase::Effect)
            && self
                .state
                .dispatch
                .as_ref()
                .is_none_or(|record| record.state == DispatchState::Ready)
        {
            validate_rights(&self.state.request, &self.state.dependency)?;
        }
        if self.dispatcher.is_none() {
            let mut next = self.state.clone();
            let call = match &next.phase {
                ControlPhase::Effect => next.effect_call.clone(),
                ControlPhase::Status { receipt_id } => {
                    if next.status_polls >= MAX_STATUS_POLLS {
                        return Err("control_status_limit_recovery_required".into());
                    }
                    let call = status_call(&next.request, receipt_id)?;
                    let pinned = &next.status_selection;
                    let expected = ProviderStatusRequest {
                        provider_sequence: next.request.provider_sequence,
                        operation_id: next.request.operation_id.clone(),
                        transition_id: next.request.transition_id.clone(),
                        receipt_id: receipt_id.clone(),
                    };
                    if call.service != pinned.service
                        || call.method != pinned.method
                        || call.interface_major != pinned.interface_major
                        || call.interface_minor != pinned.interface_minor
                        || call.approval != pinned.approval
                        || call.arguments
                            != serde_json::to_value(expected).map_err(|e| e.to_string())?
                        || call.operation_id == next.request.operation_id
                    {
                        return Err("control_status_selection_mismatch".into());
                    }
                    next.status_polls += 1;
                    call
                }
                _ => unreachable!(),
            };
            let dispatch = DispatchRecord {
                call,
                state: DispatchState::Ready,
            };
            let dispatcher = ProviderDispatcher::new(dispatch.clone())?;
            next.dispatch = Some(dispatch);
            self.persist(journal, next)?;
            self.dispatcher = Some(dispatcher);
            return Ok(());
        }
        let dispatcher = self.dispatcher.as_mut().expect("checked");
        let mut bridge = JournalBridge {
            state: &mut self.state,
            journal,
            failed: &mut self.journal_failed,
        };
        dispatcher.advance(&mut bridge, send)?;
        if dispatcher.has_request() {
            return Ok(());
        }
        let record = dispatcher.record().clone();
        match record.state {
            DispatchState::Ready => Ok(()),
            DispatchState::OutcomeUnknown => {
                let mut next = self.state.clone();
                if matches!(next.phase, ControlPhase::Status { .. }) {
                    // Status is read-only and may be retried with the same
                    // provider receipt and a fresh transport operation ID.
                    next.dispatch = None;
                } else {
                    next.phase = ControlPhase::RecoveryRequired {
                        reason: "control_outcome_unknown".into(),
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
        journal: &mut impl ControlJournal,
        validate_applied: &mut impl FnMut(
            &ResourceControlRequest,
            &ResourceOutput,
            &ConfirmedControl,
        ) -> ModuleResult<()>,
    ) -> ModuleResult<()> {
        let response: ResourceControlResponse =
            serde_json::from_value(payload).map_err(|_| "invalid_control_response")?;
        let mut next = self.state.clone();
        next.phase = match response {
            ResourceControlResponse::Pending { receipt_id }
                if !receipt_id.is_empty()
                    && receipt_id.len() <= 128
                    && !receipt_id.chars().any(char::is_control) =>
            {
                if matches!(next.phase, ControlPhase::Effect) {
                    next.pending_proof = next.dispatch.clone();
                } else if !valid_pending_proof(&next, &receipt_id) {
                    return Err("control_pending_receipt_unproven".into());
                }
                ControlPhase::Status { receipt_id }
            }
            ResourceControlResponse::Applied { output, candidate } => {
                if let ControlPhase::Status { receipt_id } = &next.phase
                    && !valid_pending_proof(&next, receipt_id)
                {
                    return Err("control_pending_receipt_unproven".into());
                }
                let proposal = &next.request.proposal;
                if output.producer != proposal.owner_provider
                    || output.producer_service != proposal.owner_service
                    || output.origin != next.dependency.origin
                    || output.resource.provider != proposal.resource.provider
                    || output.resource.resource_id != proposal.resource.resource_id
                    || output.schema != next.dependency.schema
                {
                    return Err("control_output_mismatch".into());
                }
                if let Some(binding) = &candidate {
                    let generation = match &next.request.target {
                        LifecycleTarget::Stream { generation, .. } => *generation,
                        _ => return Err("control_candidate_target_mismatch".into()),
                    };
                    if binding.resource != output.resource || binding.generation != generation {
                        return Err("control_candidate_mismatch".into());
                    }
                }
                let result = ConfirmedControl {
                    proposal_id: proposal.proposal_id.clone(),
                    output: *output,
                    candidate: candidate.map(|binding| *binding),
                };
                validate_applied(&next.request, &next.dependency, &result)?;
                ControlPhase::Applied {
                    result: Box::new(result),
                }
            }
            ResourceControlResponse::Rejected { reason }
                if !reason.is_empty() && reason.len() <= 128 =>
            {
                if matches!(next.phase, ControlPhase::Status { .. }) {
                    // A later rejection cannot prove an earlier admitted
                    // operation did not leave native effects behind.
                    ControlPhase::RecoveryRequired { reason }
                } else {
                    ControlPhase::Rejected { reason }
                }
            }
            ResourceControlResponse::RecoveryRequired { reason }
                if !reason.is_empty() && reason.len() <= 128 =>
            {
                ControlPhase::RecoveryRequired { reason }
            }
            _ => return Err("invalid_control_response".into()),
        };
        if !matches!(next.phase, ControlPhase::Applied { .. }) {
            next.dispatch = None;
        }
        self.persist(journal, next)?;
        self.dispatcher = None;
        Ok(())
    }

    fn persist(
        &mut self,
        journal: &mut impl ControlJournal,
        next: ControlDispatchState,
    ) -> ModuleResult<()> {
        if let Err(error) = journal.persist(&next) {
            self.journal_failed = true;
            return Err(error);
        }
        self.state = next;
        Ok(())
    }
}

fn valid_pending_proof(state: &ControlDispatchState, receipt_id: &str) -> bool {
    state.pending_proof.as_ref().is_some_and(|proof| {
        proof.call == state.effect_call
            && matches!(&proof.state, DispatchState::Completed { payload } if payload == &serde_json::json!({"status":"pending", "receipt_id":receipt_id}))
    })
}

struct JournalBridge<'a, J> {
    state: &'a mut ControlDispatchState,
    journal: &'a mut J,
    failed: &'a mut bool,
}
impl<J: ControlJournal> DispatchJournal for JournalBridge<'_, J> {
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

    fn fixture() -> ControlDispatchState {
        let target = json!({"kind":"stream","host_epoch":"e","session_id":"s","stream_id":"t","generation":7});
        let resource = json!({"provider":"display","resource_id":"r","incarnation":"i","revision":1,"execution_domain":"seat","mutation_domain":null,"mutation_domains":[]});
        let schema = json!({"id":"output","major":1,"minor":0,"digest":"digest"});
        let output: ResourceOutput = serde_json::from_value(json!({"output_id":"out","capability":"display","schema":schema,"producer":"display","producer_service":"display/1","origin":target,"resource":resource,"data":{}})).unwrap();
        let request: ResourceControlRequest = serde_json::from_value(json!({"version":2,"transition_id":"transition","operation_id":"effect","provider_sequence":1,"target":target,"proposal":{"proposal_id":"proposal","output_id":"out","owner_provider":"display","owner_service":"display/1","target":target,"resource":resource,"method":{"service":"display/1","method":"control","interface_major":1,"interface_minor":0,"schema_revision":1},"argument_schema":schema,"arguments":{},"access":"exclusive","mutation_domains":[]},"admission_id":"admission","permit_id":"permit","remaining_ms":5000})).unwrap();
        let call = |method: &str, arguments: Value, operation_id: &str| ProviderCall {
            service: "display/1".into(),
            method: method.into(),
            interface_major: 1,
            interface_minor: 0,
            approval: json!({"provider":"display","method":{"service":"display/1","method":method,"interface_major":1,"interface_minor":0}}),
            arguments,
            timeout_ms: 5000,
            operation_id: operation_id.into(),
        };
        let effect = call("control", serde_json::to_value(&request).unwrap(), "effect");
        let status = call("status", json!({}), "status-template");
        ControlDispatchState::new(request, output, effect, status).unwrap()
    }
    #[derive(Default)]
    struct Journal {
        states: Vec<ControlDispatchState>,
        fail: bool,
    }
    impl ControlJournal for Journal {
        fn persist(&mut self, state: &ControlDispatchState) -> ModuleResult<()> {
            if self.fail {
                return Err("write failed".into());
            }
            self.states.push(state.clone());
            Ok(())
        }
    }
    #[test]
    fn selection_and_restored_dispatch_reject_substitution() {
        let mut state = fixture();
        state.effect_call.approval["provider"] = json!("attacker");
        assert!(ControlDispatch::new(state).is_err());
        let mut state = fixture();
        let mut call = state.effect_call.clone();
        call.arguments["permit_id"] = json!("other");
        state.dispatch = Some(DispatchRecord {
            call,
            state: DispatchState::Ready,
        });
        assert!(ControlDispatch::new(state).is_err());
    }
    #[test]
    fn unknown_effect_is_fenced_and_never_replayed_after_restart() {
        let mut job = ControlDispatch::new(fixture()).unwrap();
        let mut journal = Journal::default();
        let mut replies = VecDeque::from([
            Ok(Response::Result {
                payload: job.state().effect_call.approval.clone(),
            }),
            Err("lost acknowledgement".into()),
        ]);
        let mut commands = Vec::new();
        let mut send = |command| {
            commands.push(command);
            replies.pop_front().unwrap()
        };
        let mut status = |_: &ResourceControlRequest, _: &str| -> ModuleResult<ProviderCall> {
            panic!("must not poll")
        };
        let mut validate =
            |_: &ResourceControlRequest, _: &ResourceOutput, _: &ConfirmedControl| Ok(());
        let mut rights = |_: &ResourceControlRequest, _: &ResourceOutput| Ok(());
        job.advance(
            &mut journal,
            &mut status,
            &mut rights,
            &mut validate,
            &mut send,
        )
        .unwrap();
        job.advance(
            &mut journal,
            &mut status,
            &mut rights,
            &mut validate,
            &mut send,
        )
        .unwrap();
        assert!(
            job.advance(
                &mut journal,
                &mut status,
                &mut rights,
                &mut validate,
                &mut send
            )
            .is_err()
        );
        assert_eq!(commands.len(), 2);
        let mut restored = ControlDispatch::new(journal.states.last().unwrap().clone()).unwrap();
        restored
            .advance(
                &mut journal,
                &mut status,
                &mut rights,
                &mut validate,
                &mut |_| panic!("must not replay"),
            )
            .unwrap();
        assert!(matches!(
            restored.state().phase,
            ControlPhase::RecoveryRequired { .. }
        ));
    }
    #[test]
    fn altered_owner_output_cannot_be_confirmed() {
        let mut job = ControlDispatch::new(fixture()).unwrap();
        let mut output = job.state().dependency.clone();
        output.producer = "attacker".into();
        let mut journal = Journal::default();
        let mut validate =
            |_: &ResourceControlRequest, _: &ResourceOutput, _: &ConfirmedControl| Ok(());
        assert!(
            job.settle(
                json!({"status":"applied","output":output,"candidate":null}),
                &mut journal,
                &mut validate
            )
            .is_err()
        );
        assert!(journal.states.is_empty());
    }

    #[test]
    fn pending_receipt_survives_restart_and_status_uses_original_identity() {
        let mut job = ControlDispatch::new(fixture()).unwrap();
        let mut journal = Journal::default();
        let mut validate =
            |_: &ResourceControlRequest, _: &ResourceOutput, _: &ConfirmedControl| Ok(());
        job.state.dispatch = Some(DispatchRecord {
            call: job.state.effect_call.clone(),
            state: DispatchState::Completed {
                payload: json!({"status":"pending","receipt_id":"receipt"}),
            },
        });
        job.settle(
            json!({"status":"pending","receipt_id":"receipt"}),
            &mut journal,
            &mut validate,
        )
        .unwrap();
        let mut restored = ControlDispatch::new(journal.states.last().unwrap().clone()).unwrap();
        let status_selection = restored.state().status_selection.clone();
        let mut make_status = |request: &ResourceControlRequest, receipt: &str| {
            let mut call = status_selection.clone();
            call.operation_id = "fresh-poll".into();
            call.arguments = serde_json::to_value(ProviderStatusRequest {
                provider_sequence: request.provider_sequence,
                operation_id: request.operation_id.clone(),
                transition_id: request.transition_id.clone(),
                receipt_id: receipt.into(),
            })
            .unwrap();
            Ok(call)
        };
        let mut rights = |_: &ResourceControlRequest, _: &ResourceOutput| {
            panic!("status reconciliation does not require effect authority")
        };
        restored
            .advance(
                &mut journal,
                &mut make_status,
                &mut rights,
                &mut validate,
                &mut |_| panic!("first tick persists status intent"),
            )
            .unwrap();
        let record = restored.state().dispatch.as_ref().unwrap();
        assert_eq!(record.call.arguments["operation_id"], "effect");
        assert_eq!(record.call.arguments["provider_sequence"], 1);
        assert_eq!(record.call.arguments["receipt_id"], "receipt");
    }
    #[test]
    fn status_completion_requires_retained_original_pending_receipt() {
        let mut job = ControlDispatch::new(fixture()).unwrap();
        job.state.phase = ControlPhase::Status {
            receipt_id: "receipt".into(),
        };
        job.state.pending_proof = Some(DispatchRecord {
            call: job.state.effect_call.clone(),
            state: DispatchState::Completed {
                payload: json!({"status":"pending","receipt_id":"receipt"}),
            },
        });
        let mut status_call = job.state.status_selection.clone();
        status_call.operation_id = "poll".into();
        status_call.arguments = serde_json::to_value(ProviderStatusRequest {
            provider_sequence: job.state.request.provider_sequence,
            operation_id: job.state.request.operation_id.clone(),
            transition_id: job.state.request.transition_id.clone(),
            receipt_id: "receipt".into(),
        })
        .unwrap();
        let payload = json!({"status":"applied","output":job.state.dependency,"candidate":null});
        job.state.dispatch = Some(DispatchRecord {
            call: status_call,
            state: DispatchState::Completed {
                payload: payload.clone(),
            },
        });
        let mut journal = Journal::default();
        let mut validate =
            |_: &ResourceControlRequest, _: &ResourceOutput, _: &ConfirmedControl| Ok(());
        job.settle(payload, &mut journal, &mut validate).unwrap();
        let state = journal.states.last().unwrap().clone();
        assert!(ControlDispatch::new(state.clone()).is_ok());
        let mut forged = state;
        forged.pending_proof.as_mut().unwrap().state = DispatchState::Completed {
            payload: json!({"status":"pending","receipt_id":"other"}),
        };
        assert!(ControlDispatch::new(forged).is_err());
    }
}
