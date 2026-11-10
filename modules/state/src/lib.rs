//! Durable caller-scoped JSON state, served through the runtime request queue.
mod collection;
mod draft;
mod protocol;
#[cfg(test)]
mod reliable_broker_tests;
mod snapshot;
mod store;

/// Minimal in-process State interface for cross-module integration tests.
/// The feature is disabled in production builds.
#[cfg(feature = "test-support")]
pub mod test_support {
    use serde_json::Value;
    use std::path::PathBuf;

    pub struct TestStateStore(crate::store::StateStore);

    impl TestStateStore {
        pub fn open(directory: PathBuf) -> Result<Self, String> {
            crate::store::StateStore::open(directory).map(Self)
        }

        pub fn handle(&self, caller: &str, payload: Value) -> Value {
            self.0.handle(caller, payload)
        }
    }
}

use amaterasu_module_sdk::requests::{IncomingRequest, ReliableRequestReceiver};
use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use store::StateStore;

const SERVICE: &str = "amaterasu.state/1";
const REQUESTS_PER_TICK: usize = 16;

struct StateModule {
    runtime: RuntimeClient,
    store: StateStore,
    completion: PendingCompletion,
    work: Option<PendingWork>,
    retiring: Option<u64>,
    receiver: ReliableRequestReceiver,
    quiescing: bool,
}

struct PendingWork {
    id: u64,
    caller: String,
    service: String,
    payload: serde_json::Value,
}

#[derive(Default)]
struct PendingCompletion(Option<(u64, serde_json::Value)>);

impl PendingCompletion {
    fn flush(
        &mut self,
        mut send: impl FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<bool> {
        let Some((request, payload)) = self.0.as_ref() else {
            return Ok(true);
        };
        let response = match send(Command::Complete {
            request: *request,
            payload: payload.clone(),
        }) {
            Ok(response) => response,
            // The broker may have accepted this result despite a lost reply.
            // Preserve the exact result and retry before processing more work.
            Err(_) => return Ok(false),
        };
        complete_result(response)?;
        self.0 = None;
        Ok(true)
    }
}

impl ManagedModule for StateModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let store = StateStore::open(runtime.runtime_state_root()?)?;
        let mut receiver = ReliableRequestReceiver::new()?;
        // Fail closed on an older broker or an in-flight delivery from a
        // previous provider before exposing this service to consumers.
        receiver.select(&runtime)?;
        if let Err(error) = runtime.require_success(Command::Register {
            service: SERVICE.into(),
        }) {
            // No request can have been delivered yet; restore the legacy mode
            // when registration fails so a later provider can retry startup.
            let _ = receiver.deselect(&runtime);
            return Err(error);
        }
        Ok(Self {
            runtime,
            store,
            completion: PendingCompletion::default(),
            work: None,
            retiring: None,
            receiver,
            quiescing: false,
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        if !self.advance_work()? || self.quiescing {
            return Ok(());
        }
        for _ in 0..REQUESTS_PER_TICK {
            match self.receiver.poll(&self.runtime) {
                Ok(Some(request)) => {
                    self.accept_work(request);
                    if !self.advance_work()? {
                        break;
                    }
                }
                // The receiver retains a lost offer/acceptance for retry.
                Ok(None) | Err(_) => break,
            }
        }
        Ok(())
    }

    fn quiesce(&mut self) -> ModuleResult<bool> {
        self.quiescing = true;
        if !self.advance_work()? {
            return Ok(false);
        }
        match self.receiver.drain(&self.runtime) {
            Ok(Some(request)) => {
                self.accept_work(request);
                if !self.advance_work()? {
                    return Ok(false);
                }
            }
            Ok(None) => {}
            Err(_) => return Ok(false),
        }
        if !self.receiver.is_idle() {
            return Ok(false);
        }
        // Leave the broker ready for a later legacy provider only after all
        // accepted handlers and completion receipts have retired.
        Ok(self.receiver.deselect(&self.runtime).is_ok())
    }
}

impl StateModule {
    fn accept_work(&mut self, request: IncomingRequest<serde_json::Value>) {
        self.work = Some(PendingWork {
            id: request.id,
            caller: request.caller,
            service: request.service,
            payload: request.payload,
        });
    }

    fn flush_completion(&mut self) -> ModuleResult<bool> {
        let completed = self.completion.0.as_ref().map(|(id, _)| *id);
        if !self
            .completion
            .flush(|command| self.runtime.send(command))?
        {
            return Ok(false);
        }
        if let Some(id) = completed {
            self.retiring = Some(id);
        }
        if let Some(id) = self.retiring {
            if !self.receiver.retire(&self.runtime, id).unwrap_or(false) {
                return Ok(false);
            }
            self.retiring = None;
        }
        Ok(true)
    }

