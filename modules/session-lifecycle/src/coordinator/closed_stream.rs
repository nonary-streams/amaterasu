//! Coordinator-owned historical closure certificates.
//!
//! These records are trusted only to the same extent as the private lifecycle
//! journal. They never authorize an effect or replace a fresh observation.
use super::*;
use sha2::{Digest, Sha256};

// A finite budget is deliberate: exhaustion must stop retirement rather than
// evicting an old scope fence. The journal's byte limit may be reached sooner.
const MAX_CHECKPOINTS: usize = 64;
const MAX_SCOPE_FENCES: usize = 8192;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ClosedStreamBook {
    records: Vec<ClosedStream>,
    #[serde(default)]
    expired_scopes: BTreeSet<String>,
}

pub(super) struct HistoricalProof<'a> {
    pub session_target: LifecycleTarget,
    pub resource_generation: u64,
    pub root_output: &'a amaterasu_extension_api::ResourceOutput,
}

impl ClosedStreamBook {
    /// Raw presence only; callers must reject ambiguous mixed source records.
    pub(super) fn contains_target(&self, target: &LifecycleTarget) -> bool {
        self.records.iter().any(|record| &record.target == target)
    }

    pub(super) fn validate(&self, journal: &Journal) -> bool {
        self.records.len() <= MAX_CHECKPOINTS
            && self.expired_scopes.len() <= MAX_SCOPE_FENCES
            && self.expired_scopes.iter().all(|item| valid_digest(item))
            && self.records.iter().enumerate().all(|(index, record)| {
                record.valid(journal)
                    && !self.scope_fenced(&record.target)
                    && !self.records[..index]
                        .iter()
                        .any(|old| old.target == record.target)
            })
    }

