//! Broker waiters for asynchronous, owned native capture startup.
use crate::backend::Backend;
use crate::controller::{Controller, OpenStart};
use crate::model::Operation;
use amaterasu_module_sdk::capture::SourceFormat;
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, TryLockError};

pub(crate) const MAX_PENDING_REQUESTS: usize = 32;

pub(crate) struct PendingOpen {
    request: Option<u64>,
    caller: String,
    output: String,
    operation_id: Option<String>,
    constant_rate: bool,
    source_format: Option<SourceFormat>,
    token: Option<u64>,
    reply: Option<Value>,
    stream: Option<u64>,
    completion_uncertain: bool,
}

impl PendingOpen {
    pub(crate) fn new(
        request: u64,
        caller: String,
        output: String,
        operation_id: Option<String>,
        constant_rate: bool,
        source_format: Option<SourceFormat>,
    ) -> Self {
        Self {
            request: Some(request),
            caller,
            output,
            operation_id,
            constant_rate,
            source_format,
            token: None,
            reply: None,
            stream: None,
            completion_uncertain: false,
        }
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
    if owner.operation_id.is_none() || (owner.token.is_none() && owner.reply.is_none()) {
        return Transfer::NoWaiter;
    }
    let mut unknown = None;
    for candidate in candidates {
        if candidate.token.is_some()
            || candidate.reply.is_some()
            || candidate.caller != owner.caller
            || candidate.operation_id != owner.operation_id
            || candidate.output != owner.output
            || candidate.constant_rate != owner.constant_rate
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

pub(crate) fn poll_requests<B: Backend>(
    runtime: &RuntimeClient,
    shared: &Arc<Mutex<Controller<B>>>,
    shutting_down: bool,
    opens: &mut Vec<PendingOpen>,
    lookups: &mut Vec<PendingLookup>,
) -> ModuleResult<()> {
    let mut failure = None;
    let mut publication_blocked = false;
    let mut incoming: VecDeque<_> = std::mem::take(opens).into();
    while let Some(mut pending) = incoming.pop_front() {
        if publication_blocked {
            opens.push(pending);
            continue;
        }
        // A replay that appears earlier in this queue must not publish a
        // Ready stream before its original owner settles or hands it off.
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
                    // Other waiters must not publish this operation while its
                    // owning waiter's status and result are unresolved.
                    publication_blocked = true;
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
                    publication_blocked = true;
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
                if controller.poll_open(token, false).is_none() {
                    startup_retiring = true;
                } else {
                    pending.token = None;
                }
            }
            // A completion whose outcome was unknown may have delivered this
            // stream already. Preserve it for exact operation lookup/Close.
            if let Some(stream) = pending.stream
                && (shutting_down || !pending.completion_uncertain)
                && let Err(error) = controller.abandon_open(&pending.caller, stream)
            {
                failure.get_or_insert_with(|| {
                    format!(
                        "capture open retirement failed: {}: {}",
                        error.code, error.message
                    )
                });
                publication_blocked = true;
                opens.push(pending);
                continue;
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
                if let Some((reply, stream)) = controller.poll_open(token, true) {
                    pending.token = None;
                    pending.reply = Some(reply);
                    pending.stream = stream;
                }
            } else {
                match controller.begin_open(
                    &pending.caller,
                    pending.output.clone(),
                    pending.operation_id.clone(),
                    pending.constant_rate,
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
                            publication_blocked = true;
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
                                    publication_blocked = true;
                                    opens.push(pending);
                                }
                            }
                            Err(TryLockError::WouldBlock) => {
                                pending.request = None;
                                publication_blocked = true;
                                opens.push(pending);
                            }
                            Err(TryLockError::Poisoned(_)) => {
                                failure.get_or_insert("capture controller poisoned".into());
                                pending.request = None;
                                publication_blocked = true;
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
    if publication_blocked {
        return failure.map_or(Ok(()), Err);
    }
    for mut pending in std::mem::take(lookups) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_module_sdk::abi::{
        AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_OK, Status,
    };
    use std::ffi::c_void;

    unsafe extern "C" fn ended_request(
        _: *mut c_void,
        _: BorrowedBytes,
        sink: ReplySink,
    ) -> Status {
        let reply = serde_json::to_vec(&Response::Error {
            code: "request_ended".into(),
            message: "fixture ended".into(),
        })
        .unwrap();
        // SAFETY: the caller supplies a valid reply sink for this invocation.
        unsafe { sink.send(&reply) };
        STATUS_OK
    }

    #[test]
    fn failed_abandonment_retains_exact_stream_and_fences_publication() {
        // SAFETY: this endpoint is stateless and valid for the test lifetime.
        let runtime = unsafe {
            RuntimeClient::from_endpoint(HostApi {
                header: AbiHeader::of::<HostApi>(),
                context: std::ptr::null_mut(),
                invoke: ended_request,
            })
        };
        let controller = Arc::new(Mutex::new(Controller::new(
            crate::NativeBackend::new(None),
            true,
        )));
        let mut open = PendingOpen::new(
            1,
            "video".into(),
            "display".into(),
            Some("operation".into()),
            false,
            None,
        );
        open.stream = Some(42);
        open.reply = Some(serde_json::json!({"ok":true,"stream":42}));
        let mut opens = vec![open];
        let mut lookups = vec![PendingLookup::new(2, "video".into(), "operation".into())];
        let error =
            poll_requests(&runtime, &controller, false, &mut opens, &mut lookups).unwrap_err();
        assert!(error.contains("capture open retirement failed"));
        assert_eq!(opens.len(), 1);
        assert_eq!(opens[0].stream, Some(42));
        assert_eq!(lookups.len(), 1);
    }
}
