//! Durable coordinator-owned outputs and frozen dependency inputs.
//!
//! The coordinator must atomically save mutations with participant progress and
//! obligations before exposing outputs or dispatching dependent calls. Installed
//! schema validation and active-resource reobservation are external prerequisites;
//! this module never treats serialized provider data as authorization.
use super::finalize_selection::FinalizeSelectionSnapshot;
use crate::adapter::SchemaDocument;
use crate::prepare_authority::{Admission, ControlPermit};
use crate::prepare_authority::{ControlPolicy, FinalizePolicy, MethodApproval};
use amaterasu_extension_api::{
    ConfirmedControl, LifecycleTarget, ResourceAccess, ResourceControlRequest,
    ResourceControlResponse, ResourceObligation, ResourceOutput, ResourcePlan, SchemaRef,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const MAX_OUTPUTS: usize = 64;
const MAX_INPUTS: usize = 32;
const MAX_BYTES: usize = 8 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputState {
    Retained,
    Released,
    RecoveryRequired,
}

/// Provenance is set by the coordinator from an admitted provider call, not
/// taken from a dependent request or a provider's claimed caller identity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredOutput {
    pub output: ResourceOutput,
    pub admission_id: String,
    pub transition_id: String,
    pub plan: ResourcePlan,
    pub state: OutputState,
    /// Exact broker-completed producer callback; missing legacy proof never
    /// authorizes materialization, cleanup or recovery.
    #[serde(default)]
    pub callback_proof: Option<ProducerCallbackProof>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProducerCallbackProof {
    pub participant: crate::adapter::ParticipantSettings,
    pub request: amaterasu_extension_api::ProviderRequest,
    pub terminal: crate::provider_dispatch::DispatchRecord,
    pub pending_receipt: Option<String>,
}

/// A stream-stage observation extends immutable session output provenance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StagedOutputRefinement {
    pub target: LifecycleTarget,
    pub parent_generation: u64,
    pub admission_id: String,
    pub transition_id: String,
    pub plan: ResourcePlan,
    pub before: ResourceOutput,
    pub after: ResourceOutput,
    pub callback_proof: ProducerCallbackProof,
}

/// Authenticated control confirmation retained alongside its original
/// pre-effect obligation; no field of that obligation is replaced.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObligationRefinement {
    pub request: ResourceControlRequest,
    pub original: ResourceObligation,
    pub confirmed: ConfirmedControl,
    pub owner_result: ResourceControlResponse,
    pub owner_receipt_id: Option<String>,
}

/// Frozen installed selection, broker approvals and exact upstream provenance.
/// The live coordinator must still recheck current grants, graph and observations.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionSelectionSnapshot {
    pub target: LifecycleTarget,
    pub transition_id: String,
    pub input: StoredOutput,
    pub control_policy: ControlPolicy,
    pub finalize_policy: FinalizePolicy,
    pub control_approval: MethodApproval,
    pub control_status_approval: MethodApproval,
    pub settlement_approval: MethodApproval,
    pub finalize_approval: MethodApproval,
    pub finalize_status_approval: MethodApproval,
    pub schema_documents: Vec<SchemaDocument>,
}

impl CompositionSelectionSnapshot {
    /// This checks internal consistency, not current authority or live state.
    pub fn validate_structure(&self) -> bool {
        let policy = &self.control_policy;
        let finalize = &self.finalize_policy;
        let output = &self.input.output;
        if !valid_target(&self.target)
            || !valid_text(&self.transition_id)
            || self.input.state != OutputState::Retained
            || !valid_stored(&self.input)
            || !target_allows(&output.origin, &self.target)
            || policy.owner_provider != output.producer
            || policy.owner_service != output.producer_service
            || policy.output_capability != output.capability
            || policy.output_schema != output.schema
            || finalize.owner_provider != output.producer
            || finalize.owner_service != output.producer_service
            || finalize.output_capability != output.capability
            || finalize.output_schema != output.schema
            || !approval_matches(
                &self.control_approval,
                &policy.owner_provider,
                &policy.owner_method,
            )
            || !approval_matches(
                &self.finalize_approval,
                &finalize.owner_provider,
                &finalize.method,
            )
            || !approval_matches(
                &self.control_status_approval,
                &policy.owner_provider,
                &policy.status_method,
            )
            || !approval_matches(
                &self.settlement_approval,
                &policy.owner_provider,
                &policy.settlement_method,
            )
            || !approval_matches(
                &self.finalize_status_approval,
                &finalize.owner_provider,
                &finalize.status_method,
            )
            || self.schema_documents.len() > 32
            || self
                .schema_documents
                .iter()
                .map(|schema| schema.document.len())
                .sum::<usize>()
                > 12_000
        {
            return false;
        }
        let catalog = amaterasu_extension_api::schemas::SchemaCatalog::compile(
            self.schema_documents
                .iter()
                .map(|doc| (doc.reference.clone(), doc.document.as_str())),
        );
        catalog.is_ok_and(|catalog| {
            catalog.contains(&policy.argument_schema)
                && catalog.validate(&output.schema, &output.data).is_ok()
        })
    }
}

fn approval_is_valid(approval: &MethodApproval, provider: &str) -> bool {
    let method = &approval.method;
    let data = &approval.broker_approval;
    valid_text(&method.service)
        && valid_text(&method.method)
        && method.interface_major != 0
        && method.schema_revision != 0
        && data["provider"].as_str() == Some(provider)
        && data["method"]["service"].as_str() == Some(&method.service)
        && data["method"]["method"].as_str() == Some(&method.method)
        && data["method"]["interface_major"].as_u64() == Some(u64::from(method.interface_major))
        && data["method"]["interface_minor"].as_u64() == Some(u64::from(method.interface_minor))
}
fn approval_matches(
    approval: &MethodApproval,
    provider: &str,
    expected: &amaterasu_extension_api::MethodRef,
) -> bool {
    let method = &approval.method;
    approval_is_valid(approval, provider)
        && method.service == expected.service
        && method.method == expected.method
        && method.interface_major == expected.interface_major
        && method.interface_minor == expected.interface_minor
        && method.schema_revision == expected.schema_revision
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionBook {
    outputs: Vec<StoredOutput>,
    /// Selected dependencies and approvals survive completed work and pause.
    #[serde(default)]
    selections: Vec<CompositionSelectionSnapshot>,
    #[serde(default)]
    finalize_selections: Vec<FinalizeSelectionSnapshot>,
    #[serde(default)]
    refinements: Vec<ObligationRefinement>,
    #[serde(default)]
    staged_outputs: Vec<StagedOutputRefinement>,
}

/// A policy-selected direct dependency. Schema is immutable and pinned by digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputSelection {
    pub producer: String,
    pub service: String,
    pub capability: String,
    pub schema: SchemaRef,
}

