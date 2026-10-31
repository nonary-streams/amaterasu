//! Broker adapter for the lifecycle authority. Request payloads do not establish identity.
use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const SERVICE: &str = "amaterasu.session-lifecycle/1";

/// Receives identities assigned by the broker, never identity fields from a request.
pub trait LifecycleHandler: Sized {
    /// Opens and recovers durable authority state before service registration.
    fn from_journal_path(path: &Path) -> ModuleResult<Self>;

    fn handle(&mut self, caller: &str, initiator: &str, payload: Value) -> Value;

    /// Advance bounded background work without waiting for a provider. Implementations
    /// must persist an effect's identity and intent before submitting it, and fence
    /// outcomes that cannot be confirmed. Detached broker calls run as this service,
    /// not as the initiator of an earlier request.
    fn advance(
        &mut self,
        _send: &mut dyn FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<()> {
        Ok(())
    }
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    trusted_callers: BTreeSet<String>,
    journal_path: String,
    operation_grants: BTreeMap<String, BTreeSet<String>>,
}

pub struct LifecycleAdapter<H> {
    runtime: RuntimeClient,
    trusted_callers: BTreeSet<String>,
    operation_grants: BTreeMap<String, BTreeSet<String>>,
    handler: H,
}

impl<H: LifecycleHandler> ManagedModule for LifecycleAdapter<H> {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("lifecycle settings unavailable".into());
        };
        let settings: Settings =
            serde_json::from_value(payload).map_err(|_| "invalid lifecycle settings".to_owned())?;
        if settings
            .trusted_callers
            .iter()
            .any(|caller| caller.is_empty())
        {
            return Err("invalid lifecycle trusted caller".into());
        }
        if settings
            .operation_grants
            .iter()
            .any(|(caller, operations)| {
                !settings.trusted_callers.contains(caller)
                    || operations.iter().any(|operation| operation.is_empty())
            })
        {
            return Err("invalid lifecycle operation grants".into());
        }
        if settings.journal_path.is_empty() {
            return Err("lifecycle journal path is required".into());
        }
        let handler = H::from_journal_path(Path::new(&settings.journal_path))?;
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            trusted_callers: settings.trusted_callers,
            operation_grants: settings.operation_grants,
            handler,
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        self.handler
            .advance(&mut |command| self.runtime.send(command))?;
        let (id, caller, initiator, service, payload) = match self.runtime.send(Command::Poll {})? {
            Response::Pending => return Ok(()),
            Response::Request {
                id,
                caller,
                initiator,
                service,
                payload,
                ..
            } => (id, caller, initiator, service, payload),
            response => return Err(format!("unexpected lifecycle poll response: {response:?}")),
        };
        match self.runtime.send(Command::CheckRequest { request: id })? {
            Response::Ok => {}
            Response::Error { code, .. } if code == "request_ended" || code == "not_found" => {
                return Ok(());
            }
            response => {
                return Err(format!(
                    "unexpected lifecycle request check response: {response:?}"
                ));
            }
        }
        let response = if service != SERVICE {
            json!({"ok": false, "code": "invalid_service"})
        } else if !self.trusted_callers.contains(&caller)
            || !payload
                .get("operation")
                .and_then(Value::as_str)
                .is_some_and(|operation| {
                    self.operation_grants
                        .get(&caller)
                        .is_some_and(|grants| grants.contains(operation))
                })
        {
            json!({"ok": false, "code": "permission_denied"})
        } else {
            self.handler.handle(&caller, &initiator, payload)
        };
        match self.runtime.send(Command::Complete {
            request: id,
            payload: response,
        })? {
            Response::Ok => Ok(()),
            // A committed transition survives a request cancelled after dispatch.
            Response::Error { code, .. } if code == "request_ended" || code == "not_found" => {
                Ok(())
            }
            response => Err(format!(
                "unexpected lifecycle completion response: {response:?}"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_module_sdk::abi::{self, AbiHeader, BorrowedBytes, HostApi, ReplySink};
    use std::collections::VecDeque;
    use std::ffi::c_void;

    #[derive(Default)]
    struct Handler;

    impl LifecycleHandler for Handler {
        fn from_journal_path(_path: &Path) -> ModuleResult<Self> {
            Ok(Self)
        }

        fn handle(&mut self, caller: &str, initiator: &str, payload: Value) -> Value {
            json!({"caller": caller, "initiator": initiator, "payload": payload})
        }
    }

    struct Broker {
        settings: Value,
        requests: VecDeque<Response>,
        completions: Vec<Value>,
        active: bool,
        completion_error: Option<&'static str>,
        advances: usize,
    }

    impl Broker {
        fn dispatch(&mut self, command: Command) -> Response {
            match command {
                Command::GetSettings {} => Response::Result {
                    payload: self.settings.clone(),
                },
                Command::Register { .. } => Response::Ok,
                Command::Log { .. } => {
                    self.advances += 1;
                    Response::Ok
                }
                Command::Poll {} => self.requests.pop_front().unwrap_or(Response::Pending),
                Command::CheckRequest { .. } if self.active => Response::Ok,
                Command::CheckRequest { .. } => Response::error("request_ended", "cancelled"),
                Command::Complete { payload, .. } => {
                    self.completions.push(payload);
                    match self.completion_error {
                        Some(code) => Response::error(code, "completion failed"),
                        None => Response::Ok,
                    }
                }
                _ => panic!("unexpected broker command"),
            }
        }
    }

    unsafe extern "C" fn invoke(
        context: *mut c_void,
        input: BorrowedBytes,
        reply: ReplySink,
    ) -> abi::Status {
        // SAFETY: the test keeps exclusive access to the broker through this endpoint.
        let broker = unsafe { &mut *context.cast::<Broker>() };
        // SAFETY: the SDK supplies bytes valid for the duration of this call.
        let Ok(bytes) = (unsafe { input.read() }) else {
            return abi::STATUS_INVALID_ARGUMENT;
        };
        let Ok(command) = serde_json::from_slice(bytes) else {
            return abi::STATUS_INVALID_ARGUMENT;
        };
        let Ok(output) = serde_json::to_vec(&broker.dispatch(command)) else {
            return abi::STATUS_INTERNAL_ERROR;
        };
        // SAFETY: the reply sink copies these bytes synchronously.
        unsafe { reply.send(&output) };
        abi::STATUS_OK
    }

    fn request(caller: &str, initiator: &str) -> Response {
        Response::Request {
            id: 1,
            caller: caller.into(),
            initiator: initiator.into(),
            service: SERVICE.into(),
            payload: json!({"operation":"prepare", "caller":"transport", "initiator":"forged"}),
            remaining_ms: 1000,
        }
    }

    fn run(broker: &mut Broker) -> ModuleResult<()> {
        let endpoint = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: (broker as *mut Broker).cast(),
            invoke,
        };
        // SAFETY: broker outlives the module and is exclusively accessed through the endpoint.
        let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
        let mut module = LifecycleAdapter::<Handler>::start(runtime)?;
        module.tick()
    }

    fn broker(mut settings: Value, caller: &str) -> Broker {
        if let Some(object) = settings.as_object_mut() {
            object
                .entry("journal_path")
                .or_insert(json!("/test/journal"));
        }
        Broker {
            settings,
            requests: VecDeque::from([request(caller, "real-initiator")]),
            completions: Vec::new(),
            active: true,
            completion_error: None,
            advances: 0,
        }
    }

    #[test]
    fn broker_identity_is_authority_and_empty_policy_denies() {
        let mut denied = broker(json!({}), "transport");
        run(&mut denied).unwrap();
        assert_eq!(denied.completions[0]["code"], "permission_denied");
        let mut denied = broker(
            json!({"trusted_callers":["transport"], "operation_grants":{"transport":["prepare"]}}),
            "plugin",
        );
        run(&mut denied).unwrap();
        assert_eq!(denied.completions[0]["code"], "permission_denied");
        let mut no_grant = broker(json!({"trusted_callers":["transport"]}), "transport");
        run(&mut no_grant).unwrap();
        assert_eq!(no_grant.completions[0]["code"], "permission_denied");
        let mut allowed = broker(
            json!({"trusted_callers":["transport"], "operation_grants":{"transport":["prepare"]}}),
            "transport",
        );
        run(&mut allowed).unwrap();
        assert_eq!(allowed.completions[0]["caller"], "transport");
        assert_eq!(allowed.completions[0]["initiator"], "real-initiator");
    }

    #[test]
    fn cancelled_request_is_not_dispatched_and_late_cancellation_is_tolerated() {
        let mut cancelled = broker(
            json!({"trusted_callers":["transport"], "operation_grants":{"transport":["prepare"]}}),
            "transport",
        );
        cancelled.active = false;
        run(&mut cancelled).unwrap();
        assert!(cancelled.completions.is_empty());
        for code in ["request_ended", "not_found", "permission_denied"] {
            let mut late = broker(
                json!({"trusted_callers":["transport"], "operation_grants":{"transport":["prepare"]}}),
                "transport",
            );
            late.completion_error = Some(code);
            assert_eq!(run(&mut late).is_ok(), code != "permission_denied");
            assert_eq!(late.completions.len(), 1);
        }
    }

    #[test]
    fn malformed_settings_fail_closed() {
        for settings in [
            json!({"trusted_callers":[""]}),
            json!({"journal_path":""}),
            json!({"trusted_callers":"transport"}),
            json!({"unexpected":true}),
        ] {
            let mut broker = broker(settings, "transport");
            assert!(run(&mut broker).is_err());
            assert!(broker.completions.is_empty());
        }
    }
    #[test]
    fn background_work_advances_on_idle_and_busy_ticks() {
        struct Advancing;
        impl LifecycleHandler for Advancing {
            fn from_journal_path(_: &Path) -> ModuleResult<Self> {
                Ok(Self)
            }
            fn handle(&mut self, _: &str, _: &str, _: Value) -> Value {
                json!({"ok": true})
            }
            fn advance(
                &mut self,
                send: &mut dyn FnMut(Command) -> ModuleResult<Response>,
            ) -> ModuleResult<()> {
                match send(Command::Log {
                    level: 0,
                    target: "test".into(),
                    message: "advance".into(),
                })? {
                    Response::Ok => Ok(()),
                    _ => Err("advance failed".into()),
                }
            }
        }
        let mut broker = broker(
            json!({"trusted_callers":["transport"], "operation_grants":{"transport":["prepare"]}}),
            "transport",
        );
        let endpoint = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: (&mut broker as *mut Broker).cast(),
            invoke,
        };
        // SAFETY: broker outlives the module and is exclusively accessed through the endpoint.
        let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
        let mut module = LifecycleAdapter::<Advancing>::start(runtime).unwrap();
        module.tick().unwrap();
        module.tick().unwrap();
        drop(module);
        assert_eq!(broker.advances, 2);
        assert_eq!(broker.completions.len(), 1);
    }
}
