//! Immutable owner-only finalization selection for durable composition work.
//!
//! Structural validation does not authenticate the current installation or prove
//! that the native resource is still present. The coordinator must independently
//! verify policy, broker approvals, parent generation, rights and observations.
use super::composition::{OutputState, StoredOutput};
use crate::adapter::SchemaDocument;
use crate::prepare_authority::{DomainScope, FinalizePolicy, MethodApproval};
use amaterasu_extension_api::{self as api, LifecycleTarget, MethodRef, ResourceAccess};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const MAX_SCHEMA_DOCUMENTS: usize = 32;
const MAX_SCHEMA_BYTES: usize = 12_000;

/// Finalization is independently selected even when no dependent control exists.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinalizeSelectionSnapshot {
    pub target: LifecycleTarget,
    /// Coordinator-read session generation for a session-origin child stream.
    pub parent_generation: Option<u64>,
    pub transition_id: String,
    pub input: StoredOutput,
    pub policy: FinalizePolicy,
    pub finalize_approval: MethodApproval,
    pub finalize_status_approval: MethodApproval,
    pub schema_documents: Vec<SchemaDocument>,
}

impl FinalizeSelectionSnapshot {
    /// Validate immutable data and schema bytes; current authorization and
    /// resource observations are checked separately by the owning coordinator.
    pub fn validate_structure(&self) -> bool {
        let output = &self.input.output;
        let resource = &output.resource;
        let policy = &self.policy;
        if !valid_text(&self.transition_id)
            || !valid_text(&self.input.transition_id)
            || !valid_text(&self.input.admission_id)
            || self.input.state != OutputState::Retained
            || api::validate_resource_output(output).is_err()
            || api::validate_resource_plan(&self.input.plan).is_err()
            || !api::output_in_scope(output, &self.target)
            || !scope_matches(&output.origin, &self.target, self.parent_generation)
            || output.producer != policy.owner_provider
            || output.producer_service != policy.owner_service
            || output.capability != policy.output_capability
            || output.schema != policy.output_schema
            || matches!(policy.required_access, ResourceAccess::Read)
            || !valid_text(&policy.capture_provider)
            || !valid_text(&policy.selector_schema)
            || !approved_method(
                &self.finalize_approval,
                &policy.owner_provider,
                &policy.owner_service,
                &policy.method,
            )
            || !approved_method(
                &self.finalize_status_approval,
                &policy.owner_provider,
                &policy.owner_service,
                &policy.status_method,
            )
            || !valid_domain_scope(
                policy.domain_scope,
                &policy.mutation_domains,
                &resource.mutation_domains,
            )
            || self.schema_documents.len() > MAX_SCHEMA_DOCUMENTS
            || self
                .schema_documents
                .iter()
                .map(|doc| doc.document.len())
                .sum::<usize>()
                > MAX_SCHEMA_BYTES
        {
            return false;
        }
        let admitted = self
            .input
            .plan
            .resources
            .iter()
            .filter(|planned| {
                planned.provider == resource.provider
                    && planned.resource_id == resource.resource_id
                    && planned.execution_domain == resource.execution_domain
                    && planned
                        .incarnation
                        .as_ref()
                        .is_none_or(|id| id == &resource.incarnation)
                    && planned
                        .expected_revision
                        .is_none_or(|revision| match planned.access {
                            ResourceAccess::Read => resource.revision == revision,
                            _ => resource.revision >= revision,
                        })
                    && planned.mutation_domains.iter().collect::<BTreeSet<_>>()
                        == resource.mutation_domains.iter().collect::<BTreeSet<_>>()
                    && resource
                        .mutation_domain
                        .as_ref()
                        .is_none_or(|domain| planned.mutation_domains.contains(domain))
            })
            .count()
            == 1;
        if !admitted {
            return false;
        }
        let catalog = api::schemas::SchemaCatalog::compile(
            self.schema_documents
                .iter()
                .map(|doc| (doc.reference.clone(), doc.document.as_str())),
        );
        catalog.is_ok_and(|catalog| {
            catalog
                .validate(&policy.output_schema, &output.data)
                .is_ok()
        })
    }
}

fn valid_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn scope_matches(
    origin: &LifecycleTarget,
    target: &LifecycleTarget,
    parent_generation: Option<u64>,
) -> bool {
    match (origin, target) {
        (
            LifecycleTarget::Session {
                host_epoch: a,
                session_id: b,
                generation,
            },
            LifecycleTarget::Stream {
                host_epoch: x,
                session_id: y,
                ..
            },
        ) => a == x && b == y && parent_generation == Some(*generation),
        (origin, target) => origin == target,
    }
}

fn approved_method(
    approval: &MethodApproval,
    provider: &str,
    service: &str,
    expected: &MethodRef,
) -> bool {
    let method = &approval.method;
    let broker = &approval.broker_approval;
    expected.service == service
        && valid_text(service)
        && valid_text(&expected.method)
        && expected.interface_major == 2
        && expected.schema_revision != 0
        && method.service == expected.service
        && method.method == expected.method
        && method.interface_major == expected.interface_major
        && method.interface_minor == expected.interface_minor
        && method.schema_revision == expected.schema_revision
        && broker["provider"].as_str() == Some(provider)
        && broker["method"]["service"].as_str() == Some(service)
        && broker["method"]["method"].as_str() == Some(expected.method.as_str())
        && broker["method"]["interface_major"].as_u64() == Some(u64::from(expected.interface_major))
        && broker["method"]["interface_minor"].as_u64() == Some(u64::from(expected.interface_minor))
}

