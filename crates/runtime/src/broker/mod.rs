mod api;
mod configuration;
mod effect_fence;
mod errors;
mod events;
mod historical_journal;
mod hooks;
mod requests;
mod source_slots;

use amaterasu_contracts::{
    Command, DIAGNOSTICS_COMPONENT, Grants, HOST_LIFECYCLE_CONTROL_PRINCIPAL,
    HOST_LIFECYCLE_SERVICE, HOST_LOG_SERVICE, HOST_STATE_CONTEXT_SERVICE, HostLifecycleAction,
    HostLogEvent, Response, RuntimeConfig,
};
use std::collections::{BTreeMap, VecDeque};
use std::time::Instant;

use api::{ApiInvocation, ApiQuery, ApiRegistry};
use events::EventBus;
use hooks::HookRegistry;
use requests::{RequestOptions, RequestRouter};

pub struct Broker {
    configuration: RuntimeConfig,
    requests: RequestRouter,
    events: EventBus,
    hooks: HookRegistry,
    host_lifecycle: VecDeque<HostLifecycleAction>,
    log_events: VecDeque<HostLogEvent>,
    dropped_log_records: u64,
    logging_revision: u64,
    logging_policy_received: bool,
    api: ApiRegistry,
    effect_fences: effect_fence::EffectFences,
    receipt_cleanup: BTreeMap<String, u64>,
}

const HOST_LIFECYCLE_QUEUE_CAPACITY: usize = 8;
const LOG_EVENT_QUEUE_CAPACITY: usize = 512;
const LOG_DRAIN_BATCH_CAPACITY: usize = 16;
const MAX_LOG_TARGET_BYTES: usize = 128;
const MAX_LOG_MESSAGE_BYTES: usize = 7_500;
const MAX_LOG_PATH_BYTES: usize = 4096;

fn host_retirement_state_request(payload: &serde_json::Value) -> bool {
    use amaterasu_contracts::effect_fence::{
        HostProviderInstanceRetirement, HostProviderRetirement,
    };
    let Some(key) = payload.get("key").and_then(serde_json::Value::as_str) else {
        return false;
    };
    if let Some(suffix) = key.strip_prefix("host/provider-retirement/v1/") {
        let mut parts = suffix.split('/');
        let hex_id = |value: &str| {
            value.len() == 32
                && value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        if !parts.next().is_some_and(hex_id)
            || !parts.next().is_some_and(hex_id)
            || parts.next().is_some()
        {
            return false;
        }
        return match payload.get("action").and_then(serde_json::Value::as_str) {
            Some("get") => true,
            Some("set") => {
                payload
                    .get("expected_revision")
                    .and_then(serde_json::Value::as_u64)
                    .is_some()
                    && payload
                        .get("value")
                        .and_then(|v| {
                            serde_json::from_value::<HostProviderInstanceRetirement>(v.clone()).ok()
                        })
                        .is_some_and(|receipt| {
                            receipt.version == 1
                                && receipt.provider_incarnation.version == 1
                                && receipt.state_key() == key
                        })
            }
            _ => false,
        };
    }
    if let Some(suffix) = key.strip_prefix("host/effect-retirement/v2/") {
        let mut parts = suffix.split('/');
        let Some(slot) = parts
            .next()
            .filter(|s| s.len() == 2)
            .and_then(|s| s.parse::<u8>().ok())
            .filter(|s| *s < 64)
        else {
            return false;
        };
        let Some(incarnation) = parts
            .next()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|n| *n > 0)
        else {
            return false;
        };
        let Some(sequence) = parts
            .next()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|n| *n > 0)
        else {
            return false;
        };
        if parts.next().is_some()
            || key != format!("host/effect-retirement/v2/{slot:02}/{incarnation}/{sequence}")
        {
            return false;
        }
        return match payload.get("action").and_then(serde_json::Value::as_str) {
            Some("get") => true,
            Some("delete") => payload
                .get("expected_revision")
                .and_then(serde_json::Value::as_u64)
                .is_some(),
            Some("set") => {
                payload
                    .get("expected_revision")
                    .and_then(serde_json::Value::as_u64)
                    .is_some()
                    && payload
                        .get("value")
                        .and_then(|v| {
                            serde_json::from_value::<HostProviderRetirement>(v.clone()).ok()
                        })
                        .is_some_and(|receipt| {
                            receipt.version == 1
                                && receipt.issued_lease.slot == slot
                                && receipt.issued_lease.incarnation == incarnation
                                && receipt.effect.sequence == sequence
                                && receipt.effect.scope.source.slot() == Some(slot)
                                && receipt.state_key() == key
                        })
            }
            _ => false,
        };
    }
    let Some(suffix) = key.strip_prefix("host/effect-retirement/v1/") else {
        return false;
    };
    if suffix.len() != 2 || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let Ok(slot) = suffix.parse::<u8>() else {
        return false;
    };
    if slot >= 64 {
        return false;
    }
    match payload.get("action").and_then(serde_json::Value::as_str) {
        Some("get") => true,
        Some("set") => {
            let Some(receipts) = payload
                .pointer("/value/receipts")
                .and_then(serde_json::Value::as_array)
            else {
                return false;
            };
            payload
                .get("expected_revision")
                .and_then(serde_json::Value::as_u64)
                .is_some()
                && payload
                    .pointer("/value/version")
                    .and_then(serde_json::Value::as_u64)
                    == Some(1)
                && receipts.len() <= 32
                && receipts.iter().all(|value| {
                    serde_json::from_value::<HostProviderRetirement>(value.clone())
                        .ok()
                        .is_some_and(|receipt| {
                            receipt.version == 1
                                && receipt.issued_lease.slot == slot
                                && receipt.effect.scope.source.slot() == Some(slot)
                                && receipt.broker_epoch == receipt.issued_lease.broker_epoch
                                && receipt.provider_incarnation.version == 1
                                && receipt.provider_incarnation.id.len() == 32
                                && receipt.provider_incarnation.id.bytes().all(|byte| {
                                    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
                                })
                        })
                })
        }
        _ => false,
    }
}

impl Broker {
    pub fn new(configuration: RuntimeConfig) -> Result<Self, String> {
        configuration::validate(&configuration)?;

        let mut requests = RequestRouter::default();
        if configuration.host_lifecycle_control {
            let grants = Grants {
                provide: [HOST_LOG_SERVICE.to_owned()].into(),
                ..Default::default()
            };
            let result = requests.register_service(
                HOST_LIFECYCLE_CONTROL_PRINCIPAL,
                &grants,
                HOST_LOG_SERVICE.into(),
            );
            if result.is_error() {
                return Err("could not register the reserved host log service".into());
            }
        }
        Ok(Self {
            hooks: HookRegistry::new(configuration.hooks.clone()),
            api: ApiRegistry::new(&configuration),
            effect_fences: effect_fence::EffectFences::new(&configuration)?,
            receipt_cleanup: BTreeMap::new(),
            configuration,
            requests,
            events: EventBus::default(),
            host_lifecycle: VecDeque::with_capacity(HOST_LIFECYCLE_QUEUE_CAPACITY),
            log_events: VecDeque::with_capacity(LOG_EVENT_QUEUE_CAPACITY),
            dropped_log_records: 0,
            logging_revision: 0,
            logging_policy_received: false,
        })
    }

    pub fn dispatch(&mut self, caller: &str, command: Command) -> Response {
        self.dispatch_at(caller, command, Instant::now())
    }

