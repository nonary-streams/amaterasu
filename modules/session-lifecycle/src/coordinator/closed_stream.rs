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
    #[serde(default)]
    archived: Vec<ArchivedStream>,
}

/// Compact, read-only query commitments after a successor takes over the
/// predecessor baseline. This record never grants an effect or a new stage.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchivedStream {
    target: LifecycleTarget,
    parent_generation: u64,
    resource_generation: u64,
    root_admission: String,
    root_output: String,
    owner: String,
    graph_commitment: String,
    release_commitment: String,
    finalize_commitment: String,
    finalize_tuple: String,
    finalize_policy: String,
    finalize_schemas: Vec<String>,
    settlement_commitments: Vec<String>,
    settlement_tuples: Vec<SettlementTuple>,
    closed_obligations: Vec<String>,
}

impl ArchivedStream {
    fn historical_proof<'a>(&self, journal: &'a Journal) -> Option<HistoricalProof<'a>> {
        let root = self.root(journal)?;
        Some(HistoricalProof {
            session_target: root.output.origin.clone(),
            resource_generation: self.resource_generation,
            root_output: &root.output,
        })
    }

    fn root<'a>(&self, journal: &'a Journal) -> Option<&'a composition::StoredOutput> {
        let (epoch, session_id, Some(stream_id), generation) =
            operations::target_parts(&self.target).ok()?
        else {
            return None;
        };
        if generation == 0 || !valid_id(epoch) || !valid_id(session_id) || !valid_id(stream_id) {
            return None;
        }
        let session = journal.sessions.get(session_id)?;
        if session.host_epoch != epoch || session.generation != self.parent_generation {
            return None;
        }
        let mut roots = journal.composition.records().filter(|root| {
            root.admission_id == self.root_admission && root.output.producer == self.owner
                && commitment(&root.output).as_deref() == Some(self.root_output.as_str())
                && matches!(&root.output.origin, LifecycleTarget::Session { host_epoch, session_id: id, generation }
                    if host_epoch == epoch && id == session_id && *generation == self.parent_generation)
        });
        let root = roots.next()?;
        if roots.next().is_some()
            || resource_generation::historical_generation(journal, root)
                != Ok(self.resource_generation)
        {
            return None;
        }
        Some(root)
    }

    fn valid(&self, journal: &Journal) -> bool {
        self.root(journal).is_some()
            && valid_digest(&self.root_output)
            && valid_digest(&self.graph_commitment)
            && valid_digest(&self.release_commitment)
            && valid_digest(&self.finalize_commitment)
            && valid_digest(&self.finalize_tuple)
            && valid_digest(&self.finalize_policy)
            && valid_schemas(&self.finalize_schemas)
            && valid_commitments(&self.closed_obligations)
            && valid_commitments(&self.settlement_commitments)
            && self.settlement_commitments.len() == self.settlement_tuples.len()
            && self.settlement_tuples.len() <= 128
            && self
                .settlement_tuples
                .windows(2)
                .all(|pair| pair[0] < pair[1])
            && self.settlement_tuples.iter().all(|tuple| {
                valid_id(&tuple.operation_id)
                    && valid_digest(&tuple.request_digest)
                    && valid_id(&tuple.resource_id)
                    && valid_id(&tuple.incarnation)
                    && valid_digest(&tuple.tuple)
                    && valid_digest(&tuple.policy)
                    && valid_schemas(&tuple.schemas)
            })
    }
}

pub(super) struct HistoricalProof<'a> {
    pub session_target: LifecycleTarget,
    pub resource_generation: u64,
    pub root_output: &'a amaterasu_extension_api::ResourceOutput,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RetirementOutcome {
    Retired,
    NotReady,
}

