//! Windows desktop duplication with caller-owned streams and synchronous CPU frames.
mod backend;
#[cfg(test)]
mod broker_tests;
mod controller;
#[cfg(any(windows, test))]
mod cursor;
mod enumeration;
#[cfg(test)]
mod enumeration_broker_tests;
#[cfg(test)]
mod exact_fence_tests;
mod format;
mod model;
mod native;
mod opening;
#[cfg(test)]
mod opening_broker_tests;
mod preflight;
#[cfg(test)]
mod resource_tests;
#[cfg(any(windows, test))]
mod staging;

use amaterasu_module_sdk::{
    Command, LogLevel, ManagedModule, ModuleResult, Response, RuntimeClient,
};
use backend::NativeBackend;
use controller::{CloseReceipt, Controller};
use model::{CaptureError, Operation};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

const SERVICE: &str = "amaterasu.capture-dxgi/1";
const MAX_PENDING_CLOSES: usize = 32;
const MAX_PENDING_OPERATIONS: usize = 32;
const MAX_PENDING_REPLIES: usize = 32;
type SharedController = Arc<Mutex<Controller<NativeBackend>>>;

struct PendingClose {
    request: Option<u64>,
    caller: String,
    delivery_order: u64,
    stream: u64,
    target_operation: Option<Option<String>>,
    receipt: Option<CloseReceipt>,
    reply: Option<Value>,
    admitted: bool,
}

struct PendingOperation {
    request: u64,
    caller: String,
    operation: Option<Operation>,
    reply: Option<Value>,
}

struct PendingReply {
    request: u64,
    payload: Value,
}

