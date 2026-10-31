//! Standalone historical certificates for ended sessions.
//!
//! A certificate is terminal evidence under the private Core journal trust
//! model. It grants no native effect, adoption, or recovery authority. Its book
//! is not populated until the atomic cross-book retirement transaction exists.
use super::*;
use crate::prepare_authority::Admission;
use amaterasu_extension_api::{CleanupState, ProviderOperation, ProviderResponse};
use std::collections::{BTreeMap, BTreeSet};

const CERTIFICATE_VERSION: u32 = 1;
const MAX_CERTIFICATES: usize = MAX_SESSIONS;
const MAX_CERTIFICATE_BYTES: usize = 64 * 1024;

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TerminalCallbackEvidence {
    pub admission_id: String,
    pub admission: Admission,
    pub callback: composition::ProducerCallbackProof,
}

impl TerminalCallbackEvidence {
    fn response(
        &self,
        target: &LifecycleTarget,
        provider: &str,
        phase: LifecyclePhase,
    ) -> Option<ProviderResponse> {
        let proof = &self.callback;
        let request = &proof.request;
        let crate::provider_dispatch::DispatchState::Completed { payload } = &proof.terminal.state
        else {
            return None;
        };
        let response = serde_json::from_value::<ProviderResponse>(payload.clone()).ok()?;
        (valid_id(&self.admission_id)
            && self.admission.provider == provider
            && self.admission.selection.participant == proof.participant
            && request.version == amaterasu_extension_api::COMPOSITION_VERSION
            && request.operation == ProviderOperation::LifecycleCallback
            && request.phase == phase
            && &request.target == target
            && request.admission_id.as_deref() == Some(self.admission_id.as_str())
            && request.approved_plan.as_ref() == Some(&self.admission.plan)
            && amaterasu_extension_api::validate_provider_request(request).is_ok()
            && composition::callback_approvals_valid(
                proof,
                &self.admission.selection.approvals,
                provider,
            )
            && composition::valid_callback_proof(proof, &response))
        .then_some(response)
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DependentTerminalEvidence {
    pub provider: String,
    pub prepare: TerminalCallbackEvidence,
    pub release: TerminalCallbackEvidence,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EndedSessionCertificate {
    pub version: u32,
    pub target: LifecycleTarget,
    pub owner: crate::action_authority::Principal,
    #[serde(default)]
    pub end_receipt: Option<TerminalEndReceipt>,
    pub graph: ProviderGraph,
    pub application_ref: Value,
    pub application: Option<application::ApplicationBinding>,
    pub application_operation_id: Option<String>,
    pub application_proof: application::ApplicationRetirementProof,
    pub producer: String,
    pub resource_generation: u64,
    pub root: composition::StoredOutput,
    pub root_admission: Admission,
    pub owner_release: TerminalCallbackEvidence,
    pub dependents: Vec<DependentTerminalEvidence>,
    /// No child streams are accepted until their independent checkpoint API
    /// and all cross-book retirement gates are available.
    pub closed_children: u32,
    /// Compact owner-query commitments transferred from validated child books.
    /// These are historical evidence and never grant a new resource effect.
    #[serde(default)]
    pub children: Option<closed_stream::SessionChildrenSnapshot>,
    pub provider_sequences: BTreeMap<String, u64>,
    pub operation_counter: u64,
    /// Legacy randomly generated operation IDs remain fenced by the retained
    /// authority set; newly allocated IDs use the monotonic Core counter.
    pub legacy_operation_ids: BTreeSet<String>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TerminalEndReceipt {
    key: String,
    caller: String,
    initiator: String,
    request: Value,
    response: Value,
}

impl TerminalEndReceipt {
    fn valid_for(&self, target: &LifecycleTarget) -> bool {
        let Some(id) = self.request["operation_id"]
            .as_str()
            .filter(|id| valid_id(id))
        else {
            return false;
        };
        valid_id(&self.caller)
            && valid_id(&self.initiator)
            && self.key == receipt_key(&self.caller, &self.initiator, id)
            && self.request["operation"] == "end_session"
            && serde_json::from_value::<LifecycleTarget>(self.request["target"].clone())
                .ok()
                .as_ref()
                == Some(target)
            && self.response["status"] == "succeeded"
            && self.response["operation_id"] == id
            && serde_json::from_value::<LifecycleTarget>(self.response["target"].clone())
                .ok()
                .as_ref()
                == Some(target)
    }
}

fn operation_covered(operation: &str, counter: u64, legacy: &BTreeSet<String>) -> bool {
    if let Some(suffix) = operation.strip_prefix("lifecycle-") {
        return suffix
            .parse::<u64>()
            .ok()
            .is_some_and(|n| n > 0 && n <= counter && n.to_string() == suffix);
    }
    legacy.contains(operation)
}

fn callback_operation_ids(
    proof: &composition::ProducerCallbackProof,
) -> impl Iterator<Item = &str> {
    std::iter::once(proof.request.operation_id.as_str())
        .chain(std::iter::once(proof.terminal.call.operation_id.as_str()))
        .chain(
            proof
                .pending_dispatch
                .iter()
                .map(|pending| pending.call.operation_id.as_str()),
        )
}

fn callback_operations_covered(
    proof: &composition::ProducerCallbackProof,
    counter: u64,
    legacy: &BTreeSet<String>,
) -> bool {
    callback_operation_ids(proof).all(|operation| operation_covered(operation, counter, legacy))
}

fn empty_plan(plan: &amaterasu_extension_api::ResourcePlan) -> bool {
    plan.resources.is_empty()
        && plan.mutation_domains.is_empty()
        && plan.obligations.is_empty()
        && plan.control_proposals.is_empty()
        && plan.resource_disposition.is_none()
        && plan.candidate_constraint.is_none()
        && plan.affected_consumers.is_empty()
        && plan.consumer_change == amaterasu_extension_api::ConsumerChange::Live
}

impl EndedSessionCertificate {
    /// Freeze a candidate from authenticated source books. This does not
    /// authorize deletion or a native effect. The caller must separately prove
    /// complete source coverage and no inbound references before retirement.
    #[allow(
        dead_code,
        reason = "awaits the atomic ended-session closure transaction"
    )]
    pub(super) fn from_verified(journal: &Journal, target: &LifecycleTarget) -> ModuleResult<Self> {
        let source = session_closure::preflight(journal, target)?;
        let (_, session_id, _, _) = operations::target_parts(target)?;
        let session = journal.sessions.get(session_id).ok_or("unknown_session")?;
        if journal.composition.staged_outputs().any(|stage| matches!(&stage.target, LifecycleTarget::Stream { session_id: id, .. } if id == session_id)) {
            return Err("stream_checkpoint_retirement_unavailable".into());
        }
        let root = journal
            .composition
            .records()
            .find(|root| {
                root.admission_id == source.release.root_admission && root.output.origin == *target
            })
            .ok_or("session_root_missing")?
            .clone();
        let children = journal
            .closed_streams
            .session_children_snapshot(journal, target, &root, source.release.resource_generation)
            .ok_or("stream_checkpoint_snapshot_invalid")?;
        let root_admission = journal
            .authority
            .settled
            .get(&source.release.root_admission)
            .ok_or("root_admission_not_settled")?
            .clone();
        let owner_prepare = source
            .prepares
            .iter()
            .find(|entry| {
                entry.provider == source.release.owner && entry.admission_id == root.admission_id
            })
            .ok_or("owner_prepare_missing")?;
        if owner_prepare.transition_id != root.transition_id
            || root.callback_proof.as_ref()
                != Some(&session_prepare_evidence::closed_prepare_callback(
                    journal,
                    target,
                    owner_prepare,
                )?)
        {
            return Err("owner_prepare_provenance_mismatch".into());
        }
        let (owner_callback, dependent_callbacks) =
            session_release::validated_release_callbacks(journal, target, &source.release)?;
        let owner_release = TerminalCallbackEvidence {
            admission_id: source.release.owner_release_admission.clone(),
            admission: journal
                .authority
                .settled
                .get(&source.release.owner_release_admission)
                .ok_or("release_admission_not_settled")?
                .clone(),
            callback: owner_callback,
        };
        let mut dependents = Vec::new();
        for provider in session
            .provider_graph
            .as_ref()
            .ok_or("provider_graph_missing")?
            .order
            .iter()
            .skip(1)
        {
            let release = source
                .release
                .dependents
                .iter()
                .find(|entry| &entry.provider == provider)
                .ok_or("dependent_release_missing")?;
            let prepare = source
                .prepares
                .iter()
                .find(|entry| &entry.provider == provider)
                .ok_or("dependent_prepare_missing")?;
            let callback = dependent_callbacks
                .iter()
                .find(|(id, _)| id == provider)
                .ok_or("dependent_release_missing")?
                .1
                .clone();
            dependents.push(DependentTerminalEvidence {
                provider: provider.clone(),
                prepare: TerminalCallbackEvidence {
                    admission_id: prepare.admission_id.clone(),
                    admission: journal
                        .authority
                        .settled
                        .get(&prepare.admission_id)
                        .ok_or("prepare_admission_not_settled")?
                        .clone(),
                    callback: session_prepare_evidence::closed_prepare_callback(
                        journal, target, prepare,
                    )?,
                },
                release: TerminalCallbackEvidence {
                    admission_id: release.release_admission.clone(),
                    admission: journal
                        .authority
                        .settled
                        .get(&release.release_admission)
                        .ok_or("release_admission_not_settled")?
                        .clone(),
                    callback,
                },
            });
        }
        let graph = session
            .provider_graph
            .clone()
            .ok_or("provider_graph_missing")?;
        let provider_sequences = graph
            .order
            .iter()
            .map(|provider| {
                journal
                    .authority
                    .sequences
                    .get(provider)
                    .copied()
                    .map(|sequence| (provider.clone(), sequence))
                    .ok_or_else(|| "provider_sequence_missing".to_owned())
            })
            .collect::<ModuleResult<BTreeMap<_, _>>>()?;
        let mut legacy_operation_ids = BTreeSet::new();
        let root_callback = root
            .callback_proof
            .as_ref()
            .ok_or("root_callback_missing")?;
        for operation in std::iter::once(root_callback)
            .chain(std::iter::once(&owner_release.callback))
            .chain(
                dependents
                    .iter()
                    .flat_map(|entry| [&entry.prepare.callback, &entry.release.callback]),
            )
            .flat_map(callback_operation_ids)
        {
            if !operation.starts_with("lifecycle-") {
                if !journal.authority.issued_operation_ids.contains(operation) {
                    return Err("legacy_operation_not_fenced".into());
                }
                legacy_operation_ids.insert(operation.to_owned());
            }
        }
        let certificate = Self {
            version: CERTIFICATE_VERSION,
            target: target.clone(),
            owner: session.owner.clone(),
            end_receipt: {
                let mut matches = journal.receipts.iter().filter_map(|(key, receipt)| {
                    let candidate = TerminalEndReceipt {
                        key: key.clone(),
                        caller: receipt.caller.clone(),
                        initiator: receipt.initiator.clone(),
                        request: receipt.request.clone(),
                        response: receipt.response.clone(),
                    };
                    (receipt.work.is_none()
                        && receipt.wait_target.is_none()
                        && candidate.valid_for(target))
                    .then_some(candidate)
                });
                let first = matches.next();
                if matches.next().is_some() {
                    return Err("ambiguous_end_receipt".into());
                }
                first
            },
            graph,
            application_ref: session.application_ref.clone(),
            application: session.application.clone(),
            application_operation_id: session.application_operation_id.clone(),
            application_proof: session
                .application_retirement_proof
                .clone()
                .ok_or("application_retirement_proof_missing")?,
            producer: source.release.owner,
            resource_generation: source.release.resource_generation,
            root,
            root_admission,
            owner_release,
            dependents,
            closed_children: u32::try_from(children.len())
                .map_err(|_| "too_many_closed_children")?,
            children: (children.len() != 0).then_some(children),
            provider_sequences,
            operation_counter: journal.authority.operation_counter,
            legacy_operation_ids,
        };
        if !certificate.validate() {
            return Err("invalid_ended_session_certificate".into());
        }
        Ok(certificate)
    }

    /// Validate a retained transcript without consulting source proof books.
    /// Sequence bounds are checked separately against the current journal.
    pub(super) fn validate(&self) -> bool {
        let Ok((epoch, session_id, None, generation)) = operations::target_parts(&self.target)
        else {
            return false;
        };
        if self.version != CERTIFICATE_VERSION
            || self
                .end_receipt
                .as_ref()
                .is_some_and(|receipt| !receipt.valid_for(&self.target))
            || !valid_id(epoch)
            || !valid_id(session_id)
            || generation == 0
            || !valid_id(&self.owner.caller)
            || !valid_id(&self.owner.initiator)
            || !valid_id(&self.producer)
            || self.resource_generation == 0
            || match &self.children {
                Some(children) => {
                    children.len() != self.closed_children as usize
                        || !children.valid_for(&self.target)
                }
                None => self.closed_children != 0,
            }
            || self.operation_counter == 0
            || self.legacy_operation_ids.len() > 8
            || self
                .legacy_operation_ids
                .iter()
                .any(|id| !valid_id(id) || id.starts_with("lifecycle-"))
            || self.graph.order.first() != Some(&self.producer)
            || self.graph.order.len() != self.dependents.len() + 1
            || self.graph.dependencies.len() != self.graph.order.len()
            || self
                .graph
                .dependencies
                .get(&self.producer)
                .is_none_or(|deps| !deps.is_empty())
            || self.graph.order.iter().collect::<BTreeSet<_>>().len() != self.graph.order.len()
            || self.provider_sequences.len() != self.graph.order.len()
            || self.graph.order.iter().any(|provider| {
                !valid_id(provider)
                    || self
                        .provider_sequences
                        .get(provider)
                        .is_none_or(|n| *n == 0)
            })
            || !serde_json::to_vec(self).is_ok_and(|bytes| bytes.len() <= MAX_CERTIFICATE_BYTES)
        {
            return false;
        }
        let session = SessionRecord {
            provider_graph: Some(self.graph.clone()),
            host_epoch: epoch.into(),
            id: session_id.into(),
            generation,
            phase: SessionPhase::Ended,
            owner: self.owner.clone(),
            delegates: BTreeSet::new(),
            application_ref: self.application_ref.clone(),
            settings_revision: "certificate".into(),
            application_retired: true,
            application_retirement_proof: Some(self.application_proof.clone()),
            streams: BTreeMap::new(),
            obligations: vec![],
            required_consumers: None,
            application: self.application.clone(),
            application_operation_id: self.application_operation_id.clone(),
            admission_ids: BTreeSet::new(),
            admission_targets: BTreeMap::new(),
            admission_phases: BTreeMap::new(),
            provider_settings: BTreeMap::new(),
            resource_generations: BTreeMap::new(),
        };
        if !application::valid_retirement_proof(&session, &self.target)
            || self.root.state != composition::OutputState::Released
            || self.root.output.origin != self.target
            || self.root.output.producer != self.producer
            || self.root_admission.provider != self.producer
            || self.root.plan != self.root_admission.plan
            || !composition::validate_callback_provenance(&self.root, &self.root_admission)
            || self
                .root
                .callback_proof
                .as_ref()
                .is_none_or(|proof| proof.request.phase != LifecyclePhase::SessionPrepare)
            || self
                .root
                .callback_proof
                .as_ref()
                .and_then(|proof| proof.request.resource_generation)
                != Some(self.resource_generation)
        {
            return false;
        }
        let Some(response) = self.owner_release.response(
            &self.target,
            &self.producer,
            LifecyclePhase::SessionRelease,
        ) else {
            return false;
        };
        let ProviderResponse::Cleanup { outcomes, .. } = response else {
            return false;
        };
        let request = &self.owner_release.callback.request;
        let root_request = &self
            .root
            .callback_proof
            .as_ref()
            .expect("validated root")
            .request;
        let mut admissions = BTreeSet::from([
            self.root.admission_id.as_str(),
            self.owner_release.admission_id.as_str(),
        ]);
        let mut operations = BTreeSet::from([
            root_request.operation_id.as_str(),
            request.operation_id.as_str(),
        ]);
        if admissions.len() != 2
            || operations.len() != 2
            || root_request.provider_sequence >= request.provider_sequence
            || !callback_operations_covered(
                self.root.callback_proof.as_ref().expect("validated root"),
                self.operation_counter,
                &self.legacy_operation_ids,
            )
            || !callback_operations_covered(
                &self.owner_release.callback,
                self.operation_counter,
                &self.legacy_operation_ids,
            )
            || !operation_covered(
                &root_request.operation_id,
                self.operation_counter,
                &self.legacy_operation_ids,
            )
            || !operation_covered(
                &request.operation_id,
                self.operation_counter,
                &self.legacy_operation_ids,
            )
            || self
                .provider_sequences
                .get(&self.producer)
                .is_none_or(|sequence| {
                    *sequence < request.provider_sequence
                        || *sequence < root_request.provider_sequence
                })
        {
            return false;
        }
        let obligations = &self.owner_release.admission.plan.obligations;
        if request.dependency_inputs.len() != 1
            || !self.children.as_ref().map_or(
                request.dependency_inputs.first() == Some(&self.root.output),
                |children| {
                    request.dependency_inputs.first().is_some_and(|input| {
                        children.release_input_matches(&self.root, self.resource_generation, input)
                    })
                },
            )
            || !request.control_results.is_empty()
            || request.resource_generation != Some(self.resource_generation)
            || outcomes.len() != obligations.len()
            || outcomes
                .iter()
                .any(|outcome| outcome.state != CleanupState::Released)
            || obligations.iter().any(|obligation| {
                outcomes
                    .iter()
                    .filter(|outcome| outcome.obligation_id == obligation.obligation_id)
                    .count()
                    != 1
            })
            || self
                .root
                .plan
                .obligations
                .iter()
                .any(|obligation| !obligations.contains(obligation))
        {
            return false;
        }
        let root_transition = &self.root.transition_id;
        let release_transition = &request.transition_id;
        for (index, dependent) in self.dependents.iter().enumerate() {
            if self.graph.order.get(index + 1) != Some(&dependent.provider)
                || self
                    .graph
                    .dependencies
                    .get(&dependent.provider)
                    .is_none_or(|deps| deps != std::slice::from_ref(&self.producer))
                || !empty_plan(&dependent.prepare.admission.plan)
                || !empty_plan(&dependent.release.admission.plan)
                || dependent.prepare.callback.request.transition_id != *root_transition
                || dependent.release.callback.request.transition_id != *release_transition
                || !dependent
                    .prepare
                    .callback
                    .request
                    .dependency_inputs
                    .is_empty()
                || !dependent
                    .prepare
                    .callback
                    .request
                    .control_results
                    .is_empty()
                || !dependent
                    .release
                    .callback
                    .request
                    .dependency_inputs
                    .is_empty()
                || !dependent
                    .release
                    .callback
                    .request
                    .control_results
                    .is_empty()
                || !admissions.insert(&dependent.prepare.admission_id)
                || !admissions.insert(&dependent.release.admission_id)
                || !operations.insert(&dependent.prepare.callback.request.operation_id)
                || !operations.insert(&dependent.release.callback.request.operation_id)
                || !callback_operations_covered(
                    &dependent.prepare.callback,
                    self.operation_counter,
                    &self.legacy_operation_ids,
                )
                || !callback_operations_covered(
                    &dependent.release.callback,
                    self.operation_counter,
                    &self.legacy_operation_ids,
                )
                || dependent.prepare.callback.request.provider_sequence
                    >= dependent.release.callback.request.provider_sequence
                || !operation_covered(
                    &dependent.prepare.callback.request.operation_id,
                    self.operation_counter,
                    &self.legacy_operation_ids,
                )
                || !operation_covered(
                    &dependent.release.callback.request.operation_id,
                    self.operation_counter,
                    &self.legacy_operation_ids,
                )
                || self
                    .provider_sequences
                    .get(&dependent.provider)
                    .is_none_or(|sequence| {
                        *sequence < dependent.prepare.callback.request.provider_sequence
                            || *sequence < dependent.release.callback.request.provider_sequence
                    })
            {
                return false;
            }
            if !matches!(dependent.prepare.response(&self.target, &dependent.provider, LifecyclePhase::SessionPrepare), Some(ProviderResponse::Prepared { candidate: None, obligations, outputs }) if obligations.is_empty() && outputs.is_empty())
                || !matches!(dependent.release.response(&self.target, &dependent.provider, LifecyclePhase::SessionRelease), Some(ProviderResponse::Cleanup { outcomes, outputs }) if outcomes.is_empty() && outputs.is_empty())
            {
                return false;
            }
        }
        self.dependents.len() <= 1
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EndedSessionBook {
    certificates: BTreeMap<String, EndedSessionCertificate>,
}

impl EndedSessionBook {
    /// Historical roots are evidence of closed scopes, never authority for effects.
    pub(super) fn verified_roots<'a>(
        &'a self,
        journal: &Journal,
        producer: &str,
    ) -> impl Iterator<Item = &'a composition::StoredOutput> + 'a {
        let roots: Vec<_> = self
            .certificates
            .values()
            .filter(|cert| cert.producer == producer && Self::valid_entry(journal, cert))
            .map(|cert| &cert.root)
            .collect();
        roots.into_iter()
    }

    pub(super) fn end_response(
        &self,
        journal: &Journal,
        key: &str,
        request: &Value,
    ) -> Option<Value> {
        self.certificates.values().find_map(|cert| {
            let end = cert.end_receipt.as_ref()?;
            (end.key == key && Self::valid_entry(journal, cert)).then(|| {
                if request["operation"] == "lookup" || end.request == *request {
                    end.response.clone()
                } else {
                    rejection("operation_id_conflict")
                }
            })
        })
    }
    pub(super) fn matches_children(
        &self,
        journal: &Journal,
        target: &LifecycleTarget,
        expected: &closed_stream::SessionChildrenSnapshot,
    ) -> bool {
        let Some(cert) = Self::key(target).and_then(|key| self.certificates.get(&key)) else {
            return false;
        };
        cert.target == *target
            && Self::valid_entry(journal, cert)
            && cert.closed_children as usize == expected.len()
            && if expected.len() == 0 {
                cert.children.is_none()
            } else {
                cert.children.as_ref() == Some(expected)
            }
    }

    /// Resolve compact historical evidence only; this does not grant control.
    pub(super) fn historical_children<'a>(
        &'a self,
        journal: &Journal,
        target: &LifecycleTarget,
        owner: &str,
    ) -> Option<(
        &'a closed_stream::SessionChildrenSnapshot,
        &'a EndedSessionCertificate,
    )> {
        let (epoch, id, Some(_), _) = operations::target_parts(target).ok()? else {
            return None;
        };
        let key = serde_json::to_string(&(epoch, id)).ok()?;
        let cert = self.certificates.get(&key)?;
        let children = cert.children.as_ref()?;
        (cert.producer == owner
            && Self::valid_entry(journal, cert)
            && children.contains_scope(target)
            && !journal.closed_streams.contains_target(target))
        .then_some((children, cert))
    }

    /// Resolve immutable root provenance for historical checkpoint validation.
    /// This is not authority to control the resource or re-enter the session.
    /// The caller must also compare its pinned output commitment to the result.
    #[allow(dead_code, reason = "awaits closed-stream historical root integration")]
    pub(super) fn historical_root<'a>(
        &'a self,
        journal: &Journal,
        target: &LifecycleTarget,
        admission_id: &str,
        producer: &str,
        resource_generation: u64,
    ) -> Option<&'a composition::StoredOutput> {
        let cert = self.certificates.get(&Self::key(target)?)?;
        (cert.target == *target
            && cert.root.admission_id == admission_id
            && cert.producer == producer
            && cert.resource_generation == resource_generation
            && Self::valid_entry(journal, cert))
        .then_some(&cert.root)
    }

