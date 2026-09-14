use crate::model::{Configuration, DisplayError, DisplayResult, Journal, Operation};
use crate::platform::Platform;
use crate::{JOURNAL_KEY, SERVICE, STATE_SERVICE};
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

type Transport = ModuleResult<Response>;
const MAX_QUEUED: usize = 16;
struct Context {
    request: u64,
    caller: String,
    deadline: Instant,
}
struct Incoming {
    context: Context,
    operation: Operation,
}
struct Pending {
    context: Context,
    state_request: u64,
    stage: Stage,
}
enum Stage {
    Loading(Operation),
    Armed {
        journal: Journal,
        configuration: Configuration,
    },
    Clearing(Value),
}
pub struct Pipeline<P> {
    platform: P,
    allow_mutation: bool,
    queued: VecDeque<Incoming>,
    pending: Option<Pending>,
    active_lease: Option<String>,
    sequence: u64,
}
impl<P: Platform> Pipeline<P> {
    pub fn new(platform: P, allow_mutation: bool) -> Self {
        Self {
            platform,
            allow_mutation,
            queued: VecDeque::new(),
            pending: None,
            active_lease: None,
            sequence: 0,
        }
    }
    pub fn tick(&mut self, mut send: impl FnMut(Command) -> Transport) -> ModuleResult<()> {
        if let Some(pending) = self.pending.take() {
            self.advance(pending, &mut send)?;
        }
        if self.queued.len() < MAX_QUEUED {
            self.receive(&mut send)?;
        }
        if self.pending.is_none()
            && let Some(incoming) = self.queued.pop_front()
        {
            self.start(incoming, &mut send)?;
        }
        Ok(())
    }
    fn receive(&mut self, send: &mut impl FnMut(Command) -> Transport) -> ModuleResult<()> {
        match send(Command::Poll {})? {
            Response::Pending => Ok(()),
            Response::Request {
                id,
                caller,
                service,
                payload,
                remaining_ms,
                ..
            } => {
                if service != SERVICE {
                    return complete(
                        send,
                        id,
                        failure("invalid_service", "unsupported display service"),
                    );
                }
                let operation = match serde_json::from_value(payload) {
                    Ok(operation) => operation,
                    Err(error) => {
                        return complete(send, id, failure("invalid_request", error.to_string()));
                    }
                };
                self.queued.push_back(Incoming {
                    context: Context {
                        request: id,
                        caller,
                        deadline: Instant::now() + Duration::from_millis(remaining_ms.min(30_000)),
                    },
                    operation,
                });
                Ok(())
            }
            response => Err(format!("unexpected display poll: {response:?}")),
        }
    }
    fn start(
        &mut self,
        incoming: Incoming,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        if let Operation::Enumerate {
            display,
            offset,
            limit,
        } = incoming.operation
        {
            let limit = limit.unwrap_or(16);
            let offset = offset.unwrap_or(0);
            let result = if limit == 0 || limit > 32 || offset > 4096 {
                Err(DisplayError::new(
                    "invalid_request",
                    "enumeration limit must be 1..32 and offset at most 4096",
                ))
            } else {
                self.platform.enumerate(display.as_deref(), offset, limit)
            };
            return complete(
                send,
                incoming.context.request,
                result.unwrap_or_else(DisplayError::payload),
            );
        }
        let payload = json!({"action":"get","key":JOURNAL_KEY});
        self.submit(
            incoming.context,
            payload,
            Stage::Loading(incoming.operation),
            send,
        )
    }
    fn submit(
        &mut self,
        context: Context,
        payload: Value,
        stage: Stage,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let timeout = context
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u64;
        if timeout == 0 {
            return complete(
                send,
                context.request,
                failure(
                    "deadline_exceeded",
                    "display request deadline elapsed; inspect recovery status",
                ),
            );
        }
        self.sequence = self.sequence.wrapping_add(1);
        match send(Command::Request {
            service: STATE_SERVICE.into(),
            payload,
            timeout_ms: timeout,
            operation_id: format!("display.{}.{}", context.request, self.sequence),
            parent: Some(context.request),
        })? {
            Response::Accepted { id } => {
                self.pending = Some(Pending {
                    context,
                    state_request: id,
                    stage,
                });
                Ok(())
            }
            response => complete(send, context.request, transport_failure(response)),
        }
    }
    fn advance(
        &mut self,
        pending: Pending,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        match send(Command::TakeResult {
            request: pending.state_request,
        })? {
            Response::Pending => {
                self.pending = Some(pending);
                Ok(())
            }
            Response::Result { payload } => {
                if payload["ok"] != true {
                    return complete(
                        send,
                        pending.context.request,
                        if payload["ok"] == false {
                            payload
                        } else {
                            failure("invalid_state_response", "malformed state reply")
                        },
                    );
                }
                let Some(revision) = payload["revision"].as_u64() else {
                    return complete(
                        send,
                        pending.context.request,
                        failure("invalid_state_response", "missing state revision"),
                    );
                };
                match pending.stage {
                    Stage::Loading(operation) => {
                        self.loaded(pending.context, operation, revision, payload, send)
                    }
                    Stage::Armed {
                        journal,
                        configuration,
                    } => self.apply(pending.context, revision, journal, configuration, send),
                    Stage::Clearing(reply) => {
                        self.active_lease = None;
                        complete(send, pending.context.request, reply)
                    }
                }
            }
            response => complete(send, pending.context.request, transport_failure(response)),
        }
    }
    fn loaded(
        &mut self,
        context: Context,
        operation: Operation,
        revision: u64,
        payload: Value,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let journal = match read_journal(payload) {
            Ok(journal) => journal,
            Err(error) => return complete(send, context.request, error.payload()),
        };
        if let Operation::Status = operation {
            let own = journal
                .as_ref()
                .is_some_and(|journal| journal.owner == context.caller);
            let state = match &journal {
                None => "idle",
                Some(journal) if self.active_lease.as_ref() == Some(&journal.lease) => "leased",
                Some(_) => "recovery_required",
            };
            return complete(
                send,
                context.request,
                json!({"ok":true,"platform":self.platform.name(),"native_supported":self.platform.supported(),"mutation_enabled":self.allow_mutation,"state":state,"owned_by_caller":own,"lease":journal.as_ref().filter(|_| own).map(|journal| &journal.lease)}),
            );
        }
        if !self.allow_mutation {
            return complete(
                send,
                context.request,
                failure(
                    "mutation_disabled",
                    "physical display mutations require allow_mutation startup policy",
                ),
            );
        }
        if !self.platform.supported() {
            return complete(
                send,
                context.request,
                failure(
                    "unsupported_platform",
                    "native display mutation is supported only on Windows",
                ),
            );
        }
        match operation {
            Operation::Apply { configuration } => {
                if journal.is_some() {
                    return complete(
                        send,
                        context.request,
                        failure(
                            "display_busy",
                            "a display lease or recovery journal already exists",
                        ),
                    );
                }
                self.prepare(context, revision, configuration, send)
            }
            Operation::Restore { lease } | Operation::Recover { lease } => {
                let Some(journal) = journal else {
                    return complete(
                        send,
                        context.request,
                        failure("not_found", "no display recovery journal exists"),
                    );
                };
                if journal.owner != context.caller || journal.lease != lease {
                    return complete(
                        send,
                        context.request,
                        failure(
                            "permission_denied",
                            "only the journal owner with the current lease can restore displays",
                        ),
                    );
                }
                let result = self
                    .platform
                    .restore(&journal.snapshot, &mut || check_request(send, &context));
                match result {
                    Ok(()) => self.clear(
                        context,
                        revision,
                        json!({"ok":true,"restored":true,"lease":lease}),
                        send,
                    ),
                    Err(error) => complete(
                        send,
                        context.request,
                        recovery_failure(error, &journal.lease),
                    ),
                }
            }
            _ => unreachable!("read operations handled before mutation dispatch"),
        }
    }
    fn prepare(
        &mut self,
        context: Context,
        revision: u64,
        configuration: Configuration,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        if let Err(error) = configuration.validate() {
            return complete(send, context.request, error.payload());
        }
        let snapshot = match self.platform.prepare(&configuration) {
            Ok(snapshot) => snapshot,
            Err(error) => return complete(send, context.request, error.payload()),
        };
        let Some(generation) = revision.checked_add(1) else {
            return complete(
                send,
                context.request,
                failure("resource_exhausted", "display lease generation exhausted"),
            );
        };
        let journal = Journal {
            owner: context.caller.clone(),
            lease: format!("display-{generation:016x}"),
            snapshot,
        };
        if journal.validate().is_err()
            || serde_json::to_vec(&journal).map_or(true, |bytes| bytes.len() > 12_000)
        {
            return complete(
                send,
                context.request,
                failure(
                    "resource_exhausted",
                    "complete display recovery snapshot exceeds journal bounds",
                ),
            );
        }
        let payload =
            json!({"action":"set","key":JOURNAL_KEY,"expected_revision":revision,"value":journal});
        self.submit(
            context,
            payload,
            Stage::Armed {
                journal,
                configuration,
            },
            send,
        )
    }
    fn apply(
        &mut self,
        context: Context,
        revision: u64,
        journal: Journal,
        configuration: Configuration,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let result = self
            .platform
            .apply(&configuration, &journal.snapshot, &mut || {
                check_request(send, &context)
            });
        match result {
            Ok(()) => {
                self.active_lease = Some(journal.lease.clone());
                complete(
                    send,
                    context.request,
                    json!({"ok":true,"applied":true,"lease":journal.lease}),
                )
            }
            Err(error) => {
                // A failed native apply can partially change the desktop. Returning
                // to its durable baseline is cleanup authorized by this same lease,
                // including when the parent ended after the mutation boundary.
                if error.may_have_changed
                    && let Err(rollback) = self.platform.restore(&journal.snapshot, &mut || Ok(()))
                {
                    let mut reply = recovery_failure(error, &journal.lease);
                    reply["rollback_error"] = rollback.payload()["error"].clone();
                    return complete(send, context.request, reply);
                }
                self.clear(context, revision, error.payload(), send)
            }
        }
    }
    fn clear(
        &mut self,
        context: Context,
        revision: u64,
        reply: Value,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let payload = json!({"action":"delete","key":JOURNAL_KEY,"expected_revision":revision});
        self.submit(context, payload, Stage::Clearing(reply), send)
    }
}
fn read_journal(payload: Value) -> DisplayResult<Option<Journal>> {
    match payload["found"].as_bool() {
        Some(false) => Ok(None),
        Some(true) => {
            let journal: Journal =
                serde_json::from_value(payload["value"].clone()).map_err(|_| {
                    DisplayError::new("invalid_journal", "persisted display journal is malformed")
                })?;
            journal.validate()?;
            Ok(Some(journal))
        }
        None => Err(DisplayError::new(
            "invalid_state_response",
            "state lookup omitted found",
        )),
    }
}
fn check_request(
    send: &mut impl FnMut(Command) -> Transport,
    context: &Context,
) -> DisplayResult<()> {
    if Instant::now() >= context.deadline {
        return Err(DisplayError::new(
            "deadline_exceeded",
            "display request expired before native mutation",
        ));
    }
    match send(Command::CheckRequest {
        request: context.request,
    }) {
        Ok(Response::Ok) => Ok(()),
        Ok(Response::Error { message, .. }) => Err(DisplayError::new("cancelled", message)),
        Ok(_) => Err(DisplayError::new(
            "invalid_runtime_response",
            "unexpected active-request check reply",
        )),
        Err(message) => Err(DisplayError::new("runtime_error", message)),
    }
}
fn recovery_failure(error: DisplayError, lease: &str) -> Value {
    let mut payload = error.payload();
    payload["recovery_required"] = true.into();
    payload["lease"] = lease.into();
    payload
}
fn failure(code: &'static str, message: impl Into<String>) -> Value {
    DisplayError::new(code, message).payload()
}
fn transport_failure(response: Response) -> Value {
    match response {
        Response::Error { code, message } => {
            json!({"ok":false,"error":{"code":code,"message":message}})
        }
        _ => failure(
            "invalid_state_response",
            "unexpected display state transport reply",
        ),
    }
}
fn complete(
    send: &mut impl FnMut(Command) -> Transport,
    request: u64,
    payload: Value,
) -> ModuleResult<()> {
    match send(Command::Complete { request, payload })? {
        Response::Ok => Ok(()),
        Response::Error { code, .. } if code == "request_ended" || code == "not_found" => Ok(()),
        response => Err(format!("display completion failed: {response:?}")),
    }
}
#[cfg(test)]
mod tests;
