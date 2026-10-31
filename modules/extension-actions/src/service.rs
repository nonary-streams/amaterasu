//! Serialized state-backed action admission and lifecycle-fenced dispatch.
use crate::dispatch::{Dispatch, Outcome, Stage as DispatchStage};
use crate::worker::Worker;
use amaterasu_extension_api::{ActionInvoke, EXTENSION_ACTIONS_SERVICE};
use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

const STATE: &str = "amaterasu.state/1";
const COLLECTION: &str = "extension-actions-v1";
const MAX_EPOCHS: u64 = 64;
const MAX_EPOCH_BYTES: usize = 8_000;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Epoch {
    pub(crate) caller: String,
    pub(crate) initiator: String,
    pub(crate) open_index: String,
    pub(crate) closed: bool,
    #[serde(default)]
    pub(crate) retired: u64,
    pub(crate) receipts: BTreeMap<u64, Record>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    pub(crate) request: ActionInvoke,
    pub(crate) receipt: Value,
    pub(crate) selected_provider: String,
    pub(crate) registry_revision: u64,
    pub(crate) descriptor: amaterasu_extension_api::ActionDescriptor,
    pub(crate) dispatch: Option<crate::dispatch::Dispatch>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    #[serde(default)]
    grants: Vec<Grant>,
    #[serde(default)]
    input_service: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Grant {
    principal: String,
    action_ids: Vec<String>,
    target_kinds: Vec<String>,
    execution_classes: Vec<String>,
}
impl Policy {
    fn has_principal(&self, principal: &str) -> bool {
        self.grants.iter().any(|grant| grant.principal == principal)
    }
    fn permits(&self, principal: &str, action: &str, target: &str) -> bool {
        self.grants.iter().any(|grant| {
            grant.principal == principal
                && grant.action_ids.iter().any(|id| id == action)
                && grant.target_kinds.iter().any(|kind| kind == target)
                && !grant.execution_classes.is_empty()
        })
    }
    fn permits_class(&self, principal: &str, action: &str, target: &str, class: &str) -> bool {
        self.grants.iter().any(|grant| {
            grant.principal == principal
                && grant.action_ids.iter().any(|id| id == action)
                && grant.target_kinds.iter().any(|kind| kind == target)
                && grant
                    .execution_classes
                    .iter()
                    .any(|candidate| candidate == class)
        })
    }
    fn validate(&self) -> bool {
        self.input_service
            .as_ref()
            .is_none_or(|principal| !principal.is_empty() && principal.len() <= 256)
            && self.grants.len() <= 128
            && self.grants.iter().all(|grant| {
                !grant.principal.is_empty()
                    && grant.principal.len() <= 256
                    && grant.action_ids.len() <= 128
                    && grant
                        .action_ids
                        .iter()
                        .all(|id| !id.is_empty() && id.len() <= 256)
                    && grant
                        .target_kinds
                        .iter()
                        .all(|kind| ["session", "stream", "host"].contains(&kind.as_str()))
                    && grant.execution_classes.iter().all(|class| {
                        ["read_only", "session_mutation", "host_mutation"].contains(&class.as_str())
                    })
            })
    }
}
struct Incoming {
    id: u64,
    caller: String,
    initiator: String,
    payload: Value,
    input_source: Option<crate::dispatch::Source>,
}
enum Stage {
    Read(Incoming),
    Write {
        parent: u64,
        result: Value,
        start: Option<Box<Dispatch>>,
    },
    ReservedCancelWrite {
        parent: u64,
        request: ActionInvoke,
        receipt: Value,
    },
    ReservedCancelReply {
        parent: u64,
        request: ActionInvoke,
        receipt: Value,
    },
    DispatchReply {
        key: (String, u64),
    },
    CancelReply {
        key: (String, u64),
    },
    RecoveryHead,
    RecoveryPage {
        revision: u64,
    },
    Descriptor {
        incoming: Incoming,
        revision: u64,
        metadata: Value,
        epoch: Box<Epoch>,
        invoke: Box<ActionInvoke>,
    },
    Plan(Box<PlanContext>),
    InputReservation(Box<ReservationContext>),
    ConfirmInput(Box<CommitContext>),
}
struct ReservationContext {
    incoming: Incoming,
    revision: u64,
    metadata: Value,
    epoch: Epoch,
    invoke: ActionInvoke,
}
struct CommitContext {
    incoming: Incoming,
    revision: u64,
    metadata: Value,
    epoch: Epoch,
    invoke: ActionInvoke,
    dispatch: Dispatch,
}
struct PlanContext {
    incoming: Incoming,
    revision: u64,
    metadata: Value,
    epoch: Epoch,
    invoke: ActionInvoke,
    action: amaterasu_extension_api::ActionDescriptor,
    provider: String,
    registry_revision: u64,
}
enum Recovery {
    NeedHead,
    NeedPage {
        revision: u64,
        after: Option<String>,
    },
    Waiting,
    WaitingPersist {
        epoch: String,
        sequence: u64,
    },
    Done,
    Blocked,
}
struct ActiveWork {
    dispatch: Dispatch,
    receipt: Value,
    replacement: Option<(Dispatch, Value)>,
    recovery_unconfirmed: bool,
    polls: u64,
    cancel_parent: Option<u64>,
    cancel_sent: bool,
    effect_started: bool,
}
struct Pending {
    request: u64,
    stage: Stage,
}

pub struct ActionsModule {
    runtime: RuntimeClient,
    pending: VecDeque<Pending>,
    policy: Policy,
    worker: Worker,
    active: BTreeMap<(String, u64), ActiveWork>,
    recovery: Recovery,
    recovery_operation: u64,
    recovery_epoch: String,
    recovery_retries: u8,
}
impl ManagedModule for ActionsModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("action policy unavailable".into());
        };
        let policy: Policy = serde_json::from_value(payload).map_err(|error| error.to_string())?;
        if !policy.validate() {
            return Err("invalid action policy".into());
        }
        let mut recovery_bytes = [0_u8; 16];
        getrandom::fill(&mut recovery_bytes).map_err(|error| error.to_string())?;
        let recovery_epoch = recovery_bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        runtime.require_success(Command::Register {
            service: EXTENSION_ACTIONS_SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            pending: VecDeque::new(),
            policy,
            worker: Worker::new()?,
            active: BTreeMap::new(),
            recovery: Recovery::NeedHead,
            recovery_operation: 0,
            recovery_epoch,
            recovery_retries: 0,
        })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        let completed = self.worker.tick(|command| self.runtime.send(command))?;
        for completion in completed {
            self.worker_completed(completion)?;
        }
        self.advance_recovery()?;
        if let Some(pending) = self.pending.pop_front() {
            self.advance(pending)?;
        }
        if !matches!(self.recovery, Recovery::Done | Recovery::Blocked) {
            return Ok(());
        }
        if matches!(self.recovery, Recovery::Done) && self.pending.len() < 24 {
            let idle = self.active.iter().find_map(|(key, work)| {
                let has_call = self.pending.iter().any(|pending| matches!(&pending.stage, Stage::DispatchReply { key: in_flight } | Stage::CancelReply { key: in_flight } if in_flight == key));
                (!has_call && work.replacement.is_none() && !work.recovery_unconfirmed).then(|| key.clone())
            });
            if let Some(key) = idle {
                if self
                    .active
                    .get(&key)
                    .is_some_and(|work| work.cancel_parent.is_some())
                {
                    self.persist_cancellation(key)?;
                } else {
                    self.drive(key)?;
                }
            }
        }
        // Keep status and cancellation responsive while other requests wait on
        // broker calls. The broker retains backpressure when this bound is full.
        if self.pending.len() >= 32 {
            return Ok(());
        }
        match self.runtime.send(Command::Poll {})? {
            Response::Pending => Ok(()),
            Response::Request {
                id,
                caller,
                initiator,
                payload,
                ..
            } => {
                let incoming = Incoming {
                    id,
                    caller,
                    initiator,
                    payload,
                    input_source: None,
                };
                self.receive(incoming)
            }
            response => Err(format!("action poll failed: {response:?}")),
        }
    }
}
impl ActionsModule {
    fn receive(&mut self, incoming: Incoming) -> ModuleResult<()> {
        if !self.policy.has_principal(&incoming.caller)
            || !self.policy.has_principal(&incoming.initiator)
        {
            return self.complete(incoming.id, failure("permission_denied"));
        }
        let operation = incoming
            .payload
            .get("operation")
            .and_then(Value::as_str)
            .unwrap_or("");
        if self.pending.len() >= 24
            && matches!(
                operation,
                "invoke" | "reserve_input" | "commit_input" | "open_epoch"
            )
        {
            return self.complete(incoming.id, failure("capacity"));
        }
        if matches!(operation, "invoke" | "reserve_input" | "commit_input")
            && !matches!(self.recovery, Recovery::Done)
        {
            return self.complete(incoming.id, failure("startup_recovery"));
        }
        if !matches!(
            operation,
            "open_epoch"
                | "invoke"
                | "status"
                | "lookup"
                | "cancel"
                | "close_epoch"
                | "retire_epoch"
                | "reserve_input"
                | "commit_input"
        ) {
            return self.complete(incoming.id, failure("invalid_request"));
        }
        if matches!(operation, "reserve_input" | "commit_input")
            && (self.policy.input_service.as_deref() != Some(incoming.caller.as_str())
                || incoming.caller != incoming.initiator)
        {
            return self.complete(incoming.id, failure("permission_denied"));
        }
        let key = if operation == "open_epoch" {
            let Some(open_id) = incoming
                .payload
                .get("operation_id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty() && id.len() <= 128)
            else {
                return self.complete(incoming.id, failure("invalid_operation_id"));
            };
            Some(open_key(&incoming.caller, &incoming.initiator, open_id))
        } else {
            let Some(epoch) = incoming
                .payload
                .get("epoch")
                .and_then(Value::as_str)
                .filter(|id| valid_epoch(id))
            else {
                return self.complete(incoming.id, failure("invalid_epoch"));
            };
            Some(format!("epoch/{epoch}"))
        };
        let payload = json!({"action":"collection_get","collection":COLLECTION,"key":key});
        self.submit(payload, Stage::Read(incoming))
    }
    fn submit(&mut self, payload: Value, stage: Stage) -> ModuleResult<()> {
        let parent = match &stage {
            Stage::Read(incoming) => incoming.id,
            Stage::Write { parent, .. }
            | Stage::ReservedCancelWrite { parent, .. }
            | Stage::ReservedCancelReply { parent, .. } => *parent,
            Stage::Descriptor { incoming, .. } => incoming.id,
            Stage::Plan(context) => context.incoming.id,
            Stage::InputReservation(context) => context.incoming.id,
            Stage::ConfirmInput(context) => context.incoming.id,
            Stage::DispatchReply { .. }
            | Stage::CancelReply { .. }
            | Stage::RecoveryHead
            | Stage::RecoveryPage { .. } => {
                return Err("invalid submission stage".into());
            }
        };
        let descriptor = matches!(stage, Stage::Descriptor { .. });
        let lifecycle = matches!(
            stage,
            Stage::InputReservation(_) | Stage::ReservedCancelReply { .. }
        );
        let input = matches!(stage, Stage::ConfirmInput(_));
        let phase = match &stage {
            Stage::Read(_) => "read",
            Stage::Write { .. } | Stage::ReservedCancelWrite { .. } => "write",
            Stage::ReservedCancelReply { .. } => "cancel-input-reservation",
            Stage::Descriptor { .. } => "descriptor",
            Stage::Plan(_) => "plan",
            Stage::InputReservation(_) => "input-reservation",
            Stage::ConfirmInput(_) => "input-confirm",
            Stage::DispatchReply { .. }
            | Stage::CancelReply { .. }
            | Stage::RecoveryHead
            | Stage::RecoveryPage { .. } => {
                return Err("invalid submission stage".into());
            }
        };
        match self.runtime.send(Command::Request {
            service: if descriptor {
                amaterasu_extension_api::EXTENSION_REGISTRY_SERVICE.into()
            } else if lifecycle {
                amaterasu_extension_api::SESSION_LIFECYCLE_SERVICE.into()
            } else if input {
                "amaterasu.input/1".into()
            } else {
                STATE.into()
            },
            payload,
            timeout_ms: 5_000,
            operation_id: format!("actions-{parent}-{phase}"),
            parent: if descriptor { Some(parent) } else { None },
        })? {
            Response::Accepted { id } => {
                self.pending.push_back(Pending { request: id, stage });
                Ok(())
            }
            _ => self.complete(parent, failure("state_unavailable")),
        }
    }
    fn advance(&mut self, pending: Pending) -> ModuleResult<()> {
        match self.runtime.send(Command::TakeResult {
            request: pending.request,
        })? {
            Response::Pending => {
                self.pending.push_back(pending);
                Ok(())
            }
            Response::Result { payload } => {
                let released = self.runtime.send(Command::ReleaseResult {
                    request: pending.request,
                })?;
                if !matches!(released, Response::Ok) {
                    return Err("failed to release state result".into());
                }
                match pending.stage {
                    Stage::Read(incoming) => self.after_read(incoming, payload),
                    Stage::Descriptor {
                        incoming,
                        revision,
                        metadata,
                        epoch,
                        invoke,
                    } => self
                        .after_descriptor(incoming, revision, metadata, *epoch, *invoke, payload),
                    Stage::Plan(context) => self.after_plan(*context, payload),
                    Stage::InputReservation(context) => {
                        self.after_input_reservation(*context, payload)
                    }
                    Stage::ConfirmInput(context) => {
                        self.after_input_confirmation(*context, payload)
                    }
                    Stage::Write {
                        parent,
                        result,
                        start,
                    } => {
                        if payload.get("ok") == Some(&Value::Bool(true)) {
                            self.complete(parent, result.clone())?;
                            if let Some(dispatch) = start {
                                self.start_dispatch(*dispatch, result)?;
                            }
                            Ok(())
                        } else {
                            self.complete(parent, failure("state_commit_failed"))
                        }
                    }
                    Stage::ReservedCancelWrite {
                        parent,
                        request,
                        receipt,
                    } => {
                        if payload.get("ok") == Some(&Value::Bool(true)) {
                            self.cancel_reserved_input(parent, request, receipt)
                        } else {
                            self.complete(parent, failure("state_commit_failed"))
                        }
                    }
                    Stage::ReservedCancelReply {
                        parent,
                        request,
                        receipt,
                    } => {
                        if payload.get("status").and_then(Value::as_str) == Some("succeeded")
                            && payload.get("operation_id").and_then(Value::as_str)
                                == Some(request.operation_id.as_str())
                            && payload.get("action_id").and_then(Value::as_str)
                                == Some(request.action_id.as_str())
                            && payload.get("reservation_id").and_then(Value::as_str)
                                == request.input_reservation_id.as_deref()
                        {
                            self.complete(parent, receipt)
                        } else {
                            self.complete(parent, failure("reservation_cancellation_unconfirmed"))
                        }
                    }
                    Stage::DispatchReply { key } => self.dispatch_reply(key, payload),
                    Stage::CancelReply { key } => self.cancel_reply(key, payload),
                    Stage::RecoveryHead => self.recovery_head(payload),
                    Stage::RecoveryPage { revision } => self.recovery_page(revision, payload),
                }
            }
            _ => {
                let parent = match pending.stage {
                    Stage::Read(incoming) => incoming.id,
                    Stage::Write { parent, .. }
                    | Stage::ReservedCancelWrite { parent, .. }
                    | Stage::ReservedCancelReply { parent, .. } => parent,
                    Stage::Descriptor { incoming, .. } => incoming.id,
                    Stage::Plan(context) => context.incoming.id,
                    Stage::InputReservation(context) => context.incoming.id,
                    Stage::ConfirmInput(context) => context.incoming.id,
                    Stage::DispatchReply { key } => {
                        self.dispatch_unknown(key)?;
                        return Ok(());
                    }
                    Stage::CancelReply { key } => {
                        self.cancel_reply(key, Value::Null)?;
                        return Ok(());
                    }
                    Stage::RecoveryHead | Stage::RecoveryPage { .. } => {
                        self.recovery = Recovery::Blocked;
                        return Ok(());
                    }
                };
                self.complete(parent, failure("state_unavailable"))
            }
        }
    }
    fn after_read(&mut self, incoming: Incoming, state: Value) -> ModuleResult<()> {
        let Some(revision) = state
            .get("revision")
            .and_then(Value::as_u64)
            .filter(|_| state.get("ok") == Some(&Value::Bool(true)))
        else {
            return self.complete(incoming.id, failure("state_unavailable"));
        };
        let operation = incoming
            .payload
            .get("operation")
            .and_then(Value::as_str)
            .unwrap_or("");
        let metadata = state.get("metadata").cloned().unwrap_or(Value::Null);
        if operation == "open_epoch" {
            let count = state
                .get("count")
                .and_then(Value::as_u64)
                .unwrap_or(MAX_EPOCHS);
            if state
                .get("value")
                .and_then(|value| value.get("retired"))
                .and_then(Value::as_bool)
                == Some(true)
            {
                return self.complete(incoming.id, failure("receipt_expired"));
            }
            if let Some(epoch) = state
                .get("value")
                .and_then(|value| value.get("epoch"))
                .and_then(Value::as_str)
            {
                return self.complete(incoming.id, json!({"status":"ok","epoch":epoch}));
            }
            let active = metadata.get("active").and_then(Value::as_u64).unwrap_or(0);
            if active >= MAX_EPOCHS || count >= 4096 {
                return self.complete(incoming.id, failure("capacity"));
            }
            let mut bytes = [0_u8; 16];
            if getrandom::fill(&mut bytes).is_err() {
                return self.complete(incoming.id, failure("entropy"));
            }
            let epoch: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
            let open_id = incoming.payload["operation_id"].as_str().unwrap_or("");
            let index = open_key(&incoming.caller, &incoming.initiator, open_id);
            let record = Epoch {
                caller: incoming.caller,
                initiator: incoming.initiator,
                open_index: index.clone(),
                closed: false,
                retired: 0,
                receipts: BTreeMap::new(),
            };
            let metadata = json!({"active":active + 1});
            let payload = json!({"action":"collection_commit","collection":COLLECTION,"expected_revision":revision,"metadata":metadata,"edits":[{"key":format!("epoch/{epoch}"),"value":record},{"key":index,"value":{"epoch":epoch}}]});
            return self.submit(
                payload,
                Stage::Write {
                    parent: incoming.id,
                    result: json!({"status":"ok","epoch":epoch}),
                    start: None,
                },
            );
        }
        let epoch_id = incoming.payload["epoch"].as_str().unwrap_or("");
        let Ok(mut epoch) =
            serde_json::from_value::<Epoch>(state.get("value").cloned().unwrap_or(Value::Null))
        else {
            return self.complete(incoming.id, failure("unknown_epoch"));
        };
        if epoch.caller != incoming.caller || epoch.initiator != incoming.initiator {
            return self.complete(incoming.id, failure("unknown_epoch"));
        }
        if operation == "retire_epoch" {
            if !epoch.closed
                || epoch.receipts.values().any(|entry| {
                    !matches!(
                        entry.receipt.get("status").and_then(Value::as_str),
                        Some("rejected" | "succeeded" | "failed" | "cancelled")
                    )
                })
            {
                return self.complete(incoming.id, failure("epoch_not_retirable"));
            }
            let active = metadata.get("active").and_then(Value::as_u64).unwrap_or(0);
            if active == 0 {
                return self.complete(incoming.id, failure("state_corrupt"));
            }
            let payload = json!({"action":"collection_commit","collection":COLLECTION,"expected_revision":revision,"metadata":{"active":active - 1},"edits":[{"key":format!("epoch/{epoch_id}"),"value":null},{"key":epoch.open_index,"value":{"retired":true}}]});
            return self.submit(
                payload,
                Stage::Write {
                    parent: incoming.id,
                    result: json!({"status":"ok"}),
                    start: None,
                },
            );
        }
        if operation == "close_epoch" {
            epoch.closed = true;
            return self.write(
                incoming.id,
                revision,
                metadata,
                epoch_id,
                &epoch,
                json!({"status":"ok"}),
            );
        }
        let sequence = incoming
            .payload
            .get("sequence")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if sequence == 0 {
            return self.complete(incoming.id, failure("sequence_window"));
        }
        if sequence <= epoch.retired {
            return self.complete(incoming.id, failure("receipt_expired"));
        }
        if operation == "commit_input" {
            let Some(record) = epoch.receipts.get(&sequence) else {
                return self.complete(incoming.id, failure("unknown_receipt"));
            };
            if incoming.payload.get("operation_id").and_then(Value::as_str)
                != Some(record.request.operation_id.as_str())
            {
                return self.complete(incoming.id, failure("operation_conflict"));
            }
            if record.receipt.get("status").and_then(Value::as_str) != Some("reserved") {
                return self.complete(
                    incoming.id,
                    self.observed_receipt(epoch_id, sequence, record),
                );
            }
            let Some(reservation) = record.request.input_reservation_id.clone() else {
                return self.complete(incoming.id, failure("invalid_reservation"));
            };
            let Some(dispatch) = record.dispatch.clone() else {
                return self.complete(incoming.id, failure("state_corrupt"));
            };
            let invoke = record.request.clone();
            let payload = json!({"action":"confirm_delivery","epoch":epoch_id,"sequence":sequence,"operation_id":invoke.operation_id,"reservation_id":reservation});
            return self.submit(
                payload,
                Stage::ConfirmInput(Box::new(CommitContext {
                    incoming,
                    revision,
                    metadata,
                    epoch,
                    invoke,
                    dispatch,
                })),
            );
        }
        if operation == "cancel" {
            let Some(record) = epoch.receipts.get(&sequence) else {
                return self.complete(incoming.id, failure("unknown_receipt"));
            };
            if record.request.input_reservation_id.is_some()
                && matches!(
                    record.receipt.get("status").and_then(Value::as_str),
                    Some("reserved" | "cancelled")
                )
            {
                let request = record.request.clone();
                if record.receipt.get("status").and_then(Value::as_str) == Some("cancelled") {
                    return self.cancel_reserved_input(
                        incoming.id,
                        request,
                        record.receipt.clone(),
                    );
                }
                let mut receipt = record.receipt.clone();
                receipt["status"] = json!("cancelled");
                epoch
                    .receipts
                    .get_mut(&sequence)
                    .expect("record exists")
                    .receipt = receipt.clone();
                let value = serde_json::to_value(&epoch).map_err(|error| error.to_string())?;
                let payload = json!({"action":"collection_commit","collection":COLLECTION,"expected_revision":revision,"metadata":metadata,"edits":[{"key":format!("epoch/{epoch_id}"),"value":value}]});
                return self.submit(
                    payload,
                    Stage::ReservedCancelWrite {
                        parent: incoming.id,
                        request,
                        receipt,
                    },
                );
            }
            if !matches!(
                record.receipt.get("status").and_then(Value::as_str),
                Some("queued" | "running")
            ) {
                return self.complete(
                    incoming.id,
                    self.observed_receipt(epoch_id, sequence, record),
                );
            }
            let key = (epoch_id.to_owned(), sequence);
            let Some(active) = self.active.get_mut(&key) else {
                return self.complete(incoming.id, failure("cancellation_unavailable"));
            };
            if active.cancel_parent.is_some() {
                return self.complete(incoming.id, failure("cancellation_pending"));
            }
            active.cancel_parent = Some(incoming.id);
            return self.persist_cancellation(key);
        }
        if !matches!(operation, "invoke" | "reserve_input") {
            let result = epoch
                .receipts
                .get(&sequence)
                .map(|record| {
                    if operation == "cancel"
                        && matches!(
                            record.receipt.get("status").and_then(Value::as_str),
                            Some("queued" | "running")
                        )
                    {
                        failure("cancellation_unavailable")
                    } else {
                        self.observed_receipt(epoch_id, sequence, record)
                    }
                })
                .unwrap_or_else(|| failure("unknown_receipt"));
            return self.complete(incoming.id, result);
        }
        let mut request = incoming.payload.clone();
        request
            .as_object_mut()
            .expect("validated object")
            .remove("operation");
        let Ok(invoke) = serde_json::from_value::<ActionInvoke>(request) else {
            return self.complete(incoming.id, failure("invalid_request"));
        };
        if (operation == "reserve_input") != invoke.input_reservation_id.is_some() {
            return self.complete(incoming.id, failure("invalid_reservation"));
        }
        let target_kind = match invoke.target {
            amaterasu_extension_api::ActionTarget::Session { .. } => "session",
            amaterasu_extension_api::ActionTarget::Stream { .. } => "stream",
            amaterasu_extension_api::ActionTarget::Host { .. } => "host",
        };
        if !self
            .policy
            .permits(&incoming.caller, &invoke.action_id, target_kind)
            || !self
                .policy
                .permits(&incoming.initiator, &invoke.action_id, target_kind)
        {
            return self.complete(incoming.id, failure("permission_denied"));
        }
        if invoke.epoch != epoch_id || invoke.operation_id != format!("{epoch_id}:{sequence}") {
            return self.complete(incoming.id, failure("invalid_operation_id"));
        }
        if let Some(record) = epoch.receipts.get(&sequence) {
            return self.complete(
                incoming.id,
                if record.request == invoke {
                    self.observed_receipt(epoch_id, sequence, record)
                } else {
                    failure("operation_conflict")
                },
            );
        }
        if epoch.closed {
            return self.complete(incoming.id, failure("epoch_closed"));
        }
        if advance_watermark(&mut epoch, sequence).is_err() {
            return self.complete(incoming.id, failure("sequence_window"));
        }
        if operation == "reserve_input" {
            let payload = json!({"operation":"consume_input_reservation","operation_id":invoke.operation_id,"action_id":invoke.action_id,"target":invoke.target,"reservation_id":invoke.input_reservation_id});
            return self.submit(
                payload,
                Stage::InputReservation(Box::new(ReservationContext {
                    incoming,
                    revision,
                    metadata,
                    epoch,
                    invoke,
                })),
            );
        }
        let payload = json!({"operation":"describe_action","action_id":invoke.action_id});
        self.submit(
            payload,
            Stage::Descriptor {
                incoming,
                revision,
                metadata,
                epoch: Box::new(epoch),
                invoke: Box::new(invoke),
            },
        )
    }
    fn after_descriptor(
        &mut self,
        incoming: Incoming,
        revision: u64,
        metadata: Value,
        epoch: Epoch,
        invoke: ActionInvoke,
        response: Value,
    ) -> ModuleResult<()> {
        let Ok(action) = serde_json::from_value::<amaterasu_extension_api::ActionDescriptor>(
            response.get("action").cloned().unwrap_or(Value::Null),
        ) else {
            return self.complete(incoming.id, failure("descriptor_unavailable"));
        };
        if response.get("ok") != Some(&Value::Bool(true))
            || crate::validation::validate_request(&action, &invoke).is_err()
        {
            return self.complete(incoming.id, failure("invalid_action"));
        }
        if !action.required_capabilities.is_empty() {
            return self.complete(incoming.id, failure("capability_unavailable"));
        }
        let Some(provider) = response
            .get("provider")
            .and_then(Value::as_str)
            .filter(|provider| !provider.is_empty() && provider.len() <= 256)
        else {
            return self.complete(incoming.id, failure("descriptor_unavailable"));
        };
        let Some(registry_revision) = response
            .get("revision")
            .and_then(Value::as_u64)
            .filter(|revision| *revision > 0)
        else {
            return self.complete(incoming.id, failure("descriptor_unavailable"));
        };
        let class = match action.execution {
            amaterasu_extension_api::ExecutionClass::ReadOnly => "read_only",
            amaterasu_extension_api::ExecutionClass::SessionMutation => "session_mutation",
            amaterasu_extension_api::ExecutionClass::HostMutation => "host_mutation",
        };
        let target_kind = match &invoke.target {
            amaterasu_extension_api::ActionTarget::Session { .. } => "session",
            amaterasu_extension_api::ActionTarget::Stream { .. } => "stream",
            amaterasu_extension_api::ActionTarget::Host { .. } => "host",
        };
        if !self
            .policy
            .permits_class(&incoming.caller, &invoke.action_id, target_kind, class)
            || !self.policy.permits_class(
                &incoming.initiator,
                &invoke.action_id,
                target_kind,
                class,
            )
        {
            return self.complete(incoming.id, failure("permission_denied"));
        }
        let Some(method) = action.plan.clone() else {
            return self.complete(incoming.id, failure("plan_unavailable"));
        };
        let payload = json!({"version":1,"operation_id":invoke.operation_id,"action_id":invoke.action_id,"target":invoke.target,"arguments":invoke.arguments,"expected_settings_revision":invoke.expected_settings_revision});
        let request = Command::CallApi {
            service: method.service,
            method: method.method,
            interface_major: method.interface_major,
            interface_minor: method.interface_minor,
            payload,
            timeout_ms: 5_000,
            operation_id: format!("actions-{}-plan", incoming.id),
            parent: Some(incoming.id),
        };
        let parent = incoming.id;
        let stage = Stage::Plan(Box::new(PlanContext {
            incoming,
            revision,
            metadata,
            epoch,
            invoke,
            action,
            provider: provider.to_owned(),
            registry_revision,
        }));
        match self.runtime.send(request)? {
            Response::Accepted { id } => {
                self.pending.push_back(Pending { request: id, stage });
                Ok(())
            }
            _ => self.complete(parent, failure("plan_unavailable")),
        }
    }
    fn after_plan(&mut self, context: PlanContext, response: Value) -> ModuleResult<()> {
        let PlanContext {
            incoming,
            revision,
            metadata,
            mut epoch,
            invoke,
            action,
            provider,
            registry_revision,
        } = context;
        if response.get("status").and_then(Value::as_str) != Some("planned")
            || response.get("operation_id").and_then(Value::as_str)
                != Some(invoke.operation_id.as_str())
            || response.get("action_id").and_then(Value::as_str) != Some(invoke.action_id.as_str())
            || response.get("target") != serde_json::to_value(&invoke.target).ok().as_ref()
        {
            return self.complete(incoming.id, failure("invalid_plan"));
        }
        let Ok(plan) = serde_json::from_value::<amaterasu_extension_api::ResourcePlan>(
            response.get("plan").cloned().unwrap_or(Value::Null),
        ) else {
            return self.complete(incoming.id, failure("invalid_plan"));
        };
        use amaterasu_extension_api::{ConsumerChange, ResourceAccess};
        if amaterasu_extension_api::validate_resource_plan(&plan).is_err() {
            return self.complete(incoming.id, failure("invalid_plan"));
        }
        if action.execution == amaterasu_extension_api::ExecutionClass::ReadOnly
            && (plan
                .resources
                .iter()
                .any(|resource| resource.access != ResourceAccess::Read)
                || plan
                    .mutation_domains
                    .iter()
                    .any(|domain| domain.access != ResourceAccess::Read)
                || plan.consumer_change != ConsumerChange::Live
                || !plan.affected_consumers.is_empty()
                || !plan.obligations.is_empty()
                || !plan.allowed_degradation.is_empty()
                || plan.candidate_constraint.is_some())
        {
            return self.complete(incoming.id, failure("invalid_plan"));
        }
        let Ok(mut dispatch) = crate::dispatch::Dispatch::new(
            invoke.clone(),
            action.clone(),
            provider.clone(),
            registry_revision,
            incoming.caller.clone(),
            incoming.initiator.clone(),
        ) else {
            return self.complete(incoming.id, failure("invalid_plan"));
        };
        dispatch.source = incoming.input_source.clone();
        if dispatch
            .accept_plan(response.get("plan").cloned().unwrap_or(Value::Null))
            .is_err()
        {
            return self.complete(incoming.id, failure("invalid_plan"));
        }
        if self.active.len() >= 128
            || self
                .active
                .values()
                .filter(|work| same_session(&work.dispatch.request.target, &invoke.target))
                .count()
                >= 16
        {
            return self.complete(incoming.id, failure("capacity"));
        }
        let reserved = invoke.input_reservation_id.is_some();
        let receipt = json!({"status":if reserved {"reserved"} else {"queued"},"receipt_id":invoke.operation_id,"operation_id":invoke.operation_id,"result":null,"reason":null});
        let epoch_id = invoke.epoch.clone();
        epoch.receipts.insert(
            invoke.sequence,
            Record {
                request: invoke,
                receipt: receipt.clone(),
                selected_provider: provider,
                registry_revision,
                descriptor: action,
                dispatch: Some(dispatch.clone()),
            },
        );
        while serde_json::to_vec(&epoch).map_or(true, |bytes| bytes.len() > MAX_EPOCH_BYTES) {
            if retire_one(&mut epoch).is_err() {
                return self.complete(incoming.id, failure("capacity"));
            }
        }
        self.write_start(
            incoming.id,
            (revision, metadata),
            &epoch_id,
            &epoch,
            receipt,
            if reserved { None } else { Some(dispatch) },
        )
    }
    fn after_input_reservation(
        &mut self,
        context: ReservationContext,
        response: Value,
    ) -> ModuleResult<()> {
        let ReservationContext {
            mut incoming,
            revision,
            metadata,
            epoch,
            invoke,
        } = context;
        if response.get("status").and_then(Value::as_str) != Some("succeeded")
            || response.get("operation_id").and_then(Value::as_str)
                != Some(invoke.operation_id.as_str())
            || response.get("action_id").and_then(Value::as_str) != Some(invoke.action_id.as_str())
            || response.get("reservation_id").and_then(Value::as_str)
                != invoke.input_reservation_id.as_deref()
            || response.get("target") != serde_json::to_value(&invoke.target).ok().as_ref()
        {
            return self.complete(incoming.id, failure("input_reservation_unavailable"));
        }
        let Ok(source) = serde_json::from_value::<crate::dispatch::Source>(
            response.get("source").cloned().unwrap_or(Value::Null),
        ) else {
            return self.complete(incoming.id, failure("input_reservation_unavailable"));
        };
        if source.caller.is_empty()
            || source.initiator.is_empty()
            || source.caller.len() > 256
            || source.initiator.len() > 256
        {
            return self.complete(incoming.id, failure("input_reservation_unavailable"));
        }
        incoming.input_source = Some(source);
        let payload = json!({"operation":"describe_action","action_id":invoke.action_id});
        self.submit(
            payload,
            Stage::Descriptor {
                incoming,
                revision,
                metadata,
                epoch: Box::new(epoch),
                invoke: Box::new(invoke),
            },
        )
    }
    fn after_input_confirmation(
        &mut self,
        context: CommitContext,
        response: Value,
    ) -> ModuleResult<()> {
        let CommitContext {
            incoming,
            revision,
            metadata,
            mut epoch,
            invoke,
            dispatch,
        } = context;
        if response.get("ok") != Some(&Value::Bool(true))
            || response.get("status").and_then(Value::as_str) != Some("delivered")
            || response.get("epoch").and_then(Value::as_str) != Some(invoke.epoch.as_str())
            || response.get("sequence").and_then(Value::as_u64) != Some(invoke.sequence)
            || response.get("operation_id").and_then(Value::as_str)
                != Some(invoke.operation_id.as_str())
            || response.get("reservation_id").and_then(Value::as_str)
                != invoke.input_reservation_id.as_deref()
            || response.get("action_id").and_then(Value::as_str) != Some(invoke.action_id.as_str())
            || response.get("target") != serde_json::to_value(&invoke.target).ok().as_ref()
        {
            return self.complete(incoming.id, failure("input_delivery_unconfirmed"));
        }
        let receipt = action_receipt(&dispatch, "queued", None);
        let Some(record) = epoch.receipts.get_mut(&invoke.sequence) else {
            return self.complete(incoming.id, failure("state_corrupt"));
        };
        record.receipt = receipt.clone();
        self.write_start(
            incoming.id,
            (revision, metadata),
            &invoke.epoch,
            &epoch,
            receipt,
            Some(dispatch),
        )
    }
    fn cancel_reserved_input(
        &mut self,
        parent: u64,
        request: ActionInvoke,
        receipt: Value,
    ) -> ModuleResult<()> {
        let payload = json!({"operation":"cancel_input_reservation","operation_id":request.operation_id,"action_id":request.action_id,"reservation_id":request.input_reservation_id});
        self.submit(
            payload,
            Stage::ReservedCancelReply {
                parent,
                request,
                receipt,
            },
        )
    }
    fn write(
        &mut self,
        parent: u64,
        revision: u64,
        metadata: Value,
        epoch: &str,
        record: &Epoch,
        result: Value,
    ) -> ModuleResult<()> {
        self.write_start(parent, (revision, metadata), epoch, record, result, None)
    }
    fn write_start(
        &mut self,
        parent: u64,
        state: (u64, Value),
        epoch: &str,
        record: &Epoch,
        result: Value,
        start: Option<Dispatch>,
    ) -> ModuleResult<()> {
        let (revision, metadata) = state;
        let value = serde_json::to_value(record).map_err(|error| error.to_string())?;
        if serde_json::to_vec(&value)
            .map_err(|error| error.to_string())?
            .len()
            > MAX_EPOCH_BYTES
        {
            return self.complete(parent, failure("capacity"));
        }
        let payload = json!({"action":"collection_commit","collection":COLLECTION,"expected_revision":revision,"metadata":metadata,"edits":[{"key":format!("epoch/{epoch}"),"value":value}]});
        self.submit(
            payload,
            Stage::Write {
                parent,
                result,
                start: start.map(Box::new),
            },
        )
    }
    fn advance_recovery(&mut self) -> ModuleResult<()> {
        let phase = std::mem::replace(&mut self.recovery, Recovery::Waiting);
        let (payload, stage) = match phase {
            Recovery::NeedHead => (
                json!({"action":"collection_get","collection":COLLECTION}),
                Stage::RecoveryHead,
            ),
            Recovery::NeedPage { revision, after } => (
                json!({"action":"collection_page","collection":COLLECTION,"revision":revision,"after":after,"limit":16}),
                Stage::RecoveryPage { revision },
            ),
            other => {
                self.recovery = other;
                return Ok(());
            }
        };
        self.recovery_operation = self.recovery_operation.saturating_add(1);
        match self.runtime.send(Command::Request {
            service: STATE.into(),
            payload,
            timeout_ms: 5_000,
            operation_id: format!(
                "actions-recovery-{}-{}",
                self.recovery_epoch, self.recovery_operation
            ),
            parent: None,
        }) {
            Ok(Response::Accepted { id }) => self.pending.push_back(Pending { request: id, stage }),
            _ => self.recovery = Recovery::Blocked,
        }
        Ok(())
    }
    fn recovery_head(&mut self, payload: Value) -> ModuleResult<()> {
        self.recovery = match payload.get("revision").and_then(Value::as_u64) {
            Some(revision) if payload.get("ok") == Some(&Value::Bool(true)) => Recovery::NeedPage {
                revision,
                after: None,
            },
            _ => Recovery::Blocked,
        };
        Ok(())
    }
    fn recovery_page(&mut self, revision: u64, payload: Value) -> ModuleResult<()> {
        if payload.pointer("/error/code").and_then(Value::as_str) == Some("revision_conflict")
            && self.recovery_retries < 8
        {
            self.recovery_retries += 1;
            self.recovery = Recovery::NeedHead;
            return Ok(());
        }
        if payload.get("ok") != Some(&Value::Bool(true))
            || payload.get("revision").and_then(Value::as_u64) != Some(revision)
        {
            self.recovery = Recovery::Blocked;
            return Ok(());
        }
        let Some(entries) = payload.get("entries").and_then(Value::as_array) else {
            self.recovery = Recovery::Blocked;
            return Ok(());
        };
        for item in entries {
            let Some(epoch_id) = item
                .get("key")
                .and_then(Value::as_str)
                .and_then(|key| key.strip_prefix("epoch/"))
            else {
                continue;
            };
            if !valid_epoch(epoch_id) {
                self.recovery = Recovery::Blocked;
                return Ok(());
            }
            let Ok(epoch) =
                serde_json::from_value::<Epoch>(item.get("value").cloned().unwrap_or(Value::Null))
            else {
                self.recovery = Recovery::Blocked;
                return Ok(());
            };
            for (sequence, record) in epoch.receipts {
                if !matches!(
                    record.receipt.get("status").and_then(Value::as_str),
                    Some("queued" | "running")
                ) {
                    continue;
                }
                let Some(dispatch) = record.dispatch else {
                    self.recovery = Recovery::Blocked;
                    return Ok(());
                };
                let mut replacement = dispatch.clone();
                replacement.fence_after_restart();
                if replacement == dispatch {
                    replacement.stage = DispatchStage::RecoveryRequired;
                }
                let receipt =
                    action_receipt(&replacement, "recovery_required", Some("worker_restart"));
                self.recovery = match self.worker.persist(
                    epoch_id.to_owned(),
                    dispatch,
                    replacement,
                    record.receipt,
                    receipt,
                    Instant::now() + Duration::from_secs(30),
                ) {
                    Ok(()) => Recovery::WaitingPersist {
                        epoch: epoch_id.to_owned(),
                        sequence,
                    },
                    Err(_) => Recovery::Blocked,
                };
                return Ok(());
            }
        }
        self.recovery = match payload.get("next") {
            Some(Value::String(after)) => Recovery::NeedPage {
                revision,
                after: Some(after.clone()),
            },
            Some(Value::Null) | None => Recovery::Done,
            _ => Recovery::Blocked,
        };
        Ok(())
    }
    fn observed_receipt(&self, epoch: &str, sequence: u64, record: &Record) -> Value {
        if matches!(
            record.receipt.get("status").and_then(Value::as_str),
            Some("queued" | "running")
        ) {
            let active = self.active.get(&(epoch.to_owned(), sequence));
            if active.is_none_or(|work| work.recovery_unconfirmed) {
                let mut receipt = record.receipt.clone();
                receipt["status"] = json!("recovery_required");
                receipt["reason"] = json!("worker_state_unconfirmed");
                return receipt;
            }
        }
        record.receipt.clone()
    }
    fn start_dispatch(&mut self, dispatch: Dispatch, receipt: Value) -> ModuleResult<()> {
        let key = (dispatch.request.epoch.clone(), dispatch.request.sequence);
        if self.active.contains_key(&key) {
            return Ok(());
        }
        self.active.insert(
            key.clone(),
            ActiveWork {
                dispatch,
                receipt,
                replacement: None,
                recovery_unconfirmed: false,
                polls: 0,
                cancel_parent: None,
                cancel_sent: false,
                effect_started: false,
            },
        );
        self.drive(key)
    }
    fn drive(&mut self, key: (String, u64)) -> ModuleResult<()> {
        if self.pending.len() >= 24 {
            return Ok(());
        }
        let Some(active) = self.active.get_mut(&key) else {
            return Ok(());
        };
        if active.replacement.is_some() || active.recovery_unconfirmed {
            return Ok(());
        }
        if active.receipt.get("cancellation_requested") == Some(&Value::Bool(true))
            && !active.cancel_sent
        {
            let permit = match &active.dispatch.stage {
                DispatchStage::AwaitingConsume { permit, .. }
                | DispatchStage::AwaitingTransition { permit, .. }
                | DispatchStage::ReadyToExecute { permit, .. }
                | DispatchStage::Running { permit, .. }
                | DispatchStage::ProviderPending { permit, .. } => Some(permit.clone()),
                _ => None,
            };
            active.cancel_sent = true;
            if let Some(permit) = permit {
                let operation_id = &active.dispatch.request.operation_id;
                let command = Command::Request {
                    service: amaterasu_extension_api::SESSION_LIFECYCLE_SERVICE.into(),
                    payload: json!({"operation":"cancel_action","operation_id":operation_id,"permit":permit,"target":active.dispatch.request.target}),
                    timeout_ms: 5_000,
                    operation_id: format!("{}.cancel", operation_id.replace(':', ".")),
                    parent: None,
                };
                if let Ok(Response::Accepted { id }) = self.runtime.send(command) {
                    self.pending.push_back(Pending {
                        request: id,
                        stage: Stage::CancelReply { key },
                    });
                    return Ok(());
                }
            }
        }
        if active.receipt.get("cancellation_requested") == Some(&Value::Bool(true))
            && matches!(
                active.dispatch.stage,
                DispatchStage::AwaitingConsume { .. }
                    | DispatchStage::AwaitingTransition { .. }
                    | DispatchStage::ReadyToExecute { .. }
                    | DispatchStage::Running { .. }
            )
            && !active.effect_started
        {
            return self.dispatch_unknown(key);
        }
        if matches!(active.dispatch.stage, DispatchStage::ReadyToExecute { .. }) {
            let expected = active.dispatch.clone();
            let mut replacement = expected.clone();
            if replacement.begin_effect().is_err() {
                active.recovery_unconfirmed = true;
                return Ok(());
            }
            let receipt = action_receipt(&replacement, "running", None);
            match self.worker.persist(
                key.0.clone(),
                expected,
                replacement.clone(),
                active.receipt.clone(),
                receipt.clone(),
                Instant::now() + Duration::from_secs(30),
            ) {
                Ok(()) => active.replacement = Some((replacement, receipt)),
                Err(_) => active.recovery_unconfirmed = true,
            }
            return Ok(());
        }
        let mut command = match active.dispatch.broker_call() {
            Ok(Some(command)) => command,
            Ok(None) => {
                self.active.remove(&key);
                return Ok(());
            }
            Err(_) => return self.dispatch_unknown(key),
        };
        if matches!(
            active.dispatch.stage,
            DispatchStage::ProviderPending { .. } | DispatchStage::AwaitingTransition { .. }
        ) {
            active.polls = active.polls.saturating_add(1);
            match &mut command {
                Command::CallApi { operation_id, .. } | Command::Request { operation_id, .. } => {
                    operation_id.push_str(&format!("-{}", active.polls))
                }
                _ => {}
            }
        }
        if matches!(active.dispatch.stage, DispatchStage::Running { .. }) {
            active.effect_started = true;
        }
        match self.runtime.send(command) {
            Ok(Response::Accepted { id }) => {
                self.pending.push_back(Pending {
                    request: id,
                    stage: Stage::DispatchReply { key },
                });
                Ok(())
            }
            _ => self.dispatch_unknown(key),
        }
    }
    fn cancel_reply(&mut self, key: (String, u64), payload: Value) -> ModuleResult<()> {
        let Some(active) = self.active.get_mut(&key) else {
            return Ok(());
        };
        if matches!(
            active.dispatch.stage,
            DispatchStage::AwaitingConsume { .. }
                | DispatchStage::AwaitingTransition { .. }
                | DispatchStage::ReadyToExecute { .. }
                | DispatchStage::Running { .. }
        ) && !active.effect_started
        {
            if payload.get("status").and_then(Value::as_str) != Some("cancelled")
                || payload.get("operation_id").and_then(Value::as_str)
                    != Some(active.dispatch.request.operation_id.as_str())
            {
                return self.dispatch_unknown(key);
            }
            let expected = active.dispatch.clone();
            let mut replacement = expected.clone();
            if replacement.confirm_cancelled_before_effect().is_err() {
                return self.dispatch_unknown(key);
            }
            let mut receipt = action_receipt(&replacement, "cancelled", None);
            receipt["cancellation_requested"] = json!(true);
            match self.worker.persist(
                key.0.clone(),
                expected,
                replacement.clone(),
                active.receipt.clone(),
                receipt.clone(),
                Instant::now() + Duration::from_secs(30),
            ) {
                Ok(()) => active.replacement = Some((replacement, receipt)),
                Err(_) => active.recovery_unconfirmed = true,
            }
            return Ok(());
        }
        self.drive(key)
    }
    fn dispatch_reply(&mut self, key: (String, u64), payload: Value) -> ModuleResult<()> {
        let Some(active) = self.active.get_mut(&key) else {
            return Ok(());
        };
        let expected = active.dispatch.clone();
        let mut replacement = expected.clone();
        if replacement.accept_reply(payload).is_err() {
            return self.dispatch_unknown(key);
        }
        if replacement == expected {
            if active.cancel_parent.is_some() {
                return self.persist_cancellation(key);
            }
            return self.drive(key);
        }
        let status = match &replacement.stage {
            DispatchStage::Running { .. }
            | DispatchStage::ProviderPending { .. }
            | DispatchStage::Completing { .. } => "running",
            DispatchStage::Finished {
                outcome: Outcome::Succeeded,
                ..
            } => "succeeded",
            DispatchStage::Finished {
                outcome: Outcome::Failed,
                ..
            } => "failed",
            DispatchStage::RecoveryRequired
            | DispatchStage::Finished {
                outcome: Outcome::RecoveryRequired,
                ..
            } => "recovery_required",
            _ => "queued",
        };
        let mut receipt = action_receipt(&replacement, status, None);
        if active.receipt.get("cancellation_requested") == Some(&Value::Bool(true)) {
            receipt["cancellation_requested"] = json!(true);
        }
        match self.worker.persist(
            key.0.clone(),
            expected,
            replacement.clone(),
            active.receipt.clone(),
            receipt.clone(),
            Instant::now() + Duration::from_secs(30),
        ) {
            Ok(()) => active.replacement = Some((replacement, receipt)),
            Err(_) => active.recovery_unconfirmed = true,
        }
        Ok(())
    }
    fn dispatch_unknown(&mut self, key: (String, u64)) -> ModuleResult<()> {
        let Some(active) = self.active.get_mut(&key) else {
            return Ok(());
        };
        let expected = active.dispatch.clone();
        let mut replacement = expected.clone();
        replacement.fence_after_restart();
        if replacement == expected {
            active.recovery_unconfirmed = true;
            return Ok(());
        }
        let mut receipt =
            action_receipt(&replacement, "recovery_required", Some("outcome_unknown"));
        if active.receipt.get("cancellation_requested") == Some(&Value::Bool(true)) {
            receipt["cancellation_requested"] = json!(true);
        }
        match self.worker.persist(
            key.0.clone(),
            expected,
            replacement.clone(),
            active.receipt.clone(),
            receipt.clone(),
            Instant::now() + Duration::from_secs(30),
        ) {
            Ok(()) => active.replacement = Some((replacement, receipt)),
            Err(_) => active.recovery_unconfirmed = true,
        }
        Ok(())
    }
    fn persist_cancellation(&mut self, key: (String, u64)) -> ModuleResult<()> {
        let inflight = self.pending.iter().any(|pending| matches!(&pending.stage, Stage::DispatchReply { key: pending_key } if pending_key == &key));
        let Some(active) = self.active.get_mut(&key) else {
            return Ok(());
        };
        if active.replacement.is_some() || inflight {
            return Ok(());
        }
        let Some(parent) = active.cancel_parent else {
            return Ok(());
        };
        if active.recovery_unconfirmed {
            active.cancel_parent = None;
            return self.complete(parent, failure("recovery_required"));
        }
        if active.receipt.get("cancellation_requested") == Some(&Value::Bool(true)) {
            let receipt = active.receipt.clone();
            active.cancel_parent = None;
            return self.complete(parent, receipt);
        }
        if !matches!(
            active.dispatch.stage,
            DispatchStage::AwaitingConsume { .. }
                | DispatchStage::AwaitingTransition { .. }
                | DispatchStage::ReadyToExecute { .. }
                | DispatchStage::Running { .. }
                | DispatchStage::ProviderPending { .. }
                | DispatchStage::Completing { .. }
        ) {
            active.cancel_parent = None;
            return self.complete(parent, failure("cancellation_unavailable"));
        }
        let expected = active.dispatch.clone();
        let mut receipt = active.receipt.clone();
        receipt["cancellation_requested"] = json!(true);
        match self.worker.persist(
            key.0.clone(),
            expected.clone(),
            expected.clone(),
            active.receipt.clone(),
            receipt.clone(),
            Instant::now() + Duration::from_secs(30),
        ) {
            Ok(()) => active.replacement = Some((expected, receipt)),
            Err(_) => {
                active.cancel_parent = None;
                return self.complete(parent, failure("cancellation_unavailable"));
            }
        }
        Ok(())
    }
    fn worker_completed(&mut self, completion: crate::worker::Completion) -> ModuleResult<()> {
        let key = (completion.epoch_id, completion.sequence);
        if matches!(&self.recovery, Recovery::WaitingPersist { epoch, sequence } if epoch == &key.0 && *sequence == key.1)
        {
            self.recovery = if completion.result.is_ok() {
                Recovery::NeedHead
            } else {
                Recovery::Blocked
            };
            return Ok(());
        }
        let Some(active) = self.active.get_mut(&key) else {
            return Ok(());
        };
        if completion.result.is_err() {
            active.recovery_unconfirmed = true;
            if let Some(parent) = active.cancel_parent.take() {
                return self.complete(parent, failure("recovery_required"));
            }
            return Ok(());
        }
        let Some((dispatch, receipt)) = active.replacement.take() else {
            active.recovery_unconfirmed = true;
            return Ok(());
        };
        active.dispatch = dispatch;
        active.receipt = receipt;
        if let Some(parent) = active.cancel_parent {
            if active.receipt.get("cancellation_requested") == Some(&Value::Bool(true))
                || matches!(
                    active.dispatch.stage,
                    DispatchStage::Finished { .. } | DispatchStage::RecoveryRequired
                )
            {
                let receipt = active.receipt.clone();
                active.cancel_parent = None;
                self.complete(parent, receipt)?;
            } else {
                return self.persist_cancellation(key);
            }
        }
        self.drive(key)
    }
    fn complete(&self, request: u64, payload: Value) -> ModuleResult<()> {
        match self.runtime.send(Command::Complete { request, payload })? {
            Response::Ok => Ok(()),
            Response::Error { code, .. } if code == "request_ended" || code == "not_found" => {
                Ok(())
            }
            other => Err(format!("action completion failed: {other:?}")),
        }
    }
}
fn terminal_receipt(record: &Record) -> bool {
    matches!(
        record.receipt.get("status").and_then(Value::as_str),
        Some("rejected" | "succeeded" | "failed" | "cancelled")
    )
}
fn retire_one(epoch: &mut Epoch) -> Result<(), ()> {
    let next = epoch.retired.checked_add(1).ok_or(())?;
    if !epoch.receipts.get(&next).is_some_and(terminal_receipt) {
        return Err(());
    }
    epoch.receipts.remove(&next);
    epoch.retired = next;
    Ok(())
}
fn advance_watermark(epoch: &mut Epoch, sequence: u64) -> Result<(), ()> {
    let required = sequence.saturating_sub(64);
    if required <= epoch.retired {
        return Ok(());
    }
    if required - epoch.retired > epoch.receipts.len() as u64
        || (epoch.retired + 1..=required)
            .any(|index| !epoch.receipts.get(&index).is_some_and(terminal_receipt))
    {
        return Err(());
    }
    while epoch.retired < required {
        retire_one(epoch)?;
    }
    Ok(())
}
fn same_session(
    left: &amaterasu_extension_api::ActionTarget,
    right: &amaterasu_extension_api::ActionTarget,
) -> bool {
    use amaterasu_extension_api::ActionTarget;
    match (left, right) {
        (
            ActionTarget::Session {
                host_epoch: a,
                session_id: x,
                ..
            }
            | ActionTarget::Stream {
                host_epoch: a,
                session_id: x,
                ..
            },
            ActionTarget::Session {
                host_epoch: b,
                session_id: y,
                ..
            }
            | ActionTarget::Stream {
                host_epoch: b,
                session_id: y,
                ..
            },
        ) => a == b && x == y,
        (ActionTarget::Host { host_epoch: a }, ActionTarget::Host { host_epoch: b }) => a == b,
        _ => false,
    }
}
fn action_receipt(dispatch: &Dispatch, status: &str, reason: Option<&str>) -> Value {
    let result = match &dispatch.stage {
        DispatchStage::Finished {
            provider_receipt, ..
        } => Some(provider_receipt.clone()),
        _ => None,
    };
    json!({"status":status,"receipt_id":dispatch.request.operation_id,"operation_id":dispatch.request.operation_id,"result":result,"reason":reason})
}
fn open_key(caller: &str, initiator: &str, operation_id: &str) -> String {
    let mut digest = Sha256::new();
    for part in [caller, initiator, operation_id] {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part.as_bytes());
    }
    let hash = digest.finalize();
    let suffix: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("open/{suffix}")
}
fn valid_epoch(epoch: &str) -> bool {
    epoch.len() == 32
        && epoch
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn failure(code: &str) -> Value {
    json!({"status":"error","code":code})
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_module_sdk::abi::{self, AbiHeader, BorrowedBytes, HostApi, ReplySink};
    use std::collections::{BTreeMap, VecDeque};
    use std::ffi::c_void;

    #[derive(Default)]
    struct Host {
        incoming: VecDeque<Response>,
        replies: Vec<Value>,
        state: BTreeMap<String, Value>,
        results: BTreeMap<u64, Value>,
        revision: u64,
        next: u64,
        metadata: Value,
        allow_permit: bool,
        revoke_consume: bool,
        effects: usize,
        provider_pending: bool,
        cancellation_requests: usize,
        mutation_descriptor: bool,
        required_capability: bool,
        transition_pending: usize,
        transition_count: usize,
        delivery_confirmed: bool,
        last_source: Value,
    }
    impl Host {
        fn dispatch(&mut self, command: Command) -> Response {
            match command {
                Command::Register { .. } | Command::ReleaseResult { .. } => Response::Ok,
                Command::GetSettings {} => Response::Result {
                    payload: json!({"input_service":"input","grants":[{"principal":"input","action_ids":["example/action"],"target_kinds":["session"],"execution_classes":["read_only","session_mutation"]},{"principal":"management","action_ids":["example/action"],"target_kinds":["session"],"execution_classes":["read_only","session_mutation"]},{"principal":"alice","action_ids":["example/action"],"target_kinds":["session"],"execution_classes":["read_only","session_mutation"]},{"principal":"bob","action_ids":["example/action"],"target_kinds":["session"],"execution_classes":["read_only","session_mutation"]}]}),
                },
                Command::Poll {} => self.incoming.pop_front().unwrap_or(Response::Pending),
                Command::Complete { payload, .. } => {
                    self.replies.push(payload);
                    Response::Ok
                }
                Command::Request {
                    service,
                    payload,
                    parent,
                    ..
                } => {
                    self.next += 1;
                    let value = if service == amaterasu_extension_api::EXTENSION_REGISTRY_SERVICE {
                        assert!(parent.is_some());
                        let method = json!({"service":"example.provider/1","method":"action_execute","interface_major":1,"interface_minor":0,"schema_revision":1});
                        let plan = json!({"service":"example.provider/1","method":"action_plan","interface_major":1,"interface_minor":0,"schema_revision":1});
                        let status = json!({"service":"example.provider/1","method":"action_status","interface_major":1,"interface_minor":0,"schema_revision":1});
                        let capabilities = if self.required_capability {
                            vec!["example/capability"]
                        } else {
                            vec![]
                        };
                        let execution = if self.mutation_descriptor {
                            "session_mutation"
                        } else {
                            "read_only"
                        };
                        json!({"ok":true,"provider":"example.provider","revision":1,"action":{"id":"example/action","label":"Example","description":"Example","execution":execution,"arguments":{},"allowed_targets":["session"],"required_capabilities":capabilities,"plan":plan,"execute":method,"status":status}})
                    } else if service == amaterasu_extension_api::SESSION_LIFECYCLE_SERVICE {
                        match payload["operation"].as_str() {
                            Some("consume_input_reservation") if self.allow_permit => {
                                json!({"status":"succeeded","operation_id":payload["operation_id"],"action_id":payload["action_id"],"target":payload["target"],"reservation_id":payload["reservation_id"],"source":{"caller":"stream-session","initiator":"paired-client"}})
                            }
                            Some("cancel_input_reservation") if self.allow_permit => {
                                self.cancellation_requests += 1;
                                let (epoch, sequence) = payload["operation_id"]
                                    .as_str()
                                    .unwrap()
                                    .split_once(':')
                                    .unwrap();
                                assert_eq!(
                                    self.state[&format!("epoch/{epoch}")]["receipts"][sequence]["receipt"]
                                        ["status"],
                                    "cancelled"
                                );
                                json!({"status":"succeeded","operation_id":payload["operation_id"],"action_id":payload["action_id"],"reservation_id":payload["reservation_id"]})
                            }
                            Some("authorize_action") if self.allow_permit => {
                                self.last_source = payload["source"].clone();
                                json!({"status":"succeeded","operation_id":payload["operation_id"],"target":payload["target"],"permit":"permit"})
                            }
                            Some("consume_action_permit")
                                if self.allow_permit && !self.revoke_consume =>
                            {
                                json!({"status":"succeeded","operation_id":payload["operation_id"],"target":payload["target"],"plan_digest":payload["plan_digest"]})
                            }
                            Some("cancel_action") if self.allow_permit => {
                                self.cancellation_requests += 1;
                                json!({"status":"pending"})
                            }
                            Some("begin_action_transition") if self.allow_permit => {
                                self.transition_count += 1;
                                json!({"status":if self.transition_count <= self.transition_pending {"pending"} else {"succeeded"},"operation_id":payload["operation_id"],"target":payload["target"],"plan_digest":payload["plan_digest"]})
                            }
                            Some("complete_action") if self.allow_permit => {
                                json!({"status":"succeeded","operation_id":payload["operation_id"]})
                            }
                            _ => json!({"status":"rejected"}),
                        }
                    } else if service == "amaterasu.input/1" {
                        if self.delivery_confirmed {
                            let record = &self.state
                                [&format!("epoch/{}", payload["epoch"].as_str().unwrap())]["receipts"]
                                [payload["sequence"].as_u64().unwrap().to_string()];
                            json!({"ok":true,"status":"delivered","epoch":payload["epoch"],"sequence":payload["sequence"],"operation_id":payload["operation_id"],"reservation_id":payload["reservation_id"],"action_id":record["request"]["action_id"],"target":record["request"]["target"],"batch_epoch":"input-e","batch_sequence":1})
                        } else {
                            json!({"ok":false,"status":"unavailable"})
                        }
                    } else {
                        match payload["action"].as_str() {
                            Some("collection_get") => {
                                let value = payload["key"]
                                    .as_str()
                                    .and_then(|key| self.state.get(key))
                                    .cloned();
                                json!({"ok":true,"revision":self.revision,"count":self.state.len(),"metadata":self.metadata,"value":value})
                            }
                            Some("collection_page") => {
                                let after = payload["after"].as_str();
                                let entries: Vec<_> = self
                                    .state
                                    .iter()
                                    .filter(|(key, _)| {
                                        after.is_none_or(|cursor| key.as_str() > cursor)
                                    })
                                    .map(|(key, value)| json!({"key":key,"value":value}))
                                    .collect();
                                json!({"ok":true,"revision":self.revision,"entries":entries,"next":null})
                            }
                            Some("collection_commit")
                                if payload["expected_revision"] == self.revision =>
                            {
                                for edit in payload["edits"].as_array().unwrap() {
                                    let key = edit["key"].as_str().unwrap();
                                    if edit["value"].is_null() {
                                        self.state.remove(key);
                                    } else {
                                        self.state.insert(key.into(), edit["value"].clone());
                                    }
                                }
                                self.metadata = payload["metadata"].clone();
                                self.revision += 1;
                                json!({"ok":true,"revision":self.revision})
                            }
                            _ => json!({"ok":false}),
                        }
                    };
                    self.results.insert(self.next, value);
                    Response::Accepted { id: self.next }
                }
                Command::CallApi {
                    method,
                    payload,
                    parent,
                    ..
                } => {
                    self.next += 1;
                    let result = if method == "action_plan" {
                        assert!(parent.is_some());
                        json!({"status":"planned","operation_id":payload["operation_id"],"action_id":payload["action_id"],"target":payload["target"],"plan":{"plan_id":"p1","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]}})
                    } else if method == "action_status" {
                        json!({"status":"pending","receipt":{"id":"pending"}})
                    } else {
                        assert!(parent.is_none());
                        let (epoch, sequence) = payload["operation_id"]
                            .as_str()
                            .unwrap()
                            .split_once(':')
                            .unwrap();
                        let record = &self.state[&format!("epoch/{epoch}")]["receipts"][sequence];
                        assert_eq!(record["dispatch"]["stage"]["phase"], "running");
                        assert_eq!(record["receipt"]["status"], "running");
                        self.effects += 1;
                        if self.provider_pending {
                            json!({"status":"pending","receipt":{"id":"pending"}})
                        } else {
                            json!({"status":"succeeded","receipt":{"effect":"confirmed"}})
                        }
                    };
                    self.results.insert(self.next, result);
                    Response::Accepted { id: self.next }
                }
                Command::TakeResult { request } => Response::Result {
                    payload: self.results.remove(&request).unwrap(),
                },
                _ => Response::error("unexpected", "unsupported test command"),
            }
        }
        fn request(&mut self, id: u64, initiator: &str, payload: Value) {
            self.incoming.push_back(Response::Request {
                id,
                caller: "management".into(),
                initiator: initiator.into(),
                service: EXTENSION_ACTIONS_SERVICE.into(),
                payload,
                remaining_ms: 5_000,
            });
        }
        fn request_input(&mut self, id: u64, payload: Value) {
            self.incoming.push_back(Response::Request {
                id,
                caller: "input".into(),
                initiator: "input".into(),
                service: EXTENSION_ACTIONS_SERVICE.into(),
                payload,
                remaining_ms: 5_000,
            });
        }
        fn run(&mut self) {
            let endpoint = HostApi {
                header: AbiHeader::of::<HostApi>(),
                context: (self as *mut Self).cast(),
                invoke,
            };
            // SAFETY: the host outlives the module and is used only through this endpoint.
            let client = unsafe { RuntimeClient::from_endpoint(endpoint) };
            let mut module = ActionsModule::start(client).unwrap();
            for _ in 0..200 {
                module.tick().unwrap();
            }
        }
    }
    unsafe extern "C" fn invoke(
        context: *mut c_void,
        input: BorrowedBytes,
        reply: ReplySink,
    ) -> abi::Status {
        // SAFETY: the test owns this host and the SDK supplies valid input for this call.
        let host = unsafe { &mut *context.cast::<Host>() };
        let Ok(bytes) = (unsafe { input.read() }) else {
            return abi::STATUS_INVALID_ARGUMENT;
        };
        let Ok(command) = serde_json::from_slice(bytes) else {
            return abi::STATUS_INVALID_ARGUMENT;
        };
        let output = serde_json::to_vec(&host.dispatch(command)).unwrap();
        // SAFETY: reply remains valid during the synchronous endpoint invocation.
        unsafe { reply.send(&output) };
        abi::STATUS_OK
    }
    #[test]
    fn receipts_survive_restart_and_do_not_execute_effects() {
        let mut host = Host::default();
        host.request(
            1,
            "alice",
            json!({"operation":"open_epoch","operation_id":"open-1"}),
        );
        host.run();
        let epoch = host.replies[0]["epoch"].as_str().unwrap().to_owned();
        let invoke = json!({"operation":"invoke","epoch":epoch,"sequence":1,"operation_id":format!("{epoch}:1"),"action_id":"example/action","target":{"kind":"session","host_epoch":"h","session_id":"s","expected_generation":1},"arguments":{},"expected_settings_revision":null});
        host.request(2, "alice", invoke.clone());
        host.run();
        assert_eq!(host.replies[1]["status"], "queued");
        host.request(3, "alice", invoke.clone());
        host.request(
            4,
            "bob",
            json!({"operation":"status","epoch":epoch,"sequence":1}),
        );
        let mut changed = invoke;
        changed["arguments"] = json!({"changed":true});
        host.request(5, "alice", changed);
        host.run();
        assert_eq!(host.replies[2]["status"], "recovery_required");
        assert_eq!(host.replies[3]["code"], "unknown_epoch");
        assert_eq!(host.replies[4]["code"], "operation_conflict");
        assert_eq!(host.effects, 0);
    }
    #[test]
    fn epoch_open_is_idempotent_across_restart_and_scoped_by_initiator() {
        let mut host = Host::default();
        let open = json!({"operation":"open_epoch","operation_id":"client-open-7"});
        host.request(1, "alice", open.clone());
        host.run();
        host.request(2, "alice", open.clone());
        host.request(3, "bob", open);
        host.run();
        assert_eq!(host.replies[0], host.replies[1]);
        assert_ne!(host.replies[1]["epoch"], host.replies[2]["epoch"]);
        assert_eq!(host.revision, 2);
    }
    #[test]
    fn retired_epoch_cannot_be_reopened_by_replaying_its_open_id() {
        let mut host = Host::default();
        let open = json!({"operation":"open_epoch","operation_id":"old"});
        host.request(1, "alice", open.clone());
        host.run();
        let epoch = host.replies[0]["epoch"].as_str().unwrap().to_owned();
        host.request(2, "alice", json!({"operation":"close_epoch","epoch":epoch}));
        host.run();
        host.request(
            3,
            "alice",
            json!({"operation":"retire_epoch","epoch":epoch}),
        );
        host.run();
        host.request(4, "alice", open);
        host.request(
            5,
            "alice",
            json!({"operation":"open_epoch","operation_id":"new"}),
        );
        host.run();
        assert_eq!(host.replies[1]["status"], "ok");
        assert_eq!(host.replies[2]["status"], "ok");
        assert_eq!(host.replies[3]["code"], "receipt_expired");
        assert_ne!(host.replies[4]["epoch"], epoch);
        assert_eq!(host.metadata["active"], 1);
    }
    #[test]
    fn dispatch_requires_consumed_permit_and_durable_running_record() {
        for revoked in [false, true] {
            let mut host = Host {
                allow_permit: true,
                revoke_consume: revoked,
                ..Host::default()
            };
            host.request(
                1,
                "alice",
                json!({"operation":"open_epoch","operation_id":"open"}),
            );
            host.run();
            let epoch = host.replies[0]["epoch"].as_str().unwrap().to_owned();
            host.request(2, "alice", json!({"operation":"invoke","epoch":epoch,"sequence":1,"operation_id":format!("{epoch}:1"),"action_id":"example/action","target":{"kind":"session","host_epoch":"h","session_id":"s","expected_generation":1},"arguments":{},"expected_settings_revision":null}));
            host.run();
            host.request(
                3,
                "alice",
                json!({"operation":"status","epoch":epoch,"sequence":1}),
            );
            host.run();
            assert_eq!(host.effects, usize::from(!revoked));
            assert_eq!(
                host.replies[2]["status"],
                if revoked {
                    "recovery_required"
                } else {
                    "succeeded"
                }
            );
        }
    }
    #[test]
    fn startup_persists_recovery_for_interrupted_effect_before_admission() {
        let mut host = Host {
            allow_permit: true,
            ..Host::default()
        };
        host.request(
            1,
            "alice",
            json!({"operation":"open_epoch","operation_id":"open"}),
        );
        host.run();
        let epoch = host.replies[0]["epoch"].as_str().unwrap().to_owned();
        host.request(2, "alice", json!({"operation":"invoke","epoch":epoch,"sequence":1,"operation_id":format!("{epoch}:1"),"action_id":"example/action","target":{"kind":"session","host_epoch":"h","session_id":"s","expected_generation":1},"arguments":{},"expected_settings_revision":null}));
        host.run();
        let record = &mut host.state.get_mut(&format!("epoch/{epoch}")).unwrap()["receipts"]["1"];
        record["dispatch"]["stage"] =
            json!({"phase":"running","plan":{},"digest":"digest","permit":"permit"});
        record["receipt"]["status"] = json!("running");
        host.effects = 0;
        host.request(
            3,
            "alice",
            json!({"operation":"status","epoch":epoch,"sequence":1}),
        );
        host.run();
        assert_eq!(host.replies[2]["status"], "recovery_required");
        assert_eq!(
            host.state[&format!("epoch/{epoch}")]["receipts"]["1"]["receipt"]["status"],
            "recovery_required"
        );
        assert_eq!(host.effects, 0);
    }
    #[test]
    fn cancellation_is_persisted_then_requested_without_claiming_retirement() {
        let mut host = Host {
            allow_permit: true,
            provider_pending: true,
            ..Host::default()
        };
        host.request(
            1,
            "alice",
            json!({"operation":"open_epoch","operation_id":"open"}),
        );
        host.run();
        let epoch = host.replies[0]["epoch"].as_str().unwrap().to_owned();
        host.request(2, "alice", json!({"operation":"invoke","epoch":epoch,"sequence":1,"operation_id":format!("{epoch}:1"),"action_id":"example/action","target":{"kind":"session","host_epoch":"h","session_id":"s","expected_generation":1},"arguments":{},"expected_settings_revision":null}));
        let host_ptr: *mut Host = &mut host;
        let endpoint = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: host_ptr.cast(),
            invoke,
        };
        // SAFETY: the host remains alive and test accesses are serialized between ticks.
        let client = unsafe { RuntimeClient::from_endpoint(endpoint) };
        let mut module = ActionsModule::start(client).unwrap();
        for _ in 0..300 {
            module.tick().unwrap();
            if host.effects > 0 {
                break;
            }
        }
        host.request(
            3,
            "alice",
            json!({"operation":"cancel","epoch":epoch,"sequence":1}),
        );
        for _ in 0..500 {
            module.tick().unwrap();
            if host.replies.len() >= 3 && host.cancellation_requests > 0 {
                break;
            }
        }
        drop(module);
        assert_eq!(host.effects, 1);
        assert_eq!(host.replies[2]["cancellation_requested"], true);
        assert_eq!(host.replies[2]["status"], "running");
        assert!(host.cancellation_requests > 0);
        assert_eq!(
            host.state[&format!("epoch/{epoch}")]["receipts"]["1"]["receipt"]["cancellation_requested"],
            true
        );
    }
    #[test]
    fn mutation_waits_until_lifecycle_confirms_retirement() {
        for pending in [usize::MAX, 2] {
            let mut host = Host {
                allow_permit: true,
                mutation_descriptor: true,
                transition_pending: pending,
                ..Host::default()
            };
            host.request(
                1,
                "alice",
                json!({"operation":"open_epoch","operation_id":"open"}),
            );
            host.run();
            let epoch = host.replies[0]["epoch"].as_str().unwrap().to_owned();
            host.request(2, "alice", json!({"operation":"invoke","epoch":epoch,"sequence":1,"operation_id":format!("{epoch}:1"),"action_id":"example/action","target":{"kind":"session","host_epoch":"h","session_id":"s","expected_generation":1},"arguments":{},"expected_settings_revision":null}));
            host.run();
            assert_eq!(host.replies[1]["status"], "queued");
            assert!(host.transition_count > 0);
            assert_eq!(host.effects, usize::from(pending != usize::MAX));
        }
    }
    #[test]
    fn unavailable_required_capability_rejects_before_planning() {
        let mut host = Host {
            allow_permit: true,
            required_capability: true,
            ..Host::default()
        };
        host.request(
            1,
            "alice",
            json!({"operation":"open_epoch","operation_id":"open"}),
        );
        host.run();
        let epoch = host.replies[0]["epoch"].as_str().unwrap().to_owned();
        host.request(2, "alice", json!({"operation":"invoke","epoch":epoch,"sequence":1,"operation_id":format!("{epoch}:1"),"action_id":"example/action","target":{"kind":"session","host_epoch":"h","session_id":"s","expected_generation":1},"arguments":{},"expected_settings_revision":null}));
        host.run();
        assert_eq!(host.replies[1]["code"], "capability_unavailable");
        assert_eq!(host.effects, 0);
    }
    #[test]
    fn rolling_watermark_expires_old_receipts_without_reexecution() {
        let mut host = Host {
            allow_permit: true,
            ..Host::default()
        };
        host.request(
            1,
            "alice",
            json!({"operation":"open_epoch","operation_id":"open"}),
        );
        host.run();
        let epoch = host.replies[0]["epoch"].as_str().unwrap().to_owned();
        for sequence in 1..=65 {
            host.request(sequence + 1, "alice", json!({"operation":"invoke","epoch":epoch,"sequence":sequence,"operation_id":format!("{epoch}:{sequence}"),"action_id":"example/action","target":{"kind":"session","host_epoch":"h","session_id":"s","expected_generation":1},"arguments":{},"expected_settings_revision":null}));
            host.run();
            assert_eq!(
                host.replies.last().unwrap()["status"],
                "queued",
                "sequence {sequence}: {:?}",
                host.replies.last()
            );
        }
        assert_eq!(host.effects, 65);
        host.request(
            100,
            "alice",
            json!({"operation":"status","epoch":epoch,"sequence":1}),
        );
        host.request(101, "alice", json!({"operation":"invoke","epoch":epoch,"sequence":1,"operation_id":format!("{epoch}:1"),"action_id":"example/action","target":{"kind":"session","host_epoch":"h","session_id":"s","expected_generation":1},"arguments":{},"expected_settings_revision":null}));
        host.run();
        assert_eq!(host.replies[66]["code"], "receipt_expired");
        assert_eq!(host.replies[67]["code"], "receipt_expired");
        assert_eq!(host.effects, 65);
    }
    #[test]
    fn cancelling_reserved_input_is_durable_before_revocation_and_blocks_commit() {
        let mut host = Host {
            allow_permit: true,
            mutation_descriptor: true,
            delivery_confirmed: true,
            ..Host::default()
        };
        host.request_input(
            1,
            json!({"operation":"open_epoch","operation_id":"input-open"}),
        );
        host.run();
        let epoch = host.replies[0]["epoch"].as_str().unwrap().to_owned();
        let operation_id = format!("{epoch}:1");
        host.request_input(2, json!({"operation":"reserve_input","epoch":epoch,"sequence":1,"operation_id":operation_id,"action_id":"example/action","target":{"kind":"session","host_epoch":"h","session_id":"s","expected_generation":1},"arguments":{},"expected_settings_revision":null,"input_reservation_id":"reservation"}));
        host.run();
        host.request_input(3, json!({"operation":"cancel","epoch":epoch,"sequence":1}));
        host.run();
        assert_eq!(host.replies[2]["status"], "cancelled");
        assert_eq!(host.cancellation_requests, 1);
        host.request_input(4, json!({"operation":"commit_input","epoch":epoch,"sequence":1,"operation_id":operation_id}));
        host.run();
        assert_eq!(host.replies[3]["status"], "cancelled");
        assert_eq!(host.effects, 0);
        host.request_input(5, json!({"operation":"cancel","epoch":epoch,"sequence":1}));
        host.run();
        assert_eq!(host.replies[4]["status"], "cancelled");
        assert_eq!(host.cancellation_requests, 2);
    }
    #[test]
    fn input_reservation_waits_for_independent_delivery_proof() {
        let mut host = Host {
            allow_permit: true,
            mutation_descriptor: true,
            transition_pending: 1,
            ..Host::default()
        };
        host.request_input(
            1,
            json!({"operation":"open_epoch","operation_id":"input-open"}),
        );
        host.run();
        let epoch = host.replies[0]["epoch"].as_str().unwrap().to_owned();
        let operation_id = format!("{epoch}:1");
        host.request_input(2, json!({"operation":"reserve_input","epoch":epoch,"sequence":1,"operation_id":operation_id,"action_id":"example/action","target":{"kind":"session","host_epoch":"h","session_id":"s","expected_generation":1},"arguments":{},"expected_settings_revision":null,"input_reservation_id":"reservation"}));
        host.run();
        assert_eq!(host.replies[1]["status"], "reserved");
        assert_eq!(host.effects, 0);
        host.request_input(3, json!({"operation":"commit_input","epoch":epoch,"sequence":1,"operation_id":operation_id}));
        host.run();
        assert_eq!(host.replies[2]["code"], "input_delivery_unconfirmed");
        assert_eq!(host.effects, 0);
        host.delivery_confirmed = true;
        host.request_input(4, json!({"operation":"commit_input","epoch":epoch,"sequence":1,"operation_id":operation_id}));
        host.run();
        assert_eq!(host.replies[3]["status"], "queued");
        assert_eq!(host.effects, 1);
        assert_eq!(
            host.last_source,
            json!({"caller":"stream-session","initiator":"paired-client"})
        );
        assert!(host.transition_count >= 2);
    }
}