fn close_may_overlap_open(close: &PendingClose, open: &opening::PendingOpen) -> bool {
    if close.caller != open.caller() {
        return false;
    }
    if close.receipt.as_ref().is_some_and(CloseReceipt::retired) {
        return false;
    }
    if let Some(stream) = open.target_stream() {
        return stream == close.stream;
    }
    // A Close rejected before owning any stream has no effect to order.
    if close.reply.is_some() && close.receipt.is_none() {
        return false;
    }
    match &close.target_operation {
        Some(operation) => open
            .operation_id()
            .is_some_and(|id| operation.as_deref() == Some(id)),
        // Until the controller can inspect this exact stream, an unassigned
        // Open could still resolve to it. Preserve the earlier operation.
        None => true,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    #[serde(default)]
    enabled: bool,
}

struct CaptureModule {
    runtime: RuntimeClient,
    controller: SharedController,
    shutdown_requested: bool,
    pending_closes: Vec<PendingClose>,
    pending_operations: Vec<PendingOperation>,
    pending_enumerations: Vec<enumeration::PendingEnumeration>,
    pending_opens: Vec<opening::PendingOpen>,
    pending_lookups: Vec<opening::PendingLookup>,
    pending_replies: Vec<PendingReply>,
    next_delivery_order: u64,
    poll_diagnostic_reported: bool,
    preflight: Arc<preflight::Provider<NativeBackend>>,
    _preflight_factory: Box<
        amaterasu_module_sdk::capture::SourcePreflightFactory<preflight::Provider<NativeBackend>>,
    >,
    _factory: Box<native::Factory<NativeBackend>>,
}

impl ManagedModule for CaptureModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("capture settings unavailable".into());
        };
        let settings: Settings =
            serde_json::from_value(payload).map_err(|error| error.to_string())?;
        let controller = Arc::new(Mutex::new(Controller::new(NativeBackend, settings.enabled)));
        let factory = native::Factory::new(controller.clone());
        let preflight = Arc::new(preflight::Provider::new(controller.clone(), NativeBackend)?);
        // SAFETY: the original factory's bind allocates binding metadata only.
        // Both boxed factories outlive all bindings; the query provider retains
        // caller-scoped controller state and never opens a capture source.
        let preflight_factory = unsafe {
            amaterasu_module_sdk::capture::SourcePreflightFactory::new(
                &factory.api,
                preflight.clone(),
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
            pending_closes: Vec::new(),
            pending_operations: Vec::new(),
            pending_enumerations: Vec::new(),
            pending_opens: Vec::new(),
            pending_lookups: Vec::new(),
            pending_replies: Vec::new(),
            next_delivery_order: 0,
            poll_diagnostic_reported: false,
            preflight,
            _factory: factory,
            _preflight_factory: preflight_factory,
        })
    }

    fn prepare_shutdown(&mut self, _remaining_ms: u64) -> ModuleResult<()> {
        self.shutdown_requested = true;
        self.preflight
            .stop_admission()
            .map_err(|status| format!("capture preflight admission stop failed: {status:?}"))?;
        self.stop_admission_when_available()?;
        Ok(())
    }

    fn quiesce(&mut self) -> ModuleResult<bool> {
        self.shutdown_requested = true;
        let mut failure = self
            .preflight
            .stop_admission()
            .err()
            .map(|status| format!("capture preflight admission stop failed: {status:?}"));
        let preflight_retired = match self.preflight.quiesce() {
            Ok(retired) => retired,
            Err(status) => {
                failure.get_or_insert_with(|| {
                    format!("capture preflight retirement failed: {status:?}")
                });
                false
            }
        };
        if let Err(error) = self.poll_pending_closes() {
            failure.get_or_insert(error);
        }
        if let Err(error) = self.poll_pending_operations() {
            failure.get_or_insert(error);
        }
        if let Err(error) = self.poll_enumerations() {
            failure.get_or_insert(error);
        }
        if let Err(error) = self.poll_open_requests() {
            failure.get_or_insert(error);
        }
        if let Err(error) = self.poll_pending_replies() {
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
        if let Err(error) = self.poll_pending_closes() {
            failure.get_or_insert(error);
        }
        if let Some(error) = failure {
            return Err(error);
        }
        let resources_retired = self
            ._factory
            .retirement_ready()
            .map_err(|status| format!("capture resources did not retire: {status}"))?;
        Ok(preflight_retired
            && retired
            && resources_retired
            && self.pending_closes.is_empty()
            && self.pending_operations.is_empty()
            && self.pending_enumerations.is_empty()
            && self.pending_opens.is_empty()
            && self.pending_lookups.is_empty()
            && self.pending_replies.is_empty())
    }

    fn tick(&mut self) -> ModuleResult<()> {
        let mut failure = self.stop_admission_when_available().err();
        if let Err(error) = self.poll_all_requests() {
            failure.get_or_insert(error);
        }
        for _ in 0..8 {
            // Reserve space for a reply before accepting another broker request.
            if self.pending_replies.len() >= MAX_PENDING_REPLIES {
                break;
            }
            if self.next_delivery_order == u64::MAX {
                failure.get_or_insert_with(|| "capture delivery sequence exhausted".into());
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
            let delivery_order = self.next_delivery_order;
            self.next_delivery_order += 1;
            let reply = if service != SERVICE {
                CaptureError::new("invalid_service", "unsupported capture service").payload()
            } else {
                match serde_json::from_value::<Operation>(payload) {
                    Ok(Operation::Enumerate { .. }) if self.shutdown_requested => {
                        CaptureError::new("cancelled", "capture provider is shutting down")
                            .payload()
                    }
                    Ok(Operation::Open {
                        output,
                        operation_id,
                        source_format,
                    }) => {
                        if self.pending_opens.len() < opening::MAX_PENDING_REQUESTS {
                            let mut open = opening::PendingOpen::new(
                                id,
                                caller,
                                output,
                                operation_id,
                                source_format,
                            );
                            open.set_delivery_order(delivery_order);
                            self.pending_opens.push(open);
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
                    Ok(Operation::Enumerate { offset, limit }) => {
                        if self.pending_enumerations.len() < enumeration::MAX_PENDING_REQUESTS {
                            self.pending_enumerations
                                .push(enumeration::PendingEnumeration::new(id, offset, limit));
                            continue;
                        }
                        CaptureError::new(
                            "resource_exhausted",
                            "capture enumeration waiters are full",
                        )
                        .payload()
                    }
                    Ok(Operation::Close { stream }) => {
                        if self.pending_closes.len() < MAX_PENDING_CLOSES {
                            self.pending_closes.push(PendingClose {
                                request: Some(id),
                                caller,
                                delivery_order,
                                stream,
                                target_operation: None,
                                receipt: None,
                                reply: None,
                                admitted: false,
                            });
                            continue;
                        }
                        CaptureError::new("resource_exhausted", "capture close waiters are full")
                            .payload()
                    }
                    Ok(operation @ Operation::Status) => {
                        if self.pending_operations.len() < MAX_PENDING_OPERATIONS {
                            self.pending_operations.push(PendingOperation {
                                request: id,
                                caller,
                                operation: Some(operation),
                                reply: None,
                            });
                            continue;
                        }
                        CaptureError::new("resource_exhausted", "capture request waiters are full")
                            .payload()
                    }
                    Err(error) => CaptureError::new("invalid_request", error.to_string()).payload(),
                }
            };
            self.pending_replies.push(PendingReply {
                request: id,
                payload: reply,
            });
        }
        if let Err(error) = self.poll_all_requests() {
            failure.get_or_insert(error);
        }
        if self.controller.is_poisoned() {
            return Err("capture controller poisoned".into());
        }
        if failure.is_some() {
            if !self.poll_diagnostic_reported {
                let _ = self.runtime.log(
                    LogLevel::Warning,
                    "capture.dxgi",
                    "Capture broker or worker observation failed.",
                );
                self.poll_diagnostic_reported = true;
            }
        } else {
            self.poll_diagnostic_reported = false;
        }
        Ok(())
    }
}

impl CaptureModule {
    fn poll_pending_replies(&mut self) -> ModuleResult<()> {
        let mut failure = None;
        self.pending_replies.retain(|pending| {
            match self.runtime.send(Command::Complete {
                request: pending.request,
                payload: pending.payload.clone(),
            }) {
                Ok(Response::Ok) => false,
                Ok(Response::Error { code, .. })
                    if code == "request_ended" || code == "not_found" =>
                {
                    false
                }
                result => {
                    failure.get_or_insert_with(|| {
                        format!("capture reply completion failed: {result:?}")
                    });
                    // Completion and activity checks are independent: an
                    // unavailable completion path must not retain a slot for
                    // a request whose caller has already timed out or left.
                    !matches!(
                        self.runtime.send(Command::CheckRequest { request: pending.request }),
                        Ok(Response::Error { ref code, .. }) if code == "request_ended" || code == "not_found"
                    )
                }
            }
        });
        failure.map_or(Ok(()), Err)
    }

    fn poll_open_requests(&mut self) -> ModuleResult<()> {
        // Publish the exact current Close targets before any new Open can
        // allocate an ID. If the controller is busy, defer admission until
        // this reservation set can be updated under the same lock.
        match self.controller.try_lock() {
            Ok(mut controller) => controller
                .set_close_id_reservations(self.pending_closes.iter().map(|close| close.stream)),
            Err(std::sync::TryLockError::WouldBlock) => return Ok(()),
            Err(_) => return Err("capture controller poisoned".into()),
        }
        for open in &mut self.pending_opens {
            let fenced = self.pending_closes.iter().any(|close| {
                close.delivery_order < open.delivery_order() && close_may_overlap_open(close, open)
            });
            open.set_close_fenced(fenced);
        }
        opening::poll_requests(
            &self.runtime,
            &self.controller,
            self.shutdown_requested,
            &mut self.pending_opens,
            &mut self.pending_lookups,
        )
    }

    fn poll_enumerations(&mut self) -> ModuleResult<()> {
        enumeration::poll_requests(
            &self.runtime,
            self.shutdown_requested,
            &mut self.pending_enumerations,
            || NativeBackend,
        )
    }

    fn poll_all_requests(&mut self) -> ModuleResult<()> {
        let mut failure = self.poll_pending_closes().err();
        if let Err(error) = self.poll_pending_replies() {
            failure.get_or_insert(error);
        }
        if let Err(error) = self.poll_pending_operations() {
            failure.get_or_insert(error);
        }
        if let Err(error) = self.poll_enumerations() {
            failure.get_or_insert(error);
        }
        if let Err(error) = self.poll_open_requests() {
            failure.get_or_insert(error);
        }
        failure.map_or(Ok(()), Err)
    }

    fn poll_pending_operations(&mut self) -> ModuleResult<()> {
        let mut failure = None;
        let operations = std::mem::take(&mut self.pending_operations);
        for mut pending in operations {
            if pending.operation.is_some() {
                match self.runtime.send(Command::CheckRequest {
                    request: pending.request,
                }) {
                    Ok(Response::Ok) => (),
                    Ok(Response::Error { code, .. })
                        if code == "request_ended" || code == "not_found" =>
                    {
                        continue;
                    }
                    response => {
                        failure.get_or_insert_with(|| {
                            format!("capture request activity check failed: {response:?}")
                        });
                        self.pending_operations.push(pending);
                        continue;
                    }
                }
                let mut controller = match self.controller.try_lock() {
                    Ok(controller) => controller,
                    Err(std::sync::TryLockError::WouldBlock) => {
                        self.pending_operations.push(pending);
                        continue;
                    }
                    Err(_) => {
                        failure.get_or_insert_with(|| "capture controller poisoned".into());
                        self.pending_operations.push(pending);
                        continue;
                    }
                };
                if self.shutdown_requested {
                    controller.stop_admission();
                }
                let request = pending.request;
                pending.reply = Some(controller.handle(
                    &pending.caller,
                    pending.operation.take().expect("pending operation"),
                    &mut || match self.runtime.send(Command::CheckRequest { request }) {
                        Ok(Response::Ok) => Ok(()),
                        response => Err(CaptureError::new(
                            "cancelled",
                            format!("request is no longer active: {response:?}"),
                        )),
                    },
                ));
            }
            let result = self.runtime.send(Command::Complete {
                request: pending.request,
                payload: pending.reply.as_ref().expect("handled operation").clone(),
            });
            match result {
                Ok(Response::Ok) => (),
                result => {
                    if !matches!(result, Ok(Response::Error { ref code, .. }) if code == "request_ended" || code == "not_found")
                        && !matches!(
                            self.runtime.send(Command::CheckRequest { request: pending.request }),
                            Ok(Response::Error { ref code, .. }) if code == "request_ended" || code == "not_found"
                        )
                    {
                        self.pending_operations.push(pending);
                    }
                    if !matches!(result, Ok(Response::Error { ref code, .. }) if code == "request_ended" || code == "not_found")
                    {
                        failure.get_or_insert_with(|| {
                            format!("capture completion failed: {result:?}")
                        });
                    }
                }
            }
        }
        failure.map_or(Ok(()), Err)
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
                        if pending.target_operation.is_none() {
                            pending.target_operation =
                                controller.close_target(&pending.caller, pending.stream);
                        }
                        if self.pending_opens.iter().any(|open| {
                            open.delivery_order() < pending.delivery_order
                                && close_may_overlap_open(pending, open)
                        }) {
                            continue;
                        }
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
                    // A terminal broker request no longer needs a reply. The
                    // receipt or controller still owns any unfinished native
                    // retirement; successful replies exist only after it ends.
                    if !matches!(
                        self.runtime.send(Command::CheckRequest { request }),
                        Ok(Response::Error { ref code, .. }) if code == "request_ended" || code == "not_found"
                    ) {
                        self.pending_closes.push(pending);
                    }
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