/// Work on a private clone so failure never partially retires the caller's
/// journal. The caller must atomically persist the replacement before effects.
pub(super) fn retire_closed_stream(
    next: &mut Journal,
    target: &LifecycleTarget,
    transition_id: &str,
) -> ModuleResult<RetirementOutcome> {
    let Some(certificate) = ClosedStream::from_verified(next, target, transition_id) else {
        return Ok(RetirementOutcome::NotReady);
    };
    let anchor = composition::StageAnchor {
        target: target.clone(),
        release_admission: certificate.release_admission.clone(),
        staged: certificate.staged.clone(),
        released: certificate.released.clone(),
    };
    let (epoch, session_id, Some(_), _) = operations::target_parts(target)? else {
        return Err("stream_target_required".into());
    };
    // Work on any same-session transition can hold a source admission or
    // historical output even when its own target differs.
    if !next.recovery_sources.is_empty()
        || next.actions.has_unresolved_session(epoch, session_id)
        || next.inputs.has_unresolved_session(epoch, session_id)
        || next.receipts.values().any(|receipt| {
            receipt.work.as_ref().is_some_and(|work| {
                operations::target_parts(&work.target).is_ok_and(|(_, id, _, _)| id == session_id)
            }) || receipt.wait_target.as_ref().is_some_and(|wait| {
                operations::target_parts(wait).is_ok_and(|(_, id, _, _)| id == session_id)
            })
        })
    {
        return Ok(RetirementOutcome::NotReady);
    }
    for stage in next.composition.staged_outputs().filter(|stage| {
        stage.after.producer == anchor.staged.producer
            && stage.after.output_id == anchor.staged.output_id
            && stage.after.origin == anchor.staged.origin
            && &stage.target != target
    }) {
        if !no_pending_stream_work(next, &stage.target)
            || next
                .closed_streams
                .released_output_for_target(next, &stage.target)
                .is_none()
        {
            return Ok(RetirementOutcome::NotReady);
        }
    }
    let Some(evidence) = next
        .composed_releases
        .verified_evidence_for(next, target, transition_id)
    else {
        return Err("closed_stream_proof_changed".into());
    };
    let session = next
        .sessions
        .get(session_id)
        .ok_or("closed_stream_session_missing")?;
    let removable_obligations: Vec<_> = session
        .obligations
        .iter()
        .filter(|obligation| {
            next.composed_releases
                .has_valid_released_obligation(next, target, obligation)
                && !session.admission_targets.iter().any(|(id, other_target)| {
                    other_target != target
                        && next
                            .authority
                            .admissions
                            .get(id)
                            .or_else(|| next.authority.settled.get(id))
                            .is_some_and(|admission| {
                                admission.plan.obligations.contains(obligation)
                            })
                })
        })
        .cloned()
        .collect();
    let finalize_id = evidence.finalized.request.permit_id.clone();
    let Some(finalize_permit) = next.authority.finalize_permits.get(&finalize_id) else {
        return Err("closed_stream_finalize_permit_missing".into());
    };
    if !finalize_permit.consumed
        || !finalize_permit.settled
        || next.authority.finalize_permits.values().any(|permit| {
            &permit.request.target == target && permit.request.permit_id != finalize_id
        })
    {
        return Ok(RetirementOutcome::NotReady);
    }
    let settlement_ids: BTreeSet<_> = next
        .control_settlements
        .records()
        .filter(|item| &item.settlement.request.target == target)
        .map(|item| item.settlement.settlement_id.clone())
        .collect();
    let settlement_permits: BTreeSet<_> = next
        .control_settlements
        .records()
        .filter(|item| &item.settlement.request.target == target)
        .map(|item| item.settlement.request.permit_id.clone())
        .collect();
    let target_permits: BTreeSet<_> = next
        .authority
        .control_permits
        .iter()
        .filter(|(_, permit)| &permit.request.target == target)
        .map(|(id, _)| id.clone())
        .collect();
    if settlement_permits != target_permits
        || target_permits.iter().any(|id| {
            next.authority
                .control_permits
                .get(id)
                .is_none_or(|permit| !permit.consumed || !permit.settled)
        })
    {
        return Ok(RetirementOutcome::NotReady);
    }
    let mut draft = next.clone();
    let mut book = draft.closed_streams.clone();
    if book.insert(&draft, certificate).is_err() {
        return Ok(RetirementOutcome::NotReady);
    }
    draft.closed_streams = book;
    draft
        .control_settlements
        .retire_exact(&settlement_ids, &|item| {
            &item.settlement.request.target == target
        })
        .map_err(str::to_owned)?;
    draft
        .finalize_results
        .retire_exact(target, &finalize_id)
        .map_err(str::to_owned)?;
    draft.composition.retire_stream_history(target);
    draft
        .composed_releases
        .remove_exact(target, transition_id)
        .map_err(str::to_owned)?;
    draft
        .authority
        .control_permits
        .retain(|id, _| !target_permits.contains(id));
    draft.authority.finalize_permits.remove(&finalize_id);
    draft
        .composition
        .replace_closed_stages(anchor)
        .map_err(str::to_owned)?;
    // A successor may replace the full predecessor only after the new anchor
    // is installed on this private clone. Preserve its public query commitments
    // and exact replay fence; those commitments never authorize effects.
    let current = draft
        .closed_streams
        .records
        .iter()
        .find(|item| &item.target == target)
        .ok_or("closed_stream_certificate_missing")?
        .clone();
    let predecessors: Vec<_> = draft
        .closed_streams
        .records
        .iter()
        .filter(|item| {
            item.target != current.target
                && item.root_admission == current.root_admission
                && item.root_output == current.root_output
                && item.owner == current.owner
                && item.resource_generation == current.resource_generation
        })
        .cloned()
        .collect();
    for old in predecessors {
        if !old.valid(&draft) || !no_pending_stream_work(&draft, &old.target) {
            return Ok(RetirementOutcome::NotReady);
        }
        let Some(scope) = scope_digest(&old.target) else {
            return Err("closed_stream_scope_invalid".into());
        };
        if draft.closed_streams.archived.len() >= MAX_SCOPE_FENCES
            || draft.closed_streams.expired_scopes.len() >= MAX_SCOPE_FENCES
            || draft.closed_streams.expired_scopes.contains(&scope)
            || draft
                .closed_streams
                .archived
                .iter()
                .any(|item| item.target == old.target)
        {
            return Ok(RetirementOutcome::NotReady);
        }
        let (_, old_session, Some(old_stream), _) = operations::target_parts(&old.target)? else {
            return Err("stream_target_required".into());
        };
        let old_admissions: Vec<_> = draft
            .sessions
            .get(old_session)
            .ok_or("closed_stream_session_missing")?
            .admission_targets
            .iter()
            .filter(|(_, value)| *value == &old.target)
            .map(|(id, _)| id.clone())
            .collect();
        if old_admissions.iter().any(|id| {
            draft.references_admission(id)
                || draft.authority.admissions.contains_key(id)
                || draft
                    .authority
                    .domain_locks
                    .values()
                    .any(|locked| locked == id)
        }) {
            return Ok(RetirementOutcome::NotReady);
        }
        let archived = ArchivedStream {
            target: old.target.clone(),
            parent_generation: old.parent_generation,
            resource_generation: old.resource_generation,
            root_admission: old.root_admission.clone(),
            root_output: old.root_output.clone(),
            owner: old.owner.clone(),
            graph_commitment: commitment(&old.graph).ok_or("closed_stream_graph_invalid")?,
            release_commitment: old.release_commitment.clone(),
            finalize_commitment: old.finalize_commitment.clone(),
            finalize_tuple: old.finalize_tuple.clone(),
            finalize_policy: old.finalize_policy.clone(),
            finalize_schemas: old.finalize_schemas.clone(),
            settlement_commitments: old.settlement_commitments.clone(),
            settlement_tuples: old.settlement_tuples.clone(),
            closed_obligations: old.closed_obligations.clone(),
        };
        draft.closed_streams.expired_scopes.insert(scope);
        draft
            .closed_streams
            .records
            .retain(|item| item.target != old.target);
        draft.closed_streams.archived.push(archived);
        for id in old_admissions {
            draft.authority.settled.remove(&id);
            draft.authority.output_admissions.remove(&id);
            draft.admission_transitions.remove(&id);
            let session = draft
                .sessions
                .get_mut(old_session)
                .ok_or("closed_stream_session_missing")?;
            session.admission_ids.remove(&id);
            session.admission_targets.remove(&id);
            session.admission_phases.remove(&id);
        }
        draft
            .sessions
            .get_mut(old_session)
            .ok_or("closed_stream_session_missing")?
            .streams
            .remove(old_stream);
    }
    draft
        .sessions
        .get_mut(session_id)
        .ok_or("closed_stream_session_missing")?
        .obligations
        .retain(|obligation| !removable_obligations.contains(obligation));
    let admissions: Vec<_> = draft
        .sessions
        .get(session_id)
        .ok_or("closed_stream_session_missing")?
        .admission_targets
        .iter()
        .filter(|(_, value)| *value == target)
        .map(|(id, _)| id.clone())
        .collect();
    for admission in admissions {
        if draft.references_admission(&admission)
            || draft
                .authority
                .domain_locks
                .values()
                .any(|id| id == &admission)
            || draft.authority.admissions.contains_key(&admission)
        {
            continue;
        }
        draft.authority.settled.remove(&admission);
        draft.authority.output_admissions.remove(&admission);
        draft.admission_transitions.remove(&admission);
        let session = draft
            .sessions
            .get_mut(session_id)
            .ok_or("closed_stream_session_missing")?;
        session.admission_ids.remove(&admission);
        session.admission_targets.remove(&admission);
        session.admission_phases.remove(&admission);
    }
    retention::compact(&mut draft);
    validate_journal(&draft).map_err(|_| "closed_stream_retirement_invalid".to_owned())?;
    *next = draft;
    Ok(RetirementOutcome::Retired)
}