fn valid_domain_scope(scope: DomainScope, allowed: &BTreeSet<String>, resource: &[String]) -> bool {
    let exact: BTreeSet<_> = resource.iter().cloned().collect();
    !exact.is_empty()
        && exact.len() == resource.len()
        && exact.iter().all(|id| valid_text(id))
        && match scope {
            DomainScope::Static => !allowed.is_empty() && allowed == &exact,
            DomainScope::Resource => allowed.is_empty(),
        }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use serde_json::json;
    use sha2::{Digest, Sha256};

    pub(crate) fn fixture() -> FinalizeSelectionSnapshot {
        let document = r#"{"$id":"output/1","$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"mode":{"type":"string"}},"required":["mode"],"additionalProperties":false}"#;
        let schema = json!({"id":"output/1","major":1,"minor":0,"digest":format!("{:x}",Sha256::digest(document.as_bytes()))});
        let effect = json!({"service":"owner/2","method":"finalize","interface_major":2,"interface_minor":0,"schema_revision":1});
        let status = json!({"service":"owner/2","method":"status","interface_major":2,"interface_minor":0,"schema_revision":1});
        let approval = |method: &serde_json::Value| {
            serde_json::from_value(json!({"method":method,"broker_approval":{"provider":"owner","method":{"service":method["service"],"method":method["method"],"interface_major":method["interface_major"],"interface_minor":method["interface_minor"]}}})).unwrap()
        };
        let origin =
            json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":3});
        let output = json!({"output_id":"output","capability":"display/output","schema":schema,"producer":"owner","producer_service":"owner/2","origin":origin,"resource":{"provider":"owner","resource_id":"display","incarnation":"native","revision":2,"execution_domain":"seat","mutation_domain":"topology","mutation_domains":["topology"]},"domain_observations":[{"domain":"topology","revision":1,"borrowers":{},"complete":true}],"data":{"mode":"ok"}});
        let plan = json!({"plan_id":"plan","revision":1,"resources":[{"provider":"owner","resource_id":"display","incarnation":null,"expected_revision":null,"execution_domain":"seat","mutation_domains":["topology"],"access":"exclusive"}],"mutation_domains":[{"domain":"topology","expected_revision":1,"borrowers":{},"access":"exclusive"}],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]});
        let policy = json!({"owner_provider":"owner","owner_service":"owner/2","method":effect,"status_method":status,"output_capability":"display/output","output_schema":schema,"capture_provider":"capture","selector_schema":"selector/1","required_access":"exclusive","domain_scope":"resource","mutation_domains":[]});
        FinalizeSelectionSnapshot {
            target: serde_json::from_value(json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1})).unwrap(),
            parent_generation:Some(3),transition_id:"finalize-transition".into(),
            input:StoredOutput {output:serde_json::from_value(output).unwrap(),admission_id:"admission".into(),transition_id:"prepare-transition".into(),plan:serde_json::from_value(plan).unwrap(),state:OutputState::Retained,callback_proof:None},
            policy:serde_json::from_value(policy).unwrap(),finalize_approval:approval(&effect),finalize_status_approval:approval(&status),
            schema_documents:vec![SchemaDocument {reference:serde_json::from_value(schema).unwrap(),document:document.into()}],
        }
    }
    #[test]
    fn owner_only_finalization_freezes_exact_output_and_method_approvals() {
        let selection = fixture();
        assert!(selection.validate_structure());
        let roundtrip: FinalizeSelectionSnapshot =
            serde_json::from_value(serde_json::to_value(&selection).unwrap()).unwrap();
        assert_eq!(roundtrip, selection);
        assert!(roundtrip.validate_structure());
        let mut missing = selection.clone();
        missing.finalize_status_approval.broker_approval["provider"] = json!("other");
        assert!(!missing.validate_structure());
        let mut wrong = selection.clone();
        wrong.finalize_approval.method.schema_revision += 1;
        assert!(!wrong.validate_structure());
        let mut wrong = selection.clone();
        wrong.input.output.resource.resource_id = "other".into();
        assert!(!wrong.validate_structure());
        let mut wrong = selection.clone();
        wrong.input.state = OutputState::Released;
        assert!(!wrong.validate_structure());
    }
    #[test]
    fn exact_parent_generation_and_schema_are_required() {
        let selection = fixture();
        let mut wrong = selection.clone();
        wrong.parent_generation = Some(4);
        assert!(!wrong.validate_structure());
        let mut wrong = selection.clone();
        wrong.input.output.data = json!({"mode":12});
        assert!(!wrong.validate_structure());
        let mut wrong = selection.clone();
        wrong.schema_documents[0].document.push(' ');
        assert!(!wrong.validate_structure());
        let mut wrong = selection.clone();
        wrong.policy.domain_scope = DomainScope::Static;
        wrong.policy.mutation_domains.insert("other".into());
        assert!(!wrong.validate_structure());
        wrong.policy.mutation_domains.clear();
        wrong.policy.mutation_domains.insert("topology".into());
        assert!(wrong.validate_structure());
    }
}
