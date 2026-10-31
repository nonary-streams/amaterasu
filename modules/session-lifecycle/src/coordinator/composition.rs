//! Durable coordinator-owned outputs and frozen dependency inputs.
//!
//! The coordinator must atomically save mutations with participant progress and
//! obligations before exposing outputs or dispatching dependent calls. Installed
//! schema validation and active-resource reobservation are external prerequisites;
//! this module never treats serialized provider data as authorization.
use crate::adapter::SchemaDocument;
use crate::prepare_authority::{Admission, ControlPermit};
use crate::prepare_authority::{ControlPolicy, FinalizePolicy, MethodApproval};
use amaterasu_extension_api::{
    LifecycleTarget, ResourceAccess, ResourceControlRequest, ResourceObligation, ResourceOutput,
    ResourcePlan, SchemaRef,
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
}

/// Authenticated control confirmation retained alongside its original
/// pre-effect obligation; no field of that obligation is replaced.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObligationRefinement {
    pub request: ResourceControlRequest,
    pub original: ResourceObligation,
    pub confirmed: ResourceOutput,
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
            || !approval_is_valid(&self.control_status_approval, &policy.owner_provider)
            || !approval_is_valid(&self.finalize_status_approval, &policy.owner_provider)
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
    refinements: Vec<ObligationRefinement>,
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
        self.validate_structure() && self.outputs.iter().all(|item| verify_schema(&item.output))
    }

    /// Structural checks do not authorize outputs or validate schema semantics.
    pub fn validate_structure(&self) -> bool {
        self.outputs.len() <= MAX_OUTPUTS
            && self.selections.len() <= MAX_OUTPUTS
            && self.refinements.len() <= MAX_OUTPUTS
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
        self.outputs
            .iter()
            .any(|item| item.admission_id == admission_id)
            || self
                .selections
                .iter()
                .any(|selection| selection.input.admission_id == admission_id)
            || self
                .refinements
                .iter()
                .any(|item| item.request.admission_id == admission_id)
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
            || permit.context.output.schema != item.confirmed.schema
            || permit.context.output.origin != item.confirmed.origin
            || permit.context.output.capability != item.confirmed.capability
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
    let output = &item.confirmed;
    valid_text(&request.operation_id)
        && valid_text(&request.transition_id)
        && valid_text(&request.admission_id)
        && valid_text(&request.permit_id)
        && request.provider_sequence > 0
        && request.target == proposal.target
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
        serde_json::from_value(json!({"output_id":"output", "capability":"screen", "schema":{"id":"screen/1","major":1,"minor":0,"digest":"a".repeat(64)}, "producer":"screen", "producer_service":"screen/1", "origin":target(), "resource":{"provider":"screen","resource_id":"display","incarnation":"native","revision":2,"execution_domain":"seat","mutation_domain":null,"mutation_domains":[]},"data":{"desired":true,"observed":false}})).unwrap()
    }
    fn record() -> StoredOutput {
        let plan: ResourcePlan = serde_json::from_value(json!({"plan_id":"plan", "revision":1, "resources":[{"provider":"screen","resource_id":"display","incarnation":null,"expected_revision":1,"execution_domain":"seat","mutation_domains":[],"access":"exclusive"}],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]})).unwrap();
        StoredOutput {
            output: output(),
            admission_id: "admission".into(),
            transition_id: "transition".into(),
            plan,
            state: OutputState::Retained,
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
        let document = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","$id":"screen/1","type":"object"}"#;
        let mut stored = record();
        stored.output.schema.digest = format!("{:x}", Sha256::digest(document.as_bytes()));
        let schema = stored.output.schema.clone();
        let method = json!({"service":"screen/1","method":"control","interface_major":1,"interface_minor":0,"schema_revision":1});
        let status = json!({"service":"screen/1","method":"status","interface_major":1,"interface_minor":0,"schema_revision":1});
        let approval = |m: &Value| {
            serde_json::from_value::<MethodApproval>(
                json!({"method":m,"broker_approval":{"provider":"screen","method":m}}),
            )
            .unwrap()
        };
        let policy: ControlPolicy = serde_json::from_value(json!({"dependent_provider":"settings","owner_provider":"screen","owner_service":"screen/1","owner_method":method,"argument_schema":schema,"output_capability":"screen","output_schema":schema,"max_access":"exclusive","mutation_domains":[]})).unwrap();
        let finalize: FinalizePolicy = serde_json::from_value(json!({"owner_provider":"screen","owner_service":"screen/1","method":method,"output_capability":"screen","output_schema":schema,"capture_provider":"capture","selector_schema":"selector/1","required_access":"exclusive","mutation_domains":[]})).unwrap();
        let snapshot = CompositionSelectionSnapshot {
            target: target(),
            transition_id: "consumer-transition".into(),
            input: stored.clone(),
            control_policy: policy,
            finalize_policy: finalize,
            control_approval: approval(&method),
            control_status_approval: approval(&status),
            finalize_approval: approval(&method),
            finalize_status_approval: approval(&status),
            schema_documents: vec![SchemaDocument {
                reference: schema,
                document: document.into(),
            }],
        };
        assert!(snapshot.validate_structure());
        let mut book = CompositionBook::default();
        let selected = OutputSelection {
            producer: stored.output.producer.clone(),
            service: stored.output.producer_service.clone(),
            capability: stored.output.capability.clone(),
            schema: stored.output.schema.clone(),
        };
        book.record(stored, &selected, &target(), &|_| true)
            .unwrap();
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
        let request: ResourceControlRequest = serde_json::from_value(json!({"version":1,"transition_id":"transition","operation_id":"operation","provider_sequence":1,"target":target(),"proposal":{"proposal_id":"proposal","output_id":"output","owner_provider":"screen","owner_service":"screen/1","target":target(),"resource":input.resource,"method":{"service":"screen/1","method":"control","interface_major":1,"interface_minor":0,"schema_revision":1},"argument_schema":input.schema,"arguments":{},"access":"exclusive","mutation_domains":[]},"admission_id":"admission","permit_id":"permit","remaining_ms":1000})).unwrap();
        let item = ObligationRefinement {
            request,
            original: original.clone(),
            confirmed: input,
        };
        assert!(valid_refinement_shape(&item));
        assert_eq!(item.original, original);
        assert_eq!(item.original.payload, json!({"output_id":"output"}));
        let mut forged = item;
        forged.confirmed.resource.incarnation = "replacement".into();
        assert!(!valid_refinement_shape(&forged));
    }
}
