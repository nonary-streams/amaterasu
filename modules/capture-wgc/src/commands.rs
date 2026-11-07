//! Bounded broker waiters for read-only capture commands.
use crate::backend::Backend;
use crate::controller::{Controller, enumeration_reply};
use crate::model::{CaptureError, Operation};
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};
use serde_json::Value;
use std::sync::{Arc, Mutex, TryLockError};
use std::thread::JoinHandle;

pub(crate) const MAX_PENDING_COMMANDS: usize = 32;

pub(crate) struct PendingCommand {
    request: Option<u64>,
    caller: String,
    operation: Option<Operation>,
    worker: Option<JoinHandle<Value>>,
    reply: Option<Value>,
}

impl PendingCommand {
    pub(crate) fn new(request: u64, caller: String, operation: Operation) -> Self {
        Self {
            request: Some(request),
            caller,
            operation: Some(operation),
            worker: None,
            reply: None,
        }
    }
}

impl Drop for PendingCommand {
    fn drop(&mut self) {
        // A native enumeration may still execute provider code. Never detach it
        // when the module is destroyed without a successful quiescence.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub(crate) fn poll_requests<B: Backend>(
    runtime: &RuntimeClient,
    shared: &Arc<Mutex<Controller<B>>>,
    shutting_down: bool,
    pending: &mut Vec<PendingCommand>,
) -> ModuleResult<()> {
    let mut failure = None;
    for mut command in std::mem::take(pending) {
        if shutting_down {
            command.request = None;
        } else if let Some(request) = command.request {
            match runtime.send(Command::CheckRequest { request }) {
                Ok(Response::Ok) => (),
                Ok(Response::Error { code, .. })
                    if code == "request_ended" || code == "not_found" =>
                {
                    command.request = None
                }
                response => {
                    failure.get_or_insert_with(|| {
                        format!("capture command activity check failed: {response:?}")
                    });
                    pending.push(command);
                    continue;
                }
            }
        }
        if let Some(worker) = command.worker.as_ref() {
            match crate::worker::thread_finished(worker) {
                Ok(true) => (),
                Ok(false) => {
                    pending.push(command);
                    continue;
                }
                Err(error) => {
                    failure.get_or_insert_with(|| format!("{}: {}", error.code, error.message));
                    pending.push(command);
                    continue;
                }
            }
            let worker = command.worker.take().expect("checked worker");
            command.reply = Some(worker.join().unwrap_or_else(|_| {
                CaptureError::new("internal_error", "capture enumeration worker panicked").payload()
            }));
        }
        if command.request.is_none() {
            continue;
        }
        if command.reply.is_none() {
            let mut controller = match shared.try_lock() {
                Ok(controller) => controller,
                Err(TryLockError::WouldBlock) => {
                    pending.push(command);
                    continue;
                }
                Err(TryLockError::Poisoned(_)) => {
                    failure.get_or_insert("capture controller poisoned".into());
                    pending.push(command);
                    continue;
                }
            };
            match command.operation.take().expect("unstarted command") {
                Operation::Status => {
                    command.reply =
                        Some(controller.handle(&command.caller, Operation::Status, &mut || Ok(())))
                }
                Operation::Enumerate { offset, limit } => {
                    let backend = controller.backend();
                    drop(controller);
                    match std::thread::Builder::new()
                        .name("capture-wgc-enumerate".into())
                        .spawn(move || {
                            backend
                                .enumerate()
                                .and_then(|outputs| enumeration_reply(outputs, offset, limit))
                                .unwrap_or_else(CaptureError::payload)
                        }) {
                        Ok(worker) => {
                            command.worker = Some(worker);
                            pending.push(command);
                            continue;
                        }
                        Err(error) => {
                            command.reply = Some(
                                CaptureError::new(
                                    "internal_error",
                                    format!("capture enumeration worker could not start: {error}"),
                                )
                                .payload(),
                            )
                        }
                    }
                }
                _ => unreachable!("only read-only commands are queued"),
            }
        }
        let request = command.request.expect("active command");
        match runtime.send(Command::Complete {
            request,
            payload: command.reply.as_ref().expect("command reply").clone(),
        }) {
            Ok(Response::Ok) => (),
            Ok(Response::Error { code, .. }) if code == "request_ended" || code == "not_found" => {}
            response => {
                failure.get_or_insert_with(|| {
                    format!("capture command completion failed: {response:?}")
                });
                pending.push(command);
            }
        }
    }
    failure.map_or(Ok(()), Err)
}