impl CompositionBook {
    /// Validate decoded state against the still-installed schema and selection
    /// policy before relying on it after recovery. Stored observations are not
    /// evidence that a native resource remains present after restart.
    pub fn validate(&self, verify_schema: &impl Fn(&ResourceOutput) -> bool) -> bool {
        self.validate_structure()
            && self.outputs.iter().all(|item| verify_schema(&item.output))
            && self
                .staged_outputs
                .iter()
                .all(|stage| verify_schema(&stage.after))
    }

    /// Structural checks do not authorize outputs or validate schema semantics.
    pub fn validate_structure(&self) -> bool {
        self.outputs.len() <= MAX_OUTPUTS
            && self.selections.len() <= MAX_OUTPUTS
            && self.finalize_selections.len() <= MAX_OUTPUTS
            && self.refinements.len() <= MAX_OUTPUTS
            && self.staged_outputs.len() <= MAX_OUTPUTS
            && self
                .staged_outputs
                .iter()
                .enumerate()
                .all(|(index, stage)| self.validate_stage_at(index, stage))
            && self.outputs.iter().enumerate().all(|(index, item)| {
                valid_stored(item)
                    && !self.outputs[..index]
                        .iter()
                        .any(|old| same_key(&old.output, &item.output))
            })
            && self
                .selections
                .iter()
                .enumerate()
                .all(|(index, selection)| {
                    selection.validate_structure()
                        && !self.selections[..index]
                            .iter()
                            .any(|old| same_selection(old, selection))
                })
            && self
                .finalize_selections
                .iter()
                .enumerate()
                .all(|(index, selection)| {
                    selection.validate_structure()
                        && !self.finalize_selections[..index]
                            .iter()
                            .any(|old| same_finalize_selection(old, selection))
                })
            && self.refinements.iter().enumerate().all(|(index, item)| {
                valid_refinement_shape(item)
                    && !self.refinements[..index].iter().any(|old| {
                        old.request.permit_id == item.request.permit_id
                            && old.original.obligation_id == item.original.obligation_id
                    })
            })
    }

    pub fn records(&self) -> impl Iterator<Item = &StoredOutput> {
        self.outputs.iter()
    }

    pub fn references_admission(&self, admission_id: &str) -> bool {
        self.staged_outputs
            .iter()
            .any(|stage| stage.admission_id == admission_id)
            || self
                .outputs
                .iter()
                .any(|item| item.admission_id == admission_id)
            || self
                .selections
                .iter()
                .any(|selection| selection.input.admission_id == admission_id)
            || self
                .finalize_selections
                .iter()
                .any(|selection| selection.input.admission_id == admission_id)
            || self
                .refinements
                .iter()
                .any(|item| item.request.admission_id == admission_id)
    }

    pub fn staged_outputs(&self) -> impl DoubleEndedIterator<Item = &StagedOutputRefinement> {
        self.staged_outputs.iter()
    }