    fn advance_work(&mut self) -> ModuleResult<bool> {
        if !self.flush_completion()? {
            return Ok(false);
        }
        let Some(work) = self.work.as_ref() else {
            return Ok(true);
        };
        match self
            .runtime
            .send(Command::CheckRequest { request: work.id })
        {
            Ok(Response::Ok) => {}
            Ok(Response::Error { code, .. }) if code == "request_ended" || code == "not_found" => {
                self.retiring = Some(work.id);
                self.work = None;
                return self.flush_completion();
            }
            // A lost or malformed status response does not retire the request.
            _ => return Ok(false),
        }
        // This check avoids work already known to have ended. Cancellation can
        // still race the synchronous write; callers must reconcile that case.
        let work = self.work.take().expect("request just checked");
        let result = if work.service == SERVICE {
            self.store.handle(&work.caller, work.payload)
        } else {
            protocol::StateError::new("invalid_service", "unsupported state service").payload()
        };
        self.completion.0 = Some((work.id, result));
        self.flush_completion()
    }
}

fn complete_result(response: Response) -> ModuleResult<()> {
    match response {
        Response::Ok => Ok(()),
        // Persistence may finish after a request expires or its caller cancels.
        Response::Error { code, .. } if code == "request_ended" || code == "not_found" => Ok(()),
        Response::Error { code, message } => Err(format!("{code}: {message}")),
        response => Err(format!(
            "unexpected state completion response: {response:?}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "test-support")]
    #[test]
    fn test_support_opens_real_caller_scoped_store() {
        let directory = std::env::temp_dir().join(format!(
            "amaterasu-state-test-support-{}",
            amaterasu_storage::random_identifier().unwrap()
        ));
        let store = test_support::TestStateStore::open(directory.clone()).unwrap();
        assert_eq!(
            store.handle(
                "first",
                serde_json::json!({"action":"set","key":"one","value":1})
            )["ok"],
            true
        );
        assert_eq!(
            store.handle("first", serde_json::json!({"action":"get","key":"one"}))["value"],
            1
        );
        assert_eq!(
            store.handle("second", serde_json::json!({"action":"get","key":"one"}))["found"],
            false
        );
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn completion_discards_expired_requests_but_surfaces_other_errors() {
        assert!(complete_result(Response::error("request_ended", "expired")).is_ok());
        assert!(complete_result(Response::error("not_found", "removed")).is_ok());
        assert!(complete_result(Response::error("invalid_payload", "too large")).is_err());
    }

    #[test]
    fn lost_completion_reply_keeps_committed_result_for_retry_and_shutdown() {
        let payload = serde_json::json!({"ok":true,"revision":8});
        let mut completion = PendingCompletion(Some((42, payload.clone())));
        assert!(
            !completion
                .flush(|command| {
                    assert!(matches!(command, Command::Complete { request: 42, .. }));
                    Err("reply lost after commit".into())
                })
                .unwrap()
        );
        assert!(completion.0.is_some());
        assert!(
            completion
                .flush(|command| {
                    let Command::Complete {
                        request,
                        payload: retried,
                    } = command
                    else {
                        panic!("unexpected retry command");
                    };
                    assert_eq!(request, 42);
                    assert_eq!(retried, payload);
                    Ok(Response::error("request_ended", "already accepted"))
                })
                .unwrap()
        );
        assert!(completion.0.is_none());
    }

    #[test]
    fn module_waits_for_completion_before_quiescence_after_a_real_write() {
        use amaterasu_module_sdk::abi::{
            AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_INTERNAL_ERROR, STATUS_OK, Status,
        };
        use std::ffi::c_void;

        #[derive(Default)]
        struct Host {
            polls: usize,
            inspections: usize,
            checks: usize,
            retirements: usize,
            completions: Vec<(u64, serde_json::Value)>,
        }
        unsafe extern "C" fn invoke(
            context: *mut c_void,
            input: BorrowedBytes,
            reply: ReplySink,
        ) -> Status {
            // SAFETY: the test retains the host and its input buffers for this call.
            let host = unsafe { &mut *context.cast::<Host>() };
            let Ok(bytes) = (unsafe { input.read() }) else {
                return STATUS_INTERNAL_ERROR;
            };
            let Ok(command) = serde_json::from_slice::<Command>(bytes) else {
                return STATUS_INTERNAL_ERROR;
            };
            let response = match command {
                Command::PollV2 { .. } => {
                    host.polls += 1;
                    return STATUS_INTERNAL_ERROR;
                }
                Command::InspectRequestOffer { .. } => {
                    host.inspections += 1;
                    Response::Request {
                        id: 42,
                        caller: "configuration".into(),
                        initiator: "client".into(),
                        service: SERVICE.into(),
                        payload: serde_json::json!({"action":"set","key":"test","value":1,"expected_revision":0}),
                        remaining_ms: 30_000,
                    }
                }
                Command::Complete { request, payload } => {
                    host.completions.push((request, payload));
                    if host.completions.len() == 1 {
                        return STATUS_INTERNAL_ERROR;
                    }
                    Response::error("request_ended", "reply was already accepted")
                }
                Command::CheckRequest { request: 42 } => {
                    host.checks += 1;
                    if host.checks == 1 {
                        return STATUS_INTERNAL_ERROR;
                    }
                    Response::Ok
                }
                Command::AcceptRequest { request: 42, .. } => Response::Ok,
                Command::RetireRequestDelivery { request: 42, .. } => {
                    host.retirements += 1;
                    if host.retirements == 1 {
                        return STATUS_INTERNAL_ERROR;
                    }
                    Response::error("not_found", "retirement was already accepted")
                }
                Command::SelectRequestDeliveryMode { reliable: false } => Response::Ok,
                _ => return STATUS_INTERNAL_ERROR,
            };
            let Ok(bytes) = serde_json::to_vec(&response) else {
                return STATUS_INTERNAL_ERROR;
            };
            // SAFETY: the runtime reply copies this buffer synchronously.
            unsafe { reply.send(&bytes) };
            STATUS_OK
        }

        let root = std::env::temp_dir().join(format!(
            "amaterasu-state-completion-{}",
            amaterasu_storage::random_identifier().unwrap()
        ));
        let mut host = Host::default();
        let endpoint = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: (&mut host as *mut Host).cast(),
            invoke,
        };
        // SAFETY: the test retains the host until the module is dropped.
        let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
        let mut module = StateModule {
            runtime,
            store: StateStore::open(root.clone()).unwrap(),
            completion: PendingCompletion::default(),
            work: None,
            retiring: None,
            receiver: ReliableRequestReceiver::new().unwrap(),
            quiescing: false,
        };
        module.tick().unwrap();
        assert_eq!(host.polls, 1);
        assert!(module.work.is_none());
        assert!(!module.receiver.is_idle());
        assert_eq!(
            module.store.handle(
                "configuration",
                serde_json::json!({"action":"get","key":"test"})
            )["found"],
            false
        );
        assert!(!module.quiesce().unwrap());
        assert!(!module.quiesce().unwrap());
        assert_eq!(
            module.store.handle(
                "configuration",
                serde_json::json!({"action":"get","key":"test"})
            )["value"],
            1
        );
        assert!(!module.quiesce().unwrap());
        assert!(module.quiesce().unwrap());
        assert_eq!(host.polls, 1);
        assert_eq!(host.inspections, 1);
        assert_eq!(host.checks, 2);
        assert_eq!(host.retirements, 2);
        assert_eq!(host.completions.len(), 2);
        assert_eq!(host.completions[0], host.completions[1]);
        drop(module);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn already_ended_request_does_not_mutate_state() {
        use amaterasu_module_sdk::abi::{
            AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_OK, Status,
        };
        use std::ffi::c_void;

        unsafe extern "C" fn ended(
            _context: *mut c_void,
            input: BorrowedBytes,
            reply: ReplySink,
        ) -> Status {
            // SAFETY: the host supplies a readable command for this call.
            let command: Command =
                serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
            let response = match command {
                Command::CheckRequest { .. } => Response::error("request_ended", "cancelled"),
                Command::RetireRequestDelivery { .. }
                | Command::SelectRequestDeliveryMode { reliable: false } => Response::Ok,
                _ => return amaterasu_module_sdk::abi::STATUS_INTERNAL_ERROR,
            };
            let bytes = serde_json::to_vec(&response).unwrap();
            // SAFETY: the reply copies this buffer during the call.
            unsafe { reply.send(&bytes) };
            STATUS_OK
        }
        let root = std::env::temp_dir().join(format!(
            "amaterasu-state-ended-{}",
            amaterasu_storage::random_identifier().unwrap()
        ));
        let endpoint = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: std::ptr::null_mut(),
            invoke: ended,
        };
        // SAFETY: the callback is static and does not use its context.
        let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
        let mut module = StateModule {
            runtime,
            store: StateStore::open(root.clone()).unwrap(),
            completion: PendingCompletion::default(),
            work: Some(PendingWork {
                id: 42,
                caller: "configuration".into(),
                service: SERVICE.into(),
                payload: serde_json::json!({"action":"set","key":"test","value":1,"expected_revision":0}),
            }),
            retiring: None,
            receiver: ReliableRequestReceiver::new().unwrap(),
            quiescing: false,
        };
        assert!(module.quiesce().unwrap());
        assert_eq!(
            module.store.handle(
                "configuration",
                serde_json::json!({"action":"get","key":"test"})
            )["found"],
            false
        );
        drop(module);
        std::fs::remove_dir_all(root).unwrap();
    }
}

amaterasu_module_sdk::export_module!(
    amaterasu_module_sdk::service_worker::ServiceWorker<StateModule>
);
