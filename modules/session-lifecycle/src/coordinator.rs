//! Durable lifecycle authority; native work advances outside request handlers.
#[allow(dead_code)] // Closure preflight is not enabled until atomic retirement exists.
mod action_closed_history;
#[allow(dead_code)]
// Frozen dispatch evidence is not authority until cleanup and cursor checks are wired.
mod action_dispatch_proof;
mod action_disposition;
mod action_endpoints;
#[allow(dead_code)] // Action replan remains fenced until all proof writers are wired.
mod action_release_proof;
mod action_root_retirement;
mod action_target;
mod application;
mod checkpoint_queries;
#[allow(dead_code)] // Checkpoint integration is not yet enabled.
mod closed_stream;
mod composed_release_proof;
#[allow(dead_code)] // Admission and control remain disabled until schema verification is wired.
mod composition;
#[allow(dead_code)] // Controls stay gated until sequencing is qualified.
mod composition_control;
#[allow(dead_code)] // Composed finalization is still gated.
mod composition_finalize;
mod composition_prepare;
mod composition_progress;
#[allow(dead_code)] // Release remains gated until provider and proof hooks are wired.
mod composition_release;
mod control_backend;
mod control_baseline;
#[allow(dead_code)] // Control dispatch remains gated until coordinator sequencing is wired.
mod control_dispatch;
mod control_endpoints;
mod control_history;
#[allow(dead_code)]
mod control_settlement;
mod finalize_backend;
#[allow(dead_code)]
mod finalize_dispatch;
mod finalize_endpoints;
#[allow(dead_code)]
mod finalize_result;
mod finalize_result_verify;
#[allow(dead_code)] // Finalization selection remains gated until composition sequencing is wired.
mod finalize_selection;
mod observation_scheduler;
mod operations;
#[allow(dead_code)]
mod plan_observation;
mod prepared_plans;
mod provider_observations;
mod providers;
mod recovery;
mod refresh_receipt;
#[allow(dead_code)] // Generation lookup is gated on authenticated endpoints.
mod resource_generation;
mod retention;
mod security;
mod session_closed_history;
mod session_closure;
mod session_prepare_evidence;
#[allow(dead_code)]
mod session_release;
mod settings_scope;
mod settings_snapshot;
#[allow(dead_code)] // Settlement dispatch is gated until composition sequencing is wired.
mod settlement_dispatch;
mod stage_backend;
mod stream_closure_lookup;
#[allow(dead_code)]
mod stream_release;
#[allow(dead_code, reason = "awaits the atomic ended-session transaction")]
mod terminal_root;

use crate::adapter::LifecycleSettings;
use crate::prepare_authority::{AuthorityState, ProviderSelection};
use crate::prepare_driver::{PrepareDriver, PrepareState};
use crate::store::LifecycleStore;
use amaterasu_extension_api::{
    CaptureBinding, LifecyclePhase, LifecycleTarget, ReleaseReason, StreamIntent, VideoMode,
};
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    io,
    path::Path,
    rc::Rc,
};

const MAX_RECEIPTS: usize = 4096;
const MAX_SESSIONS: usize = 128;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    caller: String,
    initiator: String,
    request: Value,
    response: Value,
    #[serde(default)]
    work: Option<Work>,
    #[serde(default)]
    wait_target: Option<LifecycleTarget>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderGraph {
    order: Vec<String>,
    dependencies: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Work {
    /// Inert until composition sequencing and owner verification are installed.
    #[serde(default)]
    composition_progress: Option<composition_progress::CompositionProgress>,
    #[serde(default, skip_serializing_if = "is_false")]
    compacted_release: bool,
    #[serde(default)]
    owner_observations: BTreeMap<String, plan_observation::PlanObservationState>,
    #[serde(default)]
    reprepare: Option<ReprepareContinuation>,
    phase: LifecyclePhase,
    target: LifecycleTarget,
    intent: Option<StreamIntent>,
    reason: Option<ReleaseReason>,
    video: Option<VideoMode>,
    transition_id: String,
    providers: Vec<String>,
    #[serde(default)]
    provider_graph: Option<ProviderGraph>,
    #[serde(default)]
    cleanup_blockers: BTreeSet<String>,
    index: usize,
    states: BTreeMap<String, PrepareState>,
    candidate: Option<CaptureBinding>,
    waiting_retirement: bool,
    #[serde(default)]
    failure: Option<String>,
    #[serde(default)]
    failed_preparation: Option<FailedPreparation>,
    #[serde(default)]
    action_transfer: Option<ActionTransfer>,
    #[serde(default)]
    recovery_authorized_epoch: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !value
}

/// Frozen child work is resumed only after a new producer root is proved.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReprepareContinuation {
    owner: String,
    old_generation: u64,
    old_incarnation: String,
    next: Box<Work>,
}

impl Work {
    fn valid_reprepare_shape(&self) -> bool {
        let Some(continuation) = &self.reprepare else {
            return true;
        };
        let Ok((epoch, session, None, _)) = operations::target_parts(&self.target) else {
            return false;
        };
        let Ok((next_epoch, next_session, Some(_), _)) =
            operations::target_parts(&continuation.next.target)
        else {
            return false;
        };
        self.phase == LifecyclePhase::SessionPrepare
            && valid_id(&continuation.owner)
            && valid_id(&continuation.old_incarnation)
            && continuation.old_generation > 0
            && self.providers == [continuation.owner.clone()]
            && self
                .provider_graph
                .as_ref()
                .is_some_and(|graph| graph.order.contains(&continuation.owner))
            && serde_json::to_value(&self.provider_graph).ok()
                == serde_json::to_value(&continuation.next.provider_graph).ok()
            && self.action_transfer.is_none()
            && continuation.next.phase == LifecyclePhase::StreamPrepare
            && continuation.next.reprepare.is_none()
            && continuation.next.action_transfer.is_none()
            && continuation.next.states.is_empty()
            && continuation.next.index == 0
            && epoch == next_epoch
            && session == next_session
    }

