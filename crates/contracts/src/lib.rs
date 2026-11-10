//! Version 1 control-plane messages. These types are independent of the bootstrap ABI.
pub mod encoder;
pub mod processing_composition;
pub mod virtual_display;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Maximum serialized broker result, leaving room for its ABI message envelope.
pub const MAX_BROKER_RESULT_BYTES: usize = 256 * 1024;

/// Service name required in a module's explicit effective `call` grants before
/// it may request that the trusted host restart or stop the selected runtime.
pub const HOST_LIFECYCLE_SERVICE: &str = "amaterasu.host-lifecycle/1";

/// One module-drain budget announced before consumers begin shutdown.
pub const HOST_MODULE_SHUTDOWN_TIMEOUT_MS: u64 = 20_000;
/// Allows the worker drain and a bounded process-exit tail before containment.
pub const HOST_WORKER_SHUTDOWN_TIMEOUT_MS: u64 = 25_000;
/// The service manager must outlast the worker's own shutdown allowance.
pub const HOST_SERVICE_STOP_TIMEOUT_MS: u64 = 30_000;

/// Explicit host grant to request a signed package trial.
pub const HOST_PACKAGE_ACTIVATION_SERVICE: &str = "amaterasu.host-package-activation/1";

/// Trusted provider contract for a non-disruptive package replacement barrier.
pub const HOST_PACKAGE_BARRIER_SERVICE: &str = "amaterasu.host-package-barrier/1";

/// Effective call grant allowing a module to read the host-selected state root.
/// It does not grant access to package metadata or host lifecycle commands.
pub const HOST_STATE_CONTEXT_SERVICE: &str = "amaterasu.host-state-context/1";

/// Error code of a direct call the provider did not serve. It proves that no
/// provider effect happened, unlike a lost or timed-out broker request.
pub const DIRECT_UNAVAILABLE: &str = "direct_unavailable";

/// Reserved runtime endpoint identity injected by the verified host. Package
/// component identities cannot use this name because it is not a valid name.
pub const HOST_LIFECYCLE_CONTROL_PRINCIPAL: &str = "@amaterasu.host-lifecycle";
pub const DIAGNOSTICS_COMPONENT: &str = "amaterasu.diagnostics";

/// Reserved host-owned product-log service. Only management may call it;
/// browser authorization is enforced separately by the management module.
pub const HOST_LOG_SERVICE: &str = "amaterasu.host-logs/1";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostLifecycleAction {
    Restart,
    Quit,
}

