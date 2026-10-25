use crate::configuration_option::DisplayConfigurationOption;
use crate::model::{
    Configuration, DisplayError, DisplayResult, Journal, Operation, Rational, RecoveryBaseline,
    RouteBinding, Snapshot,
};
use crate::platform::{Platform, TopologyShape};
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
struct PrepareIntent {
    expected_topology_revision: Option<String>,
    selection_revision: Option<String>,
    preferred_modes: Vec<(String, u32, u32, Rational)>,
    operation_id: Option<String>,
    request_identity: Value,
    topology_change: bool,
    native_target: Option<(String, TopologyShape)>,
    pending_route: Option<RouteBinding>,
}
struct KScreenLeaseIntent {
    configuration: Configuration,
    expected_topology_revision: Option<String>,
    operation_id: Option<String>,
    request_identity: Value,
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
        native_target: Option<(String, TopologyShape)>,
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
            && (mode_policy.is_some() && mode_policies.is_some()
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
                    "invalid_request",
                    "configuration options require one selected mode_policy",
                ),
            );
        }
        if let Operation::Snapshot = incoming.operation {
            let result = self.platform.read_configuration_snapshot().map(|snapshot| {
                let mut payload = snapshot;
                if let Some(object) = payload.as_object_mut() {
                    object.insert("ok".into(), json!(true));
                }
                payload
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
                        native_target,
                    } => self.apply(
                        pending.context,
                        revision,
                        journal,
                        configuration,
                        native_target,
                        send,
                    ),
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
                let request_identity = serde_json::to_value(Operation::Apply {
                    configuration: configuration.clone(),
                    expected_topology_revision: expected_topology_revision.clone(),
                    operation_id: operation_id.clone(),
                    mode_policy: mode_policy.clone(),
                    mode_policies: mode_policies.clone(),
                })
                .expect("parsed display request serializes");
                if let Some(journal) = journal {
                    if operation_id.is_some()
                        && operation_id == journal.operation_id
                        && journal.owner == context.caller
                    {
                        if journal.request_identity.as_ref() == Some(&request_identity)
                            || (journal.request_identity.is_none()
                                && mode_policy.is_none()
                                && mode_policies.is_none()
                                && expected_topology_revision == journal.expected_topology_revision
                                && journal
                                    .requested_configuration
                                    .as_ref()
                                    .is_some_and(|saved| {
                                        serde_json::to_value(saved).ok()
                                            == serde_json::to_value(&configuration).ok()
                                    }))
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
                let kscreen_baseline = match self.platform.capture_kscreen_snapshot() {
                    Ok(snapshot) => snapshot,
                    Err(error) => return complete(send, context.request, error.payload()),
                };
                if let Some(kscreen_baseline) = kscreen_baseline {
                    if mode_policy.is_some() || mode_policies.is_some() {
                        return complete(
                            send,
                            context.request,
                            failure(
                                "unsupported_topology",
                                "KScreen direct apply does not yet support mode or topology policies",
                            ),
                        );
                    }
                    return self.prepare_kscreen_lease(
                        context,
                        revision,
                        KScreenLeaseIntent {
                            configuration,
                            expected_topology_revision,
                            operation_id,
                            request_identity,
                        },
                        kscreen_baseline,
                        send,
                    );
                }
                let topology_option = mode_policy
                    .as_ref()
                    .and_then(|policy| policy.configuration_option);
                let topology_change = matches!(
                    topology_option,
                    Some(
                        DisplayConfigurationOption::EnsureActive
                            | DisplayConfigurationOption::EnsurePrimary
                            | DisplayConfigurationOption::EnsureOnlyDisplay
                    )
                );
                let mut native_target = None;
                let mut pending_route = None;
                let mut clone_baseline = None;
                if topology_change {
                    let Some(expected) = expected_topology_revision.as_deref() else {
                        return complete(
                            send,
                            context.request,
                            failure(
                                "invalid_request",
                                "topology-changing configuration option requires expected_topology_revision",
                            ),
                        );
                    };
                    let snapshot = match self.platform.snapshot() {
                        Ok(snapshot) => snapshot,
                        Err(error) => return complete(send, context.request, error.payload()),
                    };
                    clone_baseline = Some(snapshot.clone());
                    if snapshot.revision().as_ref().ok().map(String::as_str) != Some(expected) {
                        return complete(
                            send,
                            context.request,
                            failure(
                                "topology_changed",
                                "active display topology changed before configuration option",
                            ),
                        );
                    }
                    if topology_option == Some(DisplayConfigurationOption::EnsureOnlyDisplay)
                        && let Some(policy) = mode_policy.as_ref()
                        && snapshot.paths.iter().any(|path| {
                            !path.id.eq_ignore_ascii_case(&policy.display_id)
                                && path.hdr_enabled.is_none()
                        })
                    {
                        return complete(
                            send,
                            context.request,
                            failure(
                                "unsupported_topology",
                                "deactivated display has no queryable HDR recovery baseline",
                            ),
                        );
                    }
                    if matches!(
                        topology_option,
                        Some(
                            DisplayConfigurationOption::EnsureActive
                                | DisplayConfigurationOption::EnsurePrimary
                        )
                    ) && snapshot.paths.len() > 1
                    {
                        native_target = mode_policy.as_ref().map(|policy| {
                            (
                                policy.display_id.clone(),
                                if topology_option
                                    == Some(DisplayConfigurationOption::EnsurePrimary)
                                {
                                    TopologyShape::PrimaryActive
                                } else {
                                    TopologyShape::PreserveActive
                                },
                            )
                        });
                    }
                    if matches!(
                        topology_option,
                        Some(
                            DisplayConfigurationOption::EnsureActive
                                | DisplayConfigurationOption::EnsurePrimary
                                | DisplayConfigurationOption::EnsureOnlyDisplay
                        )
                    ) && let Some(policy) = mode_policy.as_ref()
                        && !snapshot
                            .paths
                            .iter()
                            .any(|path| path.id.eq_ignore_ascii_case(&policy.display_id))
                    {
                        if configuration.displays.iter().any(|display| {
                            display.id.eq_ignore_ascii_case(&policy.display_id)
                                && display.hdr_enabled == Some(true)
                        }) {
                            return complete(
                                send,
                                context.request,
                                failure(
                                    "unsupported_display",
                                    "inactive HDR target has no recoverable baseline",
                                ),
                            );
                        }
                        pending_route = match self
                            .platform
                            .bind_inactive(&policy.display_id, &snapshot)
                        {
                            Ok(route) => Some(route),
                            Err(error) => return complete(send, context.request, error.payload()),
                        };
                        native_target = Some((
                            policy.display_id.clone(),
                            if topology_option
                                == Some(DisplayConfigurationOption::EnsureOnlyDisplay)
                            {
                                TopologyShape::OnlySelected
                            } else if topology_option
                                == Some(DisplayConfigurationOption::EnsurePrimary)
                            {
                                TopologyShape::TranslateActive
                            } else {
                                TopologyShape::PreserveActive
                            },
                        ));
                    }
                    configuration = match compose_configuration(
                        &configuration,
                        &snapshot,
                        mode_policy.as_ref().expect("topology policy exists"),
                    ) {
                        Ok(configuration) => configuration,
                        Err(error) => return complete(send, context.request, error.payload()),
                    };
                }
                let policies =
                    match collect_mode_policies(&configuration, mode_policy, mode_policies) {
                        Ok(policies) => policies,
                        Err(error) => return complete(send, context.request, error.payload()),
                    };
                if !policies.is_empty() && clone_baseline.is_none() {
                    clone_baseline = match self.platform.snapshot() {
                        Ok(snapshot) => Some(snapshot),
                        Err(error) => return complete(send, context.request, error.payload()),
                    };
                }
                let selection_revision = clone_baseline.as_ref().map(physical_revision).transpose();
                let selection_revision = match selection_revision {
                    Ok(revision) => revision,
                    Err(error) => return complete(send, context.request, error.payload()),
                };
                let mut preferred_modes = Vec::new();
                let singular_nonclone = policies.len() == 1
                    && !configuration
                        .displays
                        .iter()
                        .enumerate()
                        .any(|(index, left)| {
                            configuration.displays.iter().skip(index + 1).any(|right| {
                                left.x == right.x
                                    && left.y == right.y
                                    && left.width == right.width
                                    && left.height == right.height
                            })
                        });
                let mut resolved_clone_modes = std::collections::BTreeMap::new();
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
                    let resolution = if singular_nonclone {
                        configuration.resolve_mode_policy(&policy, modes.as_deref())
                    } else {
                        configuration.resolve_mode_policy_in_batch(&policy, modes.as_deref())
                    };
                    if let Err(error) = resolution {
                        return complete(send, context.request, error.payload());
                    }
                    if modes.is_some() {
                        let selected = configuration
                            .displays
                            .iter()
                            .find(|display| display.id.eq_ignore_ascii_case(&policy.display_id))
                            .expect("policy display validated");
                        preferred_modes.push((
                            policy.display_id.clone(),
                            selected.width,
                            selected.height,
                            selected.refresh,
                        ));
                    }
                    if clone_baseline.is_none() {
                        clone_baseline = match self.platform.snapshot() {
                            Ok(snapshot) => Some(snapshot),
                            Err(error) => return complete(send, context.request, error.payload()),
                        };
                    }
                    if let Some(snapshot) = &clone_baseline {
                        if let Some(path) = snapshot
                            .paths
                            .iter()
                            .find(|path| path.id.eq_ignore_ascii_case(&policy.display_id))
                        {
                            let source = (
                                path.source_address.adapter_low,
                                path.source_address.adapter_high,
                                path.source_address.id,
                            );
                            let wanted = configuration
                                .displays
                                .iter()
                                .find(|display| display.id.eq_ignore_ascii_case(&policy.display_id))
                                .expect("policy display validated");
                            let mode = (wanted.width, wanted.height, wanted.x, wanted.y);
                            if resolved_clone_modes
                                .insert(source, mode)
                                .is_some_and(|earlier| earlier != mode)
                            {
                                return complete(
                                    send,
                                    context.request,
                                    failure(
                                        "unsupported_topology",
                                        "clone targets requested conflicting source modes",
                                    ),
                                );
                            }
                        }
                        synchronize_clone_group(&mut configuration, snapshot, &policy.display_id);
                    }
                }
                if let Err(error) = configuration.validate() {
                    return complete(send, context.request, error.payload());
                }
                if !topology_change
                    && expected_topology_revision.is_none()
                    && configuration
                        .displays
                        .iter()
                        .enumerate()
                        .any(|(index, left)| {
                            configuration.displays.iter().skip(index + 1).any(|right| {
                                left.x == right.x
                                    && left.y == right.y
                                    && left.width == right.width
                                    && left.height == right.height
                            })
                        })
                {
                    let baseline = match self.platform.snapshot() {
                        Ok(snapshot) => snapshot,
                        Err(error) => return complete(send, context.request, error.payload()),
                    };
                    if configuration.displays.iter().any(|display| {
                        !baseline
                            .paths
                            .iter()
                            .any(|path| path.id.eq_ignore_ascii_case(&display.id))
                    }) {
                        return complete(
                            send,
                            context.request,
                            failure(
                                "invalid_request",
                                "inactive clone activation requires expected_topology_revision",
                            ),
                        );
                    }
                }
                if !topology_change && expected_topology_revision.is_some() {
                    let baseline = match self.platform.snapshot() {
                        Ok(snapshot) => snapshot,
                        Err(error) => return complete(send, context.request, error.payload()),
                    };
                    if let Some(expected) = &expected_topology_revision
                        && baseline.revision().as_ref().ok() != Some(expected)
                    {
                        return complete(
                            send,
                            context.request,
                            failure(
                                "topology_changed",
                                "active display topology changed before clone binding",
                            ),
                        );
                    }
                    match clone_activation(&configuration, &baseline) {
                        Ok(Some((selected, source))) => {
                            pending_route =
                                match self.platform.bind_clone(&selected, &source, &baseline) {
                                    Ok(route) => Some(route),
                                    Err(error) => {
                                        return complete(send, context.request, error.payload());
                                    }
                                };
                            native_target = Some((selected, TopologyShape::CloneActive));
                        }
                        Ok(None) => {}
                        Err(error) => return complete(send, context.request, error.payload()),
                    }
                }
                if pending_route.as_ref().is_some_and(|route| {
                    configuration.displays.iter().any(|display| {
                        display.id.eq_ignore_ascii_case(&route.id)
                            && display.hdr_enabled == Some(true)
                    })
                }) {
                    return complete(
                        send,
                        context.request,
                        failure(
                            "unsupported_display",
                            "inactive HDR target has no recoverable baseline",
                        ),
                    );
                }
                self.prepare(
                    context,
                    revision,
                    configuration,
                    PrepareIntent {
                        expected_topology_revision,
                        selection_revision,
                        preferred_modes,
                        operation_id,
                        request_identity,
                        topology_change: topology_change
                            || native_target
                                .as_ref()
                                .is_some_and(|(_, shape)| *shape == TopologyShape::CloneActive),
                        native_target,
                        pending_route,
                    },
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
                let result = restore_journal_snapshot(&mut self.platform, &journal, &mut || {
                    check_request(send, &context)
                });
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
    fn prepare_kscreen_lease(
        &mut self,
        context: Context,
        revision: u64,
        intent: KScreenLeaseIntent,
        baseline: crate::model::KScreenSnapshot,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let KScreenLeaseIntent {
            configuration,
            expected_topology_revision,
            operation_id,
            request_identity,
        } = intent;
        if let Err(error) = configuration.validate() {
            return complete(send, context.request, error.payload());
        }
        if let Err(error) = baseline.validate_requested_topology(&configuration) {
            return complete(send, context.request, error.payload());
        }
        let baseline_revision = match baseline.revision() {
            Ok(revision) => revision,
            Err(error) => return complete(send, context.request, error.payload()),
        };
        if expected_topology_revision
            .as_ref()
            .is_some_and(|expected| expected != &baseline_revision)
        {
            return complete(
                send,
                context.request,
                failure(
                    "topology_changed",
                    "KScreen topology changed before journal prepare",
                ),
            );
        }
        if let Err(error) = self
            .platform
            .prepare_kscreen_snapshot(&configuration, &baseline)
        {
            return complete(send, context.request, error.payload());
        }
        let readback = match self.platform.capture_kscreen_snapshot() {
            Ok(Some(readback)) => readback,
            Ok(None) => {
                return complete(
                    send,
                    context.request,
                    failure(
                        "unsupported_platform",
                        "KScreen recovery capability disappeared during preparation",
                    ),
                );
            }
            Err(error) => return complete(send, context.request, error.payload()),
        };
        if readback.revision().as_ref().ok() != Some(&baseline_revision) {
            return complete(
                send,
                context.request,
                failure(
                    "topology_changed",
                    "KScreen topology changed during journal prepare",
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
        let request_identity = operation_id.as_ref().map(|_| request_identity);
        let journal = Journal {
            owner: context.caller.clone(),
            lease: format!("display-{generation:016x}"),
            snapshot: RecoveryBaseline::KScreen(baseline),
            operation_id,
            requested_configuration: None,
            request_identity,
            expected_topology_revision,
            pending_route: None,
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
                native_target: None,
            },
            send,
        )
    }

    fn prepare(
        &mut self,
        context: Context,
        revision: u64,
        configuration: Configuration,
        intent: PrepareIntent,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let PrepareIntent {
            expected_topology_revision,
            selection_revision,
            preferred_modes,
            operation_id,
            request_identity,
            topology_change,
            native_target,
            pending_route,
        } = intent;
        if let Err(error) = configuration.validate() {
            return complete(send, context.request, error.payload());
        }
        if let Some(selected_revision) = &selection_revision {
            let current_revision = self
                .platform
                .snapshot()
                .and_then(|snapshot| physical_revision(&snapshot));
            if current_revision.as_ref().ok() != Some(selected_revision) {
                return complete(
                    send,
                    context.request,
                    failure(
                        "topology_changed",
                        "active display topology changed after mode selection",
                    ),
                );
            }
        }
        for (display, width, height, refresh) in &preferred_modes {
            let modes = match self.platform.available_modes(display) {
                Ok(modes) => modes,
                Err(error) => return complete(send, context.request, error.payload()),
            };
            if !modes.iter().any(|mode| {
                mode.width == *width
                    && mode.height == *height
                    && mode.bits_per_pixel == 32
                    && mode.refresh == *refresh
            }) {
                return complete(
                    send,
                    context.request,
                    failure(
                        "topology_changed",
                        "selected display mode is no longer available",
                    ),
                );
            }
        }
        let prepared = match native_target.as_ref() {
            Some((selected, shape)) => self.platform.prepare_preserving(
                &configuration,
                selected,
                pending_route.as_ref(),
                *shape,
            ),
            None => self.platform.prepare(&configuration),
        };
        let snapshot = match prepared {
            Ok(snapshot) => snapshot,
            Err(error) => return complete(send, context.request, error.payload()),
        };
        if let Some(selected_revision) = &selection_revision {
            let prepared_revision = match physical_revision(&snapshot) {
                Ok(revision) => revision,
                Err(error) => return complete(send, context.request, error.payload()),
            };
            if &prepared_revision != selected_revision {
                return complete(
                    send,
                    context.request,
                    failure(
                        "topology_changed",
                        "physical display topology changed during mode preparation",
                    ),
                );
            }
        }
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
            if let Err(error) = snapshot.configuration().and_then(|_| {
                if topology_change {
                    Ok(())
                } else {
                    validate_active_ids(&configuration, &snapshot)
                }
            }) {
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
        let request_identity = operation_id.as_ref().map(|_| request_identity);
        let journal = Journal {
            owner: context.caller.clone(),
            lease: format!("display-{generation:016x}"),
            snapshot: RecoveryBaseline::Ccd(snapshot),
            operation_id,
            requested_configuration: None,
            request_identity,
            expected_topology_revision,
            pending_route,
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
                native_target,
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
        native_target: Option<(String, TopologyShape)>,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let result = match (&journal.snapshot, native_target.as_ref()) {
            (RecoveryBaseline::KScreen(_), Some(_)) => Err(DisplayError::new(
                "unsupported_topology",
                "KScreen direct apply cannot use CCD topology-preserving routes",
            )),
            (RecoveryBaseline::KScreen(kscreen), None) => {
                apply_kscreen_transaction(&mut self.platform, kscreen, &configuration, &mut || {
                    check_request(send, &context)
                })
            }
            (RecoveryBaseline::Ccd(snapshot), Some((selected, shape))) => {
                self.platform.apply_preserving(
                    &configuration,
                    snapshot,
                    selected,
                    journal.pending_route.as_ref(),
                    *shape,
                    &mut || check_request(send, &context),
                )
            }
            (RecoveryBaseline::Ccd(snapshot), None) => {
                self.platform.apply(&configuration, snapshot, &mut || {
                    check_request(send, &context)
                })
            }
        };
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
                    && let Err(rollback) =
                        restore_journal_snapshot(&mut self.platform, &journal, &mut || Ok(()))
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
fn restore_journal_snapshot<P: Platform>(
    platform: &mut P,
    journal: &Journal,
    guard: &mut dyn FnMut() -> DisplayResult<()>,
) -> DisplayResult<()> {
    match &journal.snapshot {
        RecoveryBaseline::Ccd(snapshot) => platform.restore(snapshot, guard),
        RecoveryBaseline::KScreen(baseline) => {
            platform.restore_kscreen_snapshot(baseline, guard)?;
            let current = platform
                .capture_kscreen_snapshot()
                .map_err(DisplayError::after_mutation)?
                .ok_or_else(|| {
                    DisplayError::new(
                        "verification_failed",
                        "KScreen recovery provider disappeared after restore",
                    )
                    .after_mutation()
                })?;
            if !baseline.matches_restoration(&current) {
                return Err(DisplayError::new(
                    "verification_failed",
                    "KScreen restored topology does not match its durable baseline",
                )
                .after_mutation());
            }
            Ok(())
        }
    }
}

fn apply_kscreen_transaction<P: Platform>(
    platform: &mut P,
    baseline: &crate::model::KScreenSnapshot,
    configuration: &Configuration,
    guard: &mut dyn FnMut() -> DisplayResult<()>,
) -> DisplayResult<()> {
    let current = platform.capture_kscreen_snapshot()?.ok_or_else(|| {
        DisplayError::new(
            "unsupported_platform",
            "KScreen recovery provider disappeared before apply",
        )
    })?;
    if current.revision()? != baseline.revision()? {
        return Err(DisplayError::new(
            "topology_changed",
            "KScreen topology changed after journal arm and before apply",
        ));
    }
    platform.apply_kscreen_snapshot(configuration, baseline, guard)?;
    verify_kscreen_configuration(platform, baseline, configuration)
}

fn verify_kscreen_configuration<P: Platform>(
    platform: &mut P,
    baseline: &crate::model::KScreenSnapshot,
    configuration: &Configuration,
) -> DisplayResult<()> {
    let current = platform
        .capture_kscreen_snapshot()
        .map_err(DisplayError::after_mutation)?
        .ok_or_else(|| {
            DisplayError::new(
                "verification_failed",
                "KScreen provider disappeared after apply",
            )
            .after_mutation()
        })?;
    if !current.matches_configuration(baseline, configuration) {
        return Err(DisplayError::new(
            "verification_failed",
            "KScreen readback does not match the requested configuration",
        )
        .after_mutation());
    }
    Ok(())
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
fn physical_revision(snapshot: &Snapshot) -> DisplayResult<String> {
    let mut physical = snapshot.clone();
    for path in &mut physical.paths {
        // HDR capability may be unavailable while CCD routes and modes stay the same.
        path.hdr_enabled = None;
    }
    physical.revision()
}

fn synchronize_clone_group(configuration: &mut Configuration, snapshot: &Snapshot, selected: &str) {
    let Some(source) = snapshot
        .paths
        .iter()
        .find(|path| path.id.eq_ignore_ascii_case(selected))
        .map(|path| &path.source_address)
    else {
        return;
    };
    let Some(wanted) = configuration
        .displays
        .iter()
        .find(|display| display.id.eq_ignore_ascii_case(selected))
    else {
        return;
    };
    let (x, y, width, height) = (wanted.x, wanted.y, wanted.width, wanted.height);
    for peer in &snapshot.paths {
        if peer.source_address == *source
            && let Some(display) = configuration
                .displays
                .iter_mut()
                .find(|display| display.id.eq_ignore_ascii_case(&peer.id))
        {
            display.x = x;
            display.y = y;
            display.width = width;
            display.height = height;
        }
    }
}

fn compose_configuration(
    requested: &Configuration,
    snapshot: &Snapshot,
    policy: &crate::model::ModePolicy,
) -> DisplayResult<Configuration> {
    requested.validate()?;
    let active = snapshot.configuration()?;
    let mut selected = requested
        .displays
        .iter()
        .find(|display| display.id.eq_ignore_ascii_case(&policy.display_id))
        .cloned()
        .ok_or_else(|| {
            DisplayError::new(
                "invalid_request",
                "selected display is absent from configuration",
            )
        })?;
    let existing = active
        .displays
        .iter()
        .position(|display| display.id.eq_ignore_ascii_case(&selected.id));
    // Inactive targets need route identity bound across the journal boundary.
    // Active targets can use the saved CCD route for every untouched monitor.
    let configuration = match policy.configuration_option {
        Some(
            DisplayConfigurationOption::EnsureActive | DisplayConfigurationOption::EnsurePrimary,
        ) if existing.is_some() => {
            let index = existing.expect("checked active selected target");
            let mut configuration = active;
            let origin_x = configuration.displays[index].x;
            let origin_y = configuration.displays[index].y;
            selected.x = origin_x;
            selected.y = origin_y;
            configuration.displays[index] = selected;
            let selected_width = configuration.displays[index].width;
            let selected_height = configuration.displays[index].height;
            let selected_source = &snapshot
                .paths
                .iter()
                .find(|path| path.id.eq_ignore_ascii_case(&policy.display_id))
                .expect("selected active path")
                .source_address;
            for peer in &snapshot.paths {
                if peer.source_address == *selected_source {
                    let peer_index = configuration
                        .displays
                        .iter()
                        .position(|display| display.id.eq_ignore_ascii_case(&peer.id))
                        .expect("active path in configuration");
                    let display = &mut configuration.displays[peer_index];
                    display.width = selected_width;
                    display.height = selected_height;
                }
            }
            if policy.configuration_option == Some(DisplayConfigurationOption::EnsurePrimary) {
                for display in &mut configuration.displays {
                    display.x = display.x.checked_sub(origin_x).ok_or_else(|| {
                        DisplayError::new(
                            "invalid_request",
                            "primary translation overflows desktop",
                        )
                    })?;
                    display.y = display.y.checked_sub(origin_y).ok_or_else(|| {
                        DisplayError::new(
                            "invalid_request",
                            "primary translation overflows desktop",
                        )
                    })?;
                }
            }
            configuration
        }
        Some(DisplayConfigurationOption::EnsureActive) => {
            let mut configuration = active;
            selected.x = configuration
                .displays
                .iter()
                .try_fold(0_i32, |right, display| {
                    display
                        .x
                        .checked_add(i32::try_from(display.width).ok()?)
                        .map(|edge| right.max(edge))
                })
                .ok_or_else(|| {
                    DisplayError::new("invalid_request", "active desktop coordinate overflow")
                })?;
            selected.y = 0;
            configuration.displays.push(selected);
            configuration
        }
        Some(DisplayConfigurationOption::EnsurePrimary) => {
            let mut configuration = active;
            let anchor = configuration
                .displays
                .iter()
                .min_by_key(|display| display.x)
                .ok_or_else(|| DisplayError::new("unsupported_topology", "no active desktop"))?;
            let width = i32::try_from(selected.width).map_err(|_| {
                DisplayError::new(
                    "invalid_request",
                    "selected width overflows desktop coordinates",
                )
            })?;
            let shift = width.checked_sub(anchor.x).ok_or_else(|| {
                DisplayError::new("invalid_request", "primary translation overflows desktop")
            })?;
            let vertical_shift = anchor.y.checked_neg().ok_or_else(|| {
                DisplayError::new("invalid_request", "primary translation overflows desktop")
            })?;
            for display in &mut configuration.displays {
                display.x = display.x.checked_add(shift).ok_or_else(|| {
                    DisplayError::new("invalid_request", "primary translation overflows desktop")
                })?;
                display.y = display.y.checked_add(vertical_shift).ok_or_else(|| {
                    DisplayError::new("invalid_request", "primary translation overflows desktop")
                })?;
            }
            selected.x = 0;
            selected.y = 0;
            configuration.displays.push(selected);
            configuration
        }
        Some(DisplayConfigurationOption::EnsureOnlyDisplay) => {
            selected.x = 0;
            selected.y = 0;
            Configuration {
                displays: vec![selected],
            }
        }
        _ => {
            return Err(DisplayError::new(
                "invalid_request",
                "configuration option does not change topology",
            ));
        }
    };
    configuration.validate()?;
    Ok(configuration)
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
fn clone_activation(
    configuration: &Configuration,
    snapshot: &Snapshot,
) -> DisplayResult<Option<(String, crate::model::Address)>> {
    let active = snapshot.configuration()?;
    let added: Vec<_> = configuration
        .displays
        .iter()
        .filter(|display| {
            !snapshot
                .paths
                .iter()
                .any(|path| path.id.eq_ignore_ascii_case(&display.id))
        })
        .collect();
    if added.is_empty() {
        return Ok(None);
    }
    if added.len() != 1 || configuration.displays.len() != active.displays.len() + 1 {
        return Err(DisplayError::new(
            "unsupported_display",
            "revisioned apply requires active identities plus exactly one new clone target",
        ));
    }
    for saved in &active.displays {
        let Some(wanted) = configuration
            .displays
            .iter()
            .find(|display| display.id.eq_ignore_ascii_case(&saved.id))
        else {
            return Err(DisplayError::new(
                "unsupported_topology",
                "clone activation omitted an active target",
            ));
        };
        if serde_json::to_value(wanted).ok() != serde_json::to_value(saved).ok() {
            return Err(DisplayError::new(
                "unsupported_topology",
                "clone activation must preserve active target settings",
            ));
        }
    }
    let added = added[0];
    if added.hdr_enabled == Some(true) {
        return Err(DisplayError::new(
            "unsupported_display",
            "inactive clone has no recoverable HDR baseline",
        ));
    }
    let source = snapshot
        .paths
        .iter()
        .find(|path| {
            path.source.x == added.x
                && path.source.y == added.y
                && path.source.width == added.width
                && path.source.height == added.height
        })
        .ok_or_else(|| {
            DisplayError::new(
                "unsupported_topology",
                "new clone target must match an active source rectangle",
            )
        })?;
    Ok(Some((added.id.clone(), source.source_address.clone())))
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
