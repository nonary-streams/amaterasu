//! Serialized state-backed action receipt admission. Native dispatch remains closed.
use amaterasu_extension_api::{ActionInvoke, EXTENSION_ACTIONS_SERVICE};
use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const STATE: &str = "amaterasu.state/1";
const COLLECTION: &str = "extension-actions-v1";
const MAX_EPOCHS: u64 = 64;
const MAX_EPOCH_BYTES: usize = 8_000;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Epoch {
    caller: String,
    initiator: String,
    closed: bool,
    receipts: BTreeMap<u64, Record>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    request: ActionInvoke,
    receipt: Value,
}

struct Incoming {
    id: u64,
    caller: String,
    initiator: String,
    payload: Value,
}
enum Stage {
    Read(Incoming),
    Write { parent: u64, result: Value },
}
struct Pending {
    request: u64,
    stage: Stage,
}

pub struct ActionsModule {
    runtime: RuntimeClient,
    pending: Option<Pending>,
}
impl ManagedModule for ActionsModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        runtime.require_success(Command::Register {
            service: EXTENSION_ACTIONS_SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            pending: None,
        })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        if let Some(pending) = self.pending.take() {
            return self.advance(pending);
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
        let operation = incoming
            .payload
            .get("operation")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !matches!(
            operation,
            "open_epoch" | "invoke" | "status" | "lookup" | "cancel" | "close_epoch"
        ) {
            return self.complete(incoming.id, failure("invalid_request"));
        }
        let key = if operation == "open_epoch" {
            None
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
        };
        match self.runtime.send(Command::Request {
            service: STATE.into(),
            payload,
            timeout_ms: 5_000,
            operation_id: format!(
                "actions-state-{parent}-{}",
                if matches!(stage, Stage::Read(_)) {
                    "read"
                } else {
                    "write"
                }
            ),
            parent: None,
        })? {
            Response::Accepted { id } => {
                self.pending = Some(Pending { request: id, stage });
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
                self.pending = Some(pending);
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
            if count >= MAX_EPOCHS {
                return self.complete(incoming.id, failure("capacity"));
            }
            let mut bytes = [0_u8; 16];
            if getrandom::fill(&mut bytes).is_err() {
                return self.complete(incoming.id, failure("entropy"));
            }
            let epoch: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
            let record = Epoch {
                caller: incoming.caller,
                initiator: incoming.initiator,
                closed: false,
                receipts: BTreeMap::new(),
            };
            return self.write(
                incoming.id,
                revision,
                metadata,
                &epoch,
                &record,
                json!({"status":"ok","epoch":epoch}),
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
        let receipt = json!({"status":"rejected","receipt_id":invoke.operation_id,"operation_id":invoke.operation_id,"result":null,"reason":"action_dispatch_unavailable"});
        epoch.receipts.insert(
            sequence,
            Record {
                request: invoke,
                receipt: receipt.clone(),
            },
        );
        self.write(incoming.id, revision, metadata, epoch_id, &epoch, receipt)
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
    }
    impl Host {
        fn dispatch(&mut self, command: Command) -> Response {
            match command {
                Command::Register { .. } | Command::ReleaseResult { .. } => Response::Ok,
                Command::Poll {} => self.incoming.pop_front().unwrap_or(Response::Pending),
                Command::Complete { payload, .. } => {
                    self.replies.push(payload);
                    Response::Ok
                }
                Command::Request { payload, .. } => {
                    self.next += 1;
                    let value = match payload["action"].as_str() {
                        Some("collection_get") => {
                            let value = payload["key"]
                                .as_str()
                                .and_then(|key| self.state.get(key))
                                .cloned();
                            json!({"ok":true,"revision":self.revision,"count":self.state.len(),"metadata":null,"value":value})
                        }
                        Some("collection_commit")
                            if payload["expected_revision"] == self.revision =>
                        {
                            for edit in payload["edits"].as_array().unwrap() {
                                self.state.insert(
                                    edit["key"].as_str().unwrap().into(),
                                    edit["value"].clone(),
                                );
                            }
                            self.revision += 1;
                            json!({"ok":true,"revision":self.revision})
                        }
                        _ => json!({"ok":false}),
                    };
                    self.results.insert(self.next, value);
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
        host.request(1, "alice", json!({"operation":"open_epoch"}));
        host.run();
        let epoch = host.replies[0]["epoch"].as_str().unwrap().to_owned();
        let invoke = json!({"operation":"invoke","epoch":epoch,"sequence":1,"operation_id":format!("{epoch}:1"),"action_id":"example/action","target":{"kind":"session","host_epoch":"h","session_id":"s","expected_generation":1},"arguments":{},"expected_settings_revision":null});
        host.request(2, "alice", invoke.clone());
        host.run();
        assert_eq!(host.replies[1]["status"], "rejected");
        assert_eq!(host.replies[1]["reason"], "action_dispatch_unavailable");
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
}