    pub(crate) fn dispatch_at(&mut self, caller: &str, command: Command, now: Instant) -> Response {
        self.requests.expire(now);
        self.hooks.expire(now);

        if matches!(command, Command::HostLifecycleDrain {}) {
            if caller != HOST_LIFECYCLE_CONTROL_PRINCIPAL {
                return errors::permission_denied();
            }
            return Response::HostLifecycle {
                action: self.host_lifecycle.pop_front(),
            };
        }

        if matches!(command, Command::DrainLogEvents {}) {
            if caller != HOST_LIFECYCLE_CONTROL_PRINCIPAL {
                return errors::permission_denied();
            }
            let events = (0..LOG_DRAIN_BATCH_CAPACITY)
                .filter_map(|_| self.log_events.pop_front())
                .collect();
            return Response::LogEvents {
                events,
                dropped_records: std::mem::take(&mut self.dropped_log_records),
            };
        }

        if caller == HOST_LIFECYCLE_CONTROL_PRINCIPAL {
            if !self.configuration.host_lifecycle_control {
                return errors::permission_denied();
            }
            return match command {
                Command::Poll {} => self.requests.poll(caller, now),
                Command::Request { service, payload, timeout_ms, operation_id, parent } => {
                    if service != "amaterasu.state/1" || parent.is_some()
                        || !host_retirement_state_request(&payload)
                        || !self.host_retirement_write_matches(&payload)
                        || !self.configuration.service_selections.broker.get(&service).and_then(Option::as_ref).is_some_and(|selection| selection.contract == "amaterasu.state/1") {
                        return errors::permission_denied();
                    }
                    let cleanup_key = (payload.get("action").and_then(serde_json::Value::as_str) == Some("delete")).then(|| payload.get("key").and_then(serde_json::Value::as_str).map(str::to_owned)).flatten();
                    let grants = Grants { call: [service.clone()].into(), ..Grants::default() };
                    let response = self.requests.submit(caller, &grants, RequestOptions { service, payload, timeout_ms, operation_id, parent: None, response_schema: None }, now);
                    if matches!(response, Response::Accepted { .. }) && let Some(key) = cleanup_key { self.receipt_cleanup.remove(&key); }
                    response
                }
                Command::CheckRequest { request } => self.requests.check_active(caller, request),
                Command::Complete { request, payload } => {
                    self.requests.complete(caller, request, payload, now)
                }
                Command::TakeResult { request } => self.requests.result(caller, request),
                Command::ReleaseResult { request } => self.requests.release(caller, request),
                Command::EffectFence { command } if matches!(&*command, amaterasu_contracts::effect_fence::EffectFenceCommand::BrokerEpoch {} | amaterasu_contracts::effect_fence::EffectFenceCommand::RecoveryRetired { .. } | amaterasu_contracts::effect_fence::EffectFenceCommand::SourceRetired { .. } | amaterasu_contracts::effect_fence::EffectFenceCommand::HostProviderJoined { .. } | amaterasu_contracts::effect_fence::EffectFenceCommand::HostProviderInstanceReceipt { .. } | amaterasu_contracts::effect_fence::EffectFenceCommand::HostRecoverHistoricalInstance { .. } | amaterasu_contracts::effect_fence::EffectFenceCommand::HostProviderRetiring { .. } | amaterasu_contracts::effect_fence::EffectFenceCommand::HostProviderSlotReceipts { .. } | amaterasu_contracts::effect_fence::EffectFenceCommand::HostProviderSlotReceiptsPage { .. } | amaterasu_contracts::effect_fence::EffectFenceCommand::HostRetirementReceipt { .. } | amaterasu_contracts::effect_fence::EffectFenceCommand::HostRecoverHistorical { .. }) => self.handle_effect_fence(caller, *command, now),
                _ => errors::permission_denied(),
            };
        }

        let Some(grants) = self.configuration.principals.get(caller) else {
            return errors::permission_denied();
        };

        match command {
            Command::NativeRegister { .. }
            | Command::NativeBind { .. }
            | Command::NativeRegisterTyped { .. }
            | Command::NativeBindTyped { .. }
            | Command::NativeRelease { .. } => Response::error(
                "native_transport_required",
                "native interfaces require the authenticated ABI endpoint",
            ),
            Command::HostLifecycle { action } => {
                if !grants.call.contains(HOST_LIFECYCLE_SERVICE) {
                    return errors::permission_denied();
                }
                if self.host_lifecycle.len() >= HOST_LIFECYCLE_QUEUE_CAPACITY {
                    return errors::host_lifecycle_capacity_exhausted();
                }
                self.host_lifecycle.push_back(action);
                Response::Ok
            }
            Command::HostLifecycleDrain {} => unreachable!("drain handled before module dispatch"),
            Command::DrainLogEvents {} => unreachable!("log drain handled before module dispatch"),
            Command::Log {
                level,
                target,
                message,
            } => self.enqueue_log(caller, level, target, message),
            Command::LoggingPolicyChanged {
                revision,
                minimum_level,
                log_path,
            } => self.update_logging_policy(caller, revision, minimum_level, log_path),
            Command::GetSettings {} => Response::Result {
                payload: self
                    .configuration
                    .settings
                    .get(caller)
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({})),
            },
            Command::GetHostContext {} => {
                // Audio needs the host-owned state root on Linux to reconcile
                // its own Pulse cleanup journal after a worker restart.
                if caller != "amaterasu.state"
                    && !(cfg!(target_os = "linux") && caller == "amaterasu.audio")
                    && !grants.call.contains(HOST_STATE_CONTEXT_SERVICE)
                {
                    return errors::permission_denied();
                }
                Response::Result {
                    payload: serde_json::json!({
                        "schema": self.configuration.host_context.schema,
                        "state_root": self.configuration.host_context.state_root,
                    }),
                }
            }
            Command::GetPackageRoot {} => {
                if caller != "amaterasu.management" {
                    return errors::permission_denied();
                }
                match &self.configuration.host_context.package_root {
                    Some(package_root) => Response::Result {
                        payload: serde_json::json!({"package_root":package_root}),
                    },
                    None => Response::error(
                        "package_root_unavailable",
                        "host did not supply a package root",
                    ),
                }
            }
            Command::GetRunningPackageDigest {} => {
                if caller != "amaterasu.management" {
                    return errors::permission_denied();
                }
                match &self.configuration.host_context.running_package_digest {
                    Some(digest) => Response::Result {
                        payload: serde_json::json!({"digest": digest}),
                    },
                    None => Response::error(
                        "running_package_unavailable",
                        "host did not supply a loaded package digest",
                    ),
                }
            }
            Command::Register { service } => {
                if service == HOST_LOG_SERVICE
                    || self.api.owns_service(&service)
                    || !self
                        .configuration
                        .service_selections
                        .allows_broker(&service, caller)
                {
                    errors::permission_denied()
                } else {
                    self.requests.register_service(caller, grants, service)
                }
            }
            Command::RegisterApi { method } => {
                if method.service == HOST_LOG_SERVICE
                    || !self
                        .configuration
                        .service_selections
                        .allows_broker(&method.service, caller)
                {
                    return errors::permission_denied();
                }
                let selected_here = self
                    .configuration
                    .service_selections
                    .broker
                    .get(&method.service)
                    .and_then(Option::as_ref)
                    .is_some_and(|selection| selection.provider == caller);
                let registered = self.api.register(caller, grants, method.clone());
                if matches!(&registered, Response::Error { code, .. } if code == "permission_denied")
                    && selected_here
                {
                    Response::error(
                        "incompatible_api",
                        "the selected provider method is not approved",
                    )
                } else if registered.is_error() {
                    registered
                } else {
                    self.requests
                        .register_service(caller, grants, method.service)
                }
            }
            Command::QueryApi {
                service,
                method,
                interface_major,
                min_minor,
            } => self.api.query(
                caller,
                grants,
                ApiQuery {
                    service: &service,
                    method: &method,
                    major: interface_major,
                    min_minor,
                },
                false,
            ),
            Command::QueryApiApproval {
                service,
                method,
                interface_major,
                min_minor,
            } => self.api.query(
                caller,
                grants,
                ApiQuery {
                    service: &service,
                    method: &method,
                    major: interface_major,
                    min_minor,
                },
                true,
            ),
            Command::EffectFence { command } => self.handle_effect_fence(caller, *command, now),
            Command::CallApi {
                service,
                method,
                interface_major,
                interface_minor,
                payload,
                timeout_ms,
                operation_id,
                parent,
            } => {
                if service == HOST_LOG_SERVICE
                    || self
                        .effect_fences
                        .direct_call_denied(&service, &method, interface_major)
                {
                    return errors::permission_denied();
                }
                if self.configuration.api_methods.iter().any(|approval| {
                    approval.method.service == service
                        && approval.method.method == method
                        && approval.method.interface_major == interface_major
                        && self.effect_fences.provider_unavailable(&approval.provider)
                }) {
                    return Response::error("effect_fenced", "provider instance is retiring");
                }
                let initiator = match parent {
                    Some(parent_id) => {
                        match self.requests.initiator_for_parent(caller, parent_id) {
                            Ok(initiator) => initiator.to_owned(),
                            Err(error) => return error,
                        }
                    }
                    None => caller.to_owned(),
                };
                match self.api.callable(
                    caller,
                    &initiator,
                    grants,
                    ApiInvocation {
                        service: &service,
                        method: &method,
                        major: interface_major,
                        minor: interface_minor,
                        payload: &payload,
                    },
                ) {
                    Ok((payload, response_schema)) => self.requests.submit(
                        caller,
                        grants,
                        RequestOptions {
                            service,
                            payload,
                            timeout_ms,
                            operation_id,
                            parent,
                            response_schema: Some(response_schema),
                        },
                        now,
                    ),
                    Err(error) => error,
                }
            }
            Command::Request {
                service,
                payload,
                timeout_ms,
                operation_id,
                parent,
            } => {
                if (service == HOST_LOG_SERVICE && caller != "amaterasu.management")
                    || self.effect_fences.direct_service_denied(&service)
                {
                    return errors::permission_denied();
                }
                if self.api.owns_service(&service) {
                    return errors::permission_denied();
                }
                let options = RequestOptions {
                    service,
                    payload,
                    timeout_ms,
                    operation_id,
                    parent,
                    response_schema: None,
                };
                self.requests.submit(caller, grants, options, now)
            }
            Command::Poll {} => {
                let response = self.requests.poll(caller, now);
                if let Response::Request { id, .. } = &response {
                    self.effect_fences.delivered(*id);
                }
                response
            }
            Command::CheckRequest { request } => self.requests.check_active(caller, request),
            Command::Complete { request, payload } => {
                self.requests.complete(caller, request, payload, now)
            }
            Command::TakeResult { request } => self.requests.result(caller, request),
            Command::ReleaseResult { request } => self.requests.release(caller, request),
            Command::Cancel { request } => self.requests.cancel(caller, request, now),
            Command::Subscribe {
                topic,
                capacity,
                coalesce,
            } => self
                .events
                .subscribe(caller, grants, topic, capacity, coalesce),
            Command::Publish { topic, payload } => {
                self.events.publish(caller, grants, topic, payload)
            }
            Command::NextEvent { subscription } => self.events.next_event(caller, subscription),
            Command::Unsubscribe { subscription } => self.events.unsubscribe(caller, subscription),
            Command::RegisterHook { hook } => self.hooks.register(caller, grants, hook),
            Command::RunHook { hook, payload } => self.hooks.begin(caller, hook, payload, now),
            Command::RunHookOperation {
                hook,
                payload,
                operation_id,
                cleanup,
            } => self
                .hooks
                .begin_operation(caller, hook, payload, operation_id, cleanup, now),
            Command::ListApiApprovals {
                parent,
                after,
                limit,
            } => {
                let authority = match parent {
                    Some(id) => match self.requests.discovery_authority(caller, id) {
                        Ok(authority) => Some(authority),
                        Err(error) => return error,
                    },
                    None => None,
                };
                self.api
                    .list_approvals(caller, grants, authority.as_ref(), after, limit)
            }
            Command::LookupHook { operation_id } => self.hooks.lookup(caller, &operation_id),
            Command::CancelHook { invocation } => {
                self.hooks
                    .cancel(caller, invocation, now, &mut self.requests)
            }
            Command::ReleaseHook { invocation } => self.hooks.release(caller, invocation),
            Command::HookResult { invocation } => {
                self.hooks
                    .advance(caller, grants, invocation, now, &mut self.requests)
            }
        }
    }

    fn handle_effect_fence(
        &mut self,
        caller: &str,
        command: amaterasu_contracts::effect_fence::EffectFenceCommand,
        now: Instant,
    ) -> Response {
        self.handle_effect_fence_inner(caller, command, now, false)
    }

    fn handle_effect_fence_inner(
        &mut self,
        caller: &str,
        command: amaterasu_contracts::effect_fence::EffectFenceCommand,
        now: Instant,
        slotted: bool,
    ) -> Response {
        use amaterasu_contracts::effect_fence::EffectFenceCommand;
        if self.effect_fences.uses_slots()
            && !slotted
            && matches!(
                &command,
                EffectFenceCommand::OpenSource { .. }
                    | EffectFenceCommand::Arm { .. }
                    | EffectFenceCommand::ArmSelected { .. }
                    | EffectFenceCommand::Dispatch { .. }
                    | EffectFenceCommand::Fence { .. }
                    | EffectFenceCommand::Lookup { .. }
                    | EffectFenceCommand::Compact { .. }
                    | EffectFenceCommand::ProviderRetired { .. }
                    | EffectFenceCommand::RecoverFence { .. }
                    | EffectFenceCommand::LookupIdentity { .. }
                    | EffectFenceCommand::VerifyHistoricalJournal { .. }
                    | EffectFenceCommand::ConfirmHistoricalCleanup { .. }
            )
        {
            return Response::error("slot_required", "source slot lease is required");
        }
        if !self.effect_fences.uses_slots()
            && matches!(
                &command,
                EffectFenceCommand::RecoverFence { .. } | EffectFenceCommand::LookupIdentity { .. }
            )
        {
            return Response::error(
                "slot_required",
                "payload-free recovery requires source slots",
            );
        }
        match command {
            EffectFenceCommand::BrokerEpoch {} => self.effect_fences.broker_epoch(),
            EffectFenceCommand::SlotStatus { slot } => self.effect_fences.slot_status(caller, slot),
            EffectFenceCommand::RecoveryRetired { broker_epoch } => {
                self.effect_fences.recovery_retired(caller, &broker_epoch)
            }
            EffectFenceCommand::RecoverSlots {
                broker_epoch,
                retired_incarnations,
            } => self
                .effect_fences
                .recover_slots(caller, &broker_epoch, retired_incarnations),
            EffectFenceCommand::OpenSlot {
                lease,
                scope,
                retired_through,
            } => self
                .effect_fences
                .open_slot(caller, lease, scope, retired_through, None),
            EffectFenceCommand::OpenRecoverySlot {
                lease,
                scope,
                retired_through,
                issued_through,
            } => self.effect_fences.open_slot(
                caller,
                lease,
                scope,
                retired_through,
                Some(issued_through),
            ),
            EffectFenceCommand::BeginCloseSlot { lease, scope } => {
                match self.effect_fences.begin_close(caller, &lease, &scope) {
                    Ok(requests) => {
                        let issuer = self
                            .effect_fences
                            .issuer_name()
                            .expect("configured authority");
                        for request in requests {
                            let _ = self.requests.cancel(issuer, request, now);
                        }
                        Response::Ok
                    }
                    Err(error) => error,
                }
            }
            EffectFenceCommand::SourceRetired { lease, scope } => {
                self.effect_fences.source_retired(caller, &lease, &scope)
            }
            EffectFenceCommand::HostProviderRetiring {
                provider,
                provider_incarnation,
            } => {
                if caller != HOST_LIFECYCLE_CONTROL_PRINCIPAL
                    || self
                        .configuration
                        .host_context
                        .provider_incarnations
                        .get(&provider)
                        != Some(&provider_incarnation)
                {
                    return errors::permission_denied();
                }
                let requests = self
                    .effect_fences
                    .host_provider_retiring(&provider, &provider_incarnation);
                self.requests.cancel_undelivered_to_provider(&provider, now);
                if let Some(issuer) = self.effect_fences.issuer_name() {
                    for request in requests {
                        let _ = self.requests.cancel(issuer, request, now);
                    }
                }
                Response::Ok
            }
            EffectFenceCommand::HostProviderSlotReceipts {
                provider,
                provider_incarnation,
                slot,
            } => {
                if caller != HOST_LIFECYCLE_CONTROL_PRINCIPAL
                    || self
                        .configuration
                        .host_context
                        .provider_incarnations
                        .get(&provider)
                        != Some(&provider_incarnation)
                {
                    return errors::permission_denied();
                }
                self.effect_fences.host_provider_slot_receipts(
                    &provider,
                    &provider_incarnation,
                    slot,
                )
            }
            EffectFenceCommand::HostProviderSlotReceiptsPage {
                provider,
                provider_incarnation,
                slot,
                after_sequence,
            } => {
                if caller != HOST_LIFECYCLE_CONTROL_PRINCIPAL
                    || self
                        .configuration
                        .host_context
                        .provider_incarnations
                        .get(&provider)
                        != Some(&provider_incarnation)
                {
                    return errors::permission_denied();
                }
                self.effect_fences.host_provider_slot_receipts_page(
                    &provider,
                    &provider_incarnation,
                    slot,
                    after_sequence,
                )
            }
            EffectFenceCommand::HostProviderInstanceReceipt {
                provider,
                provider_incarnation,
            } => {
                if caller != HOST_LIFECYCLE_CONTROL_PRINCIPAL
                    || self
                        .configuration
                        .host_context
                        .provider_incarnations
                        .get(&provider)
                        != Some(&provider_incarnation)
                {
                    return errors::permission_denied();
                }
                self.effect_fences
                    .host_provider_instance_receipt(&provider, &provider_incarnation)
            }
            EffectFenceCommand::HostProviderJoined {
                provider,
                provider_incarnation,
            } => {
                if caller != HOST_LIFECYCLE_CONTROL_PRINCIPAL
                    || self
                        .configuration
                        .host_context
                        .provider_incarnations
                        .get(&provider)
                        != Some(&provider_incarnation)
                {
                    return errors::permission_denied();
                }
                let requests = self
                    .effect_fences
                    .host_provider_joined(&provider, &provider_incarnation);
                self.requests.cancel_undelivered_to_provider(&provider, now);
                if let Some(issuer) = self.effect_fences.issuer_name() {
                    for request in requests {
                        let _ = self.requests.cancel(issuer, request, now);
                    }
                }
                Response::Ok
            }
            EffectFenceCommand::AuthorizeHostReceiptCleanup {
                receipt,
                authority_state_request,
                host_state_request,
            } => {
                if !self.effect_fences.authority(caller) {
                    return errors::permission_denied();
                }
                let Some(Some(selection)) = self
                    .configuration
                    .service_selections
                    .broker
                    .get("amaterasu.state/1")
                else {
                    return errors::permission_denied();
                };
                let Some((request, result)) = self.requests.selected_state_read(
                    caller,
                    &selection.provider,
                    authority_state_request,
                ) else {
                    return Response::error(
                        "recovery_required",
                        "fresh authority retirement state is unavailable",
                    );
                };
                let key = format!("pairing/protocol-slot/v1/{:02}", receipt.issued_lease.slot);
                let journal = result.get("value").unwrap_or(&serde_json::Value::Null);
                if !self.effect_fences.slot_incarnation_retired(
                    receipt.issued_lease.slot,
                    receipt.issued_lease.incarnation,
                ) || request.get("key").and_then(serde_json::Value::as_str) != Some(key.as_str())
                    || result.get("ok").and_then(serde_json::Value::as_bool) != Some(true)
                    || result.get("found").and_then(serde_json::Value::as_bool) != Some(true)
                    || journal.get("version").and_then(serde_json::Value::as_u64) != Some(1)
                    || journal.get("slot").and_then(serde_json::Value::as_u64)
                        != Some(u64::from(receipt.issued_lease.slot))
                    || journal
                        .get("retired_incarnation")
                        .and_then(serde_json::Value::as_u64)
                        .is_none_or(|floor| floor < receipt.issued_lease.incarnation)
                    || journal
                        .get("pending_close")
                        .is_none_or(|value| !value.is_null())
                    || !self.host_retirement_record_matches(&receipt, host_state_request)
                {
                    return Response::error(
                        "recovery_required",
                        "durable slot retirement and exact host receipt are required",
                    );
                }
                let Some((host_request, host_result)) = self.requests.selected_state_read(
                    HOST_LIFECYCLE_CONTROL_PRINCIPAL,
                    &selection.provider,
                    host_state_request,
                ) else {
                    return errors::permission_denied();
                };
                let receipt_key = receipt.state_key();
                if host_request.get("key").and_then(serde_json::Value::as_str)
                    != Some(receipt_key.as_str())
                {
                    return errors::permission_denied();
                }
                let Some(revision) = host_result
                    .get("revision")
                    .and_then(serde_json::Value::as_u64)
                else {
                    return errors::permission_denied();
                };
                if self.receipt_cleanup.len() >= 2048
                    && !self.receipt_cleanup.contains_key(&receipt_key)
                {
                    return errors::capacity_exhausted();
                }
                self.receipt_cleanup.insert(receipt_key, revision);
                Response::Ok
            }
            EffectFenceCommand::HostRetirementReceipt { scope, sequence } => {
                if caller != HOST_LIFECYCLE_CONTROL_PRINCIPAL {
                    return errors::permission_denied();
                }
                self.effect_fences.host_retirement_receipt(&scope, sequence)
            }
            EffectFenceCommand::HostRecoverHistorical {
                lease,
                receipt,
                expected_entry,
                state_request,
                host_receipt_state_request,
            } => {
                if caller != HOST_LIFECYCLE_CONTROL_PRINCIPAL {
                    return errors::permission_denied();
                }
                if !self.effect_fences.slot_valid(&lease, &receipt.effect.scope)
                    || !self.effect_fences.recovery_only(&receipt.effect.scope)
                {
                    return Response::error("stale_source_slot", "source is not in recovery");
                }
                if receipt.version != 1
                    || receipt.broker_epoch != receipt.issued_lease.broker_epoch
                    || serde_json::to_value(&receipt.issued_lease).ok().as_ref()
                        != expected_entry.get("issued_lease")
                    || expected_entry
                        .pointer("/issued_lease/broker_epoch")
                        .and_then(serde_json::Value::as_str)
                        != Some(&receipt.broker_epoch)
                {
                    return Response::error("effect_conflict", "host retirement receipt differs");
                }
                if !self.host_retirement_record_matches(&receipt, host_receipt_state_request) {
                    return Response::error(
                        "recovery_required",
                        "host retirement record is unavailable",
                    );
                }
                if let Err(error) = self.verify_historical_journal(
                    &receipt.effect,
                    &receipt.provider_incarnation,
                    &expected_entry,
                    state_request,
                ) {
                    return error;
                }
                self.effect_fences.recover_historical(
                    receipt.effect,
                    &receipt.provider_incarnation,
                    &expected_entry,
                )
            }
            EffectFenceCommand::HostRecoverHistoricalInstance {
                lease,
                receipt,
                identity,
                expected_entry,
                state_request,
                host_receipt_state_request,
            } => {
                if caller != HOST_LIFECYCLE_CONTROL_PRINCIPAL {
                    return errors::permission_denied();
                }
                if !self.effect_fences.slot_valid(&lease, &identity.scope)
                    || !self.effect_fences.recovery_only(&identity.scope)
                {
                    return Response::error("stale_source_slot", "source is not in recovery");
                }
                if receipt.version != 1
                    || receipt.provider != identity.provider
                    || receipt.provider_incarnation.version != 1
                    || expected_entry
                        .pointer("/issued_lease/broker_epoch")
                        .and_then(serde_json::Value::as_str)
                        != Some(receipt.broker_epoch.as_str())
                    || serde_json::to_value(&receipt.provider_incarnation)
                        .ok()
                        .as_ref()
                        != expected_entry.get("provider_incarnation")
                {
                    return Response::error("effect_conflict", "host instance retirement differs");
                }
                let Some(Some(selection)) = self
                    .configuration
                    .service_selections
                    .broker
                    .get("amaterasu.state/1")
                else {
                    return errors::permission_denied();
                };
                let Some((host_request, host_result)) = self.requests.selected_state_read(
                    HOST_LIFECYCLE_CONTROL_PRINCIPAL,
                    &selection.provider,
                    host_receipt_state_request,
                ) else {
                    return Response::error(
                        "recovery_required",
                        "fresh host instance record unavailable",
                    );
                };
                if host_request.get("key").and_then(serde_json::Value::as_str)
                    != Some(receipt.state_key().as_str())
                    || host_result.get("ok").and_then(serde_json::Value::as_bool) != Some(true)
                    || host_result
                        .get("found")
                        .and_then(serde_json::Value::as_bool)
                        != Some(true)
                    || serde_json::to_value(&receipt).ok().as_ref() != host_result.get("value")
                {
                    return Response::error(
                        "recovery_required",
                        "host instance retirement record differs",
                    );
                }
                if let Err(error) = self.verify_historical_journal(
                    &identity,
                    &receipt.provider_incarnation,
                    &expected_entry,
                    state_request,
                ) {
                    return error;
                }
                self.effect_fences.recover_historical(
                    *identity,
                    &receipt.provider_incarnation,
                    &expected_entry,
                )
            }
            EffectFenceCommand::FinishCloseSlot {
                lease,
                scope,
                retired_through,
            } => self
                .effect_fences
                .finish_close(caller, &lease, &scope, retired_through),
            EffectFenceCommand::CloseReady {
                lease,
                scope,
                retired_through,
            } => self
                .effect_fences
                .close_ready(caller, &lease, &scope, retired_through),
            EffectFenceCommand::ProviderRetiredInEpoch {
                broker_epoch,
                request,
            } => {
                if !self.effect_fences.valid_epoch(&broker_epoch) {
                    Response::error("stale_source_slot", "broker epoch is stale")
                } else {
                    self.effect_fences.provider_retired(caller, request, None)
                }
            }
            EffectFenceCommand::ProviderRetiredInIncarnation {
                broker_epoch,
                provider_incarnation,
                request,
            } => {
                if !self.effect_fences.valid_epoch(&broker_epoch) {
                    Response::error("stale_source_slot", "broker epoch is stale")
                } else if self
                    .configuration
                    .host_context
                    .provider_incarnations
                    .get(caller)
                    != Some(&provider_incarnation)
                {
                    errors::permission_denied()
                } else {
                    self.effect_fences.provider_retired(
                        caller,
                        request,
                        Some(&provider_incarnation),
                    )
                }
            }
            EffectFenceCommand::SlotCommand { lease, command } => {
                let scope = match &*command {
                    EffectFenceCommand::Arm { effect }
                    | EffectFenceCommand::ArmSelected { effect, .. }
                    | EffectFenceCommand::CommitAdmission { effect, .. }
                    | EffectFenceCommand::Dispatch { effect }
                    | EffectFenceCommand::Fence { effect }
                    | EffectFenceCommand::Lookup { effect } => &effect.scope,
                    EffectFenceCommand::Compact { scope, .. } => scope,
                    EffectFenceCommand::RecoverFence { identity }
                    | EffectFenceCommand::LookupIdentity { identity } => &identity.scope,
                    EffectFenceCommand::VerifyHistoricalJournal { identity, .. } => &identity.scope,
                    EffectFenceCommand::ConfirmHistoricalCleanup { identity, .. } => {
                        &identity.scope
                    }
                    _ => {
                        return Response::error(
                            "invalid_request",
                            "invalid slotted effect command",
                        );
                    }
                };
                if !self.effect_fences.slot_valid(&lease, scope) {
                    return Response::error(
                        "stale_source_slot",
                        "source slot incarnation is stale",
                    );
                }
                self.handle_effect_fence_inner(caller, *command, now, true)
            }
            EffectFenceCommand::RecoverFence { identity } => {
                if !self.effect_fences.authority(caller) {
                    return errors::permission_denied();
                }
                if self.effect_fences.recovery_only(&identity.scope) {
                    return Response::error(
                        "historical_evidence_required",
                        "recovery requires a host retirement receipt and canonical journal",
                    );
                }
                if !self.recovery_provider_matches(&identity) {
                    return errors::permission_denied();
                }
                match self.effect_fences.recover_fence(caller, identity) {
                    Ok(Some(request)) => {
                        let issuer = self
                            .effect_fences
                            .issuer_name()
                            .expect("configured authority");
                        let _ = self.requests.cancel(issuer, request, now);
                        Response::Ok
                    }
                    Ok(None) => Response::Ok,
                    Err(error) => error,
                }
            }
            EffectFenceCommand::LookupIdentity { identity } => {
                if !self.recovery_provider_matches(&identity)
                    && !self.effect_fences.is_historical(&identity)
                {
                    return errors::permission_denied();
                }
                self.effect_fences.lookup_identity(caller, &identity)
            }
            EffectFenceCommand::VerifyHistoricalJournal {
                identity,
                provider_incarnation,
                expected_entry,
                state_request,
            } => {
                if !self.effect_fences.authority(caller) {
                    return errors::permission_denied();
                }
                if !slotted || !self.effect_fences.recovery_only(&identity.scope) {
                    return Response::error(
                        "recovery_required",
                        "historical journal verification requires a recovery-only source",
                    );
                }
                match self.verify_historical_journal(
                    &identity,
                    &provider_incarnation,
                    &expected_entry,
                    state_request,
                ) {
                    Ok(()) => Response::Result {
                        payload: serde_json::json!({"status":"journal_verified"}),
                    },
                    Err(error) => error,
                }
            }
            EffectFenceCommand::ConfirmHistoricalCleanup {
                identity,
                provider_incarnation,
                expected_entry,
                state_request,
            } => {
                if !self.effect_fences.authority(caller) {
                    return errors::permission_denied();
                }
                if !slotted || !self.effect_fences.recovery_only(&identity.scope) {
                    return Response::error("recovery_required", "source is not in recovery");
                }
                if expected_entry
                    .get("cleanup_confirmed")
                    .and_then(serde_json::Value::as_bool)
                    != Some(true)
                {
                    return Response::error("effect_pending", "durable cleanup is not confirmed");
                }
                if let Err(error) = self.verify_historical_journal(
                    &identity,
                    &provider_incarnation,
                    &expected_entry,
                    state_request,
                ) {
                    return error;
                }
                self.effect_fences.confirm_historical_cleanup(
                    &identity,
                    &provider_incarnation,
                    &expected_entry,
                )
            }
            EffectFenceCommand::OpenSource {
                scope,
                retired_through,
            } => self.effect_fences.open(caller, scope, retired_through),
            EffectFenceCommand::Compact {
                scope,
                retired_through,
            } => self.effect_fences.compact(caller, &scope, retired_through),
            EffectFenceCommand::ProviderRetired { request } => {
                self.effect_fences.provider_retired(caller, request, None)
            }
            EffectFenceCommand::Lookup { effect } => self.effect_fences.lookup(caller, &effect),
            EffectFenceCommand::SelectedMethod { method } => {
                if !self.effect_fences.authority(caller) {
                    return errors::permission_denied();
                }
                match self.selected_effect_method(&method) {
                    Some(selected) => Response::Result {
                        payload: serde_json::to_value(selected).expect("selection is serializable"),
                    },
                    None => errors::permission_denied(),
                }
            }
            EffectFenceCommand::SelectedActionApproval { method } => {
                if !self.effect_fences.authority(caller)
                    || !self.effect_fences.protects(
                        &method.service,
                        &method.method,
                        method.interface_major,
                    )
                {
                    return errors::permission_denied();
                }
                let Some(approval) = self.api.registered_approval(
                    &method.service,
                    &method.method,
                    method.interface_major,
                ) else {
                    return errors::permission_denied();
                };
                let Some(selected) = self.selected_effect_method(&method) else {
                    return errors::permission_denied();
                };
                if approval.provider != selected.provider || approval.method != selected.method {
                    return errors::permission_denied();
                }
                Response::Result {
                    payload: serde_json::json!({"approval":approval,"provider_incarnation":selected.provider_incarnation}),
                }
            }
            EffectFenceCommand::SelectedStateProvider {} => {
                if !self.effect_fences.authority(caller) {
                    return errors::permission_denied();
                }
                match self
                    .configuration
                    .service_selections
                    .broker
                    .get("amaterasu.state/1")
                    .and_then(Option::as_ref)
                {
                    Some(selection)
                        if selection.contract == "amaterasu.state/1"
                            && self
                                .configuration
                                .host_context
                                .durable_cas_provider
                                .as_deref()
                                == Some(selection.provider.as_str()) =>
                    {
                        let Some(instance) = self
                            .configuration
                            .host_context
                            .provider_incarnations
                            .get(&selection.provider)
                        else {
                            return errors::permission_denied();
                        };
                        Response::Result {
                            payload: serde_json::json!({"owner":caller,"provider":selection.provider,"contract":selection.contract,"capability":"amaterasu.state.durable-cas/1","provider_incarnation":instance}),
                        }
                    }
                    _ => errors::permission_denied(),
                }
            }
            EffectFenceCommand::SelectedCleanupMethod { method } => {
                if !self.effect_fences.authority(caller)
                    || !self.effect_fences.protects_cleanup(
                        &method.service,
                        &method.method,
                        method.interface_major,
                    )
                {
                    return errors::permission_denied();
                }
                let Some(approval) = self.api.registered_approval(
                    &method.service,
                    &method.method,
                    method.interface_major,
                ) else {
                    return errors::permission_denied();
                };
                if approval.method.privilege != amaterasu_contracts::ApiPrivilege::User
                    || self.effect_fences.provider_unavailable(&approval.provider)
                {
                    return errors::permission_denied();
                }
                Response::Result {
                    payload: serde_json::json!({"approval":approval,"provider_incarnation":self.configuration.host_context.provider_incarnations.get(&approval.provider)}),
                }
            }
            EffectFenceCommand::ArmSelected { effect, expected } => {
                if !self.effect_fences.authority(caller) {
                    return errors::permission_denied();
                }
                if self.effect_fences.requires_handoff() {
                    return Response::error(
                        "handoff_required",
                        "atomic authority request handoff is required",
                    );
                }
                let method = amaterasu_contracts::effect_fence::ProtectedMethod {
                    service: effect.service.clone(),
                    method: effect.method.clone(),
                    interface_major: effect.interface_major,
                };
                let Some(selected) = self.selected_effect_method(&method) else {
                    return errors::permission_denied();
                };
                if self.effect_fences.uses_slots() && selected.provider_incarnation.is_none() {
                    return Response::error(
                        "provider_incarnation_required",
                        "source slots require a host-issued provider incarnation",
                    );
                }
                if *expected != selected
                    || effect.interface_minor != selected.method.interface_minor
                {
                    return Response::error(
                        "incompatible_api",
                        "selected provider or API schema differs",
                    );
                }
                self.effect_fences.arm(
                    caller,
                    effect,
                    &selected.provider,
                    selected.provider_incarnation.as_ref(),
                )
            }
            EffectFenceCommand::CommitAdmission {
                request,
                effect,
                expected,
                expected_entry,
                state_request,
            } => {
                if !self.effect_fences.authority(caller) {
                    return errors::permission_denied();
                }
                if !slotted || !self.effect_fences.requires_handoff() {
                    return Response::error(
                        "unsupported",
                        "atomic slotted handoff is not configured",
                    );
                }
                let method = amaterasu_contracts::effect_fence::ProtectedMethod {
                    service: effect.service.clone(),
                    method: effect.method.clone(),
                    interface_major: effect.interface_major,
                };
                let Some(selected) = self.selected_effect_method(&method) else {
                    return errors::permission_denied();
                };
                let Some(instance) = selected.provider_incarnation.as_ref() else {
                    return Response::error(
                        "provider_incarnation_required",
                        "host-issued provider incarnation is required",
                    );
                };
                if *expected != selected
                    || effect.interface_minor != selected.method.interface_minor
                {
                    return Response::error(
                        "incompatible_api",
                        "selected provider or API schema differs",
                    );
                }
                let issuer = self.effect_fences.issuer_name().expect("configured issuer");
                if !self.requests.protocol_handoff_request_matches(
                    caller,
                    issuer,
                    request,
                    &effect,
                    &selected.provider,
                    &expected_entry,
                ) {
                    return Response::error(
                        "request_ended",
                        "authority request is not active or does not match this effect",
                    );
                }
                let identity = effect.identity(&selected.provider);
                if let Err(error) = self.verify_historical_journal(
                    &identity,
                    instance,
                    &expected_entry,
                    state_request,
                ) {
                    return error;
                }
                if serde_json::to_value(&selected).ok().as_ref()
                    != expected_entry.get("selected_method")
                {
                    return Response::error(
                        "incompatible_api",
                        "durable selection does not match current provider schema",
                    );
                }
                if expected_entry
                    .get("cleanup_confirmed")
                    .and_then(serde_json::Value::as_bool)
                    != Some(false)
                {
                    return Response::error(
                        "effect_conflict",
                        "retired journal entry cannot admit work",
                    );
                }
                let Some(lease) = self.effect_fences.current_lease(&effect.scope) else {
                    return errors::permission_denied();
                };
                let reply =
                    serde_json::json!({"ok":true,"version":1,"lease":lease,"identity":identity});
                if !errors::payload_fits(&reply) {
                    return errors::capacity_exhausted();
                }
                let existed = self.effect_fences.contains_effect(&effect);
                let armed = self.effect_fences.arm(
                    caller,
                    effect.clone(),
                    &selected.provider,
                    Some(instance),
                );
                if !matches!(armed, Response::Ok) {
                    return armed;
                }
                // All request completion conditions were checked above, while
                // this same broker lock excludes cancellation or replacement.
                let completed = self.requests.complete(caller, request, reply, now);
                if !matches!(completed, Response::Ok) {
                    if !existed {
                        self.effect_fences.rollback_new_arm(&effect);
                    }
                    return completed;
                }
                self.requests.mark_committed_admission(request);
                Response::Ok
            }
            EffectFenceCommand::Arm { effect } => {
                if self.effect_fences.uses_slots() {
                    return Response::error(
                        "expected_provider_required",
                        "source slots require provider-bound admission",
                    );
                }
                let Some(provider) = self.effect_provider(&effect) else {
                    return errors::permission_denied();
                };
                self.effect_fences.arm(caller, effect, &provider, None)
            }
            EffectFenceCommand::Fence { effect } => {
                if !self.effect_fences.authority(caller) {
                    return errors::permission_denied();
                }
                if self.effect_fences.recovery_only(&effect.scope) {
                    return Response::error(
                        "historical_evidence_required",
                        "recovery requires a host retirement receipt and canonical journal",
                    );
                }
                let Some(provider) = self.effect_provider(&effect) else {
                    return errors::permission_denied();
                };
                match self.effect_fences.fence(caller, effect, &provider) {
                    Ok(Some(request)) => {
                        let issuer = self
                            .effect_fences
                            .issuer_name()
                            .expect("configured authority");
                        // Under this lock the request has never been delivered. It may
                        // already have expired or been released.
                        let _ = self.requests.cancel(issuer, request, now);
                        Response::Ok
                    }
                    Ok(None) => Response::Ok,
                    Err(error) => error,
                }
            }
            EffectFenceCommand::Dispatch { effect } => {
                let provider = self.effect_provider(&effect);
                if provider
                    .as_ref()
                    .is_some_and(|provider| self.effect_fences.provider_unavailable(provider))
                {
                    return Response::error("effect_fenced", "provider instance has retired");
                }
                let incarnation = provider.as_ref().and_then(|name| {
                    self.configuration
                        .host_context
                        .provider_incarnations
                        .get(name)
                });
                match self
                    .effect_fences
                    .may_dispatch(caller, &effect, incarnation)
                {
                    Ok(Some(id)) => {
                        return if self.requests.retained(caller, id) {
                            Response::Accepted { id }
                        } else {
                            Response::error(
                                "effect_admission_expired",
                                "admitted request is no longer retained",
                            )
                        };
                    }
                    Ok(None) => {}
                    Err(error) => return error,
                }
                let Some(grants) = self.configuration.principals.get(caller) else {
                    return errors::permission_denied();
                };
                let initiator = match effect.parent {
                    Some(parent) => match self.requests.initiator_for_parent(caller, parent) {
                        Ok(initiator) => initiator.to_owned(),
                        Err(error) => return error,
                    },
                    None => caller.to_owned(),
                };
                let (payload, response_schema) = match self.api.callable(
                    caller,
                    &initiator,
                    grants,
                    ApiInvocation {
                        service: &effect.service,
                        method: &effect.method,
                        major: effect.interface_major,
                        minor: effect.interface_minor,
                        payload: &effect.payload,
                    },
                ) {
                    Ok(value) => value,
                    Err(error) => return error,
                };
                let response = self.requests.submit_guarded(
                    caller,
                    grants,
                    RequestOptions {
                        service: effect.service.clone(),
                        payload,
                        timeout_ms: effect.timeout_ms,
                        operation_id: effect.operation_id.clone(),
                        parent: effect.parent,
                        response_schema: Some(response_schema),
                    },
                    now,
                );
                if let Response::Accepted { id } = &response {
                    self.effect_fences.admitted(&effect, *id);
                }
                response
            }
        }
    }

    fn host_retirement_write_matches(&self, request: &serde_json::Value) -> bool {
        use amaterasu_contracts::effect_fence::{
            HostProviderInstanceRetirement, HostProviderRetirement,
        };
        if request.get("action").and_then(serde_json::Value::as_str) == Some("get") {
            return true;
        }
        let Some(Some(selection)) = self
            .configuration
            .service_selections
            .broker
            .get("amaterasu.state/1")
        else {
            return false;
        };
        let Some(key) = request.get("key").and_then(serde_json::Value::as_str) else {
            return false;
        };
        if key.starts_with("host/provider-retirement/v1/") {
            return request.get("value").and_then(|value| serde_json::from_value::<HostProviderInstanceRetirement>(value.clone()).ok()).is_some_and(|receipt| receipt.state_key() == key && matches!(self.effect_fences.host_provider_instance_receipt(&receipt.provider, &receipt.provider_incarnation), Response::Result { payload } if serde_json::to_value(receipt).ok().as_ref() == Some(&payload)));
        }
        if request.get("action").and_then(serde_json::Value::as_str) == Some("delete") {
            return request
                .get("expected_revision")
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|revision| self.receipt_cleanup.get(key) == Some(&revision));
        }
        if key.starts_with("host/effect-retirement/v2/") {
            return request
                .get("value")
                .and_then(|value| {
                    serde_json::from_value::<HostProviderRetirement>(value.clone()).ok()
                })
                .is_some_and(|receipt| {
                    receipt.state_key() == key
                        && !self.effect_fences.slot_incarnation_retired(
                            receipt.issued_lease.slot,
                            receipt.issued_lease.incarnation,
                        )
                        && self.effect_fences.captured_host_receipt(&receipt)
                });
        }
        let Some(revision) = request
            .get("expected_revision")
            .and_then(serde_json::Value::as_u64)
        else {
            return false;
        };
        let Some(receipts) = request
            .pointer("/value/receipts")
            .and_then(serde_json::Value::as_array)
        else {
            return false;
        };
        receipts.iter().all(|value| {
            let Ok(receipt) = serde_json::from_value::<HostProviderRetirement>(value.clone()) else { return false };
            let current = matches!(self.effect_fences.host_retirement_receipt(&receipt.effect.scope, receipt.effect.sequence), Response::Result { payload } if payload == *value);
            current || self.requests.retained_state_value_contains(HOST_LIFECYCLE_CONTROL_PRINCIPAL, &selection.provider, key, revision, value)
        })
    }

    fn host_retirement_record_matches(
        &self,
        receipt: &amaterasu_contracts::effect_fence::HostProviderRetirement,
        request_id: u64,
    ) -> bool {
        let Some(Some(selection)) = self
            .configuration
            .service_selections
            .broker
            .get("amaterasu.state/1")
        else {
            return false;
        };
        if selection.contract != "amaterasu.state/1" {
            return false;
        }
        let Some((request, result)) = self.requests.selected_state_read(
            HOST_LIFECYCLE_CONTROL_PRINCIPAL,
            &selection.provider,
            request_id,
        ) else {
            return false;
        };
        let key = format!("host/effect-retirement/v1/{:02}", receipt.issued_lease.slot);
        let request_key = request.get("key").and_then(serde_json::Value::as_str);
        if request_key == Some(receipt.state_key().as_str()) {
            return result.get("ok").and_then(serde_json::Value::as_bool) == Some(true)
                && result.get("found").and_then(serde_json::Value::as_bool) == Some(true)
                && serde_json::to_value(receipt).ok().as_ref() == result.get("value");
        }
        if request_key != Some(&key)
            || result.get("ok").and_then(serde_json::Value::as_bool) != Some(true)
            || result.get("found").and_then(serde_json::Value::as_bool) != Some(true)
            || result
                .pointer("/value/version")
                .and_then(serde_json::Value::as_u64)
                != Some(1)
        {
            return false;
        }
        let Some(receipts) = result
            .pointer("/value/receipts")
            .and_then(serde_json::Value::as_array)
        else {
            return false;
        };
        let Ok(expected) = serde_json::to_value(receipt) else {
            return false;
        };
        !receipts.is_empty()
            && receipts.len() <= 32
            && receipts.iter().any(|item| item == &expected)
    }

    fn verify_historical_journal(
        &self,
        identity: &amaterasu_contracts::effect_fence::GuardedEffectIdentity,
        incarnation: &amaterasu_contracts::effect_fence::ProviderIncarnation,
        expected_entry: &serde_json::Value,
        state_request: u64,
    ) -> Result<(), Response> {
        let Some(Some(selection)) = self
            .configuration
            .service_selections
            .broker
            .get("amaterasu.state/1")
        else {
            return Err(Response::error(
                "recovery_required",
                "durable State provider is not explicitly selected",
            ));
        };
        if selection.contract != "amaterasu.state/1" {
            return Err(errors::permission_denied());
        }
        let authority = self
            .effect_fences
            .authority_name()
            .ok_or_else(errors::permission_denied)?;
        let issuer = self
            .effect_fences
            .issuer_name()
            .ok_or_else(errors::permission_denied)?;
        let (request, result) = self
            .requests
            .selected_state_read(authority, &selection.provider, state_request)
            .ok_or_else(errors::permission_denied)?;
        let current_lease = self
            .effect_fences
            .current_lease(&identity.scope)
            .ok_or_else(errors::permission_denied)?;
        if historical_journal::matches(
            request,
            result,
            issuer,
            identity,
            incarnation,
            expected_entry,
            &current_lease,
        ) {
            Ok(())
        } else {
            Err(Response::error(
                "effect_conflict",
                "historical journal evidence differs",
            ))
        }
    }

    fn selected_effect_method(
        &self,
        method: &amaterasu_contracts::effect_fence::ProtectedMethod,
    ) -> Option<amaterasu_contracts::effect_fence::SelectedEffectMethod> {
        if !self
            .effect_fences
            .protects(&method.service, &method.method, method.interface_major)
        {
            return None;
        }
        let issuer = self.effect_fences.issuer_name()?;
        let grants = self.configuration.principals.get(issuer)?;
        let approval = self.api.selected_for(
            issuer,
            grants,
            &method.service,
            &method.method,
            method.interface_major,
        )?;
        if self.effect_fences.provider_unavailable(&approval.provider) {
            return None;
        }
        Some(amaterasu_contracts::effect_fence::SelectedEffectMethod {
            version: 1,
            provider: approval.provider.clone(),
            method: approval.method.clone(),
            provider_incarnation: self
                .configuration
                .host_context
                .provider_incarnations
                .get(&approval.provider)
                .cloned(),
        })
    }

    fn recovery_provider_matches(
        &self,
        identity: &amaterasu_contracts::effect_fence::GuardedEffectIdentity,
    ) -> bool {
        self.configuration.api_methods.iter().any(|approval| {
            approval.provider == identity.provider
                && approval.method.service == identity.service
                && approval.method.method == identity.method
                && approval.method.interface_major == identity.interface_major
                && approval.method.interface_minor == identity.interface_minor
        })
    }

    fn effect_provider(
        &self,
        effect: &amaterasu_contracts::effect_fence::GuardedEffect,
    ) -> Option<String> {
        self.configuration
            .api_methods
            .iter()
            .find(|approval| {
                approval.method.service == effect.service
                    && approval.method.method == effect.method
                    && approval.method.interface_major == effect.interface_major
                    && approval.method.interface_minor == effect.interface_minor
            })
            .map(|approval| approval.provider.clone())
    }

    fn enqueue_log(
        &mut self,
        caller: &str,
        level: u8,
        target: String,
        message: String,
    ) -> Response {
        if level > 5
            || target.is_empty()
            || target.len() > MAX_LOG_TARGET_BYTES
            || target.chars().any(char::is_control)
            || message.len() > MAX_LOG_MESSAGE_BYTES
            || message.chars().any(|character| character == '\0')
        {
            return Response::error("invalid_log_record", "log record is invalid");
        }
        let target = format!("{caller}::{target}");
        if target.len() > MAX_LOG_TARGET_BYTES {
            return Response::error("invalid_log_record", "log record is invalid");
        }
        if self.log_events.len() >= LOG_EVENT_QUEUE_CAPACITY {
            self.dropped_log_records = self.dropped_log_records.saturating_add(1);
            return Response::Ok;
        }
        self.log_events.push_back(HostLogEvent::Record {
            level,
            target,
            message,
        });
        Response::Ok
    }

    fn update_logging_policy(
        &mut self,
        caller: &str,
        revision: u64,
        minimum_level: u8,
        log_path: String,
    ) -> Response {
        if caller != DIAGNOSTICS_COMPONENT {
            return errors::permission_denied();
        }
        if minimum_level > 6
            || log_path.is_empty()
            || log_path.len() > MAX_LOG_PATH_BYTES
            || log_path.chars().any(char::is_control)
        {
            return Response::error("invalid_logging_policy", "logging policy is invalid");
        }
        if revision < self.logging_revision {
            return Response::error("stale_revision", "logging policy revision is stale");
        }
        if self.logging_policy_received && revision == self.logging_revision {
            return Response::Ok;
        }

        if self.log_events.len() >= LOG_EVENT_QUEUE_CAPACITY {
            if let Some(record_index) = self
                .log_events
                .iter()
                .position(|event| matches!(event, HostLogEvent::Record { .. }))
            {
                self.log_events.remove(record_index);
                self.dropped_log_records = self.dropped_log_records.saturating_add(1);
            } else {
                return Response::error("capacity_exhausted", "host log event queue is full");
            }
        }
        self.log_events.push_back(HostLogEvent::Policy {
            revision,
            minimum_level,
            log_path,
        });
        self.logging_revision = revision;
        self.logging_policy_received = true;
        Response::Ok
    }
}
