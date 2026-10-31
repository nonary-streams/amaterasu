//! Serialized state-backed action receipt admission. Native dispatch remains closed.
use amaterasu_extension_api::{ActionInvoke, EXTENSION_ACTIONS_SERVICE};
use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};

const STATE: &str = "amaterasu.state/1";
const COLLECTION: &str = "extension-actions-v1";
const MAX_EPOCHS: u64 = 64;
const MAX_EPOCH_BYTES: usize = 8_000;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Epoch {
    caller: String,
    initiator: String,
    open_index: String,
    closed: bool,
    receipts: BTreeMap<u64, Record>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    request: ActionInvoke,
    receipt: Value,
    selected_provider: String,
    registry_revision: u64,
    descriptor: amaterasu_extension_api::ActionDescriptor,
    dispatch: Option<crate::dispatch::Dispatch>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    #[serde(default)]
    grants: Vec<Grant>,
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
        self.grants.len() <= 128
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
}
enum Stage {
    Read(Incoming),
    Write {
        parent: u64,
        result: Value,
    },
    Descriptor {
        incoming: Incoming,
        revision: u64,
        metadata: Value,
        epoch: Box<Epoch>,
        invoke: Box<ActionInvoke>,
    },
    Plan(Box<PlanContext>),
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
struct Pending {
    request: u64,
    stage: Stage,
}

pub struct ActionsModule {
    runtime: RuntimeClient,
    pending: VecDeque<Pending>,
    policy: Policy,
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
        runtime.require_success(Command::Register {
            service: EXTENSION_ACTIONS_SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            pending: VecDeque::new(),
            policy,
        })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        if let Some(pending) = self.pending.pop_front() {
            self.advance(pending)?;
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
        if !matches!(
            operation,
            "open_epoch"
                | "invoke"
                | "status"
                | "lookup"
                | "cancel"
                | "close_epoch"
                | "retire_epoch"
        ) {
            return self.complete(incoming.id, failure("invalid_request"));
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
            Stage::Write { parent, .. } => *parent,
            Stage::Descriptor { incoming, .. } => incoming.id,
            Stage::Plan(context) => context.incoming.id,
        };
        let descriptor = matches!(stage, Stage::Descriptor { .. });
        let phase = match &stage {
            Stage::Read(_) => "read",
            Stage::Write { .. } => "write",
            Stage::Descriptor { .. } => "descriptor",
            Stage::Plan(_) => "plan",
        };
        match self.runtime.send(Command::Request {
            service: if descriptor {
                amaterasu_extension_api::EXTENSION_REGISTRY_SERVICE.into()
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
                    Stage::Write { parent, result } => {
                        if payload.get("ok") == Some(&Value::Bool(true)) {
                            self.complete(parent, result)
                        } else {
                            self.complete(parent, failure("state_commit_failed"))
                        }
                    }
                }
            }
            _ => {
                let parent = match pending.stage {
                    Stage::Read(incoming) => incoming.id,
                    Stage::Write { parent, .. } => parent,
                    Stage::Descriptor { incoming, .. } => incoming.id,
                    Stage::Plan(context) => context.incoming.id,
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
                receipts: BTreeMap::new(),
            };
            let metadata = json!({"active":active + 1});
            let payload = json!({"action":"collection_commit","collection":COLLECTION,"expected_revision":revision,"metadata":metadata,"edits":[{"key":format!("epoch/{epoch}"),"value":record},{"key":index,"value":{"epoch":epoch}}]});
            return self.submit(
                payload,
                Stage::Write {
                    parent: incoming.id,
                    result: json!({"status":"ok","epoch":epoch}),
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
                    entry.receipt.get("status").and_then(Value::as_str) != Some("rejected")
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
        if sequence == 0 || sequence > 64 {
            return self.complete(incoming.id, failure("sequence_window"));
        }
        if operation != "invoke" {
            let result = epoch
                .receipts
                .get(&sequence)
                .map(|record| record.receipt.clone())
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
        if invoke.input_reservation_id.is_some() {
            return self.complete(incoming.id, failure("input_reservation_unavailable"));
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
                    record.receipt.clone()
                } else {
                    failure("operation_conflict")
                },
            );
        }
        if epoch.closed {
            return self.complete(incoming.id, failure("epoch_closed"));
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
        if dispatch
            .accept_plan(response.get("plan").cloned().unwrap_or(Value::Null))
            .is_err()
        {
            return self.complete(incoming.id, failure("invalid_plan"));
        }
        let receipt = json!({"status":"rejected","receipt_id":invoke.operation_id,"operation_id":invoke.operation_id,"result":null,"reason":"lifecycle_permit_unavailable"});
        let epoch_id = invoke.epoch.clone();
        epoch.receipts.insert(
            invoke.sequence,
            Record {
                request: invoke,
                receipt: receipt.clone(),
                selected_provider: provider,
                registry_revision,
                descriptor: action,
                dispatch: Some(dispatch),
            },
        );
        self.write(incoming.id, revision, metadata, &epoch_id, &epoch, receipt)
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
        let value = serde_json::to_value(record).map_err(|error| error.to_string())?;
        if serde_json::to_vec(&value)
            .map_err(|error| error.to_string())?
            .len()
            > MAX_EPOCH_BYTES
        {
            return self.complete(parent, failure("capacity"));
        }
        let payload = json!({"action":"collection_commit","collection":COLLECTION,"expected_revision":revision,"metadata":metadata,"edits":[{"key":format!("epoch/{epoch}"),"value":value}]});
        self.submit(payload, Stage::Write { parent, result })
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
    }
    impl Host {
        fn dispatch(&mut self, command: Command) -> Response {
            match command {
                Command::Register { .. } | Command::ReleaseResult { .. } => Response::Ok,
                Command::GetSettings {} => Response::Result {
                    payload: json!({"grants":[{"principal":"management","action_ids":["example/action"],"target_kinds":["session"],"execution_classes":["session_mutation"]},{"principal":"alice","action_ids":["example/action"],"target_kinds":["session"],"execution_classes":["session_mutation"]},{"principal":"bob","action_ids":["example/action"],"target_kinds":["session"],"execution_classes":["session_mutation"]}]}),
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
                        json!({"ok":true,"provider":"example.provider","revision":1,"action":{"id":"example/action","label":"Example","description":"Example","execution":"session_mutation","arguments":{},"allowed_targets":["session"],"required_capabilities":[],"plan":method,"execute":method,"status":null}})
                    } else {
                        match payload["action"].as_str() {
                            Some("collection_get") => {
                                let value = payload["key"]
                                    .as_str()
                                    .and_then(|key| self.state.get(key))
                                    .cloned();
                                json!({"ok":true,"revision":self.revision,"count":self.state.len(),"metadata":self.metadata,"value":value})
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
                    payload, parent, ..
                } => {
                    assert!(parent.is_some());
                    self.next += 1;
                    self.results.insert(self.next, json!({"status":"planned","operation_id":payload["operation_id"],"action_id":payload["action_id"],"target":payload["target"],"plan":{"mutation_domains":[]}}));
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
        fn run(&mut self) {
            let endpoint = HostApi {
                header: AbiHeader::of::<HostApi>(),
                context: (self as *mut Self).cast(),
                invoke,
            };
            // SAFETY: the host outlives the module and is used only through this endpoint.
            let client = unsafe { RuntimeClient::from_endpoint(endpoint) };
            let mut module = ActionsModule::start(client).unwrap();
            for _ in 0..20 {
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
        assert_eq!(host.replies[1]["status"], "rejected");
        assert_eq!(host.replies[1]["reason"], "lifecycle_permit_unavailable");
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
        assert_eq!(host.replies[2], host.replies[1]);
        assert_eq!(host.replies[3]["code"], "unknown_epoch");
        assert_eq!(host.replies[4]["code"], "operation_conflict");
        assert_eq!(host.revision, 2);
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
}