    fn reprepare_authorized(&self, journal: &Journal) -> bool {
        let Some(continuation) = &self.reprepare else {
            return true;
        };
        if !self.valid_reprepare_shape() {
            return false;
        }
        let Ok((_, session_id, None, _)) = operations::target_parts(&self.target) else {
            return false;
        };
        let Some(session) = journal.sessions.get(session_id) else {
            return false;
        };
        let Some(expected_generation) = continuation.old_generation.checked_add(1) else {
            return false;
        };
        let Some(marker) = action_disposition::resolved_at(
            journal,
            session_id,
            &continuation.owner,
            continuation.old_generation,
        ) else {
            return false;
        };
        let Ok((_, _, Some(stream_id), generation)) =
            operations::target_parts(&continuation.next.target)
        else {
            return false;
        };
        marker.session_target == self.target
            && marker.resource.incarnation == continuation.old_incarnation
            && session.resource_generations.get(&continuation.owner) == Some(&expected_generation)
            && !session.application_retired
            && matches!(
                session.phase,
                SessionPhase::Preparing | SessionPhase::RecoveryRequired
            )
            && session.streams.get(stream_id).is_some_and(|stream| {
                stream.generation == generation
                    && matches!(
                        stream.phase,
                        StreamPhase::Preparing | StreamPhase::RecoveryRequired
                    )
                    && stream.preparation_transition == continuation.next.transition_id
            })
            && session
                .streams
                .iter()
                .all(|(id, stream)| id == stream_id || stream.phase == StreamPhase::Retired)
    }

    fn use_release_graph(&mut self, graph: Option<ProviderGraph>) {
        if let Some(graph) = &graph {
            self.providers = graph.order.iter().rev().cloned().collect();
        }
        self.provider_graph = graph;
    }

    // Missing or malformed persisted graphs never prove independence.
    fn cleanup_blocked(&self, provider: &str) -> bool {
        let Some(graph) = &self.provider_graph else {
            return true;
        };
        let positions: BTreeMap<_, _> = graph
            .order
            .iter()
            .enumerate()
            .map(|(index, id)| (id.as_str(), index))
            .collect();
        if positions.len() != graph.order.len()
            || graph.dependencies.len() != positions.len()
            || graph.dependencies.iter().any(|(id, dependencies)| {
                let Some(position) = positions.get(id.as_str()) else {
                    return true;
                };
                let unique: BTreeSet<_> = dependencies.iter().collect();
                unique.len() != dependencies.len()
                    || dependencies.iter().any(|dependency| {
                        positions
                            .get(dependency.as_str())
                            .is_none_or(|parent| parent >= position)
                    })
            })
            || !positions.contains_key(provider)
            || self
                .cleanup_blockers
                .iter()
                .any(|id| !positions.contains_key(id.as_str()))
        {
            return true;
        }
        let mut frontier: Vec<_> = self.cleanup_blockers.iter().map(String::as_str).collect();
        let mut visited = BTreeSet::new();
        while let Some(id) = frontier.pop() {
            if !visited.insert(id) {
                continue;
            }
            if id == provider {
                return true;
            }
            frontier.extend(graph.dependencies[id].iter().map(String::as_str));
        }
        false
    }
}

/// Exact reservation ownership transferred to a durable teardown work item.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionTransfer {
    service: String,
    permit: String,
    binding: crate::action_authority::ActionBinding,
    input_reservation_id: Option<String>,
    #[serde(default)]
    admission_id: Option<String>,
    #[serde(default)]
    snapshot: Option<ActionReleaseSnapshot>,
    #[serde(default)]
    stage: ActionTransferStage,
    #[serde(default)]
    release_proof: Option<action_release_proof::ActionReleaseProof>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ActionTransferStage {
    #[default]
    CleanupPending,
    ReplanRequired,
    Rebound,
}

