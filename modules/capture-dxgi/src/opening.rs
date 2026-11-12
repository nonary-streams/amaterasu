//! Broker waiters for asynchronous, owned native capture startup.
use crate::backend::Backend;
use crate::controller::{Controller, OpenStart};
use crate::model::Operation;
use amaterasu_module_sdk::capture::SourceFormat;
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};
use serde_json::Value;
use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex, TryLockError};

pub(crate) const MAX_PENDING_REQUESTS: usize = 32;

pub(crate) struct PendingOpen {
    request: Option<u64>,
    caller: String,
    delivery_order: u64,
    close_fenced: bool,
    output: String,
    operation_id: Option<String>,
    source_format: Option<SourceFormat>,
    token: Option<u64>,
    reply: Option<Value>,
    stream: Option<u64>,
    completion_uncertain: bool,
    retirement_failed: bool,
}

impl PendingOpen {
    pub(crate) fn new(
        request: u64,
        caller: String,
        output: String,
        operation_id: Option<String>,
        source_format: Option<SourceFormat>,
    ) -> Self {
        Self {
            request: Some(request),
            caller,
            delivery_order: 0,
            close_fenced: false,
            output,
            operation_id,
            source_format,
            token: None,
            reply: None,
            stream: None,
            completion_uncertain: false,
            retirement_failed: false,
        }
    }

    pub(crate) fn set_delivery_order(&mut self, order: u64) {
        self.delivery_order = order;
    }

    pub(crate) fn delivery_order(&self) -> u64 {
        self.delivery_order
    }

    pub(crate) fn caller(&self) -> &str {
        &self.caller
    }

    pub(crate) fn operation_id(&self) -> Option<&str> {
        self.operation_id.as_deref()
    }

    pub(crate) fn target_stream(&self) -> Option<u64> {
        self.stream.or(self.token).or_else(|| {
            self.reply.as_ref().and_then(|reply| {
                (reply.get("ok") == Some(&Value::Bool(true)))
                    .then(|| reply.get("stream").and_then(Value::as_u64))
                    .flatten()
            })
        })
    }

    pub(crate) fn set_close_fenced(&mut self, fenced: bool) {
        self.close_fenced = fenced;
    }
}

pub(crate) struct PendingLookup {
    request: u64,
    caller: String,
    operation_id: String,
    reply: Option<Value>,
}

impl PendingLookup {
    pub(crate) fn new(request: u64, caller: String, operation_id: String) -> Self {
        Self {
            request,
            caller,
            operation_id,
            reply: None,
        }
    }
}

enum Activity {
    Active,
    Ended,
    Unknown(String),
}

fn activity(runtime: &RuntimeClient, request: u64) -> Activity {
    match runtime.send(Command::CheckRequest { request }) {
        Ok(Response::Ok) => Activity::Active,
        Ok(Response::Error { code, .. }) if code == "request_ended" || code == "not_found" => {
            Activity::Ended
        }
        other => Activity::Unknown(format!("capture request activity check failed: {other:?}")),
    }
}

enum Transfer {
    Transferred,
    NoWaiter,
    Unknown(String),
}

// Transfer ownership at the point the previous waiter ends. A preflight check
// would race with cancellation between that check and the actual abandonment.
fn transfer_to_replay<'a>(
    runtime: &RuntimeClient,
    owner: &mut PendingOpen,
    candidates: impl Iterator<Item = &'a mut PendingOpen>,
) -> Transfer {
    if owner.retirement_failed
        || owner.operation_id.is_none()
        || (owner.token.is_none() && owner.reply.is_none())
    {
        return Transfer::NoWaiter;
    }
    let mut unknown = None;
    for candidate in candidates {
        if candidate.close_fenced
            || candidate.token.is_some()
            || candidate.reply.is_some()
            || candidate.caller != owner.caller
            || candidate.operation_id != owner.operation_id
            || candidate.output != owner.output
            || !same_format(candidate, owner)
        {
            continue;
        }
        let Some(request) = candidate.request else {
            continue;
        };
        match activity(runtime, request) {
            Activity::Active => {
                candidate.token = owner.token.take();
                candidate.reply = owner.reply.take();
                candidate.stream = owner.stream.take();
                candidate.completion_uncertain = owner.completion_uncertain;
                return Transfer::Transferred;
            }
            Activity::Ended => (),
            Activity::Unknown(error) => {
                unknown.get_or_insert(error);
            }
        }
    }
    unknown.map_or(Transfer::NoWaiter, Transfer::Unknown)
}

fn same_format(candidate: &PendingOpen, owner: &PendingOpen) -> bool {
    match (
        crate::format::CaptureFormat::requested(candidate.source_format.as_ref()),
        crate::format::CaptureFormat::requested(owner.source_format.as_ref()),
    ) {
        (Ok(candidate), Ok(owner)) => candidate == owner,
        _ => candidate.source_format == owner.source_format,
    }
}

fn block_operation(blocked: &mut BTreeSet<(String, String)>, pending: &PendingOpen) {
    if let Some(operation_id) = &pending.operation_id {
        blocked.insert((pending.caller.clone(), operation_id.clone()));
    }
}