    /// A target may use only its one exact staged result, and only while the
    /// producing session output remains retained. Ambiguity is fenced.
    pub fn staged_for(
        &self,
        target: &LifecycleTarget,
        producer: &str,
        output_id: &str,
    ) -> Result<&StagedOutputRefinement, &'static str> {
        let mut matches = self.staged_outputs.iter().filter(|stage| {
            &stage.target == target
                && stage.after.producer == producer
                && stage.after.output_id == output_id
        });
        let stage = matches.next().ok_or("staged_output_missing")?;
        if matches.next().is_some() || self.get(producer, output_id, &stage.after.origin).is_none()
        {
            return Err("staged_output_ambiguous_or_fenced");
        }
        Ok(stage)
    }

    fn validate_stage_at(&self, index: usize, stage: &StagedOutputRefinement) -> bool {
        let LifecycleTarget::Session {
            host_epoch,
            session_id,
            generation,
        } = &stage.before.origin
        else {
            return false;
        };
        let LifecycleTarget::Stream {
            host_epoch: epoch,
            session_id: session,
            ..
        } = &stage.target
        else {
            return false;
        };
        let Some(root) = self
            .outputs
            .iter()
            .find(|record| same_key(&record.output, &stage.before))
        else {
            return false;
        };
        let prior = self.staged_outputs[..index]
            .iter()
            .rev()
            .find(|old| same_key(&old.after, &stage.before))
            .map_or(&root.output, |old| &old.after);
        let proof = &stage.callback_proof;
        let Ok(response) = serde_json::from_value::<amaterasu_extension_api::ProviderResponse>(
            match &proof.terminal.state {
                crate::provider_dispatch::DispatchState::Completed { payload } => payload.clone(),
                _ => return false,
            },
        ) else {
            return false;
        };
        host_epoch == epoch
            && session_id == session
            && *generation == stage.parent_generation
            && prior == &stage.before
            && amaterasu_extension_api::validate_output_refinement(&stage.before, &stage.after)
                .is_ok()
            && (stage.before.data == stage.after.data
                || stage.after.resource.revision > stage.before.resource.revision)
            && valid_text(&stage.admission_id)
            && valid_text(&stage.transition_id)
            && proof.request.version == amaterasu_extension_api::COMPOSITION_VERSION
            && proof.request.operation
                == amaterasu_extension_api::ProviderOperation::LifecycleCallback
            && proof.request.target == stage.target
            && proof.request.phase == amaterasu_extension_api::LifecyclePhase::StreamPrepare
            && proof.request.transition_id == stage.transition_id
            && proof.request.admission_id.as_deref() == Some(stage.admission_id.as_str())
            && proof.request.approved_plan.as_ref() == Some(&stage.plan)
            && matches!(&response, amaterasu_extension_api::ProviderResponse::Prepared { outputs, .. } if outputs.contains(&stage.after))
            && valid_callback_proof(proof, &response)
            && !self.staged_outputs[..index]
                .iter()
                .any(|old| old.admission_id == stage.admission_id)
    }

    pub fn record_stage(
        &mut self,
        stage: StagedOutputRefinement,
        verify: &impl Fn(&ResourceOutput) -> bool,
    ) -> Result<(), &'static str> {
        if let Some(old) = self
            .staged_outputs
            .iter()
            .find(|old| old.admission_id == stage.admission_id)
        {
            return if old == &stage {
                Ok(())
            } else {
                Err("staged_output_conflict")
            };
        }
        if self
            .get(
                &stage.before.producer,
                &stage.before.output_id,
                &stage.before.origin,
            )
            .is_none()
            || self.staged_outputs.len() >= MAX_OUTPUTS
            || !self.validate_stage_at(self.staged_outputs.len(), &stage)
            || !verify(&stage.after)
        {
            return Err("invalid_staged_output");
        }
        self.staged_outputs.push(stage);
        Ok(())
    }

    pub fn refinements(&self) -> impl Iterator<Item = &ObligationRefinement> {
        self.refinements.iter()
    }

    /// The immutable settlement identity is the consumed permit ID plus the
    /// original obligation ID. These selectors never grant authority by themselves.
    pub fn refinement(
        &self,
        permit_id: &str,
        obligation_id: &str,
    ) -> Option<&ObligationRefinement> {
        self.refinements.iter().find(|item| {
            item.request.permit_id == permit_id && item.original.obligation_id == obligation_id
        })
    }

    /// The consumed permit and admission must come from the locked journal;
    /// caller authenticates the owner result. The verifier checks installed
    /// schema semantics and a confirmed non-null native snapshot/readback.
    pub fn record_refinement(
        &mut self,
        item: ObligationRefinement,
        permit: &ControlPermit,
        admission: &Admission,
        verify_snapshot: &impl Fn(&ObligationRefinement) -> bool,
    ) -> Result<(), &'static str> {
        if !permit.consumed
            || permit.request != item.request
            || permit.context.target != item.request.target
            || permit.context.transition_id != item.request.transition_id
            || permit.context.output.schema != item.confirmed.output.schema
            || permit.context.output.origin != item.confirmed.output.origin
            || permit.context.output.capability != item.confirmed.output.capability
            || admission.provider != permit.context.policy.dependent_provider
            || !admission.plan.obligations.contains(&item.original)
            || !admission
                .plan
                .control_proposals
                .contains(&item.request.proposal)
            || !valid_refinement_shape(&item)
            || !verify_snapshot(&item)
        {
            return Err("invalid_obligation_refinement");
        }
        if let Some(old) = self.refinements.iter().find(|old| {
            old.request.permit_id == item.request.permit_id
                && old.original.obligation_id == item.original.obligation_id
        }) {
            return if old == &item {
                Ok(())
            } else {
                Err("obligation_refinement_conflict")
            };
        }
        if self.refinements.len() >= MAX_OUTPUTS {
            return Err("refinement_capacity");
        }
        self.refinements.push(item);
        Ok(())
    }

    pub fn selections(&self) -> impl Iterator<Item = &CompositionSelectionSnapshot> {
        self.selections.iter()
    }

    pub fn finalize_selections(&self) -> impl Iterator<Item = &FinalizeSelectionSnapshot> {
        self.finalize_selections.iter()
    }

    /// The verifier must compare this snapshot to installed policy, exact broker
    /// approvals, current resource observation and canonical parent generation.
    pub fn record_finalize_selection(
        &mut self,
        selection: FinalizeSelectionSnapshot,
        verify_installed: &impl Fn(&FinalizeSelectionSnapshot) -> bool,
    ) -> Result<(), &'static str> {
        let requires_stage = matches!(
            (&selection.input.output.origin, &selection.target),
            (
                LifecycleTarget::Session { .. },
                LifecycleTarget::Stream { .. }
            )
        );
        if !selection.validate_structure()
            || (requires_stage && selection.stage.is_none())
            || selection.stage.as_ref().is_some_and(|stage| {
                self.staged_for(
                    &selection.target,
                    &selection.input.output.producer,
                    &selection.input.output.output_id,
                )
                .ok()
                    != Some(stage)
            })
            || !verify_installed(&selection)
            || self.get(
                &selection.input.output.producer,
                &selection.input.output.output_id,
                &selection.input.output.origin,
            ) != Some(&selection.input)
        {
            return Err("invalid_finalize_selection");
        }
        if let Some(old) = self
            .finalize_selections
            .iter()
            .find(|old| same_finalize_selection(old, &selection))
        {
            return if old == &selection {
                Ok(())
            } else {
                Err("finalize_selection_conflict")
            };
        }
        if self.finalize_selections.len() >= MAX_OUTPUTS {
            return Err("finalize_selection_capacity");
        }
        self.finalize_selections.push(selection);
        Ok(())
    }

    pub fn finalize_selection(
        &self,
        target: &LifecycleTarget,
        transition_id: &str,
        owner: &str,
        output_id: &str,
    ) -> Option<&FinalizeSelectionSnapshot> {
        self.finalize_selections.iter().find(|item| {
            &item.target == target
                && item.transition_id == transition_id
                && item.input.output.producer == owner
                && item.input.output.output_id == output_id
        })
    }

    /// The verifier must resolve policy, graph edges, all method approvals and
    /// schemas from installed policy and broker state. Persist before dispatch.
    pub fn record_selection(
        &mut self,
        selection: CompositionSelectionSnapshot,
        verify_installed: &impl Fn(&CompositionSelectionSnapshot) -> bool,
    ) -> Result<(), &'static str> {
        if !selection.validate_structure()
            || !verify_installed(&selection)
            || self.get(
                &selection.input.output.producer,
                &selection.input.output.output_id,
                &selection.input.output.origin,
            ) != Some(&selection.input)
        {
            return Err("invalid_composition_selection");
        }
        if let Some(old) = self
            .selections
            .iter()
            .find(|old| same_selection(old, &selection))
        {
            return if old == &selection {
                Ok(())
            } else {
                Err("composition_selection_conflict")
            };
        }
        if self.selections.len() >= MAX_OUTPUTS {
            return Err("composition_selection_capacity");
        }
        self.selections.push(selection);
        Ok(())
    }

    pub fn selection(
        &self,
        target: &LifecycleTarget,
        transition_id: &str,
        dependent: &str,
        owner: &str,
        output_id: &str,
    ) -> Option<&CompositionSelectionSnapshot> {
        self.selections.iter().find(|item| {
            &item.target == target
                && item.transition_id == transition_id
                && item.control_policy.dependent_provider == dependent
                && item.input.output.producer == owner
                && item.input.output.output_id == output_id
        })
    }

    /// Verify the response against the exact selected producer and admitted
    /// plan; callers must also verify admitted target and installed schema bytes.
    /// Retried identical records are idempotent; changed reuse conflicts.
    pub fn record(
        &mut self,
        item: StoredOutput,
        selected: &OutputSelection,
        expected_target: &LifecycleTarget,
        verify_schema: &impl Fn(&ResourceOutput) -> bool,
    ) -> Result<(), &'static str> {
        if item.state != OutputState::Retained
            || item.output.origin != *expected_target
            || item.output.producer != selected.producer
            || item.output.producer_service != selected.service
            || item.output.capability != selected.capability
            || item.output.schema != selected.schema
            || !valid_stored(&item)
            || !verify_schema(&item.output)
        {
            return Err("invalid_provider_output");
        }
        if let Some(old) = self
            .outputs
            .iter()
            .find(|old| same_key(&old.output, &item.output))
        {
            return if old == &item {
                Ok(())
            } else {
                Err("provider_output_conflict")
            };
        }
        if self.outputs.len() >= MAX_OUTPUTS {
            return Err("provider_output_capacity");
        }
        self.outputs.push(item);
        Ok(())
    }

    /// Resolve policy-selected direct dependencies for a target. Session outputs
    /// feed only child streams of the same host/session; stream outputs feed only
    /// the identical stream generation. Caller must verify canonical parent and
    /// current resource observation independently before use.
    pub fn inputs(
        &self,
        target: &LifecycleTarget,
        selections: &[OutputSelection],
    ) -> Result<Vec<StoredOutput>, &'static str> {
        if selections.len() > MAX_INPUTS {
            return Err("dependency_capacity");
        }
        let mut result: Vec<StoredOutput> = Vec::new();
        for selection in selections {
            let mut matches = self.outputs.iter().filter(|record| {
                let output = &record.output;
                record.state == OutputState::Retained
                    && output.producer == selection.producer
                    && output.producer_service == selection.service
                    && output.capability == selection.capability
                    && output.schema == selection.schema
                    && target_allows(&output.origin, target)
            });
            let record = matches.next().ok_or("dependency_output_missing")?;
            if matches.next().is_some()
                || result
                    .iter()
                    .any(|old| same_key(&old.output, &record.output))
            {
                return Err("dependency_output_ambiguous");
            }
            result.push(record.clone());
        }
        Ok(result)
    }

    /// Look up a coordinator-owned record for exact control admission.
    pub fn get(
        &self,
        producer: &str,
        output_id: &str,
        origin: &LifecycleTarget,
    ) -> Option<&StoredOutput> {
        self.outputs.iter().find(|record| {
            record.output.producer == producer
                && record.output.output_id == output_id
                && &record.output.origin == origin
                && record.state == OutputState::Retained
        })
    }

    /// Cached observations cannot establish native presence after host restart.
    pub fn fence_after_restart(&mut self) {
        for record in &mut self.outputs {
            if record.state == OutputState::Retained {
                record.state = OutputState::RecoveryRequired;
            }
        }
    }

    /// Move to a recovery fence or confirmed release only after the coordinator
    /// proves the appropriate consumer retirement and native outcome.
    pub fn set_state(
        &mut self,
        producer: &str,
        output_id: &str,
        origin: &LifecycleTarget,
        state: OutputState,
    ) -> Result<(), &'static str> {
        let record = self
            .outputs
            .iter_mut()
            .find(|record| {
                record.output.producer == producer
                    && record.output.output_id == output_id
                    && &record.output.origin == origin
            })
            .ok_or("dependency_output_missing")?;
        if record.state == OutputState::Released && state != OutputState::Released {
            return Err("output_already_released");
        }
        record.state = state;
        Ok(())
    }
}

