//! Typed, startup-selected virtual-display service contract.
//!
//! This is a new contract; it does not imply that a legacy `/1` provider or
//! consumer implements it. Native providers are trusted code. The broker
//! authenticates the immediate caller, which providers must use as the owner;
//! a request never chooses its own owner. A capability is a provider claim,
//! not proof that the underlying platform or hardware can perform an effect.
//! Cancellation and shutdown do not undo effects already performed. Providers
//! must bound retained work and recovery records, retire workers on shutdown,
//! and expose uncertain effects through recovery rather than report success.
use crate::requests::IncomingRequest;
use crate::service::{self, CallOptions, ServiceCall, ServiceError, ServiceMethod};
use crate::{ApiFieldKind, ApiMethod, ApiPrivilege, ApiSchema, Command, Response, RuntimeClient};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SERVICE: &str = "nonary.virtual-display/2";

/// Provider capabilities are advisory and may change after a restart. An
/// unavailable operation must return `Unavailable` without performing it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    pub create: bool,
    pub reconfigure: bool,
    pub borrow: bool,
    /// Whether recovery is supported during this provider process. This alone
    /// does not promise recovery after a restart.
    pub recovery: bool,
    /// Current provider-scoped revision for an initial fenced mutation, when available.
    pub topology: Option<TopologyRevision>,
    pub layouts: Vec<Layout>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    Exclusive,
    Extended,
    ExtendedPrimary,
    ExtendedIsolated,
    ExtendedPrimaryIsolated,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerReference {
    pub identity: String,
    pub reference_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TopologyRevision {
    pub domain_id: String,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub refresh_millihz: u32,
    pub hdr: bool,
}

/// Request-specific acceptance policy. Refresh tolerance is measured in parts
/// per million of the requested rate; zero requires an exact refresh. Providers
/// must reject a tolerance above one million and a zero requested rate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModeAcceptance {
    pub max_refresh_delta_ppm: u32,
    pub require_hdr_match: bool,
}

impl ModeAcceptance {
    /// Check an observed mode against the caller's policy without rounding the
    /// refresh difference. `None` means the policy or requested mode is invalid.
    pub fn accepts(&self, requested: &Mode, observed: &Mode) -> Option<bool> {
        if self.max_refresh_delta_ppm > 1_000_000
            || requested.refresh_millihz == 0
            || requested.width == 0
            || requested.height == 0
        {
            return None;
        }
        let delta = u64::from(requested.refresh_millihz.abs_diff(observed.refresh_millihz));
        Some(
            requested.width == observed.width
                && requested.height == observed.height
                && delta * 1_000_000
                    <= u64::from(requested.refresh_millihz) * u64::from(self.max_refresh_delta_ppm)
                && (!self.require_hdr_match || requested.hdr == observed.hdr),
        )
    }
}

/// Observations are separate from effect admission and do not imply hardware
/// support. `NotObserved` is never evidence that the requested mode was applied.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModeObservation {
    NotApplicable,
    NotObserved,
    Observed { mode: Mode },
}

/// Mutations must compare the expected topology before native effects. A
/// provider must bind every reference and borrow to the authenticated caller.
/// Recover observes a retained outcome; it is not an undo. An unknown or lost
/// record must not be reported as proof that an effect did not occur.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Capabilities,
    Create {
        owner: OwnerReference,
        operation_id: String,
        expected: TopologyRevision,
        mode: Mode,
        acceptance: ModeAcceptance,
        name: String,
        layout: Layout,
        scale: Option<i32>,
    },
    Inspect {
        owner: OwnerReference,
    },
    Remove {
        owner: OwnerReference,
        operation_id: String,
        expected: TopologyRevision,
    },
    Borrow {
        owner: OwnerReference,
    },
    ReleaseBorrow {
        owner: OwnerReference,
        borrow_id: String,
    },
    Reconfigure {
        owner: OwnerReference,
        borrow_id: String,
        operation_id: String,
        expected: TopologyRevision,
        mode: Mode,
        acceptance: ModeAcceptance,
    },
    Recover {
        owner: OwnerReference,
        operation_id: String,
    },
    /// Retry a retained failed or uncertain cleanup against the current topology.
    /// This never creates a new ownership reference.
    RetryCleanup {
        owner: OwnerReference,
        operation_id: String,
        expected: TopologyRevision,
    },
    /// Forget a settled outcome. This does not cancel or undo an effect.
    /// Never reuse the released operation ID.
    ReleaseOperation {
        owner: OwnerReference,
        operation_id: String,
    },
}

