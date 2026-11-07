//! Windows Graphics Capture with caller-owned streams and synchronous CPU frames.
mod backend;
mod commands;
mod controller;
mod format;
mod model;
#[cfg(test)]
mod native;
mod opening;
mod preflight;
mod resource;
#[cfg(any(windows, test))]
mod staging;
mod worker;

use amaterasu_module_sdk::{
    Command, LogLevel, ManagedModule, ModuleResult, Response, RuntimeClient,
};
use backend::NativeBackend;
use controller::{CloseReceipt, Controller};
use model::{CaptureError, Operation};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

const SERVICE: &str = "amaterasu.capture-wgc/1";
const MAX_PENDING_CLOSES: usize = 32;
const MAX_PENDING_REPLIES: usize = 32;
type SharedController = Arc<Mutex<Controller<NativeBackend>>>;

struct PendingClose {
    request: Option<u64>,
    caller: String,
    stream: u64,
    receipt: Option<CloseReceipt>,
    reply: Option<Value>,
    admitted: bool,
}

struct PendingReply {
    request: u64,
    payload: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    capture_cursor: Option<bool>,
}

struct CaptureModule {
    runtime: RuntimeClient,
    controller: SharedController,
    shutdown_requested: bool,
    quiescing: bool,
    pending_closes: Vec<PendingClose>,
    pending_opens: Vec<opening::PendingOpen>,
    pending_lookups: Vec<opening::PendingLookup>,
    pending_commands: Vec<commands::PendingCommand>,
    pending_replies: Vec<PendingReply>,
    poll_diagnostic_reported: bool,
    _preflight_factory: Box<
        amaterasu_module_sdk::capture::SourcePreflightFactory<preflight::Provider<NativeBackend>>,
    >,
    _factory:
        Box<amaterasu_module_sdk::capture::CpuSourceFactory<resource::Provider<NativeBackend>>>,
}

