//! Independently built, read-only replacement for amaterasu.library/1.
//! The static data is a demonstration, not a launchable application catalog.
use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

const SERVICE: &str = "amaterasu.library/1";
const REVISION: u64 = 1;

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    /// Exact immediate callers permitted to read this static demonstration.
    allowed_callers: BTreeSet<String>,
    /// Forward only sanitized read operations through the configured catalog.
    catalog_backed: bool,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    List {
        cursor: Option<Cursor>,
        limit: Option<usize>,
    },
    Resolve {
        id: String,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    revision: u64,
    after: String,
    query: String,
}

fn reply(settings: &Settings, caller: &str, initiator: &str, payload: Value) -> Value {
    // Only the broker supplies these values. A delegated call is deliberately
    // rejected; this example does not implement delegated owner policy.
    if caller != initiator || !settings.allowed_callers.contains(caller) {
        return error("unauthorized", "caller is not allowed");
    }
    let Ok(request) = serde_json::from_value::<Request>(payload) else {
        return error("invalid_request", "invalid library request");
    };
    match request {
        Request::List { cursor, limit } => {
            let limit = limit.unwrap_or(20);
            if !(1..=32).contains(&limit) {
                return error("invalid_request", "page limit must be 1..32");
            }
            // A one-entry catalog never emits a continuation. Reject fabricated
            // or stale cursors rather than silently accepting inconsistent pages.
            if let Some(cursor) = cursor {
                let _ = (cursor.revision, cursor.after, cursor.query);
                return error("invalid_cursor", "no continuation is available");
            }
            json!({"ok":true,"revision":REVISION,"entries":[{"id":"example-game","name":"Example Game"}],"next_cursor":null})
        }
        Request::Resolve { id } if id == "example-game" => {
            json!({"ok":true,"revision":REVISION,"id":id,"name":"Example Game"})
        }
        Request::Resolve { .. } => error("not_found", "application not found"),
    }
}

fn error(code: &str, message: &str) -> Value {
    json!({"ok":false,"error":{"code":code,"message":message}})
}