impl Request {
    /// Reject malformed references, revision domains and mode policy before a
    /// provider is invoked. Provider-specific limits and authorization remain
    /// the provider's responsibility.
    pub fn is_valid(&self) -> bool {
        fn token(value: &str) -> bool {
            !value.is_empty() && value.len() <= 128 && value.bytes().all(|b| b.is_ascii_graphic())
        }
        fn owner(value: &OwnerReference) -> bool {
            token(&value.identity) && token(&value.reference_id)
        }
        fn topology(value: &TopologyRevision) -> bool {
            token(&value.domain_id)
        }
        fn mode(value: &Mode, acceptance: &ModeAcceptance) -> bool {
            value.width > 0
                && value.height > 0
                && value.refresh_millihz > 0
                && acceptance.max_refresh_delta_ppm <= 1_000_000
        }
        match self {
            Self::Capabilities => true,
            Self::Create {
                owner: o,
                operation_id,
                expected,
                mode: m,
                acceptance,
                name,
                ..
            } => {
                owner(o)
                    && token(operation_id)
                    && topology(expected)
                    && mode(m, acceptance)
                    && !name.is_empty()
                    && name.len() <= 128
                    && !name.chars().any(char::is_control)
            }
            Self::Inspect { owner: o } | Self::Borrow { owner: o } => owner(o),
            Self::Remove {
                owner: o,
                operation_id,
                expected,
            } => owner(o) && token(operation_id) && topology(expected),
            Self::ReleaseBorrow {
                owner: o,
                borrow_id,
            } => owner(o) && token(borrow_id),
            Self::Reconfigure {
                owner: o,
                borrow_id,
                operation_id,
                expected,
                mode: m,
                acceptance,
            } => {
                owner(o)
                    && token(borrow_id)
                    && token(operation_id)
                    && topology(expected)
                    && mode(m, acceptance)
            }
            Self::RetryCleanup {
                owner: o,
                operation_id,
                expected,
            } => owner(o) && token(operation_id) && topology(expected),
            Self::Recover {
                owner: o,
                operation_id,
            }
            | Self::ReleaseOperation {
                owner: o,
                operation_id,
            } => owner(o) && token(operation_id),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectState {
    Pending,
    /// The provider reports the effect ready. Mode and HDR still require an
    /// observed outcome and the caller's acceptance check.
    Applied,
    Removed,
    Absent,
    /// The owned reference was released and required cleanup has settled.
    Completed,
    /// Cleanup failed and retains an obligation to reconcile or retry.
    Failed,
    /// Available evidence cannot establish the outcome.
    Unknown,
    RecoveryRequired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Capabilities {
        capabilities: Capabilities,
    },
    Effect {
        state: EffectState,
        topology: Option<TopologyRevision>,
        observation: ModeObservation,
    },
    Borrowed {
        borrow_id: String,
        topology: TopologyRevision,
    },
    Unavailable,
    StaleTopology {
        current: TopologyRevision,
    },
    NotOwned,
    /// The provider forgot its retry record; this is not display removal.
    OperationReleased,
    /// An operation record still has an unresolved effect or cleanup obligation.
    OperationNotSettled,
    Conflict,
    InvalidRequest,
    ResourceExhausted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestEnvelope {
    pub request: Request,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplyEnvelope {
    pub reply: Reply,
}

pub struct DisplayService;
impl ServiceMethod for DisplayService {
    type Request = RequestEnvelope;
    type Response = ReplyEnvelope;
    fn method() -> ApiMethod {
        let schema = |name: &str| ApiSchema {
            required: [(name.into(), ApiFieldKind::Object)].into(),
            optional: Default::default(),
            allow_extra: false,
        };
        ApiMethod {
            service: SERVICE.into(),
            method: "execute".into(),
            interface_major: 2,
            interface_minor: 0,
            request: schema("request"),
            response: schema("reply"),
            privilege: ApiPrivilege::User,
        }
    }
}

/// Submit an asynchronous call. Reuse the same broker operation ID and exact
/// payload after uncertain admission; a new logical operation needs a new ID.
pub fn call<'a>(
    runtime: &'a RuntimeClient,
    options: CallOptions<'_>,
    request: Request,
) -> Result<ServiceCall<'a, DisplayService>, ServiceError> {
    service::call::<DisplayService>(runtime, options, &RequestEnvelope { request })
}

/// Implementation boundary for a startup-selected provider. The adapter supplies
/// broker-authenticated caller identity and checks admission before calling it.
/// Implementations must retain bounded operation outcomes for retries and
/// recovery, reject ID reuse with different payloads, and never expose another
/// caller's resources. `completion_lost` must preserve or retire uncertain
/// resources; it must not assume an effect was rolled back.
pub trait Provider {
    fn execute(&mut self, caller: &str, request: &Request) -> Reply;
    fn completion_lost(&mut self, caller: &str, request: &Request, reply: &Reply);
    fn shutdown(&mut self);
}

/// Owns a provider and invokes its shutdown hook exactly once when retired.
/// The hook must synchronously retire provider workers; it is not an undo.
pub struct ProviderAdapter<P: Provider> {
    provider: P,
}

impl<P: Provider> ProviderAdapter<P> {
    pub fn new(provider: P) -> Self {
        Self { provider }
    }

    pub fn handle(
        &mut self,
        runtime: &RuntimeClient,
        incoming: &IncomingRequest<Value>,
    ) -> Result<(), ServiceError> {
        handle(runtime, &mut self.provider, incoming)
    }
}

impl<P: Provider> Drop for ProviderAdapter<P> {
    fn drop(&mut self) {
        self.provider.shutdown();
    }
}

pub fn register(runtime: &RuntimeClient) -> Result<(), ServiceError> {
    service::register::<DisplayService>(runtime)
}

/// Handle one decoded broker delivery. This does not poll or spawn workers;
/// the provider owns their bounded lifetime. Prefer `ProviderAdapter` to run
/// the shutdown hook when the adapter is retired.
pub fn handle<P: Provider>(
    runtime: &RuntimeClient,
    provider: &mut P,
    incoming: &IncomingRequest<Value>,
) -> Result<(), ServiceError> {
    let call = service::decode::<DisplayService>(incoming)?;
    match runtime
        .send(Command::CheckRequest {
            request: call.request.id,
        })
        .map_err(ServiceError::Transport)?
    {
        Response::Ok => {}
        Response::Error { code, message } => return Err(ServiceError::Broker { code, message }),
        other => return Err(ServiceError::UnexpectedResponse(format!("{other:?}"))),
    }
    let caller = &call.request.caller;
    let request = &call.request.payload.request;
    let valid = request.is_valid();
    let reply = if valid {
        provider.execute(caller, request)
    } else {
        Reply::InvalidRequest
    };
    let result = service::complete::<DisplayService>(
        runtime,
        &call,
        &ReplyEnvelope {
            reply: reply.clone(),
        },
    );
    if valid && result.is_err() {
        provider.completion_lost(caller, request, &reply);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observed_mode_is_checked_against_requested_tolerance_and_hdr() {
        let requested = Mode {
            width: 1920,
            height: 1080,
            refresh_millihz: 60_000,
            hdr: true,
        };
        let mut observed = Mode {
            refresh_millihz: 58_800,
            ..requested.clone()
        };
        let policy = ModeAcceptance {
            max_refresh_delta_ppm: 20_000,
            require_hdr_match: true,
        };
        assert_eq!(policy.accepts(&requested, &observed), Some(true));
        observed.refresh_millihz -= 1;
        assert_eq!(policy.accepts(&requested, &observed), Some(false));
        observed.refresh_millihz = 60_000;
        observed.hdr = false;
        assert_eq!(policy.accepts(&requested, &observed), Some(false));
        assert_eq!(
            ModeAcceptance {
                require_hdr_match: false,
                ..policy.clone()
            }
            .accepts(&requested, &observed),
            Some(true)
        );
        assert_eq!(
            ModeAcceptance {
                max_refresh_delta_ppm: 1_000_001,
                ..policy
            }
            .accepts(&requested, &observed),
            None
        );
        let reply = Reply::Effect {
            state: EffectState::Applied,
            topology: Some(TopologyRevision {
                domain_id: "desktop".into(),
                revision: 1,
            }),
            observation: ModeObservation::Observed { mode: observed },
        };
        assert_eq!(
            serde_json::from_value::<Reply>(serde_json::to_value(&reply).unwrap()).unwrap(),
            reply
        );
    }
}
