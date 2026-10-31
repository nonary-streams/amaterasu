//! Independently built, read-only replacement for amaterasu.library/1.
//! The static data is a demonstration, not a launchable application catalog.
use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;

const SERVICE: &str = "amaterasu.library/1";
const REVISION: u64 = 1;

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    /// Exact immediate callers permitted to read this static demonstration.
    allowed_callers: BTreeSet<String>,
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

#[derive(Deserialize)]
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

struct Replacement {
    runtime: RuntimeClient,
    settings: Settings,
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
        Ok(Self { runtime, settings })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        if let Some(request) = self.runtime.poll_serialized()? {
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
            self.runtime.complete_serialized(request.id, &response)?;
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
    fn requires_broker_identity_and_rejects_payload_spoofing() {
        let settings = Settings {
            allowed_callers: BTreeSet::from(["consumer".into()]),
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