fn same_key(a: &ResourceOutput, b: &ResourceOutput) -> bool {
    a.producer == b.producer && a.output_id == b.output_id && a.origin == b.origin
}
fn valid_refinement_shape(item: &ObligationRefinement) -> bool {
    let request = &item.request;
    let proposal = &request.proposal;
    let output = &item.confirmed.output;
    valid_text(&request.operation_id)
        && valid_text(&request.transition_id)
        && valid_text(&request.admission_id)
        && valid_text(&request.permit_id)
        && request.provider_sequence > 0
        && request.target == proposal.target
        && item.confirmed.proposal_id == proposal.proposal_id
        && item
            .owner_receipt_id
            .as_ref()
            .is_none_or(|id| valid_text(id))
        && matches!(&item.owner_result, ResourceControlResponse::Applied { output, candidate }
            if output.as_ref() == &item.confirmed.output && candidate.as_deref() == item.confirmed.candidate.as_ref())
        && valid_text(&item.original.obligation_id)
        && valid_text(&item.original.provider)
        && item.original.payload.is_object()
        && output.output_id == proposal.output_id
        && output.producer == proposal.owner_provider
        && output.producer_service == proposal.owner_service
        && output.resource.provider == proposal.resource.provider
        && output.resource.resource_id == proposal.resource.resource_id
        && output.resource.incarnation == proposal.resource.incarnation
        && output.resource.execution_domain == proposal.resource.execution_domain
        && output.resource.revision >= proposal.resource.revision
        && output.resource.mutation_domains == proposal.resource.mutation_domains
        && output.resource.mutation_domain == proposal.resource.mutation_domain
        && output.data.is_object()
        && serde_json::to_vec(item).is_ok_and(|bytes| bytes.len() <= 12 * 1024)
}
fn same_selection(a: &CompositionSelectionSnapshot, b: &CompositionSelectionSnapshot) -> bool {
    a.target == b.target
        && a.transition_id == b.transition_id
        && a.control_policy.dependent_provider == b.control_policy.dependent_provider
        && same_key(&a.input.output, &b.input.output)
}
fn same_finalize_selection(a: &FinalizeSelectionSnapshot, b: &FinalizeSelectionSnapshot) -> bool {
    a.target == b.target
        && a.transition_id == b.transition_id
        && same_key(&a.input.output, &b.input.output)
}

/// On recovery, structural self-consistency is insufficient: reconnect every
/// frozen finalization to the canonical target, transition and producer record.
pub(super) fn validate_finalize_selections(journal: &super::Journal) -> bool {
    journal.composition.finalize_selections().all(|selection| {
        let Ok((epoch, session_id, stream_id, generation)) =
            super::operations::target_parts(&selection.target)
        else {
            return false;
        };
        let Some(session) = journal.sessions.get(session_id) else {
            return false;
        };
        if session.host_epoch != epoch {
            return false;
        }
        match stream_id {
            Some(id)
                if session
                    .streams
                    .get(id)
                    .is_some_and(|stream| stream.generation == generation)
                    && selection.parent_generation == Some(session.generation) => {}
            None if session.generation == generation && selection.parent_generation.is_none() => {}
            _ => return false,
        }
        if let LifecycleTarget::Session {
            generation: origin_generation,
            ..
        } = &selection.input.output.origin
            && *origin_generation != session.generation
        {
            return false;
        }
        let requires_stage = matches!(
            (&selection.input.output.origin, &selection.target),
            (
                LifecycleTarget::Session { .. },
                LifecycleTarget::Stream { .. }
            )
        );
        if (requires_stage && selection.stage.is_none())
            || selection.stage.as_ref().is_some_and(|stage| {
                !journal
                    .composition
                    .staged_outputs()
                    .any(|item| item == stage)
            })
        {
            return false;
        }
        let output = &selection.input.output;
        let Some(canonical) = journal
            .composition
            .records()
            .find(|item| same_key(&item.output, output))
        else {
            return false;
        };
        if canonical.output != *output
            || canonical.admission_id != selection.input.admission_id
            || canonical.transition_id != selection.input.transition_id
            || canonical.plan != selection.input.plan
            || canonical.callback_proof != selection.input.callback_proof
        {
            return false;
        }
        journal.receipts.values().any(|receipt| {
            receipt.work.as_ref().is_some_and(|work| {
                work.target == selection.target && work.transition_id == selection.transition_id
            }) || (receipt.response["transition_id"].as_str() == Some(&selection.transition_id)
                && receipt
                    .response
                    .get("target")
                    .and_then(|value| serde_json::from_value::<LifecycleTarget>(value.clone()).ok())
                    .as_ref()
                    == Some(&selection.target))
        })
    })
}