impl ClosedStreamBook {
    /// Raw presence only; callers must reject ambiguous mixed source records.
    pub(super) fn contains_target(&self, target: &LifecycleTarget) -> bool {
        self.records.iter().any(|record| &record.target == target)
            || self.archived.iter().any(|record| &record.target == target)
    }

    pub(super) fn validate(&self, journal: &Journal) -> bool {
        self.records.len() <= MAX_CHECKPOINTS
            && self.archived.len() <= MAX_SCOPE_FENCES
            && self.expired_scopes.len() <= MAX_SCOPE_FENCES
            && self.expired_scopes.iter().all(|item| valid_digest(item))
            && self.archived.iter().enumerate().all(|(index, record)| {
                record.valid(journal)
                    && scope_digest(&record.target)
                        .is_some_and(|digest| self.expired_scopes.contains(&digest))
                    && !self.records.iter().any(|live| live.target == record.target)
                    && !self.archived[..index]
                        .iter()
                        .any(|old| old.target == record.target)
            })
            && self.records.iter().enumerate().all(|(index, record)| {
                record.valid(journal)
                    && scope_digest(&record.target)
                        .is_some_and(|digest| !self.expired_scopes.contains(&digest))
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

    pub(super) fn scope_fenced(&self, target: &LifecycleTarget) -> bool {
        self.contains_target(target)
            || scope_digest(target).is_some_and(|digest| self.expired_scopes.contains(&digest))
    }

    pub(super) fn released_output_for_target<'a>(
        &'a self,
        journal: &Journal,
        target: &LifecycleTarget,
    ) -> Option<(&'a str, &'a amaterasu_extension_api::ResourceOutput)> {
        if !self.validate(journal) {
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
        let amaterasu_extension_api::ResourceControlResponse::Applied { output, .. } = response
        else {
            return None;
        };
        settings
            .schema_catalog()
            .ok()?
            .validate(&output.schema, &output.data)
            .ok()?;
        if let Some(record) = self.unique_record(journal, &request.target) {
            return record
                .matches_finalize(journal, settings, caller, request, response)
                .then(|| record.historical_proof(journal))
                .flatten();
        }
        let record = self.unique_archive(journal, &request.target)?;
        if caller != record.owner
            || commitment(&(caller, request, response)).as_deref() != Some(&record.finalize_tuple)
            || !policies_installed(
                settings,
                &record.finalize_policy,
                &record.finalize_schemas,
                &record.settlement_tuples,
            )
        {
            return None;
        }
        record.historical_proof(journal)
    }

    pub(super) fn verify_settlement<'a>(
        &'a self,
        journal: &'a Journal,
        settings: &LifecycleSettings,
        caller: &str,
        request: &amaterasu_extension_api::ResourceControlSettlementRequest,
    ) -> Option<HistoricalProof<'a>> {
        settings
            .schema_catalog()
            .ok()?
            .validate(&request.output.schema, &request.output.data)
            .ok()?;
        if let Some(record) = self.unique_record(journal, &request.request.target) {
            return record
                .matches_settlement(journal, settings, caller, request)
                .then(|| record.historical_proof(journal))
                .flatten();
        }
        let record = self.unique_archive(journal, &request.request.target)?;
        let tuple = commitment(&(caller, request))?;
        if caller != record.owner
            || !record
                .settlement_tuples
                .iter()
                .any(|item| item.tuple == tuple)
            || !policies_installed(
                settings,
                &record.finalize_policy,
                &record.finalize_schemas,
                &record.settlement_tuples,
            )
        {
            return None;
        }
        record.historical_proof(journal)
    }

    /// Resolves an exact owner-acknowledged control within a fully closed
    /// stream. This read-only proof must never authorize a new effect.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn verify_closed_control(
        &self,
        journal: &Journal,
        settings: &LifecycleSettings,
        caller: &str,
        target: &LifecycleTarget,
        generation: u64,
        resource_id: &str,
        incarnation: &str,
        operation_id: &str,
        request_digest: &str,
    ) -> bool {
        let check = |owner: &str,
                     actual_generation: u64,
                     tuples: &[SettlementTuple],
                     policy: &str,
                     schemas: &[String],
                     root: &amaterasu_extension_api::ResourceOutput| {
            caller == owner
                && generation == actual_generation
                && root.resource.resource_id == resource_id
                && root.resource.incarnation == incarnation
                && valid_digest(request_digest)
                && policies_installed(settings, policy, schemas, tuples)
                && tuples
                    .iter()
                    .filter(|item| {
                        item.operation_id == operation_id
                            && item.request_digest == request_digest
                            && item.resource_id == resource_id
                            && item.incarnation == incarnation
                    })
                    .count()
                    == 1
        };
        if let Some(record) = self.unique_record(journal, target) {
            return record.historical_proof(journal).is_some_and(|proof| {
                check(
                    &record.owner,
                    record.resource_generation,
                    &record.settlement_tuples,
                    &record.finalize_policy,
                    &record.finalize_schemas,
                    proof.root_output,
                )
            });
        }
        self.unique_archive(journal, target).is_some_and(|record| {
            record.historical_proof(journal).is_some_and(|proof| {
                check(
                    &record.owner,
                    record.resource_generation,
                    &record.settlement_tuples,
                    &record.finalize_policy,
                    &record.finalize_schemas,
                    proof.root_output,
                )
            })
        })
    }