/// Identifies a candidate already fetched into the trusted repository cache.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostPackageActivation {
    pub repository_id: String,
    pub catalog_digest: String,
    pub manifest_digest: String,
    pub expected_active: String,
    pub expected_revision: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grants {
    #[serde(default)]
    pub native_provide: BTreeSet<String>,
    #[serde(default)]
    pub native_use: BTreeSet<String>,
    #[serde(default)]
    pub provide: BTreeSet<String>,
    #[serde(default)]
    pub call: BTreeSet<String>,
    #[serde(default)]
    pub publish: BTreeSet<String>,
    #[serde(default)]
    pub subscribe: BTreeSet<String>,
    #[serde(default)]
    pub hook: BTreeSet<String>,
}
impl Grants {
    pub fn intersect(&self, policy: &Self) -> Self {
        fn both(a: &BTreeSet<String>, b: &BTreeSet<String>) -> BTreeSet<String> {
            a.intersection(b).cloned().collect()
        }
        Self {
            native_provide: both(&self.native_provide, &policy.native_provide),
            native_use: both(&self.native_use, &policy.native_use),
            provide: both(&self.provide, &policy.provide),
            call: both(&self.call, &policy.call),
            publish: both(&self.publish, &policy.publish),
            subscribe: both(&self.subscribe, &policy.subscribe),
            hook: both(&self.hook, &policy.hook),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookSpec {
    pub owner: String,
    pub participants: Vec<String>,
    pub timeout_ms: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    pub principals: BTreeMap<String, Grants>,
    /// Trusted composition approvals for legacy untyped native factories, keyed
    /// by provider principal and exact service. This does not grant native access.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub legacy_native_contracts: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub api_methods: Vec<ApiMethodApproval>,
    #[serde(default)]
    pub hooks: BTreeMap<String, HookSpec>,
    /// Principals that trusted composition stacks on a service's provider,
    /// keyed by exact service and listed outermost first. A consumer's call
    /// reaches the first layer; a layer's own call to that service reaches
    /// the next one, and the last layer's call reaches the provider. A layer
    /// acts with the authority of every caller of the service it wraps.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub service_layers: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub settings: BTreeMap<String, Value>,
    /// Per-principal processing selections approved by trusted composition.
    /// These are separate from each provider's private settings and confer no grants.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub processing_compositions: BTreeMap<String, processing_composition::ProcessingComposition>,
    /// Host-owned runtime facts are never serialized into a signed package
    /// policy. The host injects this context only after the selected package
    /// has been verified.
    #[serde(default)]
    pub host_context: RuntimeHostContext,
    /// Host-only runtime bootstrap flag. Package verification leaves this false;
    /// the host sets it after verification when creating its private control endpoint.
    #[serde(default)]
    pub host_lifecycle_control: bool,
    /// Host-injected capability; signed package policy cannot enable it.
    #[serde(default)]
    pub host_package_activation_control: bool,
}

/// A wire contract can describe arbitrary namespaced methods. `Any` and
/// `Object` keep nested payloads extensible; the broker still bounds byte size.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiFieldKind {
    Any,
    Null,
    Boolean,
    Integer,
    Number,
    String,
    Array,
    Object,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiSchema {
    pub required: BTreeMap<String, ApiFieldKind>,
    #[serde(default)]
    pub optional: BTreeMap<String, ApiFieldKind>,
    #[serde(default)]
    pub allow_extra: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiPrivilege {
    User,
    Elevated,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiMethod {
    pub service: String,
    pub method: String,
    pub interface_major: u32,
    pub interface_minor: u32,
    pub request: ApiSchema,
    pub response: ApiSchema,
    pub privilege: ApiPrivilege,
}

/// Installed policy, not a publisher's manifest, controls provider identity,
/// callable identities, and whether a handler is approved at all.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiMethodApproval {
    pub provider: String,
    pub callers: BTreeSet<String>,
    pub method: ApiMethod,
}

/// Bounded identity metadata for one component in the verified running package.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunningModuleIdentity {
    pub id: String,
    pub version: String,
    pub sha256: String,
    pub runtime: bool,
}

/// Facts supplied by the trusted host for one worker process.
///
/// `state_root` is intentionally separate from module settings: a release
/// policy may choose modules and grants, but it must not choose where a
/// user-token process persists state.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeHostContext {
    pub schema: u32,
    #[serde(default)]
    pub state_root: Option<String>,
    /// The verified package root is injected by the host, never by package settings.
    /// Only the management principal may request it from the runtime broker.
    #[serde(default)]
    pub package_root: Option<String>,
    /// Verified package digest loaded into this worker, overwritten by the host.
    /// Only management may request it; persisted selection can differ.
    #[serde(default)]
    pub running_package_digest: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    NativeRegister {
        service: String,
        factory: usize,
    },
    /// Registers a native factory with an immutable interface-family identity.
    /// Contract metadata is copied and checked before invoking a typed binding.
    NativeRegisterTyped {
        service: String,
        factory: usize,
        contract: String,
    },
    NativeBindTyped {
        service: String,
        interface_major: u32,
        min_minor: u32,
        min_size: u32,
        contract: String,
    },
    NativeBind {
        service: String,
        interface_major: u32,
        min_minor: u32,
        min_size: u32,
    },
    NativeRelease {
        binding: u64,
    },
    /// Queues one of the host's fixed lifecycle actions. Runtime authorization
    /// requires the caller's explicit lifecycle service call grant.
    HostLifecycle {
        action: HostLifecycleAction,
    },
    /// Request a host-verified package trial; only approved management may call.
    HostPackageActivate {
        request: HostPackageActivation,
    },
    /// Observe or cancel a pending activation before the host starts it.
    HostPackageActivationStatus {},
    HostPackageActivationCancel {},
    /// The trusted barrier provider blocks new work and reports safe retirement.
    HostPackageBarrierRegister {},
    HostPackageBarrierPending {},
    HostPackageBarrierReady {
        ready: bool,
    },
    /// Reserved for the host control endpoint.
    HostPackageActivationDrain {},
    /// Emits one bounded product log record through the trusted host transport.
    Log {
        level: u8,
        target: String,
        message: String,
    },
    /// Applies a settings revision to the host-owned product logger. Only
    /// Diagnostics may submit this trusted policy update.
    LoggingPolicyChanged {
        revision: u64,
        minimum_level: u8,
        log_path: String,
    },
    /// Reserved for the runtime endpoint created by the verified host.
    HostLifecycleDrain {},
    /// Reserved for the host endpoint; returns pending product log events.
    DrainLogEvents {},
    GetSettings {},
    /// Return only this principal's approved SDK processing composition.
    GetProcessingComposition {},
    /// A bounded readiness hint; carries no provider data or effect authority.
    NotifyWorkReady {},
    /// Returns this endpoint's flow trace sink. Older runtimes reject the
    /// operation, which leaves tracing disabled for the caller.
    TraceBind {},
    /// Returns bounded recent flow-trace history: per-step windows, slow flows
    /// and slow spans. Only management and Diagnostics may read it.
    TraceSnapshot {},
    /// Calls the registered provider of `service` synchronously on the calling
    /// thread. Requires the same `call` grant as `Request`. The reply is
    /// `Result`, or the `direct_unavailable` error when the provider does not
    /// serve this operation directly; nothing was done in that case and the
    /// caller may submit an ordinary request.
    CallDirect {
        service: String,
        payload: Value,
        /// Set by a layer to pass on the direct call it is serving for this
        /// service. The next hop is then shown that call's caller. Without it
        /// the call is the caller's own.
        #[serde(default, skip_serializing_if = "is_false")]
        forward: bool,
    },
    /// Returns host-owned context to the state service, and to the Linux audio
    /// module for its restart-safe cleanup journal. This prevents a module
    /// policy from impersonating a host decision through `GetSettings`.
    GetHostContext {},
    /// Returns the host-owned package root only to the management principal.
    GetPackageRoot {},
    /// Returns the verified package digest actually loaded by this worker.
    GetRunningPackageDigest {},
    Register {
        service: String,
    },
    RegisterApi {
        method: ApiMethod,
    },
    QueryApi {
        service: String,
        method: String,
        interface_major: u32,
        min_minor: u32,
    },
    /// Returns the installed approval, including its broker-selected provider.
    /// Subject to the same registration and caller checks as `QueryApi`.
    QueryApiApproval {
        service: String,
        method: String,
        interface_major: u32,
        min_minor: u32,
    },
    CallApi {
        service: String,
        method: String,
        interface_major: u32,
        interface_minor: u32,
        payload: Value,
        timeout_ms: u64,
        operation_id: String,
        #[serde(default)]
        parent: Option<u64>,
    },
    Request {
        service: String,
        payload: Value,
        timeout_ms: u64,
        operation_id: String,
        #[serde(default)]
        parent: Option<u64>,
    },
    /// Finds a retained ordinary request without admitting new work. A missing
    /// result is only evidence about this runtime's current retained records;
    /// callers must account for expiry, release, and concurrent submissions.
    LookupRequest {
        service: String,
        payload: Value,
        operation_id: String,
        #[serde(default)]
        parent: Option<u64>,
    },
    /// Looks up a retained typed API call without submitting new work.
    LookupApiRequest {
        service: String,
        method: String,
        interface_major: u32,
        interface_minor: u32,
        payload: Value,
        operation_id: String,
        #[serde(default)]
        parent: Option<u64>,
    },
    Poll {},
    /// Select a request delivery mode when no delivery is in flight.
    SelectRequestDeliveryMode {
        reliable: bool,
    },
    /// Offer one request repeatedly until accepted or ended.
    PollV2 {
        receiver: String,
    },
    /// Inspect this receiver's unaccepted offer without offering queued work.
    InspectRequestOffer {
        receiver: String,
    },
    /// Accept an offered request before invoking provider effects.
    AcceptRequest {
        request: u64,
        receiver: String,
    },
    /// Report that the accepted delivery's handler and effects have retired.
    /// The broker requires a terminal caller result and retains this receipt
    /// independently of result expiry. Only the accepting receiver may retire.
    RetireRequestDelivery {
        request: u64,
        receiver: String,
    },
    Complete {
        request: u64,
        payload: Value,
    },
    /// Ends a delivered, live request without a result. The provider asserts
    /// that it did not perform the operation; the requester reads the error
    /// code `refused.<code>`. The broker's own terminal codes are not accepted.
    Refuse {
        request: u64,
        code: String,
        message: String,
    },
    CheckRequest {
        request: u64,
    },
    TakeResult {
        request: u64,
    },
    ReleaseResult {
        request: u64,
    },
    Cancel {
        request: u64,
    },
    Subscribe {
        topic: String,
        capacity: usize,
        #[serde(default)]
        coalesce: bool,
    },
    Publish {
        topic: String,
        payload: Value,
    },
    NextEvent {
        subscription: u64,
    },
    Unsubscribe {
        subscription: u64,
    },
    RegisterHook {
        hook: String,
    },
    RunHook {
        hook: String,
        payload: Value,
    },
    HookResult {
        invocation: u64,
    },
    /// Idempotent, owner-scoped invocation with retained results. With
    /// `cleanup`, every participant is attempted and reported; otherwise the
    /// first absent or failed participant ends the invocation. With `chain`,
    /// each participant receives the previous participant's result as input.
    /// A `transient` operation gives its identifier up when the invocation
    /// is released or expires, so a later use starts new work; otherwise the
    /// identifier stays fenced for the broker's lifetime.
    RunHookOperation {
        hook: String,
        payload: Value,
        operation_id: String,
        cleanup: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        chain: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        transient: bool,
    },
    ListApiApprovals {
        parent: Option<u64>,
        after: Option<ApiApprovalCursor>,
        limit: u16,
    },
    LookupHook {
        operation_id: String,
    },
    CancelHook {
        invocation: u64,
    },
    ReleaseHook {
        invocation: u64,
    },
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Prefix of the error code a requester reads after a provider refused its
/// request with `Command::Refuse`. No broker-issued code starts with it.
pub const REFUSED_PREFIX: &str = "refused.";

/// Exclusive, stable ordering key for approved API discovery.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApiApprovalCursor {
    pub revision: u64,
    pub service: String,
    pub method: String,
    pub interface_major: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    NativeBound {
        binding: u64,
        address: usize,
    },
    /// Process-local address of the caller's `TraceSinkApi` table.
    TraceBound {
        address: usize,
    },
    Ok,
    Error {
        code: String,
        message: String,
    },
    Accepted {
        id: u64,
    },
    Pending,
    Result {
        payload: Value,
    },
    Request {
        id: u64,
        caller: String,
        initiator: String,
        service: String,
        payload: Value,
        remaining_ms: u64,
    },
    Event {
        publisher: String,
        topic: String,
        sequence: u64,
        payload: Value,
    },
    ResyncRequired {
        topic: String,
    },
    HookResult {
        proposals: Vec<Proposal>,
    },
    /// Each cleanup participant is attempted in configured order until the deadline.
    ApiApprovals {
        revision: u64,
        approvals: Vec<ApiMethodApproval>,
        next: Option<ApiApprovalCursor>,
    },
    HookCleanupResult {
        outcomes: Vec<HookOutcome>,
    },
    /// Returned only to the reserved host control endpoint.
    HostLifecycle {
        action: Option<HostLifecycleAction>,
    },
    /// Returned only to the reserved host control endpoint.
    HostPackageActivation {
        request: Option<Box<HostPackageActivation>>,
    },
    /// Bounded activation and barrier status for authorized participants.
    HostPackageActivationState {
        pending: bool,
        started: bool,
    },
    /// Returned only to the reserved host control endpoint.
    LogEvents {
        events: Vec<HostLogEvent>,
        dropped_records: u64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HostLogEvent {
    Record {
        level: u8,
        target: String,
        message: String,
    },
    Policy {
        revision: u64,
        minimum_level: u8,
        log_path: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HookOutcome {
    pub participant: String,
    pub result: Box<Response>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Proposal {
    pub participant: String,
    pub payload: Value,
}

impl Response {
    pub fn error(code: &str, message: impl Into<String>) -> Self {
        Self::Error {
            code: code.into(),
            message: message.into(),
        }
    }
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error { .. })
    }
}

/// At most this many services can carry layers.
pub const MAX_LAYERED_SERVICES: usize = 64;
/// At most this many layers can be stacked on one service.
pub const MAX_LAYERS_PER_SERVICE: usize = 8;

impl RuntimeConfig {
    /// Checks the layer listing on its own terms. A layer stands in the path
    /// of every call to its service, so policy must name real principals,
    /// name each one once, and keep the roles apart: a service's provider
    /// cannot also be one of its layers. The runtime and package verification
    /// apply the same rules, so a package the runtime would refuse to start
    /// with is rejected before it is activated.
    pub fn validate_service_layers(&self) -> Result<(), String> {
        if self.service_layers.len() > MAX_LAYERED_SERVICES {
            return Err("too many layered services".into());
        }
        for (service, layers) in &self.service_layers {
            self.validate_layers_of(service, layers)?;
        }
        Ok(())
    }

    fn validate_layers_of(&self, service: &str, layers: &[String]) -> Result<(), String> {
        let host_owned = matches!(
            service,
            HOST_LIFECYCLE_SERVICE
                | HOST_LOG_SERVICE
                | HOST_PACKAGE_ACTIVATION_SERVICE
                | HOST_PACKAGE_BARRIER_SERVICE
                | HOST_STATE_CONTEXT_SERVICE
        );
        if !valid_name(service) || host_owned || service.starts_with("hooks/") {
            return Err(format!("service cannot be layered: {service}"));
        }
        let unique_layers: BTreeSet<_> = layers.iter().collect();
        if layers.len() > MAX_LAYERS_PER_SERVICE || unique_layers.len() != layers.len() {
            return Err(format!(
                "too many or duplicate layers for service {service}"
            ));
        }
        for layer in layers {
            let Some(grants) = self.principals.get(layer) else {
                return Err(format!("unknown layer {layer} for service {service}"));
            };
            if layer == HOST_LIFECYCLE_CONTROL_PRINCIPAL || grants.provide.contains(service) {
                return Err(format!(
                    "layer {layer} cannot also provide service {service}"
                ));
            }
        }
        Ok(())
    }
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-' | b'/')
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn older_configurations_default_to_no_settings() {
        let configuration: RuntimeConfig =
            serde_json::from_value(json!({"principals": {}})).unwrap();
        assert!(configuration.settings.is_empty());
    }

    #[test]
    fn native_grants_default_empty_and_intersect_independently() {
        let old: Grants = serde_json::from_value(json!({"call":["frames/1"]})).unwrap();
        assert!(old.native_use.is_empty());
        assert!(old.native_provide.is_empty());
        let requested = Grants {
            native_use: ["frames/1".into()].into(),
            native_provide: ["frames/1".into()].into(),
            ..Default::default()
        };
        let policy = Grants {
            native_use: ["frames/1".into()].into(),
            call: ["frames/1".into()].into(),
            ..Default::default()
        };
        let effective = requested.intersect(&policy);
        assert_eq!(effective.native_use, requested.native_use);
        assert!(effective.native_provide.is_empty());
        assert!(effective.call.is_empty());
    }

    #[test]
    fn control_envelopes_reject_duplicate_and_unknown_fields() {
        for envelope in [
            r#"{"op":"get_settings","caller":"other"}"#,
            r#"{"op":"native_bind","service":"frames/1","interface_major":1,"min_minor":0,"min_size":32,"caller":"other"}"#,
            r#"{"op":"poll","op":"get_settings"}"#,
            r#"{"op":"cancel","request":1,"request":2}"#,
            r#"{"op":"request","service":"work/1","payload":null,"timeout_ms":1,"operation_id":"one","authority":{}}"#,
        ] {
            assert!(
                serde_json::from_str::<Command>(envelope).is_err(),
                "{envelope}"
            );
        }
        assert!(
            serde_json::from_value::<RuntimeConfig>(json!({
                "principals": {}, "settings": {}, "unknown": true,
            }))
            .is_err()
        );
    }

    #[test]
    fn host_lifecycle_commands_encode_only_fixed_actions() {
        assert_eq!(
            serde_json::to_value(Command::HostLifecycle {
                action: HostLifecycleAction::Restart,
            })
            .unwrap(),
            json!({"op":"host_lifecycle","action":"restart"})
        );
        assert!(
            serde_json::from_value::<Command>(json!({
                "op":"host_lifecycle",
                "action":"execute"
            }))
            .is_err()
        );
    }
}