    fn insert(&mut self, journal: &Journal, record: ClosedStream) -> Result<(), &'static str> {
        if self.records.len() >= MAX_CHECKPOINTS
            || self.records.iter().any(|old| old.target == record.target)
            || self.scope_fenced(&record.target)
            || !record.valid(journal)
        {
            return Err("closed_stream_capacity_or_conflict");
        }
        self.records.push(record);
        Ok(())
    }

    fn scope_fenced(&self, target: &LifecycleTarget) -> bool {
        scope_digest(target).is_some_and(|digest| self.expired_scopes.contains(&digest))
    }

    fn released_output_for_target<'a>(
        &'a self,
        journal: &Journal,
        target: &LifecycleTarget,
    ) -> Option<(&'a str, &'a amaterasu_extension_api::ResourceOutput)> {
        if !self.validate(journal) || self.scope_fenced(target) {
            return None;
        }
        let record = self.records.iter().find(|item| &item.target == target)?;
        Some((&record.release_admission, &record.released))
    }

    pub(super) fn verify_finalize<'a>(
        &'a self,
        journal: &'a Journal,
        settings: &LifecycleSettings,
        caller: &str,
        request: &amaterasu_extension_api::ResourceFinalizeRequest,
        response: &amaterasu_extension_api::ResourceControlResponse,
    ) -> Option<HistoricalProof<'a>> {
        let record = self.unique_record(journal, &request.target)?;
        if !record.matches_finalize(journal, settings, caller, request, response) {
            return None;
        }
        let amaterasu_extension_api::ResourceControlResponse::Applied { output, .. } = response
        else {
            return None;
        };
        settings
            .schema_catalog()
            .ok()?
            .validate(&output.schema, &output.data)
            .ok()?;
        record.historical_proof(journal)
    }

    pub(super) fn verify_settlement<'a>(
        &'a self,
        journal: &'a Journal,
        settings: &LifecycleSettings,
        caller: &str,
        request: &amaterasu_extension_api::ResourceControlSettlementRequest,
    ) -> Option<HistoricalProof<'a>> {
        let record = self.unique_record(journal, &request.request.target)?;
        if !record.matches_settlement(journal, settings, caller, request) {
            return None;
        }
        settings
            .schema_catalog()
            .ok()?
            .validate(&request.output.schema, &request.output.data)
            .ok()?;
        record.historical_proof(journal)
    }

    fn unique_record<'a>(
        &'a self,
        journal: &Journal,
        target: &LifecycleTarget,
    ) -> Option<&'a ClosedStream> {
        if !self.validate(journal) || self.scope_fenced(target) {
            return None;
        }
        let mut matches = self
            .records
            .iter()
            .filter(|record| &record.target == target);
        let record = matches.next()?;
        matches.next().is_none().then_some(record)
    }

    /// Enumerates only verified historical children. The caller must still
    /// prove that no live stream, action, receipt, or external reference is left.
    pub(super) fn verified_children<'a>(
        &'a self,
        journal: &Journal,
        session_target: &LifecycleTarget,
    ) -> Option<Vec<&'a ClosedStream>> {
        let (epoch, session_id, None, generation) =
            operations::target_parts(session_target).ok()?
        else {
            return None;
        };
        let session = journal.sessions.get(session_id)?;
        if !self.validate(journal)
            || epoch != session.host_epoch
            || generation != session.generation
        {
            return None;
        }
        Some(
            self.records
                .iter()
                .filter(|record| {
                    operations::target_parts(&record.target).is_ok_and(
                        |(child_epoch, child_session, stream, _)| {
                            child_epoch == epoch
                                && child_session == session_id
                                && stream.is_some()
                                && record.parent_generation == generation
                        },
                    )
                })
                .collect(),
        )
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ClosedStream {
    target: LifecycleTarget,
    transition_id: String,
    parent_generation: u64,
    resource_generation: u64,
    graph: ProviderGraph,
    root_admission: String,
    root_output: String,
    owner: String,
    predecessor_admission: Option<String>,
    before: amaterasu_extension_api::ResourceOutput,
    staged: amaterasu_extension_api::ResourceOutput,
    release_admission: String,
    released: amaterasu_extension_api::ResourceOutput,
    /// Commitments support exact historical tuple matching, not new authority.
    release_commitment: String,
    finalize_commitment: String,
    finalize_tuple: String,
    finalize_policy: String,
    finalize_schemas: Vec<String>,
    settlement_commitments: Vec<String>,
    settlement_tuples: Vec<SettlementTuple>,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettlementTuple {
    tuple: String,
    policy: String,
    schemas: Vec<String>,
}

fn commitment(value: &impl Serialize) -> Option<String> {
    let mut value = serde_json::to_value(value).ok()?;
    canonicalize(&mut value);
    let bytes = serde_json::to_vec(&value).ok()?;
    Some(format!("{:x}", Sha256::digest(bytes)))
}

fn scope_digest(target: &LifecycleTarget) -> Option<String> {
    let (epoch, session_id, Some(stream_id), generation) = operations::target_parts(target).ok()?
    else {
        return None;
    };
    if generation == 0 || !valid_id(epoch) || !valid_id(session_id) || !valid_id(stream_id) {
        return None;
    }
    commitment(&("closed-stream-scope-v1", target))
}

fn canonicalize(value: &mut Value) {
    match value {
        Value::Array(items) => items.iter_mut().for_each(canonicalize),
        Value::Object(object) => {
            let old = std::mem::take(object);
            let sorted = old
                .into_iter()
                .collect::<std::collections::BTreeMap<_, _>>();
            for (key, mut item) in sorted {
                canonicalize(&mut item);
                object.insert(key, item);
            }
        }
        _ => {}
    }
}

fn schema_commitments(documents: &[crate::adapter::SchemaDocument]) -> Option<Vec<String>> {
    let mut values = documents
        .iter()
        .map(commitment)
        .collect::<Option<Vec<_>>>()?;
    values.sort();
    Some(values)
}

impl ClosedStream {
    fn historical_proof<'a>(&self, journal: &'a Journal) -> Option<HistoricalProof<'a>> {
        let mut roots = journal.composition.records().filter(|item| {
            item.admission_id == self.root_admission
                && commitment(&item.output).as_deref() == Some(self.root_output.as_str())
        });
        let root = roots.next()?;
        if roots.next().is_some() {
            return None;
        }
        Some(HistoricalProof {
            session_target: root.output.origin.clone(),
            resource_generation: self.resource_generation,
            root_output: &root.output,
        })
    }
    /// Build only from the exact release after all graph evidence is verified.
    pub(super) fn from_verified(
        journal: &Journal,
        target: &LifecycleTarget,
        transition_id: &str,
    ) -> Option<Self> {
        let proof =
            journal
                .composed_releases
                .verified_evidence_for(journal, target, transition_id)?;
        let owner = &proof.released_output.producer;
        let stage = proof.finalized.selection.stage.as_ref()?;
        // Action markers revalidate the full graph and root admissions. Until
        // they have their own closure proof, retain that source lineage.
        if journal.action_dispositions.values().any(|marker| {
            marker.cleanup_work.target == *target
                || marker.root_admission_id == proof.finalized.selection.input.admission_id
        }) {
            return None;
        }
        let release_admission = proof.work.states.get(owner)?.admission_id.clone()?;
        let records = journal
            .control_settlements
            .records()
            .filter(|item| item.settlement.request.target == *target)
            .collect::<Vec<_>>();
        let mut settlements = records
            .iter()
            .map(|item| {
                let permit = journal
                    .authority
                    .control_permits
                    .get(&item.settlement.request.permit_id)?;
                if item.owner_ack.is_none()
                    || item.owner_dispatch.is_none()
                    || !permit.consumed
                    || !permit.settled
                    || !item.valid_under_permit(permit)
                {
                    return None;
                }
                commitment(*item)
            })
            .collect::<Option<Vec<_>>>()?;
        settlements.sort();
        let mut settlement_tuples = records
            .iter()
            .map(|item| {
                let permit = journal
                    .authority
                    .control_permits
                    .get(&item.settlement.request.permit_id)?;
                let selection = journal.composition.selection(
                    target,
                    &item.selection_transition_id,
                    &permit.context.policy.dependent_provider,
                    owner,
                    &item.settlement.request.proposal.output_id,
                )?;
                Some(SettlementTuple {
                    tuple: commitment(&(owner, &item.settlement))?,
                    policy: commitment(&selection.control_policy)?,
                    schemas: schema_commitments(&selection.schema_documents)?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        settlement_tuples.sort();
        let result = Self {
            target: target.clone(),
            transition_id: transition_id.into(),
            parent_generation: proof.finalized.selection.parent_generation?,
            resource_generation: resource_generation::current_root_generation(
                journal,
                &proof.finalized.selection.input,
            )
            .ok()?,
            graph: proof.work.provider_graph.clone()?,
            root_admission: proof.finalized.selection.input.admission_id.clone(),
            root_output: commitment(&proof.finalized.selection.input.output)?,
            owner: owner.clone(),
            predecessor_admission: stage.prior_release_admission.clone(),
            before: stage.before.clone(),
            staged: stage.after.clone(),
            release_admission,
            released: proof.released_output.clone(),
            release_commitment: commitment(proof.work)?,
            finalize_commitment: commitment(proof.finalized)?,
            finalize_tuple: commitment(&(
                owner,
                &proof.finalized.request,
                &proof.finalized.applied,
            ))?,
            finalize_policy: commitment(&proof.finalized.selection.policy)?,
            finalize_schemas: schema_commitments(&proof.finalized.selection.schema_documents)?,
            settlement_commitments: settlements,
            settlement_tuples,
        };
        result.valid(journal).then_some(result)
    }

    /// Historical proof queries retain the same installed-policy checks as the
    /// full record. This still does not establish any live mutation authority.
    fn installed(&self, settings: &LifecycleSettings) -> bool {
        let policy_installed =
            |expected: &str, policies: &[crate::prepare_authority::ControlPolicy]| {
                policies
                    .iter()
                    .any(|policy| commitment(policy).as_deref() == Some(expected))
            };
        let documents_installed = |expected: &[String]| {
            expected.iter().all(|digest| {
                settings
                    .schema_documents
                    .iter()
                    .any(|document| commitment(document).as_ref() == Some(digest))
            })
        };
        settings
            .finalize_policies
            .iter()
            .any(|policy| commitment(policy).as_deref() == Some(&self.finalize_policy))
            && documents_installed(&self.finalize_schemas)
            && self.settlement_tuples.iter().all(|tuple| {
                policy_installed(&tuple.policy, &settings.control_policies)
                    && documents_installed(&tuple.schemas)
            })
    }

    fn matches_finalize(
        &self,
        journal: &Journal,
        settings: &LifecycleSettings,
        caller: &str,
        request: &amaterasu_extension_api::ResourceFinalizeRequest,
        response: &amaterasu_extension_api::ResourceControlResponse,
    ) -> bool {
        self.valid(journal)
            && self.installed(settings)
            && caller == self.owner
            && request.target == self.target
            && commitment(&(caller, request, response)).as_deref() == Some(&self.finalize_tuple)
    }

    fn matches_settlement(
        &self,
        journal: &Journal,
        settings: &LifecycleSettings,
        caller: &str,
        request: &amaterasu_extension_api::ResourceControlSettlementRequest,
    ) -> bool {
        self.valid(journal)
            && self.installed(settings)
            && caller == self.owner
            && request.request.target == self.target
            && commitment(&(caller, request)).is_some_and(|digest| {
                self.settlement_tuples
                    .iter()
                    .any(|item| item.tuple == digest)
            })
    }

    fn valid(&self, journal: &Journal) -> bool {
        let Ok((epoch, session_id, Some(stream_id), generation)) =
            operations::target_parts(&self.target)
        else {
            return false;
        };
        let Some(session) = journal.sessions.get(session_id) else {
            return false;
        };
        let mut roots = journal.composition.records().filter(|item| {
            item.admission_id == self.root_admission
                && commitment(&item.output).as_deref() == Some(self.root_output.as_str())
        });
        let Some(root) = roots.next() else {
            return false;
        };
        if roots.next().is_some() {
            return false;
        }
        let root_output = &root.output;
        generation > 0
            && valid_id(epoch)
            && valid_id(session_id)
            && valid_id(stream_id)
            && session.host_epoch == epoch
            && session.generation == self.parent_generation
            && matches!(&root_output.origin, LifecycleTarget::Session {
                host_epoch, session_id: root_session, generation: root_generation
            } if host_epoch == epoch && root_session == session_id && *root_generation == self.parent_generation)
            && valid_graph(&self.graph, &self.owner)
            && root_output.producer == self.owner
            && session.streams.get(stream_id).is_none_or(|stream| {
                stream.generation == generation
                    && stream.provider_graph.as_ref() == Some(&self.graph)
            })
            && resource_generation::historical_generation(journal, root)
                == Ok(self.resource_generation)
            && valid_digest(&self.root_output)
            && valid_id(&self.transition_id)
            && valid_id(&self.release_admission)
            && self.predecessor_admission.as_deref().is_none_or(valid_id)
            && (self.predecessor_admission.is_some() || &self.before == root_output)
            && amaterasu_extension_api::validate_output_refinement(root_output, &self.before)
                .is_ok()
            && amaterasu_extension_api::validate_output_refinement(&self.before, &self.staged)
                .is_ok()
            && amaterasu_extension_api::validate_resource_output(&self.released).is_ok()
            && amaterasu_extension_api::validate_output_refinement(root_output, &self.released)
                .is_ok()
            && amaterasu_extension_api::validate_cleanup_output(
                &self.staged.producer,
                &self.staged,
                &self.released,
            )
            .is_ok()
            && valid_digest(&self.release_commitment)
            && valid_digest(&self.finalize_commitment)
            && valid_digest(&self.finalize_tuple)
            && valid_digest(&self.finalize_policy)
            && valid_schemas(&self.finalize_schemas)
            && valid_commitments(&self.settlement_commitments)
            && self.settlement_tuples.len() <= 128
            && self
                .settlement_tuples
                .windows(2)
                .all(|pair| pair[0] < pair[1])
            && self.settlement_tuples.iter().all(|entry| {
                valid_digest(&entry.tuple)
                    && valid_digest(&entry.policy)
                    && valid_schemas(&entry.schemas)
            })
            && self.settlement_commitments.len() == self.settlement_tuples.len()
    }
}

fn valid_graph(graph: &ProviderGraph, owner: &str) -> bool {
    graph.order.first().map(String::as_str) == Some(owner)
        && graph.order.len() >= 2
        && graph.order.len() == graph.dependencies.len()
        && graph.order.iter().collect::<BTreeSet<_>>().len() == graph.order.len()
        && graph.order.iter().enumerate().all(|(index, provider)| {
            valid_id(provider)
                && graph
                    .dependencies
                    .get(provider)
                    .is_some_and(|dependencies| {
                        dependencies.iter().collect::<BTreeSet<_>>().len() == dependencies.len()
                            && dependencies
                                .iter()
                                .all(|dependency| graph.order[..index].contains(dependency))
                            && (index == 0 || !dependencies.is_empty())
                    })
        })
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_commitments(values: &[String]) -> bool {
    values.len() <= 128
        && values.iter().all(|value| valid_digest(value))
        && values.windows(2).all(|pair| pair[0] < pair[1])
}

fn valid_schemas(values: &[String]) -> bool {
    values.len() <= 32 && valid_commitments(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commitments_are_bounded_sorted_unique_and_owner_scoped() {
        let a = commitment(&("owner-a", "tuple")).unwrap();
        let b = commitment(&("owner-b", "tuple")).unwrap();
        assert_ne!(a, b);
        let mut values = vec![a.clone(), b];
        values.sort();
        assert!(valid_commitments(&values));
        assert!(!valid_commitments(&[a.clone(), a]));
        assert!(!valid_commitments(&vec!["a".repeat(64); 129]));
        assert!(!valid_digest(&"A".repeat(64)));
    }

    #[test]
    fn closed_graph_must_keep_one_ordered_owner_root() {
        let graph = ProviderGraph {
            order: vec!["owner".into(), "dependent".into()],
            dependencies: [
                ("owner".into(), vec![]),
                ("dependent".into(), vec!["owner".into()]),
            ]
            .into(),
        };
        assert!(valid_graph(&graph, "owner"));
        assert!(!valid_graph(&graph, "dependent"));
        let mut orphan = graph.clone();
        orphan.dependencies.get_mut("dependent").unwrap().clear();
        assert!(!valid_graph(&orphan, "owner"));
        let mut cycle = graph;
        cycle
            .dependencies
            .get_mut("owner")
            .unwrap()
            .push("dependent".into());
        assert!(!valid_graph(&cycle, "owner"));
    }

    #[test]
    fn empty_book_cannot_prove_a_closed_stream() {
        let journal: Journal = serde_json::from_value(json!({
            "version":3,"host_epoch":"epoch","receipts":{},"sessions":{}
        }))
        .unwrap();
        let target: LifecycleTarget = serde_json::from_value(json!({
            "kind":"stream","host_epoch":"epoch","session_id":"session",
            "stream_id":"stream","generation":1
        }))
        .unwrap();
        let book = ClosedStreamBook::default();
        assert!(book.validate(&journal));
        assert!(book.unique_record(&journal, &target).is_none());
        assert!(book.released_output_for_target(&journal, &target).is_none());
    }
}
