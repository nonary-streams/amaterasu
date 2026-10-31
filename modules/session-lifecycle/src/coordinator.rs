//! Durable lifecycle authority; native work advances outside request handlers.
mod action_endpoints;
mod action_target;
mod application;
#[allow(dead_code)] // Admission and control remain disabled until schema verification is wired.
mod composition;
mod operations;
mod provider_observations;
mod providers;
mod recovery;
mod retention;
mod security;
mod settings_scope;
mod settings_snapshot;

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

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderGraph {
    order: Vec<String>,
    dependencies: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Work {
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

impl Work {
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
    #[serde(default)]
    provider_settings: BTreeMap<String, settings_snapshot::FrozenSettings>,
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
    composition: composition::CompositionBook,
    /// Immutable producing transition of each retained admission.
    #[serde(default)]
    admission_transitions: BTreeMap<String, String>,
}

struct SharedJournal {
    store: LifecycleStore,
    value: Journal,
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
    retention::compact(&mut next);
    let mut shared = shared.borrow_mut();
    shared
        .store
        .replace(&next)
        .map_err(|_| "journal_write_failed".to_owned())?;
    shared.value = next;
    Ok(())
}
fn validate_journal(journal: &Journal) -> io::Result<()> {
    if !matches!(journal.version, 2 | 3)
        || !valid_id(&journal.host_epoch)
        || journal.receipts.len() > MAX_RECEIPTS
        || journal.sessions.len() > MAX_SESSIONS
        || !retention::validate(journal)
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
        || !journal.composition.validate_structure()
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
            }) || receipt.caller.is_empty()
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
            composition: Default::default(),
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
            })),
            settings: LifecycleSettings::default(),
            configured: false,
            selections: BTreeMap::new(),
            approval_cursor: 0,
            drivers: BTreeMap::new(),
            validations: BTreeMap::new(),
            revocations: BTreeMap::new(),
            application_jobs: BTreeMap::new(),
            ephemeral: BTreeMap::new(),
            work_cursor: 0,
            settings_jobs: BTreeMap::new(),
        })
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
        if operation == "verify_provider_epoch" {
            return self.verify_provider(caller, &payload);
        }
        if operation == "resolve_settings_scope" {
            return self.resolve_settings_scope(caller, payload);
        }
        if operation == "describe_action_target" {
            return self.describe_action_target(caller, &payload);
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
                | "consume_input_reservation"
                | "cancel_input_reservation"
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
        let journal = self.shared.borrow().value.clone();
        if retention::expired(&journal, &key) {
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
            return if receipt.request == payload {
                receipt.response.clone()
            } else {
                rejection("operation_id_conflict")
            };
        }
        if matches!(operation, "prepare" | "prepare_stream" | "resume_stream")
            && retention::at_capacity(&journal)
        {
            return rejection("retention_capacity_exhausted");
        }
        if journal.receipts.len() >= MAX_RECEIPTS {
            return rejection("capacity_exhausted");
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
        self.advance_work(send)
    }
}

#[cfg(test)]
#[path = "coordinator_tests.rs"]
mod tests;