impl ManagedModule for CaptureModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("capture settings unavailable".into());
        };
        let settings: Settings =
            serde_json::from_value(payload).map_err(|error| error.to_string())?;
        let controller = Arc::new(Mutex::new(Controller::new(
            NativeBackend::new(settings.capture_cursor),
            settings.enabled,
        )));
        let factory = amaterasu_module_sdk::capture::CpuSourceFactory::new(Arc::new(
            resource::Provider::new(controller.clone()),
        ));
        // SAFETY: the original factory's bind allocates binding metadata only.
        // Both boxed factories outlive all bindings; the query provider retains
        // caller-scoped controller state and never opens a capture source.
        let preflight_factory = unsafe {
            amaterasu_module_sdk::capture::SourcePreflightFactory::new(
                &factory.api,
                Arc::new(preflight::Provider::new(controller.clone())),
            )
        };
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        // SAFETY: factory is boxed and retained through module destruction;
        // bindings own independent references to controller state.
        unsafe {
            runtime.register_native_typed(
                SERVICE,
                amaterasu_module_sdk::media::native_contracts::CAPTURE,
                &preflight_factory.api,
            )
        }?;
        Ok(Self {
            runtime,
            controller,
            shutdown_requested: false,
            quiescing: false,
            pending_closes: Vec::new(),
            pending_opens: Vec::new(),
            pending_lookups: Vec::new(),
            pending_commands: Vec::new(),
            pending_replies: Vec::new(),
            poll_diagnostic_reported: false,
            _factory: factory,
            _preflight_factory: preflight_factory,
        })
    }

    fn prepare_shutdown(&mut self, _remaining_ms: u64) -> ModuleResult<()> {
        self.shutdown_requested = true;
        self.stop_admission_when_available()?;
        Ok(())
    }

    fn quiesce(&mut self) -> ModuleResult<bool> {
        self.shutdown_requested = true;
        self.quiescing = true;
        let open_failure = self.poll_open_requests().err();
        let command_failure = self.poll_commands(true).err();
        let mut failure = self.poll_pending_closes().err();
        if let Err(error) = self.poll_pending_replies() {
            failure.get_or_insert(error);
        }
        if let Some(error) = open_failure {
            failure.get_or_insert(error);
        }
        if let Some(error) = command_failure {
            failure.get_or_insert(error);
        }
        let retired = match self.controller.try_lock() {
            Ok(mut controller) => controller
                .quiesce()
                .map_err(|error| format!("{}: {}", error.code, error.message)),
            Err(std::sync::TryLockError::WouldBlock) => Ok(false),
            Err(_) => Err("capture controller poisoned".into()),
        };
        let retired = match retired {
            Ok(retired) => retired,
            Err(error) => {
                failure.get_or_insert(error);
                false
            }
        };
        if retired && let Err(error) = self.poll_pending_closes() {
            failure.get_or_insert(error);
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(retired
            && self.pending_closes.is_empty()
            && self.pending_opens.is_empty()
            && self.pending_lookups.is_empty()
            && self.pending_commands.is_empty()
            && self.pending_replies.is_empty())
    }

    fn tick(&mut self) -> ModuleResult<()> {
        let mut failure = self.stop_admission_when_available().err();
        let opens_ready = retain_failure(&mut failure, self.poll_open_requests());
        let commands_ready = retain_failure(&mut failure, self.poll_commands(self.quiescing));
        let closes_ready = retain_failure(&mut failure, self.poll_pending_closes());
        let replies_ready = retain_failure(&mut failure, self.poll_pending_replies());
        for _ in 0..8 {
            // Every immediate reply needs a slot before we take ownership of
            // another broker request. An unknown Complete may have delivered.
            if self.pending_replies.len() == MAX_PENDING_REPLIES {
                break;
            }
            let (id, caller, service, payload) = match self.runtime.send(Command::Poll {}) {
                Ok(Response::Pending) => break,
                Ok(Response::Request {
                    id,
                    caller,
                    service,
                    payload,
                    ..
                }) => (id, caller, service, payload),
                response => {
                    failure.get_or_insert_with(|| format!("capture poll failed: {response:?}"));
                    break;
                }
            };
            let reply = if service != SERVICE {
                CaptureError::new("invalid_service", "unsupported capture service").payload()
            } else {
                match serde_json::from_value::<Operation>(payload) {
                    Ok(Operation::Open {
                        output,
                        operation_id,
                        constant_rate,
                        source_format,
                    }) => {
                        if self.pending_opens.len() < opening::MAX_PENDING_REQUESTS {
                            self.pending_opens.push(opening::PendingOpen::new(
                                id,
                                caller,
                                output,
                                operation_id,
                                constant_rate,
                                source_format,
                            ));
                            continue;
                        }
                        CaptureError::new("resource_exhausted", "capture open waiters are full")
                            .payload()
                    }
                    Ok(Operation::Lookup { operation_id }) => {
                        if self.pending_lookups.len() < opening::MAX_PENDING_REQUESTS {
                            self.pending_lookups.push(opening::PendingLookup::new(
                                id,
                                caller,
                                operation_id,
                            ));
                            continue;
                        }
                        CaptureError::new("resource_exhausted", "capture lookup waiters are full")
                            .payload()
                    }
                    Ok(Operation::Close { stream }) => {
                        if self.pending_closes.len() < MAX_PENDING_CLOSES {
                            self.pending_closes.push(PendingClose {
                                request: Some(id),
                                caller,
                                stream,
                                receipt: None,
                                reply: None,
                                admitted: false,
                            });
                            continue;
                        }
                        CaptureError::new("resource_exhausted", "capture close waiters are full")
                            .payload()
                    }
                    Ok(operation @ (Operation::Status | Operation::Enumerate { .. })) => {
                        if self.quiescing {
                            CaptureError::new("capture_disabled", "capture is quiescing").payload()
                        } else if self.pending_commands.len() < commands::MAX_PENDING_COMMANDS {
                            self.pending_commands
                                .push(commands::PendingCommand::new(id, caller, operation));
                            continue;
                        } else {
                            CaptureError::new(
                                "resource_exhausted",
                                "capture command waiters are full",
                            )
                            .payload()
                        }
                    }
                    Err(error) => CaptureError::new("invalid_request", error.to_string()).payload(),
                }
            };
            self.pending_replies.push(PendingReply {
                request: id,
                payload: reply,
            });
        }
        if opens_ready {
            retain_failure(&mut failure, self.poll_open_requests());
        }
        if commands_ready {
            retain_failure(&mut failure, self.poll_commands(self.quiescing));
        }
        if closes_ready {
            retain_failure(&mut failure, self.poll_pending_closes());
        }
        if replies_ready {
            retain_failure(&mut failure, self.poll_pending_replies());
        }
        // Observation errors leave their exact owned work in the queues for
        // the next host tick. Poison is an invariant failure, even when an
        // earlier broker failure would otherwise mask it.
        if self.controller.is_poisoned() {
            return Err("capture controller poisoned".into());
        }
        if failure.is_some() {
            if !self.poll_diagnostic_reported {
                let _ = self.runtime.log(
                    LogLevel::Warning,
                    "capture.wgc",
                    "Capture broker or worker observation failed; owned work is retained for retry.",
                );
                self.poll_diagnostic_reported = true;
            }
        } else {
            self.poll_diagnostic_reported = false;
        }
        Ok(())
    }
}

fn retain_failure(failure: &mut Option<String>, result: ModuleResult<()>) -> bool {
    match result {
        Ok(()) => true,
        Err(error) => {
            failure.get_or_insert(error);
            false
        }
    }
}

impl CaptureModule {
    fn poll_pending_replies(&mut self) -> ModuleResult<()> {
        let mut failure = None;
        self.pending_replies.retain(|reply| {
            match self.runtime.send(Command::Complete {
                request: reply.request,
                payload: reply.payload.clone(),
            }) {
                Ok(Response::Ok) => false,
                Ok(Response::Error { code, .. })
                    if code == "request_ended" || code == "not_found" =>
                {
                    false
                }
                _ => {
                    failure.get_or_insert("capture reply completion is uncertain".to_owned());
                    true
                }
            }
        });
        failure.map_or(Ok(()), Err)
    }

