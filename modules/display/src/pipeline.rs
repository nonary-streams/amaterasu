use crate::configuration_option::DisplayConfigurationOption;
use crate::model::{Configuration, DisplayError, DisplayResult, Journal, Operation, Snapshot};
use crate::platform::Platform;
use crate::{JOURNAL_KEY, SERVICE, STATE_SERVICE};
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde_json::{Value, json};
use std::collections::{BTreeSet, VecDeque};
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
        if let Operation::Apply {
            configuration,
            expected_topology_revision,
            mode_policy: Some(policy),
            mode_policies: None,
            ..
        } = &incoming.operation
            && matches!(
                policy.configuration_option,
                Some(DisplayConfigurationOption::Disabled | DisplayConfigurationOption::VerifyOnly)
            )
        {
            let result = read_only_apply(
                &mut self.platform,
                configuration,
                policy,
                expected_topology_revision.as_deref(),
            );
            return complete(
                send,
                incoming.context.request,
                result.unwrap_or_else(DisplayError::payload),
            );
        }
        if let Operation::Apply {
            mode_policy,
            mode_policies,
            ..
        } = &incoming.operation
            && (mode_policy
                .as_ref()
                .is_some_and(|policy| policy.configuration_option.is_some())
                || mode_policies.as_ref().is_some_and(|policies| {
                    policies
                        .iter()
                        .any(|policy| policy.configuration_option.is_some())
                }))
        {
            return complete(
                send,
                incoming.context.request,
                failure(
                    "unsupported_configuration_option",
                    "topology-changing configuration options require provider runtime support",
                ),
            );
        }
        if let Operation::Snapshot = incoming.operation {
            let result = self.platform.snapshot().and_then(|snapshot| {
                Ok(json!({"ok":true,"configuration":snapshot.configuration()?,"topology_revision":snapshot.revision()?}))
            });
            return complete(
                send,
                incoming.context.request,
                result.unwrap_or_else(DisplayError::payload),
            );
        }
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
                json!({"ok":true,"platform":self.platform.name(),"native_supported":self.platform.supported(),"mutation_enabled":self.allow_mutation,"state":state,"owned_by_caller":own,"lease":journal.as_ref().filter(|_| own).map(|journal| &journal.lease),"operation_id":journal.as_ref().filter(|_| own).and_then(|journal| journal.operation_id.as_ref())}),
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
            Operation::Apply {
                mut configuration,
                expected_topology_revision,
                operation_id,
                mode_policy,
                mode_policies,
            } => {
                if let Some(journal) = journal {
                    if operation_id.is_some()
                        && operation_id == journal.operation_id
                        && journal.owner == context.caller
                    {
                        if mode_policy.is_none()
                            && mode_policies.is_none()
                            && expected_topology_revision == journal.expected_topology_revision
                            && journal
                                .requested_configuration
                                .as_ref()
                                .is_some_and(|saved| {
                                    serde_json::to_value(saved).ok()
                                        == serde_json::to_value(&configuration).ok()
                                })
                        {
                            // The reply may have been lost while a driver or third party
                            // changed the desktop. Never claim an unverified native effect.
                            return complete(
                                send,
                                context.request,
                                json!({"ok":true,"applied":false,"state":"recovery_required","lease":journal.lease,"operation_id":operation_id}),
                            );
                        }
                        return complete(
                            send,
                            context.request,
                            failure(
                                "operation_conflict",
                                "operation identity already has a different display request",
                            ),
                        );
                    }
                    return complete(
                        send,
                        context.request,
                        failure(
                            "display_busy",
                            "a display lease or recovery journal already exists",
                        ),
                    );
                }
                if operation_id.as_ref().is_some_and(|id| {
                    id.is_empty()
                        || id.len() > 128
                        || !id.bytes().all(|byte| byte.is_ascii_graphic())
                }) {
                    return complete(
                        send,
                        context.request,
                        failure(
                            "invalid_request",
                            "operation_id must be 1..128 printable characters",
                        ),
                    );
                }
                if expected_topology_revision.as_ref().is_some_and(|revision| {
                    revision.len() != 32
                        || !revision
                            .bytes()
                            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
                }) {
                    return complete(
                        send,
                        context.request,
                        failure(
                            "invalid_request",
                            "expected_topology_revision must be a provider snapshot token",
                        ),
                    );
                }
                let singular_policy = mode_policies.is_none();
                let policies =
                    match collect_mode_policies(&configuration, mode_policy, mode_policies) {
                        Ok(policies) => policies,
                        Err(error) => return complete(send, context.request, error.payload()),
                    };
                for policy in policies {
                    let modes = if policy.refresh_rate_option
                        == Some(crate::refresh_rate_option::RefreshRateOption::PreferHighest)
                        && !policy.context.client_display_mode_override
                    {
                        match self.platform.available_modes(&policy.display_id) {
                            Ok(modes) => Some(modes),
                            Err(error) => return complete(send, context.request, error.payload()),
                        }
                    } else {
                        None
                    };
                    let resolution = if singular_policy {
                        configuration.resolve_mode_policy(&policy, modes.as_deref())
                    } else {
                        configuration.resolve_mode_policy_in_batch(&policy, modes.as_deref())
                    };
                    if let Err(error) = resolution {
                        return complete(send, context.request, error.payload());
                    }
                }
                self.prepare(
                    context,
                    revision,
                    configuration,
                    expected_topology_revision,
                    operation_id,
                    send,
                )
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
        expected_topology_revision: Option<String>,
        operation_id: Option<String>,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        if let Err(error) = configuration.validate() {
            return complete(send, context.request, error.payload());
        }
        let snapshot = match self.platform.prepare(&configuration) {
            Ok(snapshot) => snapshot,
            Err(error) => return complete(send, context.request, error.payload()),
        };
        if let Some(expected) = &expected_topology_revision {
            if snapshot.revision().as_ref().ok() != Some(expected) {
                return complete(
                    send,
                    context.request,
                    failure(
                        "topology_changed",
                        "active display topology changed before journal prepare",
                    ),
                );
            }
            if let Err(error) = snapshot
                .configuration()
                .and_then(|_| validate_active_ids(&configuration, &snapshot))
            {
                return complete(send, context.request, error.payload());
            }
        }
        let readback = self
            .platform
            .snapshot()
            .and_then(|current| current.revision());
        if readback.as_ref().ok() != snapshot.revision().as_ref().ok() {
            return complete(
                send,
                context.request,
                failure(
                    "topology_changed",
                    "active display topology changed during journal prepare",
                ),
            );
        }
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
            operation_id,
            requested_configuration: Some(configuration.clone()),
            expected_topology_revision,
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
                    json!({"ok":true,"applied":true,"lease":journal.lease,"operation_id":journal.operation_id}),
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
fn read_only_apply<P: Platform>(
    platform: &mut P,
    configuration: &Configuration,
    policy: &crate::model::ModePolicy,
    expected_revision: Option<&str>,
) -> DisplayResult<Value> {
    configuration.validate()?;
    if !configuration
        .displays
        .iter()
        .any(|display| display.id.eq_ignore_ascii_case(&policy.display_id))
    {
        return Err(DisplayError::new(
            "invalid_request",
            "mode policy display is not in the configuration",
        ));
    }
    if policy.configuration_option == Some(DisplayConfigurationOption::Disabled) {
        return Ok(json!({"ok":true,"applied":false,"configuration_option":"disabled"}));
    }
    let snapshot = platform.snapshot()?;
    snapshot.configuration()?;
    if let Some(expected) = expected_revision
        && snapshot.revision()? != expected
    {
        return Err(DisplayError::new(
            "topology_changed",
            "active display topology changed before verification",
        ));
    }
    if !snapshot
        .paths
        .iter()
        .any(|path| path.id.eq_ignore_ascii_case(&policy.display_id))
    {
        return Err(DisplayError::new(
            "unsupported_display",
            "verify_only requires an active display",
        ));
    }
    Ok(json!({"ok":true,"applied":false,"verified":true,"configuration_option":"verify_only"}))
}
fn validate_active_ids(configuration: &Configuration, snapshot: &Snapshot) -> DisplayResult<()> {
    let requested: BTreeSet<_> = configuration
        .displays
        .iter()
        .map(|display| display.id.to_lowercase())
        .collect();
    let active: BTreeSet<_> = snapshot
        .paths
        .iter()
        .map(|path| path.id.to_lowercase())
        .collect();
    if requested != active {
        return Err(DisplayError::new(
            "unsupported_display",
            "revisioned apply requires exactly the active display identities",
        ));
    }
    Ok(())
}
fn collect_mode_policies(
    configuration: &Configuration,
    mode_policy: Option<crate::model::ModePolicy>,
    mode_policies: Option<Vec<crate::model::ModePolicy>>,
) -> DisplayResult<Vec<crate::model::ModePolicy>> {
    configuration.validate()?;
    if mode_policy.is_some() && mode_policies.is_some() {
        return Err(DisplayError::new(
            "invalid_request",
            "apply accepts either mode_policy or mode_policies, not both",
        ));
    }
    let policies = mode_policies.unwrap_or_else(|| mode_policy.into_iter().collect());
    let mut targets = BTreeSet::new();
    for policy in &policies {
        if !targets.insert(policy.display_id.to_lowercase()) {
            return Err(DisplayError::new(
                "invalid_request",
                "mode policies must target distinct displays",
            ));
        }
        if !configuration
            .displays
            .iter()
            .any(|display| display.id.eq_ignore_ascii_case(&policy.display_id))
        {
            return Err(DisplayError::new(
                "invalid_request",
                "mode policy display is not in the configuration",
            ));
        }
    }
    Ok(policies)
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