/// Collect only an authenticated driver's completed v2 callback. The caller
/// owns the coordinator journal lock and persists the resulting journal in the
/// same transaction as the participant's obligations and progress. This does
/// not activate v2 dispatch, finalization, or downstream execution.
pub(super) fn collect_verified_outputs(
    journal: &mut super::Journal,
    settings: &crate::adapter::LifecycleSettings,
    selections: &std::collections::BTreeMap<String, crate::prepare_authority::ProviderSelection>,
    work: &super::Work,
    provider: &str,
    state: &crate::prepare_driver::PrepareState,
) -> Result<(), String> {
    use crate::prepare_driver::PreparePhase;
    use amaterasu_extension_api::{
        COMPOSITION_VERSION, LifecyclePhase, ProviderOperation, ProviderResponse,
    };
    let response = state.response.as_ref().ok_or("provider_response_missing")?;
    let ProviderResponse::Prepared { outputs, .. } = response else {
        return Err("provider_response_invalid".into());
    };
    if state.phase != PreparePhase::Prepared
        || state.provider != provider
        || state.request.version != COMPOSITION_VERSION
        || state.request.operation != ProviderOperation::LifecycleCallback
        || !matches!(
            work.phase,
            LifecyclePhase::SessionPrepare | LifecyclePhase::StreamPrepare
        )
        || state.request.phase != work.phase
        || state.request.target != work.target
        || state.request.transition_id != work.transition_id
        || state.request.admission_id != state.admission_id
    {
        return Err("provider_output_context_invalid".into());
    }
    amaterasu_extension_api::validate_provider_response(&state.request, response)?;
    let persisted = journal.receipts.values().any(|receipt| {
        receipt.work.as_ref().is_some_and(|saved| {
            saved.target == work.target
                && saved.transition_id == work.transition_id
                && saved.states.get(provider).is_some_and(|saved_state| {
                    serde_json::to_value(saved_state).ok() == serde_json::to_value(state).ok()
                })
        })
    });
    if !persisted {
        return Err("provider_result_not_journaled".into());
    }
    let selected = selections.get(provider).ok_or("provider_not_selected")?;
    if selected.participant.provider != provider
        || state.participant.provider != provider
        || selected.participant.callback_method != state.participant.callback_method
        || !selected.approvals.iter().any(|entry| {
            entry.method == state.participant.callback_method && approval_is_valid(entry, provider)
        })
    {
        return Err("provider_approval_mismatch".into());
    }
    let proof = ProducerCallbackProof {
        participant: state.participant.clone(),
        request: state.request.clone(),
        terminal: state
            .terminal_dispatch
            .clone()
            .ok_or("provider_terminal_proof_missing")?,
        pending_receipt: state.pending_receipt.clone(),
    };
    if !valid_callback_proof(&proof, response)
        || !selected.approvals.iter().any(|entry| {
            entry.broker_approval == proof.terminal.call.approval
                && approval_matches_call(entry, &proof.terminal.call)
                && approval_is_valid(entry, provider)
        })
    {
        return Err("provider_terminal_proof_invalid".into());
    }
    let admission_id = state.admission_id.as_deref().ok_or("admission_missing")?;
    let admission = journal
        .authority
        .admissions
        .get(admission_id)
        .or_else(|| journal.authority.settled.get(admission_id))
        .ok_or("admission_missing")?;
    let plan = state.plan.as_ref().ok_or("plan_missing")?;
    if admission.provider != provider
        || &admission.plan != plan
        || state.request.approved_plan.as_ref() != Some(plan)
        || journal
            .admission_transitions
            .get(admission_id)
            .map(String::as_str)
            != Some(work.transition_id.as_str())
    {
        return Err("admission_provenance_mismatch".into());
    }
    let (_, session_id, _, _) = super::operations::target_parts(&work.target)?;
    let session = journal.sessions.get(session_id).ok_or("unknown_session")?;
    if session.admission_targets.get(admission_id) != Some(&work.target)
        || session.admission_phases.get(admission_id) != Some(&work.phase)
    {
        return Err("admission_target_mismatch".into());
    }
    let graph = work
        .provider_graph
        .as_ref()
        .ok_or("provider_graph_missing")?;
    if !graph.order.iter().any(|id| id == provider) {
        return Err("provider_graph_mismatch".into());
    }
    let catalog = settings.schema_catalog()?;
    let mut next = journal.composition.clone();
    for output in outputs {
        let control_selected = settings.control_policies.iter().any(|policy| {
            policy.owner_provider == provider
                && policy.owner_service == output.producer_service
                && policy.output_capability == output.capability
                && policy.output_schema == output.schema
                && graph
                    .dependencies
                    .get(&policy.dependent_provider)
                    .is_some_and(|deps| deps.iter().any(|id| id == provider))
        });
        let finalize_selected = settings.finalize_policies.iter().any(|policy| {
            policy.owner_provider == provider
                && policy.owner_service == output.producer_service
                && policy.output_capability == output.capability
                && policy.output_schema == output.schema
        });
        if !control_selected && !finalize_selected {
            return Err("output_not_selected".into());
        }
        let output_selection = OutputSelection {
            producer: provider.into(),
            service: output.producer_service.clone(),
            capability: output.capability.clone(),
            schema: output.schema.clone(),
        };
        if matches!(
            (&output.origin, &work.target),
            (
                LifecycleTarget::Session { .. },
                LifecycleTarget::Stream { .. }
            )
        ) {
            let root = next
                .get(provider, &output.output_id, &output.origin)
                .ok_or("stage_root_missing")?;
            let before = next
                .staged_outputs()
                .rev()
                .find(|old| same_key(&old.after, output))
                .map_or_else(|| root.output.clone(), |old| old.after.clone());
            let LifecycleTarget::Session {
                generation: parent_generation,
                ..
            } = &output.origin
            else {
                unreachable!()
            };
            let stage = StagedOutputRefinement {
                target: work.target.clone(),
                parent_generation: *parent_generation,
                admission_id: admission_id.into(),
                transition_id: work.transition_id.clone(),
                plan: plan.clone(),
                before,
                after: output.clone(),
                callback_proof: proof.clone(),
            };
            next.record_stage(stage, &|value| {
                catalog.validate(&value.schema, &value.data).is_ok()
            })
            .map_err(str::to_owned)?;
            continue;
        }
        let record = StoredOutput {
            output: output.clone(),
            admission_id: admission_id.into(),
            transition_id: work.transition_id.clone(),
            plan: plan.clone(),
            state: OutputState::Retained,
            callback_proof: Some(proof.clone()),
        };
        next.record(record, &output_selection, &work.target, &|value| {
            amaterasu_extension_api::validate_resource_output(value).is_ok()
                && catalog.validate(&value.schema, &value.data).is_ok()
        })
        .map_err(str::to_owned)?;
    }
    journal.composition = next;
    Ok(())
}
pub(super) fn approval_matches_call(
    approval: &MethodApproval,
    call: &crate::provider_dispatch::ProviderCall,
) -> bool {
    approval.method.service == call.service
        && approval.method.method == call.method
        && approval.method.interface_major == call.interface_major
        && approval.method.interface_minor == call.interface_minor
}