    fn poll_commands(&mut self, quiescing: bool) -> ModuleResult<()> {
        commands::poll_requests(
            &self.runtime,
            &self.controller,
            quiescing,
            &mut self.pending_commands,
        )
    }

    fn poll_open_requests(&mut self) -> ModuleResult<()> {
        opening::poll_requests(
            &self.runtime,
            &self.controller,
            self.shutdown_requested,
            &mut self.pending_opens,
            &mut self.pending_lookups,
        )
    }

    fn poll_pending_closes(&mut self) -> ModuleResult<()> {
        let activity_failure = self.check_close_waiters();
        let retirement_failure = self.poll_close_jobs();
        let completion_failure = self.complete_close_waiters();
        activity_failure
            .or(retirement_failure)
            .or(completion_failure)
            .map_or(Ok(()), Err)
    }

    fn check_close_waiters(&mut self) -> Option<String> {
        let mut failure = None;
        // Cancellation retires a broker waiter, never a source whose cleanup
        // has started. Its receipt remains attached to the owned stream.
        self.pending_closes.retain_mut(|pending| {
            if pending.reply.is_some() {
                return true;
            }
            let Some(request) = pending.request else {
                return true;
            };
            match self.runtime.send(Command::CheckRequest { request }) {
                Ok(Response::Ok) => {
                    pending.admitted = true;
                    true
                }
                Ok(Response::Error { code, .. })
                    if code == "request_ended" || code == "not_found" =>
                {
                    if pending.admitted && pending.receipt.is_none() {
                        // Admission can precede acquiring a controller held by
                        // a native read. Retain that exact cleanup target even
                        // when the broker waiter goes away before begin_close.
                        pending.request = None;
                        true
                    } else {
                        false
                    }
                }
                response => {
                    failure.get_or_insert_with(|| {
                        format!("capture close activity check failed: {response:?}")
                    });
                    true
                }
            }
        });
        failure
    }

    fn poll_close_jobs(&mut self) -> Option<String> {
        match self.controller.try_lock() {
            Ok(mut controller) => {
                for pending in &mut self.pending_closes {
                    if pending.admitted && pending.receipt.is_none() && pending.reply.is_none() {
                        match controller.begin_close(&pending.caller, pending.stream) {
                            Ok(receipt) => pending.receipt = Some(receipt),
                            Err(error) => pending.reply = Some(error.payload()),
                        }
                    }
                }
                let errors = controller.poll_closing();
                for pending in &mut self.pending_closes {
                    if pending.reply.is_some() {
                        continue;
                    }
                    if pending.receipt.as_ref().is_some_and(CloseReceipt::retired) {
                        pending.reply = Some(json!({"ok":true}));
                    } else if pending.receipt.is_some()
                        && let Some((_, error)) =
                            errors.iter().find(|(stream, _)| *stream == pending.stream)
                    {
                        pending.reply =
                            Some(CaptureError::new(error.code, error.message.clone()).payload());
                    }
                }
                None
            }
            Err(std::sync::TryLockError::WouldBlock) => None,
            Err(_) => Some("capture controller poisoned".into()),
        }
    }

    fn complete_close_waiters(&mut self) -> Option<String> {
        let mut failure = None;
        let waiters = std::mem::take(&mut self.pending_closes);
        for pending in waiters {
            let Some(request) = pending.request else {
                if pending.receipt.is_none() && pending.reply.is_none() {
                    self.pending_closes.push(pending);
                }
                // Once begun, the stream owns cleanup without broker metadata.
                continue;
            };
            let Some(reply) = &pending.reply else {
                self.pending_closes.push(pending);
                continue;
            };
            match self.runtime.send(Command::Complete {
                request,
                payload: reply.clone(),
            }) {
                Ok(Response::Ok) => (),
                Ok(Response::Error { code, .. })
                    if code == "request_ended" || code == "not_found" => {}
                result => {
                    failure.get_or_insert_with(|| {
                        format!("capture close completion failed: {result:?}")
                    });
                    self.pending_closes.push(pending);
                }
            }
        }
        failure
    }

    fn stop_admission_when_available(&self) -> ModuleResult<()> {
        if self.shutdown_requested {
            match self.controller.try_lock() {
                Ok(mut controller) => controller.stop_admission(),
                // A native read already owns this mutex. The module flag
                // rejects Open when its control request is processed later.
                Err(std::sync::TryLockError::WouldBlock) => (),
                Err(_) => return Err("capture controller poisoned".into()),
            }
        }
        Ok(())
    }
}

impl Drop for CaptureModule {
    fn drop(&mut self) {
        if let Ok(mut controller) = self.controller.lock() {
            controller.shutdown();
        }
    }
}
amaterasu_module_sdk::export_module!(CaptureModule);

#[cfg(test)]
mod tests;