    fn unique_archive<'a>(
        &'a self,
        journal: &Journal,
        target: &LifecycleTarget,
    ) -> Option<&'a ArchivedStream> {
        if !self.validate(journal) {
            return None;
        }
        let mut records = self
            .archived
            .iter()
            .filter(|record| &record.target == target);
        let record = records.next()?;
        records.next().is_none().then_some(record)
    }

    fn unique_record<'a>(
        &'a self,
        journal: &Journal,
        target: &LifecycleTarget,
    ) -> Option<&'a ClosedStream> {
        if !self.validate(journal) {
            return None;
        }
        let mut matches = self
            .records
            .iter()
            .filter(|record| &record.target == target);
        let record = matches.next()?;
        matches.next().is_none().then_some(record)
    }

    pub(super) fn validates_anchor(
        &self,
        journal: &Journal,
        anchor: &composition::StageAnchor,
    ) -> bool {
        self.unique_record(journal, &anchor.target)
            .is_some_and(|record| {
                record.release_admission == anchor.release_admission
                    && record.staged == anchor.staged
                    && record.released == anchor.released
            })
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
    #[serde(default)]
    closed_obligations: Vec<String>,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettlementTuple {
    operation_id: String,
    request_digest: String,
    resource_id: String,
    incarnation: String,
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

/// Necessary, not sufficient, for a closed stream retirement. The exact
/// verified graph, cross-book inbound references and replay-fence budget must
/// also be checked on the same journal clone before committing any removal.
fn no_pending_stream_work(journal: &Journal, target: &LifecycleTarget) -> bool {
    let Ok((epoch, session_id, Some(stream_id), generation)) = operations::target_parts(target)
    else {
        return false;
    };
    let Some(session) = journal.sessions.get(session_id) else {
        return false;
    };
    let Some(stream) = session.streams.get(stream_id) else {
        return false;
    };
    session.host_epoch == epoch
        && stream.generation == generation
        && stream.phase == StreamPhase::Retired
        && stream.required.is_subset(&stream.retirements)
        && !journal
            .inputs
            .has_unresolved_stream(epoch, session_id, stream_id)
        && !journal
            .actions
            .has_unresolved_stream(epoch, session_id, stream_id)
        && !journal
            .input_bindings
            .values()
            .any(|binding| binding.session_id == session_id && binding.stream_id == stream_id)
        && !journal.receipts.values().any(|receipt| {
            receipt
                .work
                .as_ref()
                .is_some_and(|work| &work.target == target)
                || receipt.wait_target.as_ref() == Some(target)
        })
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
        if !no_pending_stream_work(journal, target) {
            return None;
        }
        let proof =
            journal
                .composed_releases
                .verified_evidence_for(journal, target, transition_id)?;
        let owner = &proof.released_output.producer;
        let stage = proof.finalized.selection.stage.as_ref()?;
        let (_, session_id, Some(_), _) = operations::target_parts(target).ok()? else {
            return None;
        };
        let session = journal.sessions.get(session_id)?;
        let mut closed_obligations = session
            .obligations
            .iter()
            .filter(|obligation| {
                journal
                    .composed_releases
                    .has_valid_released_obligation(journal, target, obligation)
            })
            .map(commitment)
            .collect::<Option<Vec<_>>>()?;
        closed_obligations.sort();
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
                    operation_id: item.settlement.request.operation_id.clone(),
                    request_digest: format!(
                        "{:x}",
                        Sha256::digest(serde_json::to_vec(&item.settlement.request).ok()?)
                    ),
                    resource_id: item
                        .settlement
                        .request
                        .proposal
                        .resource
                        .resource_id
                        .clone(),
                    incarnation: item
                        .settlement
                        .request
                        .proposal
                        .resource
                        .incarnation
                        .clone(),
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
            closed_obligations,
        };
        result.valid(journal).then_some(result)
    }

    /// Historical proof queries retain the same installed-policy checks as the
    /// full record. This still does not establish any live mutation authority.
    fn installed(&self, settings: &LifecycleSettings) -> bool {
        policies_installed(
            settings,
            &self.finalize_policy,
            &self.finalize_schemas,
            &self.settlement_tuples,
        )
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
            && valid_commitments(&self.closed_obligations)
            && self.settlement_tuples.len() <= 128
            && self
                .settlement_tuples
                .windows(2)
                .all(|pair| pair[0] < pair[1])
            && self.settlement_tuples.iter().all(|entry| {
                valid_id(&entry.operation_id)
                    && valid_digest(&entry.request_digest)
                    && valid_id(&entry.resource_id)
                    && valid_id(&entry.incarnation)
                    && valid_digest(&entry.tuple)
                    && valid_digest(&entry.policy)
                    && valid_schemas(&entry.schemas)
            })
            && self.settlement_commitments.len() == self.settlement_tuples.len()
    }
}