fn valid_callback_proof(
    proof: &ProducerCallbackProof,
    response: &amaterasu_extension_api::ProviderResponse,
) -> bool {
    use crate::provider_dispatch::DispatchState;
    let call = &proof.terminal.call;
    let DispatchState::Completed { payload } = &proof.terminal.state else {
        return false;
    };
    if serde_json::from_value::<amaterasu_extension_api::ProviderResponse>(payload.clone())
        .ok()
        .as_ref()
        != Some(response)
        || amaterasu_extension_api::validate_provider_response(&proof.request, response).is_err()
        || call
            .approval
            .get("provider")
            .and_then(serde_json::Value::as_str)
            != Some(proof.participant.provider.as_str())
    {
        return false;
    }
    let callback = &proof.participant.callback_method;
    let status = &proof.participant.status_method;
    let matches_method = |method: &crate::adapter::MethodSettings| {
        call.service == method.service
            && call.method == method.method
            && call.interface_major == method.interface_major
            && call.interface_minor == method.interface_minor
    };
    if matches_method(callback) {
        return call.operation_id == proof.request.operation_id
            && serde_json::to_value(&proof.request).ok().as_ref() == Some(&call.arguments);
    }
    if !matches_method(status) {
        return false;
    }
    let Ok(request) = serde_json::from_value::<amaterasu_extension_api::ProviderStatusRequest>(
        call.arguments.clone(),
    ) else {
        return false;
    };
    request.operation_id == proof.request.operation_id
        && request.transition_id == proof.request.transition_id
        && Some(&request.receipt_id) == proof.pending_receipt.as_ref()
        && request.provider_sequence > proof.request.provider_sequence
}

pub(super) fn validate_callback_provenance(record: &StoredOutput, admission: &Admission) -> bool {
    let Some(proof) = &record.callback_proof else {
        return false;
    };
    let Ok(response) = serde_json::from_value::<amaterasu_extension_api::ProviderResponse>(
        match &proof.terminal.state {
            crate::provider_dispatch::DispatchState::Completed { payload } => payload.clone(),
            _ => return false,
        },
    ) else {
        return false;
    };
    amaterasu_extension_api::validate_provider_request(&proof.request).is_ok()
        && proof.request.version == amaterasu_extension_api::COMPOSITION_VERSION
        && proof.request.operation == amaterasu_extension_api::ProviderOperation::LifecycleCallback
        && matches!(
            proof.request.phase,
            amaterasu_extension_api::LifecyclePhase::SessionPrepare
                | amaterasu_extension_api::LifecyclePhase::StreamPrepare
        )
        && proof.participant == admission.selection.participant
        && proof.request.target == record.output.origin
        && proof.request.admission_id.as_deref() == Some(record.admission_id.as_str())
        && proof.request.transition_id == record.transition_id
        && proof.request.approved_plan.as_ref() == Some(&record.plan)
        && admission.selection.approvals.iter().any(|approval| {
            approval.broker_approval == proof.terminal.call.approval
                && approval_matches_call(approval, &proof.terminal.call)
                && approval_is_valid(approval, &admission.provider)
        })
        && matches!(&response, amaterasu_extension_api::ProviderResponse::Prepared { outputs, .. } if outputs.contains(&record.output))
        && valid_callback_proof(proof, &response)
}