fn valid_id(value: &str) -> bool {
    value.len() == 20
        && value.starts_with("app-")
        && value[4..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && value[4..].bytes().any(|byte| byte != b'0')
}

/// Keep paths and any unknown catalog data outside the public read facade.
fn project_catalog(payload: Value, resolve: Option<&str>, limit: usize) -> Value {
    if payload["ok"] == false {
        return error("catalog_error", "catalog rejected request");
    }
    let Some(revision) = payload["revision"].as_u64() else {
        return error("invalid_catalog_response", "catalog response is invalid");
    };
    let valid_name = |value: &str| {
        !value.is_empty() && value.len() <= 160 && !value.chars().any(char::is_control)
    };
    if let Some(expected) = resolve {
        if payload["ok"] == true
            && payload["id"] == expected
            && let Some(name) = payload["name"].as_str()
            && valid_name(name)
        {
            return json!({"ok":true,"revision":revision,"id":expected,"name":name});
        }
    } else if payload["ok"] == true
        && let Some(entries) = payload["entries"].as_array()
        && entries.len() <= limit
    {
        let rows: Option<Vec<Value>> = entries
            .iter()
            .map(|entry| {
                let id = entry["id"].as_str()?;
                let name = entry["name"].as_str()?;
                (valid_id(id) && valid_name(name)).then(|| json!({"id":id,"name":name}))
            })
            .collect();
        if let (Some(rows), Some(cursor)) = (rows, payload.get("next_cursor")) {
            let valid_cursor = cursor.is_null()
                || serde_json::from_value::<Cursor>(cursor.clone()).is_ok_and(|c| {
                    c.revision == revision && valid_id(&c.after) && c.query.is_empty()
                });
            if valid_cursor {
                return json!({"ok":true,"revision":revision,"entries":rows,"next_cursor":cursor});
            }
        }
    }
    error("invalid_catalog_response", "catalog response is invalid")
}

struct Admission {
    parent: u64,
    command: Command,
    resolve: Option<String>,
    limit: usize,
}
struct Pending {
    parent: u64,
    child: Option<u64>,
    resolve: Option<String>,
    limit: usize,
    result: Option<Value>,
    released: bool,
}

struct Replacement {
    runtime: RuntimeClient,
    settings: Settings,
    admission: Option<Admission>,
    pending: Option<Pending>,
}

impl Replacement {
    fn advance_admission(&mut self, admission: Admission) {
        match self.runtime.send(admission.command.clone()) {
            Ok(Response::Accepted { id }) => {
                self.pending = Some(Pending {
                    parent: admission.parent,
                    child: Some(id),
                    resolve: admission.resolve,
                    limit: admission.limit,
                    result: None,
                    released: false,
                })
            }
            Err(_) => self.admission = Some(admission),
            _ => {
                self.pending = Some(Pending {
                    parent: admission.parent,
                    child: None,
                    resolve: admission.resolve,
                    limit: admission.limit,
                    result: Some(error("catalog_unavailable", "catalog request not admitted")),
                    released: true,
                })
            }
        }
    }

    fn advance_pending(&mut self, mut pending: Pending) {
        if pending.result.is_none() {
            let child = pending.child.expect("pending catalog result has child");
            match self.runtime.send(Command::TakeResult { request: child }) {
                Ok(Response::Result { payload }) => {
                    pending.result = Some(project_catalog(
                        payload,
                        pending.resolve.as_deref(),
                        pending.limit,
                    ))
                }
                Ok(Response::Pending) | Err(_) => {
                    self.pending = Some(pending);
                    return;
                }
                _ => pending.result = Some(error("catalog_unavailable", "catalog request failed")),
            }
        }
        if !pending.released {
            let child = pending.child.expect("unreleased catalog result has child");
            match self.runtime.send(Command::ReleaseResult { request: child }) {
                Ok(Response::Ok) => pending.released = true,
                Ok(Response::Error { code, .. })
                    if matches!(code.as_str(), "not_found" | "request_ended" | "cancelled") =>
                {
                    pending.released = true
                }
                _ => {
                    self.pending = Some(pending);
                    return;
                }
            }
        }
        let payload = pending
            .result
            .as_ref()
            .expect("catalog result retained")
            .clone();
        match self.runtime.send(Command::Complete {
            request: pending.parent,
            payload,
        }) {
            Ok(Response::Ok) => {}
            Ok(Response::Error { code, .. })
                if matches!(code.as_str(), "not_found" | "request_ended" | "cancelled") => {}
            _ => self.pending = Some(pending),
        }
    }
}

impl ManagedModule for Replacement {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("replacement settings unavailable".into());
        };
        let settings: Settings = serde_json::from_value(payload).map_err(|e| e.to_string())?;
        if settings.allowed_callers.len() > 16
            || settings.allowed_callers.iter().any(|caller| {
                caller.is_empty()
                    || caller.len() > 128
                    || !caller.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
                    })
            })
        {
            return Err("invalid caller settings".into());
        }
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            settings,
            admission: None,
            pending: None,
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        if let Some(admission) = self.admission.take() {
            self.advance_admission(admission);
            return Ok(());
        }
        if let Some(pending) = self.pending.take() {
            self.advance_pending(pending);
            return Ok(());
        }
        if let Some(request) = self.runtime.poll_serialized()? {
            if !self.settings.catalog_backed {
                let response = if request.service == SERVICE {
                    reply(
                        &self.settings,
                        &request.caller,
                        &request.initiator,
                        request.payload,
                    )
                } else {
                    error("invalid_request", "unknown service")
                };
                return self.runtime.complete_serialized(request.id, &response);
            }
            if request.service != SERVICE
                || request.caller != request.initiator
                || !self.settings.allowed_callers.contains(&request.caller)
            {
                return self.runtime.complete_serialized(
                    request.id,
                    &error("unauthorized", "caller is not allowed"),
                );
            }
            let Ok(action) = request.decode::<Request>() else {
                return self.runtime.complete_serialized(
                    request.id,
                    &error("invalid_request", "invalid library request"),
                );
            };
            let (payload, resolve, limit) = match action {
                Request::List { cursor, limit }
                    if limit.is_none_or(|limit| (1..=32).contains(&limit))
                        && cursor.as_ref().is_none_or(|cursor| {
                            valid_id(&cursor.after) && cursor.query.is_empty()
                        }) =>
                {
                    (
                        json!({"action":"export_list","cursor":cursor,"limit":limit}),
                        None,
                        limit.unwrap_or(20),
                    )
                }
                Request::Resolve { id } if valid_id(&id) => {
                    (json!({"action":"resolve_exported","id":id}), Some(id), 1)
                }
                _ => {
                    return self.runtime.complete_serialized(
                        request.id,
                        &error("invalid_request", "invalid catalog query"),
                    );
                }
            };
            let command = Command::Request {
                service: "amaterasu.catalog/1".into(),
                payload,
                timeout_ms: request.remaining_ms.min(30_000),
                operation_id: format!("replacement.{}", request.id),
                parent: Some(request.id),
            };
            self.advance_admission(Admission {
                parent: request.id,
                command,
                resolve,
                limit,
            });
        }
        Ok(())
    }
}