/// Immutable pre-cleanup lineage captured in the action-transfer transaction.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionReleaseSnapshot {
    session_target: LifecycleTarget,
    stream_target: LifecycleTarget,
    session_graph: ProviderGraph,
    stream_graph: ProviderGraph,
    session_admissions: BTreeMap<String, ActionSnapshotAdmission>,
    stream_admissions: BTreeMap<String, ActionSnapshotAdmission>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionSnapshotAdmission {
    admission: crate::prepare_authority::Admission,
    transition_id: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FailedPreparation {
    reason: String,
    uncertain: bool,
    states: BTreeMap<String, PrepareState>,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SessionPhase {
    Preparing,
    Ready,
    Paused,
    Ending,
    Retired,
    Ended,
    RecoveryRequired,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StreamPhase {
    Preparing,
    Ready,
    Active,
    Quiescing,
    Retired,
    RecoveryRequired,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StreamRecord {
    #[serde(default)]
    provider_graph: Option<ProviderGraph>,
    generation: u64,
    phase: StreamPhase,
    candidate: Option<CaptureBinding>,
    required: BTreeSet<String>,
    acknowledgements: BTreeSet<String>,
    retirements: BTreeSet<String>,
    #[serde(default)]
    binding_owner: String,
    #[serde(default)]
    preparation_transition: String,
    #[serde(default)]
    provider_settings: BTreeMap<String, settings_snapshot::FrozenSettings>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionRecord {
    #[serde(
        default,
        skip_serializing_if = "amaterasu_extension_api::CaptureBindingMode::is_managed"
    )]
    capture_binding_mode: amaterasu_extension_api::CaptureBindingMode,
    #[serde(default)]
    provider_graph: Option<ProviderGraph>,
    host_epoch: String,
    id: String,
    generation: u64,
    phase: SessionPhase,
    owner: crate::action_authority::Principal,
    delegates: BTreeSet<String>,
    application_ref: Value,
    settings_revision: String,
    application_retired: bool,
    #[serde(default)]
    application_retirement_proof: Option<application::ApplicationRetirementProof>,
    streams: BTreeMap<String, StreamRecord>,
    #[serde(default)]
    obligations: Vec<amaterasu_extension_api::ResourceObligation>,
    #[serde(default)]
    required_consumers: Option<BTreeSet<String>>,
    #[serde(default)]
    application: Option<application::ApplicationBinding>,
    #[serde(default)]
    application_operation_id: Option<String>,
    #[serde(default)]
    admission_ids: BTreeSet<String>,
    /// Exact coordinator-issued target for each retained admission. Older
    /// journals may lack provenance; operations requiring it must fail closed.
    #[serde(default)]
    admission_targets: BTreeMap<String, LifecycleTarget>,
    /// Original lifecycle phase; missing legacy entries cannot establish ownership.
    #[serde(default)]
    admission_phases: BTreeMap<String, LifecyclePhase>,
    /// Only admissions whose authenticated callback reported Prepared may
    /// supply agreed constraints to a later application launch.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    prepared_plan_ids: BTreeSet<String>,
    #[serde(default)]
    provider_settings: BTreeMap<String, settings_snapshot::FrozenSettings>,
    /// Coordinator-owned generation of each session resource producer.
    #[serde(default)]
    resource_generations: BTreeMap<String, u64>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u32,
    host_epoch: String,
    receipts: BTreeMap<String, Receipt>,
    #[serde(default)]
    sessions: BTreeMap<String, SessionRecord>,
    #[serde(default)]
    authority: AuthorityState,
    #[serde(default)]
    inputs: crate::input_authority::InputAuthority,
    #[serde(default)]
    actions: crate::action_authority::ActionAuthority,
    #[serde(default)]
    input_bindings: BTreeMap<String, crate::input_authority::ActiveBinding>,
    #[serde(default)]
    revoked_inputs: BTreeSet<String>,
    #[serde(default)]
    recovery_sources: BTreeMap<String, String>,
    #[serde(default)]
    input_boot_epochs: BTreeMap<String, String>,
    #[serde(default)]
    retention: retention::Retention,
    #[serde(default)]
    ended_session_history: session_closed_history::EndedSessionBook,
    #[serde(default)]
    composition: composition::CompositionBook,
    #[serde(default)]
    closed_streams: closed_stream::ClosedStreamBook,
    #[serde(default)]
    closed_actions: action_closed_history::ClosedActionBook,
    #[serde(default)]
    control_settlements: control_settlement::ControlSettlementBook,
    #[serde(default)]
    finalize_results: finalize_result::FinalizeResultBook,
    #[serde(default)]
    stream_releases: stream_release::StreamReleaseBook,
    #[serde(default)]
    composed_releases: composed_release_proof::ComposedReleaseBook,
    #[serde(default)]
    session_releases: session_release::SessionReleaseBook,
    #[serde(default)]
    session_prepare_evidence: session_prepare_evidence::SessionPrepareEvidence,
    #[serde(default)]
    action_dispositions: BTreeMap<String, action_disposition::ReleasedActionMarker>,
    /// Immutable producing transition of each retained admission.
    #[serde(default)]
    admission_transitions: BTreeMap<String, String>,
}

impl Journal {
    fn validate_composition(&self) -> bool {
        self.composition.validate_structure()
            && self
                .composition
                .stage_anchors()
                .all(|anchor| self.closed_streams.validates_anchor(self, anchor))
    }

    fn verified_released_output(
        &self,
        target: &LifecycleTarget,
    ) -> Option<(&str, &amaterasu_extension_api::ResourceOutput)> {
        match (
            self.stream_releases.has_released(target),
            self.composed_releases.contains_target(target),
            self.closed_streams.released_output_for_target(self, target),
        ) {
            (true, false, None) if self.stream_releases.has_valid_released(self, target) => {
                self.stream_releases.released_output_for_target(target)
            }
            (false, true, None) if self.composed_releases.has_valid_release(self, target) => {
                self.composed_releases.released_output_for_target(target)
            }
            (false, false, Some(output)) => Some(output),
            _ => None,
        }
    }

    fn references_admission(&self, id: &str) -> bool {
        self.action_dispositions.values().any(|marker| {
            marker.root_admission_id == id
                || marker
                    .cleanup_work
                    .action_transfer
                    .as_ref()
                    .is_some_and(|transfer| {
                        transfer.admission_id.as_deref() == Some(id)
                            || transfer.snapshot.as_ref().is_some_and(|snapshot| {
                                snapshot.session_admissions.contains_key(id)
                                    || snapshot.stream_admissions.contains_key(id)
                            })
                    })
        }) || self.composed_releases.references_admission(id)
            || self.session_prepare_evidence.references_admission(id)
            || self.session_releases.references_admission(id)
            || self.stream_releases.references_admission(id)
            || self
                .finalize_results
                .records()
                .any(|item| item.request.admission_id == id)
            || self.composition.references_admission(id)
            || self
                .control_settlements
                .records()
                .any(|record| record.settlement.request.admission_id == id)
    }
}

struct SharedJournal {
    store: LifecycleStore,
    value: Journal,
    // Closure eligibility depends on durable state. Recheck after a successful
    // replacement, rather than cloning and validating history on every tick.
    retirement_scan_pending: bool,
}
type Shared = Rc<RefCell<SharedJournal>>;

fn receipt_key(caller: &str, initiator: &str, operation_id: &str) -> String {
    serde_json::to_string(&(caller, initiator, operation_id)).expect("string tuple serializes")
}
fn identifier() -> ModuleResult<String> {
    amaterasu_storage::random_identifier().map_err(|_| "entropy_unavailable".into())
}
fn rejection(reason: &str) -> Value {
    json!({"status":"rejected","reason":reason})
}
fn valid_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}
fn save(shared: &Shared, mut next: Journal) -> ModuleResult<()> {
    action_closed_history::compact_released_actions(&mut next);
    retention::compact(&mut next);
    action_root_retirement::compact_replaced_roots(&mut next);
    let mut shared = shared.borrow_mut();
    shared
        .store
        .replace(&next)
        .map_err(|_| "journal_write_failed".to_owned())?;
    shared.value = next;
    shared.retirement_scan_pending = true;
    Ok(())
}
fn validate_journal(journal: &Journal) -> io::Result<()> {
    if !matches!(journal.version, 2 | 3)
        || !valid_id(&journal.host_epoch)
        || journal.receipts.len() > MAX_RECEIPTS
        || journal.sessions.len() > MAX_SESSIONS
        || journal.sessions.values().any(|session| {
            session.prepared_plan_ids.len() > 128
                || session.prepared_plan_ids.iter().any(|id| !valid_id(id))
        })
        || journal.receipts.values().any(|receipt| {
            receipt.work.as_ref().is_some_and(|work| {
                !work.reprepare_authorized(journal)
                    || !observation_scheduler::validate_work_observations(journal, work)
                    || (work.compacted_release
                        && work.action_transfer.as_ref().is_none_or(|transfer| {
                            !matches!(
                                transfer.stage,
                                ActionTransferStage::ReplanRequired | ActionTransferStage::Rebound
                            )
                        }))
                    || work.action_transfer.as_ref().is_some_and(|transfer| {
                        (matches!(
                            transfer.stage,
                            ActionTransferStage::ReplanRequired | ActionTransferStage::Rebound
                        ) && (work.compacted_release
                            || journal.composed_releases.contains_target(&work.target))
                            && !journal
                                .composed_releases
                                .verified_work_for(journal, &work.target, &work.transition_id)
                                .is_some_and(|closed| {
                                    control_history::matching_release_work(journal, work, closed)
                                }))
                            || transfer.snapshot.as_ref().is_some_and(|snapshot| {
                                !action_endpoints::action_snapshot_matches(
                                    journal, receipt, work, transfer, snapshot,
                                )
                            })
                            || match transfer.stage {
                                ActionTransferStage::CleanupPending => {
                                    transfer.release_proof.is_some()
                                }
                                ActionTransferStage::ReplanRequired => {
                                    !action_release_proof::validate_replan_stage(
                                        journal, receipt, work, transfer,
                                    )
                                }
                                ActionTransferStage::Rebound => {
                                    !action_release_proof::validate_rebound_stage(
                                        journal, receipt, work, transfer,
                                    )
                                }
                            }
                    })
            })
        })
        || journal.sessions.values().any(|session| {
            session
                .application_retirement_proof
                .as_ref()
                .is_some_and(|_| {
                    let target = LifecycleTarget::Session {
                        host_epoch: session.host_epoch.clone(),
                        session_id: session.id.clone(),
                        generation: session.generation,
                    };
                    !application::valid_retirement_proof(session, &target)
                })
                || session.resource_generations.len() > 32
                || session
                    .resource_generations
                    .iter()
                    .any(|(provider, generation)| !valid_id(provider) || *generation == 0)
        })
        || journal.action_dispositions.len() > MAX_SESSIONS * 32
        || journal.action_dispositions.iter().any(|(key, marker)| {
            let LifecycleTarget::Session { session_id, .. } = &marker.session_target else {
                return true;
            };
            key != &action_disposition::marker_key(
                session_id,
                &marker.owner,
                marker.resource_generation,
            ) || action_disposition::verified_marker_at(
                journal,
                session_id,
                &marker.owner,
                marker.resource_generation,
            )
            .is_none()
        })
        || !retention::validate(journal)
        || !journal.ended_session_history.validate(journal)
        || journal.admission_transitions.len() > 128
        || journal
            .admission_transitions
            .iter()
            .any(|(id, transition)| {
                !valid_id(id)
                    || !valid_id(transition)
                    || (!journal.authority.admissions.contains_key(id)
                        && !journal.authority.settled.contains_key(id))
            })
        || !journal.closed_streams.validate(journal)
        || !journal.closed_actions.validate()
        || !journal.validate_composition()
        || !composition::validate_finalize_selections(journal)
        || journal.composition.records().any(|record| {
            let target = &record.output.origin;
            let session_id = match target {
                LifecycleTarget::Session { session_id, .. }
                | LifecycleTarget::Stream { session_id, .. } => session_id,
                LifecycleTarget::Validation { .. } => return true,
            };
            let Some(session) = journal.sessions.get(session_id) else {
                return true;
            };
            let Some(admission) = journal
                .authority
                .admissions
                .get(&record.admission_id)
                .or_else(|| journal.authority.settled.get(&record.admission_id))
            else {
                return true;
            };
            !session.admission_ids.contains(&record.admission_id)
                || session.admission_targets.get(&record.admission_id) != Some(target)
                || admission.provider != record.output.producer
                || admission.plan != record.plan
                || !composition::validate_callback_provenance(record, admission)
                || journal.admission_transitions.get(&record.admission_id)
                    != Some(&record.transition_id)
                || !matches!(
                    (target, session.admission_phases.get(&record.admission_id)),
                    (
                        LifecycleTarget::Session { .. },
                        Some(LifecyclePhase::SessionPrepare)
                    ) | (
                        LifecycleTarget::Stream { .. },
                        Some(LifecyclePhase::StreamPrepare)
                    )
                )
        })
        || journal.composition.selections().any(|selection| {
            let Ok((epoch, session_id, stream_id, generation)) =
                operations::target_parts(&selection.target)
            else {
                return true;
            };
            let Some(session) = journal.sessions.get(session_id) else {
                return true;
            };
            if session.host_epoch != epoch {
                return true;
            }
            let graph = match stream_id {
                Some(stream_id) => {
                    let Some(stream) = session.streams.get(stream_id) else {
                        return true;
                    };
                    if stream.generation != generation {
                        return true;
                    }
                    &stream.provider_graph
                }
                None => {
                    if session.generation != generation {
                        return true;
                    }
                    &session.provider_graph
                }
            };
            let Some(graph) = graph else {
                return true;
            };
            if !graph
                .dependencies
                .get(&selection.control_policy.dependent_provider)
                .is_some_and(|deps| deps.contains(&selection.control_policy.owner_provider))
            {
                return true;
            }
            let output = &selection.input.output;
            if let LifecycleTarget::Session {
                generation: origin_generation,
                ..
            } = &output.origin
                && *origin_generation != session.generation
            {
                return true;
            }
            let canonical = journal.composition.records().find(|record| {
                record.output.producer == output.producer
                    && record.output.output_id == output.output_id
                    && record.output.origin == output.origin
            });
            if canonical.is_none_or(|record| {
                record.output != *output
                    || record.admission_id != selection.input.admission_id
                    || record.transition_id != selection.input.transition_id
                    || record.plan != selection.input.plan
                    || record.callback_proof != selection.input.callback_proof
            }) {
                return true;
            }
            !journal.receipts.values().any(|receipt| {
                receipt.work.as_ref().is_some_and(|work| {
                    work.target == selection.target && work.transition_id == selection.transition_id
                }) || (receipt.response["transition_id"].as_str() == Some(&selection.transition_id)
                    && receipt
                        .response
                        .get("target")
                        .and_then(|value| {
                            serde_json::from_value::<LifecycleTarget>(value.clone()).ok()
                        })
                        .as_ref()
                        == Some(&selection.target))
            })
        })
        || journal
            .composition
            .staged_outputs()
            .enumerate()
            .any(|(index, stage)| {
                let previous = journal
                    .composition
                    .staged_outputs()
                    .take(index)
                    .filter(|old| {
                        old.after.producer == stage.before.producer
                            && old.after.output_id == stage.before.output_id
                            && old.after.origin == stage.before.origin
                    })
                    .last();
                let prior_release_valid = match (&stage.prior_release_admission, previous) {
                    (None, None) => true,
                    (Some(admission), Some(previous)) => journal
                        .verified_released_output(&previous.target)
                        .is_some_and(|(id, output)| id == admission && output == &stage.before),
                    (Some(admission), None) => journal
                        .composition
                        .stage_anchor(&stage.before)
                        .and_then(|anchor| journal.verified_released_output(&anchor.target))
                        .is_some_and(|(id, output)| id == admission && output == &stage.before),
                    _ => false,
                };
                let Ok((epoch, session_id, Some(stream_id), generation)) =
                    operations::target_parts(&stage.target)
                else {
                    return true;
                };
                let Some(session) = journal.sessions.get(session_id) else {
                    return true;
                };
                let Some(stream) = session.streams.get(stream_id) else {
                    return true;
                };
                let Some(admission) = journal
                    .authority
                    .admissions
                    .get(&stage.admission_id)
                    .or_else(|| journal.authority.settled.get(&stage.admission_id))
                else {
                    return true;
                };
                !prior_release_valid
                    || session.host_epoch != epoch
                    || session.generation != stage.parent_generation
                    || stream.generation != generation
                    || !session.admission_ids.contains(&stage.admission_id)
                    || session.admission_targets.get(&stage.admission_id) != Some(&stage.target)
                    || session.admission_phases.get(&stage.admission_id)
                        != Some(&LifecyclePhase::StreamPrepare)
                    || journal.admission_transitions.get(&stage.admission_id)
                        != Some(&stage.transition_id)
                    || admission.provider != stage.after.producer
                    || admission.plan != stage.plan
                    || admission.selection.participant != stage.callback_proof.participant
                    || !composition::callback_approvals_valid(
                        &stage.callback_proof,
                        &admission.selection.approvals,
                        &admission.provider,
                    )
            })
        || !journal.session_prepare_evidence.validate(journal)
        || !journal.session_releases.validate(journal)
        || !journal.stream_releases.validate(journal)
        || !journal.composed_releases.validate(journal)
        || !journal.finalize_results.validate_structure()
        || journal.finalize_results.records().any(|item| {
            let request = &item.request;
            let Some(permit) = journal.authority.finalize_permits.get(&request.permit_id) else {
                return true;
            };
            let Some(admission) = journal
                .authority
                .admissions
                .get(&request.admission_id)
                .or_else(|| journal.authority.settled.get(&request.admission_id))
            else {
                return true;
            };
            let Some(selection) = journal.composition.finalize_selection(
                &request.target,
                &request.transition_id,
                &item.selection.policy.owner_provider,
                &request.output.output_id,
            ) else {
                return true;
            };
            let Ok((_, session_id, _, _)) = operations::target_parts(&request.target) else {
                return true;
            };
            let Some(session) = journal.sessions.get(session_id) else {
                return true;
            };
            !item.validate_control_chain(journal)
                || !permit.consumed
                || !permit.settled
                || !journal
                    .authority
                    .output_admissions
                    .contains(&request.admission_id)
                || !session.admission_ids.contains(&request.admission_id)
                || session.admission_targets.get(&request.admission_id) != Some(&request.target)
                || session.admission_phases.get(&request.admission_id)
                    != Some(&item.plan_request.phase)
                || permit.request != *request
                || permit.context.target != request.target
                || permit.context.transition_id != request.transition_id
                || permit.context.policy != item.selection.policy
                || permit.context.output != request.output
                || permit.context.control_results != request.control_results
                || permit.context.parent_generation != item.selection.parent_generation
                || selection != &item.selection
                || admission.provider != item.selection.policy.owner_provider
                || admission.plan != item.plan
                || item.plan_approval.as_ref().is_none_or(|approval| {
                    !admission.selection.approvals.contains(approval)
                        || approval.method != admission.selection.participant.plan_method
                })
                || journal.admission_transitions.get(&request.admission_id)
                    != Some(&request.transition_id)
        })
        || !journal.control_settlements.validate_structure()
        || journal.control_settlements.records().any(|item| {
            let request = &item.settlement.request;
            let Some(permit) = journal.authority.control_permits.get(&request.permit_id) else {
                return true;
            };
            let Some(admission) = journal
                .authority
                .admissions
                .get(&request.admission_id)
                .or_else(|| journal.authority.settled.get(&request.admission_id))
            else {
                return true;
            };
            let Some(selection) = journal.composition.selection(
                &request.target,
                &item.selection_transition_id,
                &permit.context.policy.dependent_provider,
                &request.proposal.owner_provider,
                &request.proposal.output_id,
            ) else {
                return true;
            };
            let catalog = amaterasu_extension_api::schemas::SchemaCatalog::compile(
                selection
                    .schema_documents
                    .iter()
                    .map(|doc| (doc.reference.clone(), doc.document.as_str())),
            );
            !item.valid_under_permit(permit)
                || !permit.consumed
                || permit.request != *request
                || permit.context.target != request.target
                || permit.context.transition_id != request.transition_id
                || permit.context.policy != selection.control_policy
                || control_history::authenticated_input(journal, request, selection).as_ref()
                    != Some(&permit.context.output)
                || admission.provider != selection.control_policy.dependent_provider
                || !admission.plan.control_proposals.contains(&request.proposal)
                || permit.settled != item.owner_ack.is_some()
                || ![
                    &selection.control_approval,
                    &selection.control_status_approval,
                ]
                .iter()
                .any(|approval| {
                    approval.broker_approval == item.effect_dispatch.call.approval
                        && composition::approval_matches_call(approval, &item.effect_dispatch.call)
                })
                || item.effect_pending_proof.as_ref().is_some_and(|proof| {
                    proof.call.approval != selection.control_approval.broker_approval
                        || !composition::approval_matches_call(
                            &selection.control_approval,
                            &proof.call,
                        )
                })
                || item.owner_dispatch.as_ref().is_some_and(|dispatch| {
                    dispatch.call.approval != selection.settlement_approval.broker_approval
                        || !composition::approval_matches_call(
                            &selection.settlement_approval,
                            &dispatch.call,
                        )
                })
                || item.confirmed.output.schema != selection.input.output.schema
                || item.confirmed.output.origin != selection.input.output.origin
                || !catalog.is_ok_and(|catalog| {
                    catalog
                        .validate(&item.confirmed.output.schema, &item.confirmed.output.data)
                        .is_ok()
                })
        })
        || journal.composition.refinements().any(|item| {
            let Some(permit) = journal
                .authority
                .control_permits
                .get(&item.request.permit_id)
            else {
                return true;
            };
            let Some(admission) = journal
                .authority
                .admissions
                .get(&item.request.admission_id)
                .or_else(|| journal.authority.settled.get(&item.request.admission_id))
            else {
                return true;
            };
            let mut settlements = journal.control_settlements.records().filter(|record| {
                record.settlement.request == item.request
                    && record.confirmed == item.confirmed
                    && record.applied == item.owner_result
                    && record.owner_receipt_id == item.owner_receipt_id
            });
            let Some(settlement) = settlements.next() else {
                return true;
            };
            if settlements.next().is_some() {
                return true;
            }
            let Some(selection) = journal.composition.selection(
                &item.request.target,
                &settlement.selection_transition_id,
                &permit.context.policy.dependent_provider,
                &item.request.proposal.owner_provider,
                &item.request.proposal.output_id,
            ) else {
                return true;
            };
            let catalog = amaterasu_extension_api::schemas::SchemaCatalog::compile(
                selection
                    .schema_documents
                    .iter()
                    .map(|doc| (doc.reference.clone(), doc.document.as_str())),
            );
            !permit.consumed
                || permit.request != item.request
                || permit.context.policy != selection.control_policy
                || control_history::authenticated_input(journal, &item.request, selection).as_ref()
                    != Some(&permit.context.output)
                || admission.provider != selection.control_policy.dependent_provider
                || !admission.plan.obligations.contains(&item.original)
                || !admission
                    .plan
                    .control_proposals
                    .contains(&item.request.proposal)
                || item.confirmed.output.schema != selection.input.output.schema
                || item.confirmed.output.origin != selection.input.output.origin
                || !catalog.is_ok_and(|catalog| {
                    catalog
                        .validate(&item.confirmed.output.schema, &item.confirmed.output.data)
                        .is_ok()
                })
        })
        || !journal.inputs.validate()
        || journal.actions.validate().is_err()
        || journal
            .input_boot_epochs
            .iter()
            .any(|(token, epoch)| !journal.input_bindings.contains_key(token) || !valid_id(epoch))
        || journal.receipts.iter().any(|(key, receipt)| {
            let id = receipt.request.get("operation_id").and_then(Value::as_str);
            id.is_none_or(|id| {
                !valid_id(id)
                    || *key != receipt_key(&receipt.caller, &receipt.initiator, id)
                    || receipt.response.get("operation_id").and_then(Value::as_str) != Some(id)
            }) || !refresh_receipt::valid_compact(receipt)
                || receipt.caller.is_empty()
                || receipt.initiator.is_empty()
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid lifecycle journal",
        ));
    }
    Ok(())
}

pub struct Coordinator {
    shared: Shared,
    settings: LifecycleSettings,
    configured: bool,
    selections: BTreeMap<String, ProviderSelection>,
    approval_cursor: usize,
    drivers: BTreeMap<String, PrepareDriver>,
    finalize_drivers: BTreeMap<String, finalize_dispatch::FinalizeDispatch>,
    observation_drivers: BTreeMap<String, plan_observation::PlanObservationDriver>,
    control_drivers: BTreeMap<String, control_dispatch::ControlDispatch>,
    settlement_drivers: BTreeMap<String, settlement_dispatch::SettlementDispatch>,
    validations: BTreeMap<String, security::ValidationJob>,
    revocations: BTreeMap<String, security::RevocationJob>,
    application_jobs: BTreeMap<String, application::ApplicationJob>,
    ephemeral: BTreeMap<String, (Receipt, Instant)>,
    work_cursor: usize,
    settings_jobs: BTreeMap<String, settings_snapshot::SettingsSnapshotJob>,
}

impl Coordinator {
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut store = LifecycleStore::new(path.to_path_buf())?;
        let epoch = identifier().map_err(io::Error::other)?;
        let mut journal: Journal = store.load()?.unwrap_or_else(|| Journal {
            version: 3,
            host_epoch: epoch.clone(),
            receipts: BTreeMap::new(),
            sessions: BTreeMap::new(),
            authority: AuthorityState::default(),
            inputs: Default::default(),
            actions: Default::default(),
            input_bindings: BTreeMap::new(),
            revoked_inputs: BTreeSet::new(),
            recovery_sources: BTreeMap::new(),
            input_boot_epochs: BTreeMap::new(),
            retention: Default::default(),
            ended_session_history: Default::default(),
            composition: Default::default(),
            closed_streams: Default::default(),
            closed_actions: Default::default(),
            control_settlements: Default::default(),
            finalize_results: Default::default(),
            stream_releases: Default::default(),
            composed_releases: Default::default(),
            session_releases: Default::default(),
            session_prepare_evidence: Default::default(),
            action_dispositions: Default::default(),
            admission_transitions: Default::default(),
        });
        validate_journal(&journal)?;
        journal.version = 3;
        for receipt in journal.receipts.values_mut() {
            if receipt.response["status"] == "pending" {
                receipt.response["status"] = json!("recovery_required");
                receipt.response["reason"] = json!("coordinator_restarted");
            }
        }
        for session in journal.sessions.values_mut() {
            if session.phase != SessionPhase::Ended {
                session.phase = SessionPhase::RecoveryRequired;
            }
            for stream in session.streams.values_mut() {
                if stream.phase != StreamPhase::Retired {
                    stream.phase = StreamPhase::RecoveryRequired;
                }
            }
        }
        journal.inputs.recover_after_restart();
        journal.composition.fence_after_restart();
        journal.input_bindings.clear();
        journal.input_boot_epochs.clear();
        journal.revoked_inputs.clear();
        journal.actions.recover_after_restart().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "invalid action recovery state")
        })?;
        journal.host_epoch = epoch;
        retention::compact(&mut journal);
        store.replace(&journal)?;
        Ok(Self {
            shared: Rc::new(RefCell::new(SharedJournal {
                store,
                value: journal,
                retirement_scan_pending: true,
            })),
            settings: LifecycleSettings::default(),
            configured: false,
            selections: BTreeMap::new(),
            approval_cursor: 0,
            drivers: BTreeMap::new(),
            finalize_drivers: BTreeMap::new(),
            observation_drivers: BTreeMap::new(),
            control_drivers: BTreeMap::new(),
            settlement_drivers: BTreeMap::new(),
            validations: BTreeMap::new(),
            revocations: BTreeMap::new(),
            application_jobs: BTreeMap::new(),
            ephemeral: BTreeMap::new(),
            work_cursor: 0,
            settings_jobs: BTreeMap::new(),
        })
    }

    // The composed graph is exercised directly while its production profile remains gated.
    #[cfg(test)]
    pub(crate) fn configure_for_test(&mut self, settings: LifecycleSettings) {
        self.settings = settings;
        self.configured = true;
    }

    pub fn configured(settings: &LifecycleSettings) -> ModuleResult<Self> {
        settings.validate()?;
        let mut coordinator =
            Self::open(Path::new(&settings.journal_path)).map_err(|e| e.to_string())?;
        coordinator.settings = settings.clone();
        coordinator.configured = true;
        Ok(coordinator)
    }

    pub fn handle(&mut self, caller: &str, initiator: &str, payload: Value) -> Value {
        if caller.is_empty() || initiator.is_empty() {
            return rejection("missing_authenticated_identity");
        }
        let Some(operation) = payload.get("operation").and_then(Value::as_str) else {
            return rejection("invalid_request");
        };
        if operation == "snapshot" {
            return self.snapshot(caller, initiator);
        }
        if matches!(
            operation,
            "describe_resource_control" | "verify_resource_control" | "validate_resource_control"
        ) {
            return self.control_operation(caller, payload);
        }
        if operation == "lookup_control_baseline" {
            return self.lookup_control_baseline(caller, payload);
        }
        if operation == "lookup_stream_closure" {
            return self.lookup_stream_closure(caller, payload);
        }
        if operation == "verify_control_settlement" {
            return self.verify_control_settlement(caller, payload);
        }
        if operation == "verify_provider_epoch" {
            return self.verify_provider(caller, &payload);
        }
        if operation == "read_prepared_plans" {
            return self.read_prepared_plans(caller, payload);
        }
        if operation == "read_provider_context" {
            return self.read_provider_context(caller, payload);
        }
        if operation == "verify_finalize_stage" {
            return self.verify_finalize_stage_operation(caller, &payload);
        }
        if matches!(
            operation,
            "describe_resource_finalize"
                | "verify_resource_finalize"
                | "validate_resource_finalize"
        ) {
            return self.finalize_operation(caller, payload);
        }
        if operation == "resolve_settings_scope" {
            return self.resolve_settings_scope(caller, payload);
        }
        if operation == "describe_action_target" {
            return self.describe_action_target(caller, &payload);
        }
        if operation == "verify_control_settlement" {
            return self.verify_control_settlement(caller, payload);
        }
        if operation == "verify_finalize_result" {
            return self.verify_finalize_result(caller, payload);
        }
        if matches!(
            operation,
            "describe_resource_finalize"
                | "verify_resource_finalize"
                | "validate_resource_finalize"
        ) {
            return self.finalize_operation(caller, payload);
        }
        if matches!(
            operation,
            "describe_resource_control" | "verify_resource_control" | "validate_resource_control"
        ) {
            return self.control_operation(caller, payload);
        }
        if matches!(
            operation,
            "issue_input_binding"
                | "validate_input_binding"
                | "reserve_input_action"
                | "lookup_input_action"
                | "finish_input_action"
                | "authorize_action"
                | "consume_action_permit"
                | "complete_action"
                | "begin_action_transition"
                | "rebind_action_plan"
                | "consume_input_reservation"
                | "cancel_input_reservation"
                | "inspect_action_effect"
                | "verify_action_effect"
                | "check_action_effect"
        ) {
            return self.security_operation(caller, initiator, &payload);
        }
        let Some(id) = payload
            .get("operation_id")
            .and_then(Value::as_str)
            .filter(|id| valid_id(id))
        else {
            return rejection("invalid_operation_id");
        };
        let key = receipt_key(caller, initiator, id);
        self.ephemeral.retain(|_, (receipt, created)| {
            receipt.response["status"] == "pending" || created.elapsed() < Duration::from_secs(5)
        });
        if let Some((receipt, _)) = self.ephemeral.get(&key) {
            if operation == "lookup" || receipt.request == payload {
                return receipt.response.clone();
            }
            return rejection("operation_id_conflict");
        }
        {
            let shared = self.shared.borrow();
            let journal = &shared.value;
            if let Some(response) = journal
                .ended_session_history
                .end_response(journal, &key, &payload)
            {
                return response;
            }
            if retention::expired(journal, &key) {
                return rejection("receipt_expired");
            }
            if operation == "lookup" {
                if let Some(source) = journal.recovery_sources.get(&key)
                    && let Some(original) = journal.receipts.get(source)
                {
                    let mut response = original.response.clone();
                    response["operation_id"] = json!(id);
                    response["original_operation_id"] = original.request["operation_id"].clone();
                    return response;
                }
                return journal
                    .receipts
                    .get(&key)
                    .map(|r| r.response.clone())
                    .unwrap_or_else(|| json!({"status":"not_found"}));
            }
            if let Some(receipt) = journal.receipts.get(&key) {
                return if refresh_receipt::matches(receipt, &payload) {
                    receipt.response.clone()
                } else {
                    rejection("operation_id_conflict")
                };
            }
            if matches!(operation, "prepare" | "prepare_stream" | "resume_stream")
                && retention::at_capacity(journal)
            {
                return rejection("retention_capacity_exhausted");
            }
            if journal.receipts.len() >= MAX_RECEIPTS {
                return rejection("capacity_exhausted");
            }
        }
        let result = if operation == "recover_session" {
            self.recover_session(caller, initiator, payload, key)
        } else if operation == "refresh_provider_observations" {
            self.refresh_provider_observations(caller, initiator, payload, key)
        } else {
            self.admit(caller, initiator, payload, key)
        };
        match result {
            Ok(response) => response,
            Err(reason) if reason == "journal_write_failed" => {
                json!({"status":"recovery_required","reason":reason})
            }
            Err(reason) => rejection(&reason),
        }
    }

    pub fn advance(
        &mut self,
        send: &mut dyn FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<()> {
        if !self.configured {
            return Ok(());
        }
        self.refresh_approvals(send)?;
        self.advance_applications(send)?;
        self.advance_work(send)?;
        self.retire_ended_sessions_if_idle()?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "coordinator_tests.rs"]
mod tests;