fn valid_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}
fn valid_stored(item: &StoredOutput) -> bool {
    let output = &item.output;
    if !valid_text(&item.admission_id)
        || !valid_text(&item.transition_id)
        || !valid_text(&output.output_id)
        || !valid_text(&output.capability)
        || !valid_text(&output.producer)
        || !valid_text(&output.producer_service)
        || !valid_target(&output.origin)
        || !valid_text(&output.resource.resource_id)
        || !valid_text(&output.resource.incarnation)
        || !valid_text(&output.resource.execution_domain)
        || output.resource.revision == 0
        || output
            .resource
            .mutation_domains
            .iter()
            .collect::<BTreeSet<_>>()
            .len()
            != output.resource.mutation_domains.len()
        || !valid_text(&output.schema.id)
        || output.schema.major == 0
        || output.schema.digest.len() != 64
        || !output
            .schema
            .digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || output.resource.provider != output.producer
        || !output.data.is_object()
        || amaterasu_extension_api::validate_resource_output(output).is_err()
        || !serde_json::to_vec(output).is_ok_and(|v| v.len() <= MAX_BYTES)
        || amaterasu_extension_api::validate_resource_plan(&item.plan).is_err()
    {
        return false;
    }
    item.plan.resources.iter().any(|planned| {
        let resource = &output.resource;
        planned.provider == resource.provider
            && planned.resource_id == resource.resource_id
            && planned.execution_domain == resource.execution_domain
            && planned
                .incarnation
                .as_ref()
                .is_none_or(|v| v == &resource.incarnation)
            && planned
                .expected_revision
                .is_none_or(|revision| match planned.access {
                    ResourceAccess::Read => resource.revision == revision,
                    _ => resource.revision >= revision,
                })
            && resource
                .mutation_domain
                .as_ref()
                .is_none_or(|v| planned.mutation_domains.contains(v))
            && resource.mutation_domains.iter().collect::<BTreeSet<_>>()
                == planned.mutation_domains.iter().collect::<BTreeSet<_>>()
    })
}
fn valid_target(target: &LifecycleTarget) -> bool {
    match target {
        LifecycleTarget::Session {
            host_epoch,
            session_id,
            generation,
        } => valid_text(host_epoch) && valid_text(session_id) && *generation > 0,
        LifecycleTarget::Stream {
            host_epoch,
            session_id,
            stream_id,
            generation,
        } => {
            valid_text(host_epoch)
                && valid_text(session_id)
                && valid_text(stream_id)
                && *generation > 0
        }
        LifecycleTarget::Validation { .. } => false,
    }
}
fn target_allows(origin: &LifecycleTarget, target: &LifecycleTarget) -> bool {
    match (origin, target) {
        (
            LifecycleTarget::Session {
                host_epoch: a,
                session_id: b,
                generation: c,
            },
            LifecycleTarget::Session {
                host_epoch: x,
                session_id: y,
                generation: z,
            },
        ) => a == x && b == y && c == z,
        (
            LifecycleTarget::Session {
                host_epoch: a,
                session_id: b,
                ..
            },
            LifecycleTarget::Stream {
                host_epoch: x,
                session_id: y,
                ..
            },
        ) => a == x && b == y,
        (
            LifecycleTarget::Stream {
                host_epoch: a,
                session_id: b,
                stream_id: c,
                generation: d,
            },
            LifecycleTarget::Stream {
                host_epoch: w,
                session_id: x,
                stream_id: y,
                generation: z,
            },
        ) => a == w && b == x && c == y && d == z,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn target() -> LifecycleTarget {
        LifecycleTarget::Session {
            host_epoch: "epoch".into(),
            session_id: "session".into(),
            generation: 1,
        }
    }
    fn output() -> ResourceOutput {
        serde_json::from_value(json!({"output_id":"output", "capability":"screen", "schema":{"id":"screen/2","major":1,"minor":0,"digest":"a".repeat(64)}, "producer":"screen", "producer_service":"screen/2", "origin":target(), "resource":{"provider":"screen","resource_id":"display","incarnation":"native","revision":2,"execution_domain":"seat","mutation_domain":null,"mutation_domains":[]},"data":{"desired":true,"observed":false}})).unwrap()
    }
    fn record() -> StoredOutput {
        let plan: ResourcePlan = serde_json::from_value(json!({"plan_id":"plan", "revision":1, "resources":[{"provider":"screen","resource_id":"display","incarnation":null,"expected_revision":1,"execution_domain":"seat","mutation_domains":[],"access":"exclusive"}],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]})).unwrap();
        StoredOutput {
            output: output(),
            admission_id: "admission".into(),
            transition_id: "transition".into(),
            plan,
            state: OutputState::Retained,
            callback_proof: None,
        }
    }
    fn selection() -> OutputSelection {
        let output = output();
        OutputSelection {
            producer: output.producer,
            service: output.producer_service,
            capability: output.capability,
            schema: output.schema,
        }
    }

    #[test]
    fn staged_output_keeps_root_and_requires_exact_callback_lineage() {
        let mut book = CompositionBook::default();
        let root = record();
        book.record(root.clone(), &selection(), &target(), &|_| true)
            .unwrap();
        let stream = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1});
        let mut plan = root.plan.clone();
        plan.resources[0].expected_revision = Some(root.output.resource.revision);
        let method = json!({"service":"screen/2","method":"callback","interface_major":2,"interface_minor":0,"schema_revision":1});
        let participant = serde_json::from_value(json!({"provider":"screen","plan_method":method,"callback_method":method,"status_method":method,"capabilities":[],"cleanup_methods":[],"settings_revision":"revision","settings":{}})).unwrap();
        let request: amaterasu_extension_api::ProviderRequest = serde_json::from_value(json!({"provider_sequence":2,"operation":"lifecycle_callback","version":2,"transition_id":"stage-transition","operation_id":"stage-call","target":stream,"phase":"stream.prepare","settings_revision":"revision","settings":{},"intent":"initial","reason":null,"requested_video":null,"consumer_requirements":{"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["selector/1"]},"remaining_ms":1000,"plan_revision":plan.revision,"admission_id":"stage-admission","approved_plan":plan})).unwrap();
        let response: amaterasu_extension_api::ProviderResponse = serde_json::from_value(
            json!({"status":"prepared","candidate":null,"obligations":[],"outputs":[root.output]}),
        )
        .unwrap();
        let terminal = crate::provider_dispatch::DispatchRecord {
            call: crate::provider_dispatch::ProviderCall {
                service: "screen/2".into(),
                method: "callback".into(),
                interface_major: 2,
                interface_minor: 0,
                approval: json!({"provider":"screen"}),
                arguments: serde_json::to_value(&request).unwrap(),
                timeout_ms: 1000,
                operation_id: "stage-call".into(),
            },
            state: crate::provider_dispatch::DispatchState::Completed {
                payload: serde_json::to_value(response).unwrap(),
            },
        };
        let stage = StagedOutputRefinement {
            target: serde_json::from_value(stream).unwrap(),
            parent_generation: 1,
            admission_id: "stage-admission".into(),
            transition_id: "stage-transition".into(),
            plan,
            before: root.output.clone(),
            after: root.output.clone(),
            callback_proof: ProducerCallbackProof {
                participant,
                request,
                terminal,
                pending_receipt: None,
            },
        };
        let mut wrong_parent = stage.clone();
        wrong_parent.parent_generation += 1;
        assert!(book.record_stage(wrong_parent, &|_| true).is_err());
        let mut wrong_call = stage.clone();
        wrong_call.callback_proof.terminal.call.arguments["operation_id"] = json!("different");
        assert!(book.record_stage(wrong_call, &|_| true).is_err());
        book.record_stage(stage.clone(), &|_| true).unwrap();
        assert_eq!(book.records().next().unwrap(), &root);
        assert_eq!(
            book.staged_for(&stage.target, "screen", "output").unwrap(),
            &stage
        );
        let mut changed = stage.clone();
        changed.after.resource.revision += 1;
        assert!(book.record_stage(changed, &|_| true).is_err());
        let restored: CompositionBook =
            serde_json::from_value(serde_json::to_value(book).unwrap()).unwrap();
        assert!(restored.validate_structure());
    }

    #[test]
    fn exact_selected_output_roundtrips_and_feeds_only_its_session() {
        let mut book = CompositionBook::default();
        book.record(record(), &selection(), &target(), &|_| true)
            .unwrap();
        book.record(record(), &selection(), &target(), &|_| true)
            .unwrap();
        let encoded = serde_json::to_vec(&book).unwrap();
        let mut restored: CompositionBook = serde_json::from_slice(&encoded).unwrap();
        assert!(restored.validate(&|_| true));
        let child = LifecycleTarget::Stream {
            host_epoch: "epoch".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            generation: 1,
        };
        let inputs = restored.inputs(&child, &[selection()]).unwrap();
        assert_eq!(
            inputs[0].output.data,
            json!({"desired":true,"observed":false})
        );
        let foreign = LifecycleTarget::Stream {
            host_epoch: "epoch".into(),
            session_id: "other".into(),
            stream_id: "stream".into(),
            generation: 1,
        };
        assert!(restored.inputs(&foreign, &[selection()]).is_err());
        restored.fence_after_restart();
        assert!(restored.inputs(&child, &[selection()]).is_err());
    }

    #[test]
    fn rejects_forged_output_conflicts_and_unvalidated_schema() {
        let mut book = CompositionBook::default();
        assert!(
            book.record(record(), &selection(), &target(), &|_| false)
                .is_err()
        );
        let mut bad = record();
        bad.output.resource.incarnation.clear();
        assert!(
            book.record(bad, &selection(), &target(), &|_| true)
                .is_err()
        );
        let mut bad = record();
        bad.output.resource.resource_id = "other".into();
        assert!(
            book.record(bad, &selection(), &target(), &|_| true)
                .is_err()
        );
        book.record(record(), &selection(), &target(), &|_| true)
            .unwrap();
        let mut changed = record();
        changed.output.data = json!({"desired":false});
        assert_eq!(
            book.record(changed, &selection(), &target(), &|_| true),
            Err("provider_output_conflict")
        );
        assert!(book.inputs(&target(), &[selection(), selection()]).is_err());
    }

    #[test]
    fn rejects_journal_output_without_admission_provenance() {
        let mut book = CompositionBook::default();
        book.record(record(), &selection(), &target(), &|_| true)
            .unwrap();
        let journal: super::super::Journal = serde_json::from_value(json!({
            "version":3,"host_epoch":"epoch","receipts":{},"sessions":{},"composition":book
        }))
        .unwrap();
        assert!(super::super::validate_journal(&journal).is_err());
    }

    #[test]
    fn selection_pins_output_schemas_and_broker_approvals() {
        use sha2::{Digest, Sha256};
        let document = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","$id":"screen/2","type":"object"}"#;
        let mut stored = record();
        let mut materialized = serde_json::to_value(&stored).unwrap();
        materialized["output"]["resource"]["mutation_domain"] = json!("topology");
        materialized["output"]["resource"]["mutation_domains"] = json!(["topology"]);
        materialized["output"]["domain_observations"] =
            json!([{"domain":"topology","revision":1,"borrowers":{},"complete":true}]);
        materialized["plan"]["resources"][0]["mutation_domains"] = json!(["topology"]);
        materialized["plan"]["mutation_domains"] = json!([{"domain":"topology","expected_revision":1,"borrowers":{},"access":"exclusive"}]);
        stored = serde_json::from_value(materialized).unwrap();
        stored.output.schema.digest = format!("{:x}", Sha256::digest(document.as_bytes()));
        let schema = stored.output.schema.clone();
        let method = json!({"service":"screen/2","method":"control","interface_major":2,"interface_minor":0,"schema_revision":1});
        let status = json!({"service":"screen/2","method":"status","interface_major":2,"interface_minor":0,"schema_revision":1});
        let approval = |m: &Value| {
            serde_json::from_value::<MethodApproval>(
                json!({"method":m,"broker_approval":{"provider":"screen","method":m}}),
            )
            .unwrap()
        };
        let policy: ControlPolicy = serde_json::from_value(json!({"dependent_provider":"settings","owner_provider":"screen","owner_service":"screen/2","owner_method":method,"status_method":status,"settlement_method":method,"argument_schema":schema,"output_capability":"screen","output_schema":schema,"max_access":"exclusive","mutation_domains":[]})).unwrap();
        let finalize: FinalizePolicy = serde_json::from_value(json!({"owner_provider":"screen","owner_service":"screen/2","method":method,"status_method":status,"output_capability":"screen","output_schema":schema,"capture_provider":"capture","selector_schema":"selector/1","required_access":"exclusive","mutation_domains":["topology"]})).unwrap();
        let snapshot = CompositionSelectionSnapshot {
            target: target(),
            transition_id: "consumer-transition".into(),
            input: stored.clone(),
            control_policy: policy,
            finalize_policy: finalize,
            control_approval: approval(&method),
            control_status_approval: approval(&status),
            settlement_approval: approval(&method),
            finalize_approval: approval(&method),
            finalize_status_approval: approval(&status),
            schema_documents: vec![SchemaDocument {
                reference: schema,
                document: document.into(),
            }],
        };
        assert!(snapshot.validate_structure());
        let mut wrong_status = snapshot.clone();
        wrong_status.control_status_approval.method.method = "other".into();
        wrong_status.control_status_approval.broker_approval["method"]["method"] = json!("other");
        assert!(!wrong_status.validate_structure());
        let mut wrong_finalize_status = snapshot.clone();
        wrong_finalize_status
            .finalize_status_approval
            .method
            .schema_revision += 1;
        assert!(!wrong_finalize_status.validate_structure());
        let mut book = CompositionBook::default();
        let selected = OutputSelection {
            producer: stored.output.producer.clone(),
            service: stored.output.producer_service.clone(),
            capability: stored.output.capability.clone(),
            schema: stored.output.schema.clone(),
        };
        book.record(stored, &selected, &target(), &|_| true)
            .unwrap();
        let finalize = FinalizeSelectionSnapshot {
            target: target(),
            parent_generation: None,
            stage: None,
            transition_id: "finalize-transition".into(),
            input: snapshot.input.clone(),
            policy: snapshot.finalize_policy.clone(),
            finalize_approval: snapshot.finalize_approval.clone(),
            finalize_status_approval: snapshot.finalize_status_approval.clone(),
            schema_documents: snapshot.schema_documents.clone(),
        };
        assert!(finalize.validate_structure());
        assert!(
            book.record_finalize_selection(finalize.clone(), &|_| false)
                .is_err()
        );
        book.record_finalize_selection(finalize.clone(), &|_| true)
            .unwrap();
        assert_eq!(
            book.finalize_selection(&target(), "finalize-transition", "screen", "output"),
            Some(&finalize)
        );
        let mut tampered = finalize;
        tampered.input.output.resource.incarnation = "foreign".into();
        assert!(book.record_finalize_selection(tampered, &|_| true).is_err());
        assert!(book.record_selection(snapshot.clone(), &|_| false).is_err());
        book.record_selection(snapshot.clone(), &|_| true).unwrap();
        let mut forged = snapshot;
        forged.control_status_approval.broker_approval["provider"] = json!("other");
        assert!(!forged.validate_structure());
        book.fence_after_restart();
        assert!(
            book.selection(
                &target(),
                "consumer-transition",
                "settings",
                "screen",
                "output"
            )
            .is_some()
        );
    }

    #[test]
    fn refinement_keeps_original_obligation_and_rejects_foreign_native_resource() {
        let original: ResourceObligation = serde_json::from_value(json!({"obligation_id":"restore","provider":"settings","resource_id":"display","mutation_domain":null,"depends_on":[],"cleanup_method":{"service":"settings/1","method":"restore","interface_major":1,"interface_minor":0,"schema_revision":1},"payload":{"output_id":"output"}})).unwrap();
        let input = output();
        let request: ResourceControlRequest = serde_json::from_value(json!({"version":1,"transition_id":"transition","operation_id":"operation","provider_sequence":1,"target":target(),"proposal":{"proposal_id":"proposal","output_id":"output","owner_provider":"screen","owner_service":"screen/2","target":target(),"resource":input.resource,"method":{"service":"screen/2","method":"control","interface_major":1,"interface_minor":0,"schema_revision":1},"argument_schema":input.schema,"arguments":{},"access":"exclusive","mutation_domains":[]},"admission_id":"admission","permit_id":"permit","remaining_ms":1000})).unwrap();
        let item = ObligationRefinement {
            request,
            original: original.clone(),
            confirmed: ConfirmedControl {
                proposal_id: "proposal".into(),
                output: input.clone(),
                candidate: None,
            },
            owner_result: ResourceControlResponse::Applied {
                output: Box::new(input),
                candidate: None,
            },
            owner_receipt_id: Some("receipt".into()),
        };
        assert!(valid_refinement_shape(&item));
        assert_eq!(item.original, original);
        assert_eq!(item.original.payload, json!({"output_id":"output"}));
        let mut forged = item;
        forged.confirmed.output.resource.incarnation = "replacement".into();
        assert!(!valid_refinement_shape(&forged));
    }
}