amaterasu_module_sdk::export_module!(Replacement);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_and_handles_a_broker_request_through_sdk_transport() {
        use amaterasu_module_sdk::abi::{AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_OK};
        use std::{cell::RefCell, collections::VecDeque, ffi::c_void};

        struct Host {
            commands: RefCell<Vec<Value>>,
            replies: RefCell<VecDeque<Value>>,
        }
        unsafe extern "C" fn invoke(
            context: *mut c_void,
            input: BorrowedBytes,
            sink: ReplySink,
        ) -> i32 {
            // SAFETY: fixture and input are live for this synchronous host call.
            let host = unsafe { &*context.cast::<Host>() };
            // SAFETY: the SDK supplies readable request bytes for this call.
            let command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
            host.commands.borrow_mut().push(command);
            let bytes =
                serde_json::to_vec(&host.replies.borrow_mut().pop_front().unwrap()).unwrap();
            // SAFETY: SDK reply sink is valid for the synchronous invoke.
            unsafe { sink.send(&bytes) };
            STATUS_OK
        }
        let host = Host {
            commands: RefCell::new(Vec::new()),
            replies: RefCell::new(VecDeque::from([
                json!({"kind":"result","payload":{"allowed_callers":["consumer"]}}),
                json!({"kind":"ok"}),
                json!({"kind":"request","id":7,"caller":"consumer","initiator":"consumer",
                    "service":SERVICE,"payload":{"action":"resolve","id":"example-game"},"remaining_ms":100}),
                json!({"kind":"ok"}),
            ])),
        };
        let endpoint = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: (&host as *const Host).cast_mut().cast(),
            invoke,
        };
        // SAFETY: host fixture remains live until after the module is dropped.
        let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
        let mut module = Replacement::start(runtime).unwrap();
        module.tick().unwrap();
        let commands = host.commands.borrow();
        assert_eq!(commands[1]["service"], SERVICE);
        assert_eq!(commands[3]["request"], 7);
        assert_eq!(commands[3]["payload"]["name"], "Example Game");
    }

    #[test]
    fn catalog_proxy_forwards_attested_parent_and_sanitizes_result() {
        use amaterasu_module_sdk::abi::{AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_OK};
        use std::{cell::RefCell, collections::VecDeque, ffi::c_void};
        struct Host {
            commands: RefCell<Vec<Value>>,
            replies: RefCell<VecDeque<Value>>,
        }
        unsafe extern "C" fn invoke(
            context: *mut c_void,
            input: BorrowedBytes,
            sink: ReplySink,
        ) -> i32 {
            // SAFETY: fixture and input are valid for this synchronous call.
            let host = unsafe { &*context.cast::<Host>() };
            // SAFETY: SDK request bytes remain readable during invoke.
            host.commands
                .borrow_mut()
                .push(serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap());
            let bytes =
                serde_json::to_vec(&host.replies.borrow_mut().pop_front().unwrap()).unwrap();
            // SAFETY: SDK reply callback remains live until invoke returns.
            unsafe { sink.send(&bytes) };
            STATUS_OK
        }
        let host = Host {
            commands: RefCell::new(vec![]),
            replies: RefCell::new(VecDeque::from([
                json!({"kind":"result","payload":{"allowed_callers":["consumer"],"catalog_backed":true}}),
                json!({"kind":"ok"}),
                json!({"kind":"request","id":7,"caller":"consumer","initiator":"consumer","service":SERVICE,"payload":{"action":"list","limit":1},"remaining_ms":100}),
                json!({"kind":"accepted","id":8}),
                json!({"kind":"result","payload":{"ok":true,"revision":1,"entries":[{"id":"app-0000000000000001","name":"Game","path":"private"}],"next_cursor":null}}),
                json!({"kind":"ok"}),
                json!({"kind":"ok"}),
            ])),
        };
        let endpoint = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: (&host as *const Host).cast_mut().cast(),
            invoke,
        };
        // SAFETY: fixture outlives this client and all module calls.
        let mut module =
            Replacement::start(unsafe { RuntimeClient::from_endpoint(endpoint) }).unwrap();
        module.tick().unwrap();
        module.tick().unwrap();
        let commands = host.commands.borrow();
        assert_eq!(commands[3]["service"], "amaterasu.catalog/1");
        assert_eq!(commands[3]["parent"], 7);
        assert_eq!(commands[3]["payload"]["action"], "export_list");
        assert_eq!(
            commands[6]["payload"]["entries"][0],
            json!({"id":"app-0000000000000001","name":"Game"})
        );
        assert!(
            project_catalog(
                json!({"ok":true,"revision":1,"id":"app-0000000000000002","name":"Game"}),
                Some("app-0000000000000001"),
                1
            )["ok"]
                == false
        );
    }

    #[test]
    fn catalog_read_retries_lost_replies_and_finishes_after_parent_ended() {
        use amaterasu_module_sdk::abi::{
            AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_INTERNAL_ERROR, STATUS_OK,
        };
        use std::{cell::RefCell, collections::VecDeque, ffi::c_void};
        struct Host {
            commands: RefCell<Vec<Value>>,
            replies: RefCell<VecDeque<Option<Value>>>,
        }
        unsafe extern "C" fn invoke(
            context: *mut c_void,
            input: BorrowedBytes,
            sink: ReplySink,
        ) -> i32 {
            // SAFETY: fixture and request are live during this synchronous call.
            let host = unsafe { &*context.cast::<Host>() };
            // SAFETY: SDK supplies readable input until invoke returns.
            host.commands
                .borrow_mut()
                .push(serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap());
            let Some(reply) = host.replies.borrow_mut().pop_front().unwrap() else {
                return STATUS_INTERNAL_ERROR;
            };
            let bytes = serde_json::to_vec(&reply).unwrap();
            // SAFETY: reply sink remains live through this call.
            unsafe { sink.send(&bytes) };
            STATUS_OK
        }
        let host = Host {
            commands: RefCell::new(vec![]),
            replies: RefCell::new(VecDeque::from([
                Some(
                    json!({"kind":"result","payload":{"allowed_callers":["consumer"],"catalog_backed":true}}),
                ),
                Some(json!({"kind":"ok"})),
                Some(
                    json!({"kind":"request","id":7,"caller":"consumer","initiator":"consumer","service":SERVICE,"payload":{"action":"list","limit":1},"remaining_ms":100}),
                ),
                None,
                Some(json!({"kind":"accepted","id":8})),
                None,
                Some(
                    json!({"kind":"result","payload":{"ok":true,"revision":1,"entries":[{"id":"app-0000000000000001","name":"Game"}],"next_cursor":null}}),
                ),
                None,
                Some(json!({"kind":"error","code":"not_found","message":"released"})),
                None,
                Some(json!({"kind":"error","code":"request_ended","message":"parent ended"})),
            ])),
        };
        let endpoint = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: (&host as *const Host).cast_mut().cast(),
            invoke,
        };
        // SAFETY: endpoint fixture outlives the module and calls.
        let mut module =
            Replacement::start(unsafe { RuntimeClient::from_endpoint(endpoint) }).unwrap();
        for _ in 0..6 {
            module.tick().unwrap();
        }
        assert!(module.admission.is_none() && module.pending.is_none());
        let commands = host.commands.borrow();
        assert_eq!(commands[3], commands[4]);
        assert_eq!(commands[7], commands[8]);
        assert_eq!(commands[9], commands[10]);
    }

    #[test]
    fn catalog_parent_cancellation_releases_child_and_requested_limit_is_enforced() {
        let too_many = json!({"ok":true,"revision":1,"entries":[{"id":"app-0000000000000001","name":"One"},{"id":"app-0000000000000002","name":"Two"}],"next_cursor":null});
        assert_eq!(
            project_catalog(too_many, None, 1)["error"]["code"],
            "invalid_catalog_response"
        );
        // The explicit canceled-child transport path is exercised separately
        // from the lost-result retry sequence above.
        use amaterasu_module_sdk::abi::{AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_OK};
        use std::{cell::RefCell, collections::VecDeque, ffi::c_void};
        struct Host {
            replies: RefCell<VecDeque<Value>>,
        }
        unsafe extern "C" fn invoke(
            context: *mut c_void,
            _: BorrowedBytes,
            sink: ReplySink,
        ) -> i32 {
            // SAFETY: the test keeps the fixture alive throughout the call.
            let host = unsafe { &*context.cast::<Host>() };
            let bytes =
                serde_json::to_vec(&host.replies.borrow_mut().pop_front().unwrap()).unwrap();
            // SAFETY: the SDK supplies the live synchronous callback.
            unsafe { sink.send(&bytes) };
            STATUS_OK
        }
        let host = Host {
            replies: RefCell::new(VecDeque::from([
                json!({"kind":"result","payload":{"allowed_callers":["consumer"],"catalog_backed":true}}),
                json!({"kind":"ok"}),
                json!({"kind":"request","id":7,"caller":"consumer","initiator":"consumer","service":SERVICE,"payload":{"action":"list","limit":1},"remaining_ms":100}),
                json!({"kind":"accepted","id":8}),
                json!({"kind":"error","code":"cancelled","message":"parent canceled"}),
                json!({"kind":"error","code":"not_found","message":"released"}),
                json!({"kind":"error","code":"request_ended","message":"parent ended"}),
            ])),
        };
        let endpoint = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: (&host as *const Host).cast_mut().cast(),
            invoke,
        };
        // SAFETY: the host fixture outlives the module and all synchronous calls.
        let mut module =
            Replacement::start(unsafe { RuntimeClient::from_endpoint(endpoint) }).unwrap();
        module.tick().unwrap();
        module.tick().unwrap();
        assert!(module.admission.is_none() && module.pending.is_none());
    }

    #[test]
    fn requires_broker_identity_and_rejects_payload_spoofing() {
        let settings = Settings {
            allowed_callers: BTreeSet::from(["consumer".into()]),
            catalog_backed: false,
        };
        let list = json!({"action":"list","limit":1});
        assert_eq!(
            reply(&settings, "consumer", "consumer", list.clone())["entries"][0]["id"],
            "example-game"
        );
        assert_eq!(
            reply(&settings, "other", "consumer", list.clone())["error"]["code"],
            "unauthorized"
        );
        assert_eq!(
            reply(&settings, "consumer", "other", list)["error"]["code"],
            "unauthorized"
        );
        assert_eq!(
            reply(
                &settings,
                "consumer",
                "consumer",
                json!({"action":"resolve","id":"example-game","caller":"consumer"})
            )["error"]["code"],
            "invalid_request"
        );
    }

    #[test]
    fn rejects_bad_limits_and_cursors_and_resolves_exact_id() {
        let settings = Settings {
            allowed_callers: BTreeSet::from(["consumer".into()]),
            catalog_backed: false,
        };
        let call = |value| reply(&settings, "consumer", "consumer", value);
        assert_eq!(
            call(json!({"action":"list","limit":33}))["error"]["code"],
            "invalid_request"
        );
        assert_eq!(
            call(
                json!({"action":"list","cursor":{"revision":1,"after":"example-game","query":""}})
            )["error"]["code"],
            "invalid_cursor"
        );
        assert_eq!(
            call(json!({"action":"resolve","id":"example-game"}))["name"],
            "Example Game"
        );
        assert_eq!(
            call(json!({"action":"resolve","id":"unknown"}))["error"]["code"],
            "not_found"
        );
    }
}
