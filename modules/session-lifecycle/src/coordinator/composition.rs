//! Durable coordinator-owned outputs and frozen dependency inputs.
//!
//! The coordinator must atomically save mutations with participant progress and
//! obligations before exposing outputs or dispatching dependent calls. Installed
//! schema validation and active-resource reobservation are external prerequisites;
//! this module never treats serialized provider data as authorization.
use amaterasu_extension_api::{
    LifecycleTarget, ResourceAccess, ResourceOutput, ResourcePlan, SchemaRef,
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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionBook {
    outputs: Vec<StoredOutput>,
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
            && self.outputs.iter().enumerate().all(|(index, item)| {
                valid_stored(item)
                    && !self.outputs[..index]
                        .iter()
                        .any(|old| same_key(&old.output, &item.output))
            })
    }

    pub fn records(&self) -> impl Iterator<Item = &StoredOutput> {
        self.outputs.iter()
    }

    pub fn references_admission(&self, admission_id: &str) -> bool {
        self.outputs
            .iter()
            .any(|item| item.admission_id == admission_id)
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
    use serde_json::json;

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
}