    fn valid_entry(journal: &Journal, cert: &EndedSessionCertificate) -> bool {
        cert.validate()
            && retention::session_expired(journal, &cert.target)
            && cert.end_receipt.as_ref().is_none_or(|end| {
                retention::expired(journal, &end.key) && !journal.receipts.contains_key(&end.key)
            })
            && operations::target_parts(&cert.target)
                .ok()
                .is_some_and(|(_, id, _, _)| !journal.sessions.contains_key(id))
            && journal.authority.operation_counter >= cert.operation_counter
            && cert
                .legacy_operation_ids
                .is_subset(&journal.authority.issued_operation_ids)
            && cert.provider_sequences.iter().all(|(provider, sequence)| {
                journal
                    .authority
                    .sequences
                    .get(provider)
                    .is_some_and(|current| current >= sequence)
            })
    }

    /// Stage only on a private book clone; the caller must revalidate the
    /// source and atomically install the target fence and retire its complete
    /// cross-book lineage before persisting the staged book.
    #[allow(
        dead_code,
        reason = "awaits the atomic ended-session closure transaction"
    )]
    pub(super) fn stage_verified(
        &mut self,
        source: &Journal,
        target: &LifecycleTarget,
    ) -> ModuleResult<()> {
        let cert = EndedSessionCertificate::from_verified(source, target)?;
        let key = Self::key(target).ok_or("session_target_required")?;
        if let Some(old) = self.certificates.get(&key) {
            return if old == &cert {
                Ok(())
            } else {
                Err("ended_session_certificate_conflict".into())
            };
        }
        if self.certificates.len() >= MAX_CERTIFICATES {
            return Err("ended_session_certificate_capacity".into());
        }
        self.certificates.insert(key, cert);
        Ok(())
    }
    fn key(target: &LifecycleTarget) -> Option<String> {
        let (epoch, session_id, stream, _) = operations::target_parts(target).ok()?;
        stream
            .is_none()
            .then(|| serde_json::to_string(&(epoch, session_id)).expect("string tuple serializes"))
    }

    /// Recovery validation requires the exact target fence and retained
    /// monotonic authority counters; it never reopens the ended target.
    #[allow(dead_code, reason = "awaits atomic ended-session journal integration")]
    pub(super) fn validate(&self, journal: &Journal) -> bool {
        let mut end_keys = BTreeSet::new();
        self.certificates.len() <= MAX_CERTIFICATES
            && self.certificates.iter().all(|(key, cert)| {
                Self::key(&cert.target).as_ref() == Some(key)
                    && Self::valid_entry(journal, cert)
                    && cert
                        .end_receipt
                        .as_ref()
                        .is_none_or(|end| end_keys.insert(&end.key))
                    && journal
                        .closed_streams
                        .session_children_raw(&cert.target)
                        .is_some_and(|snapshot| snapshot.len() == 0)
            })
    }
}