fn policies_installed(
    settings: &LifecycleSettings,
    finalize_policy: &str,
    finalize_schemas: &[String],
    settlements: &[SettlementTuple],
) -> bool {
    let docs = |expected: &[String]| {
        expected.iter().all(|digest| {
            settings
                .schema_documents
                .iter()
                .any(|doc| commitment(doc).as_ref() == Some(digest))
        })
    };
    settings
        .finalize_policies
        .iter()
        .any(|policy| commitment(policy).as_deref() == Some(finalize_policy))
        && docs(finalize_schemas)
        && settlements.iter().all(|tuple| {
            settings
                .control_policies
                .iter()
                .any(|policy| commitment(policy).as_ref() == Some(&tuple.policy))
                && docs(&tuple.schemas)
        })
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
        let mut fenced = book;
        fenced.expired_scopes.insert(scope_digest(&target).unwrap());
        assert!(fenced.scope_fenced(&target));
        let other: LifecycleTarget = serde_json::from_value(json!({
            "kind":"stream","host_epoch":"epoch","session_id":"session",
            "stream_id":"other","generation":1
        }))
        .unwrap();
        assert!(!fenced.scope_fenced(&other));
    }

    #[test]
    fn pending_wait_or_unretired_consumer_blocks_stream_retirement() {
        let target: LifecycleTarget = serde_json::from_value(json!({
            "kind":"stream","host_epoch":"epoch","session_id":"session",
            "stream_id":"stream","generation":1
        }))
        .unwrap();
        let mut journal: Journal = serde_json::from_value(json!({
            "version":3,"host_epoch":"epoch","receipts":{},"sessions":{"session":{
                "host_epoch":"epoch","id":"session","generation":1,"phase":"paused",
                "owner":{"caller":"caller","initiator":"client"},"delegates":[],
                "application_ref":null,"settings_revision":"one","application_retired":false,
                "streams":{"stream":{"generation":1,"phase":"retired","candidate":null,
                    "required":["capture"],"acknowledgements":[],"retirements":["capture"]}}
            }}
        }))
        .unwrap();
        assert!(no_pending_stream_work(&journal, &target));
        journal
            .sessions
            .get_mut("session")
            .unwrap()
            .streams
            .get_mut("stream")
            .unwrap()
            .retirements
            .clear();
        assert!(!no_pending_stream_work(&journal, &target));
        journal
            .sessions
            .get_mut("session")
            .unwrap()
            .streams
            .get_mut("stream")
            .unwrap()
            .retirements
            .insert("capture".into());
        let receipt: Receipt = serde_json::from_value(json!({
            "caller":"caller","initiator":"client","request":{},"response":{"status":"pending"},
            "work":null,"wait_target":target
        }))
        .unwrap();
        journal.receipts.insert("pending".into(), receipt);
        assert!(!no_pending_stream_work(&journal, &target));
    }
}