fn operation_blocked(blocked: &BTreeSet<(String, String)>, caller: &str, id: &str) -> bool {
    blocked.contains(&(caller.to_owned(), id.to_owned()))
}

pub(crate) fn poll_requests<B: Backend>(
    runtime: &RuntimeClient,
    shared: &Arc<Mutex<Controller<B>>>,
    shutting_down: bool,
    opens: &mut Vec<PendingOpen>,
    lookups: &mut Vec<PendingLookup>,
) -> ModuleResult<()> {
    let mut failure = None;
    let mut blocked_operations = BTreeSet::new();
    let mut incoming: VecDeque<_> = std::mem::take(opens).into();
    while let Some(mut pending) = incoming.pop_front() {
        if pending.close_fenced {
            opens.push(pending);
            continue;
        }
        if pending
            .operation_id
            .as_deref()
            .is_some_and(|id| operation_blocked(&blocked_operations, &pending.caller, id))
        {
            opens.push(pending);
            continue;
        }
        // A replay queued before the owning waiter cannot publish its stream
        // while that waiter may still have to hand off or abandon the open.
        if pending.token.is_none()
            && pending.reply.is_none()
            && pending.operation_id.is_some()
            && opens.iter().chain(incoming.iter()).any(|other| {
                other.caller == pending.caller
                    && other.operation_id == pending.operation_id
                    && (other.token.is_some() || other.reply.is_some())
            })
        {
            opens.push(pending);
            continue;
        }
        let active = match pending.request.map(|request| activity(runtime, request)) {
            Some(Activity::Active) => true,
            Some(Activity::Ended) | None => false,
            Some(Activity::Unknown(error)) => {
                failure.get_or_insert(error);
                if pending.token.is_some() || pending.reply.is_some() {
                    block_operation(&mut blocked_operations, &pending);
                }
                opens.push(pending);
                continue;
            }
        };
        let abandon = shutting_down || !active;
        if !shutting_down && !active {
            match transfer_to_replay(
                runtime,
                &mut pending,
                opens.iter_mut().chain(incoming.iter_mut()),
            ) {
                Transfer::Transferred => continue,
                Transfer::Unknown(error) => {
                    failure.get_or_insert(error);
                    block_operation(&mut blocked_operations, &pending);
                    opens.push(pending);
                    continue;
                }
                Transfer::NoWaiter => (),
            }
        }
        let mut controller = match shared.try_lock() {
            Ok(controller) => controller,
            Err(TryLockError::WouldBlock) => {
                opens.push(pending);
                continue;
            }
            Err(TryLockError::Poisoned(_)) => {
                failure.get_or_insert("capture controller poisoned".into());
                opens.push(pending);
                continue;
            }
        };
        if abandon {
            let mut startup_retiring = false;
            if let Some(token) = pending.token {
                controller.cancel_open(token);
                match controller.poll_open(token, false) {
                    Ok(None) => startup_retiring = true,
                    Ok(Some(_)) => pending.token = None,
                    Err(error) => {
                        failure.get_or_insert_with(|| {
                            format!(
                                "capture startup retirement failed: {}: {}",
                                error.code, error.message
                            )
                        });
                        drop(controller);
                        block_operation(&mut blocked_operations, &pending);
                        opens.push(pending);
                        continue;
                    }
                }
            }
            // A completion whose outcome was unknown may have delivered this
            // stream already. Preserve it for exact operation lookup/Close.
            // If delivery was uncertain, the consumer may already have
            // closed this stream. Global quiescence independently retires
            // every remaining stream; do not infer a missing receipt here.
            if let Some(stream) = pending.stream
                && !pending.completion_uncertain
            {
                if let Err(error) = controller.abandon_open(&pending.caller, stream) {
                    failure.get_or_insert_with(|| {
                        format!(
                            "capture open retirement failed: {}: {}",
                            error.code, error.message
                        )
                    });
                    drop(controller);
                    pending.retirement_failed = true;
                    block_operation(&mut blocked_operations, &pending);
                    opens.push(pending);
                    continue;
                }
                pending.stream = None;
            }
            drop(controller);
            if shutting_down && active {
                let request = pending.request.expect("active open request");
                let reply = crate::model::CaptureError::new(
                    "cancelled",
                    "capture provider is shutting down",
                )
                .payload();
                match runtime.send(Command::Complete {
                    request,
                    payload: reply,
                }) {
                    Ok(Response::Ok) => pending.request = None,
                    Ok(Response::Error { code, .. })
                        if code == "request_ended" || code == "not_found" =>
                    {
                        pending.request = None
                    }
                    result => {
                        failure.get_or_insert_with(|| {
                            format!("capture shutdown completion failed: {result:?}")
                        });
                        opens.push(pending);
                        continue;
                    }
                }
            } else if !active {
                pending.request = None;
            }
            if startup_retiring {
                opens.push(pending);
            }
            continue;
        }
        if pending.reply.is_none() {
            if let Some(token) = pending.token {
                match controller.poll_open(token, true) {
                    Ok(Some((reply, stream))) => {
                        pending.token = None;
                        pending.reply = Some(reply);
                        pending.stream = stream;
                    }
                    Ok(None) => (),
                    Err(error) => {
                        failure.get_or_insert_with(|| {
                            format!(
                                "capture startup observation failed: {}: {}",
                                error.code, error.message
                            )
                        });
                        drop(controller);
                        block_operation(&mut blocked_operations, &pending);
                        opens.push(pending);
                        continue;
                    }
                }
            } else {
                match controller.begin_open(
                    &pending.caller,
                    pending.output.clone(),
                    pending.operation_id.clone(),
                    pending.source_format,
                ) {
                    Ok(OpenStart::Ready(reply)) => pending.reply = Some(reply),
                    Ok(OpenStart::Pending(token)) => pending.token = Some(token),
                    Err(error)
                        if error.code == "cleanup_pending"
                            && pending.operation_id.as_deref().is_some_and(|id| {
                                controller.operation_pending(&pending.caller, id)
                            }) => {}
                    Err(error) => pending.reply = Some(error.payload()),
                }
            }
        }
        drop(controller);
        if let (Some(request), Some(reply)) = (pending.request, pending.reply.as_ref()) {
            match runtime.send(Command::Complete {
                request,
                payload: reply.clone(),
            }) {
                Ok(Response::Ok) => continue,
                Ok(Response::Error { code, .. })
                    if code == "request_ended" || code == "not_found" =>
                {
                    match transfer_to_replay(
                        runtime,
                        &mut pending,
                        opens.iter_mut().chain(incoming.iter_mut()),
                    ) {
                        Transfer::Transferred => continue,
                        Transfer::Unknown(error) => {
                            failure.get_or_insert(error);
                            block_operation(&mut blocked_operations, &pending);
                            pending.request = None;
                            opens.push(pending);
                            continue;
                        }
                        Transfer::NoWaiter => (),
                    }
                    if let Some(stream) = pending.stream
                        && !pending.completion_uncertain
                    {
                        match shared.try_lock() {
                            Ok(mut controller) => {
                                if let Err(error) = controller.abandon_open(&pending.caller, stream)
                                {
                                    failure.get_or_insert_with(|| {
                                        format!(
                                            "capture open retirement failed: {}: {}",
                                            error.code, error.message
                                        )
                                    });
                                    pending.request = None;
                                    pending.retirement_failed = true;
                                    block_operation(&mut blocked_operations, &pending);
                                    opens.push(pending);
                                }
                            }
                            Err(TryLockError::WouldBlock) => {
                                pending.request = None;
                                block_operation(&mut blocked_operations, &pending);
                                opens.push(pending);
                            }
                            Err(TryLockError::Poisoned(_)) => {
                                failure.get_or_insert("capture controller poisoned".into());
                                pending.request = None;
                                block_operation(&mut blocked_operations, &pending);
                                opens.push(pending);
                            }
                        }
                    }
                    continue;
                }
                result => {
                    pending.completion_uncertain = true;
                    failure.get_or_insert_with(|| {
                        format!("capture open completion failed: {result:?}")
                    });
                }
            }
        }
        opens.push(pending);
    }
    for mut pending in std::mem::take(lookups) {
        if operation_blocked(&blocked_operations, &pending.caller, &pending.operation_id) {
            lookups.push(pending);
            continue;
        }
        match activity(runtime, pending.request) {
            Activity::Ended => continue,
            Activity::Unknown(error) => {
                failure.get_or_insert(error);
                lookups.push(pending);
                continue;
            }
            Activity::Active => (),
        }
        if pending.reply.is_none() {
            if opens.iter().any(|open| {
                open.caller == pending.caller
                    && open.operation_id.as_deref() == Some(pending.operation_id.as_str())
            }) {
                lookups.push(pending);
                continue;
            }
            let mut controller = match shared.try_lock() {
                Ok(controller) => controller,
                Err(TryLockError::WouldBlock) => {
                    lookups.push(pending);
                    continue;
                }
                Err(TryLockError::Poisoned(_)) => {
                    failure.get_or_insert("capture controller poisoned".into());
                    lookups.push(pending);
                    continue;
                }
            };
            if controller.operation_pending(&pending.caller, &pending.operation_id) {
                lookups.push(pending);
                continue;
            }
            pending.reply = Some(controller.handle(
                &pending.caller,
                Operation::Lookup {
                    operation_id: pending.operation_id.clone(),
                },
                &mut || Ok(()),
            ));
        }
        match runtime.send(Command::Complete {
            request: pending.request,
            payload: pending.reply.as_ref().expect("lookup reply").clone(),
        }) {
            Ok(Response::Ok) => (),
            Ok(Response::Error { code, .. }) if code == "request_ended" || code == "not_found" => {}
            result => {
                failure
                    .get_or_insert_with(|| format!("capture lookup completion failed: {result:?}"));
                lookups.push(pending);
            }
        }
    }
    failure.map_or(Ok(()), Err)
}
