//! Durable action permits and reservations; integration must journal each change.
//!
//! This is not an identity provider: the coordinator supplies the configured
//! broker service identity, independently resolved target ownership, and its
//! approved immutable plan. Clone, mutate, persist, then publish the clone and
//! reply; never authorize effects if persistence has an uncertain outcome.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const MAX_OPERATIONS: usize = 1024;
const MAX_RETIRED_OPERATIONS: usize = 1024;
const MAX_ID: usize = 128;
const MAX_RECEIPT: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Principal {
    pub caller: String,
    pub initiator: String,
}

/// Wire-compatible with the public ActionTarget. Only coordinator-resolved,
/// active targets may be used for authorization or consumption.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActionTarget {
    Session {
        host_epoch: String,
        session_id: String,
        expected_generation: u64,
    },
    Stream {
        host_epoch: String,
        session_id: String,
        stream_id: String,
        expected_generation: u64,
    },
    Host {
        host_epoch: String,
    },
}

impl ActionTarget {
    fn epoch(&self) -> &str {
        match self {
            Self::Session { host_epoch, .. }
            | Self::Stream { host_epoch, .. }
            | Self::Host { host_epoch } => host_epoch,
        }
    }
    fn session(&self) -> Option<&str> {
        match self {
            Self::Session { session_id, .. } | Self::Stream { session_id, .. } => Some(session_id),
            Self::Host { .. } => None,
        }
    }
    fn valid(&self) -> bool {
        valid_id(self.epoch())
            && match self {
                Self::Session {
                    session_id,
                    expected_generation,
                    ..
                } => valid_id(session_id) && *expected_generation > 0,
                Self::Stream {
                    session_id,
                    stream_id,
                    expected_generation,
                    ..
                } => valid_id(session_id) && valid_id(stream_id) && *expected_generation > 0,
                Self::Host { .. } => true,
            }
    }
    // Conservative: operations within the same session, and host operations,
    // conflict even when their individual stream IDs differ.
    fn conflicts(&self, other: &Self) -> bool {
        self.session().is_none() || other.session().is_none() || self.session() == other.session()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionBinding {
    pub operation_id: String,
    pub target: ActionTarget,
    pub action_id: String,
    pub plan_digest: String,
    pub source: Principal,
}

/// Values are supplied by trusted coordinator state, never decoded from a plugin
/// request as proof of ownership, active status, policy approval, or plan approval.
pub struct CanonicalAction<'a> {
    pub action_service: &'a str,
    pub target: &'a ActionTarget,
    pub owner: &'a Principal,
    pub action_id: &'a str,
    pub plan_digest: &'a str,
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", deny_unknown_fields)]
enum ReservationState {
    Issued { token: String, expires_at: u64 },
    Running,
    RecoveryRequired { receipt: Value },
    Succeeded { receipt: Value },
    Failed { receipt: Value },
    Cancelled,
    Expired,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionExecution {
    provider: String,
    method: amaterasu_extension_api::MethodRef,
    plan: amaterasu_extension_api::ResourcePlan,
    #[serde(default)]
    approval_snapshot: Option<ActionApprovalSnapshot>,
    claimed: bool,
    #[serde(default)]
    guard_nonces: std::collections::BTreeSet<String>,
}

/// Exact broker approvals attested by the configured action service before authorization.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionApprovalSnapshot {
    #[serde(default)]
    pub invoke: Option<amaterasu_extension_api::ActionInvoke>,
    pub descriptor: amaterasu_extension_api::ActionDescriptor,
    pub execute_approval: amaterasu_contracts::ApiMethodApproval,
    pub status_approval: Option<amaterasu_contracts::ApiMethodApproval>,
}

fn approval_matches(
    approval: &amaterasu_contracts::ApiMethodApproval,
    provider: &str,
    method: &amaterasu_extension_api::MethodRef,
    action_service: &str,
) -> bool {
    approval.provider == provider
        && approval.callers.contains(action_service)
        && approval.method.service == method.service
        && approval.method.method == method.method
        && approval.method.interface_major == method.interface_major
        && approval.method.interface_minor == method.interface_minor
}

fn valid_approval_snapshot(
    snapshot: &ActionApprovalSnapshot,
    provider: &str,
    method: &amaterasu_extension_api::MethodRef,
    binding: &ActionBinding,
    action_service: &str,
) -> bool {
    snapshot.invoke.as_ref().is_none_or(|invoke| {
        invoke.operation_id == binding.operation_id
            && invoke.action_id == binding.action_id
            && serde_json::to_value(&invoke.target).ok()
                == serde_json::to_value(&binding.target).ok()
    }) && snapshot.descriptor.id == binding.action_id
        && snapshot.descriptor.execute == *method
        && approval_matches(&snapshot.execute_approval, provider, method, action_service)
        && match (&snapshot.descriptor.status, &snapshot.status_approval) {
            (Some(method), Some(approval)) => {
                approval_matches(approval, provider, method, action_service)
            }
            (None, None) => true,
            _ => false,
        }
        && serde_json::to_vec(snapshot).is_ok_and(|bytes| bytes.len() <= 32 * 1024)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionRebind {
    old_token: String,
    old_binding: ActionBinding,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reservation {
    action_service: String,
    permit_token: String,
    #[serde(default)]
    rebind: Option<ActionRebind>,
    /// Set only when the running action is owned by a durable teardown.
    transition_id: Option<String>,
    transition_ready: bool,
    execution: Option<ActionExecution>,
    binding: ActionBinding,
    state: ReservationState,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetiredAction {
    service_operation_digest: String,
    permit_digest: String,
    old_permit_digest: Option<String>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionAuthority {
    reservations: BTreeMap<String, Reservation>,
    #[serde(default)]
    retired: BTreeMap<String, RetiredAction>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthorityError {
    Invalid,
    NotAuthorized,
    Conflict,
    Expired,
    Capacity,
    RandomUnavailable,
    InvalidState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActionOutcome {
    Succeeded,
    Failed,
    RecoveryRequired,
}

fn valid_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_ID && !value.chars().any(char::is_control)
}
fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn valid_token(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn canonical_plan_digest(plan: &amaterasu_extension_api::ResourcePlan) -> Option<String> {
    let value = serde_json::to_value(plan).ok()?;
    let bytes = serde_json::to_vec(&value).ok()?;
    Some(format!("{:x}", Sha256::digest(bytes)))
}

fn valid_binding(binding: &ActionBinding) -> bool {
    valid_id(&binding.operation_id)
        && binding.target.valid()
        && valid_id(&binding.action_id)
        && valid_digest(&binding.plan_digest)
        && valid_id(&binding.source.caller)
        && valid_id(&binding.source.initiator)
}
fn key(binding: &ActionBinding) -> String {
    serde_json::to_string(&(
        &binding.source.caller,
        &binding.source.initiator,
        &binding.operation_id,
    ))
    .expect("string tuple serializes")
}
fn fence_digest<T: Serialize>(kind: &str, value: &T) -> String {
    let bytes = serde_json::to_vec(&(kind, value)).expect("serializable replay identity");
    format!("{:x}", Sha256::digest(bytes))
}
fn operation_fence(binding: &ActionBinding) -> String {
    fence_digest("action-operation", &key(binding))
}
fn service_operation_fence(service: &str, operation_id: &str, action_id: &str) -> String {
    fence_digest(
        "action-service-operation",
        &(service, operation_id, action_id),
    )
}
fn permit_fence(token: &str) -> String {
    fence_digest("action-permit", &token)
}
fn valid_fence(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn receipt_valid(receipt: &Value) -> bool {
    receipt.is_object() && serde_json::to_vec(receipt).is_ok_and(|bytes| bytes.len() <= MAX_RECEIPT)
}
fn is_barrier(state: &ReservationState) -> bool {
    matches!(
        state,
        ReservationState::Issued { .. }
            | ReservationState::Running
            | ReservationState::RecoveryRequired { .. }
    )
}
fn check_canonical(
    caller: &str,
    binding: &ActionBinding,
    canonical: &CanonicalAction<'_>,
) -> Result<(), AuthorityError> {
    if !valid_binding(binding) || !valid_id(canonical.action_service) {
        return Err(AuthorityError::Invalid);
    }
    if caller != canonical.action_service
        || !canonical.active
        || &binding.target != canonical.target
        || &binding.source != canonical.owner
        || binding.action_id != canonical.action_id
        || binding.plan_digest != canonical.plan_digest
    {
        return Err(AuthorityError::NotAuthorized);
    }
    Ok(())
}

impl ActionAuthority {
    fn retired_token(&self, token: &str) -> bool {
        let digest = permit_fence(token);
        self.retired.values().any(|item| {
            item.permit_digest == digest || item.old_permit_digest.as_ref() == Some(&digest)
        })
    }

    /// Validate untrusted persisted data before making it authoritative.
    pub fn validate(&self) -> Result<(), AuthorityError> {
        if self.reservations.len() > MAX_OPERATIONS || self.retired.len() > MAX_RETIRED_OPERATIONS {
            return Err(AuthorityError::InvalidState);
        }
        let mut retired_tokens = std::collections::BTreeSet::new();
        for (operation, item) in &self.retired {
            if !valid_fence(operation)
                || !valid_fence(&item.service_operation_digest)
                || !valid_fence(&item.permit_digest)
                || !retired_tokens.insert(&item.permit_digest)
                || item
                    .old_permit_digest
                    .as_ref()
                    .is_some_and(|old| !valid_fence(old) || !retired_tokens.insert(old))
            {
                return Err(AuthorityError::InvalidState);
            }
        }
        let mut tokens = std::collections::BTreeSet::new();
        for (stored_key, reservation) in &self.reservations {
            if !valid_id(&reservation.action_service)
                || !valid_binding(&reservation.binding)
                || !valid_token(&reservation.permit_token)
                || reservation
                    .transition_id
                    .as_deref()
                    .is_some_and(|id| !valid_id(id))
                || (reservation.transition_ready && reservation.transition_id.is_none())
                || reservation.execution.as_ref().is_some_and(|execution| {
                    !valid_id(&execution.provider)
                        || !valid_id(&execution.method.service)
                        || !valid_id(&execution.method.method)
                        || execution.method.interface_major == 0
                        || execution.method.schema_revision == 0
                        || execution
                            .approval_snapshot
                            .as_ref()
                            .is_some_and(|snapshot| {
                                !valid_approval_snapshot(
                                    snapshot,
                                    &execution.provider,
                                    &execution.method,
                                    &reservation.binding,
                                    &reservation.action_service,
                                )
                            })
                        || execution.guard_nonces.len() > 128
                        || execution.guard_nonces.iter().any(|nonce| !valid_id(nonce))
                        || (!execution.claimed && !execution.guard_nonces.is_empty())
                        || (execution.claimed && !reservation.transition_ready)
                })
                || reservation.rebind.as_ref().is_some_and(|old| {
                    !valid_token(&old.old_token)
                        || !valid_binding(&old.old_binding)
                        || old.old_token == reservation.permit_token
                        || key(&old.old_binding) != key(&reservation.binding)
                        || old.old_binding.action_id != reservation.binding.action_id
                        || old.old_binding.target != reservation.binding.target
                        || reservation.transition_id.is_none()
                        || reservation.execution.as_ref().is_none_or(|execution| {
                            canonical_plan_digest(&execution.plan).as_deref()
                                != Some(reservation.binding.plan_digest.as_str())
                        })
                })
                || !tokens.insert(&reservation.permit_token)
                || retired_tokens.contains(&permit_fence(&reservation.permit_token))
                || self
                    .retired
                    .contains_key(&operation_fence(&reservation.binding))
                || *stored_key != key(&reservation.binding)
            {
                return Err(AuthorityError::InvalidState);
            }
            match &reservation.state {
                ReservationState::Issued { token, expires_at }
                    if token == &reservation.permit_token && *expires_at > 0 => {}
                ReservationState::Running
                | ReservationState::Cancelled
                | ReservationState::Expired => {}
                ReservationState::RecoveryRequired { receipt }
                | ReservationState::Succeeded { receipt }
                | ReservationState::Failed { receipt }
                    if receipt_valid(receipt) => {}
                _ => return Err(AuthorityError::InvalidState),
            }
        }
        for reservation in self.reservations.values() {
            if let Some(old) = &reservation.rebind
                && (!tokens.insert(&old.old_token)
                    || retired_tokens.contains(&permit_fence(&old.old_token)))
            {
                return Err(AuthorityError::InvalidState);
            }
        }
        Ok(())
    }

    /// Issue a one-use permit after the coordinator independently resolves the
    /// active target, original principal, action approval and exact plan digest.
    pub fn authorize_action(
        &mut self,
        caller: &str,
        binding: &ActionBinding,
        canonical: &CanonicalAction<'_>,
        now: u64,
        expires_at: u64,
    ) -> Result<String, AuthorityError> {
        self.validate()?;
        check_canonical(caller, binding, canonical)?;
        if expires_at <= now {
            return Err(AuthorityError::Expired);
        }
        let operation_key = key(binding);
        if self.retired.contains_key(&operation_fence(binding)) {
            return Err(AuthorityError::Conflict);
        }
        if let Some(existing) = self.reservations.get(&operation_key) {
            if existing.binding != *binding || existing.action_service != caller {
                return Err(AuthorityError::Conflict);
            }
            return match &existing.state {
                ReservationState::Issued { token, expires_at } if *expires_at > now => {
                    Ok(token.clone())
                }
                _ => Err(AuthorityError::Conflict),
            };
        }
        if self.reservations.len() >= MAX_OPERATIONS {
            return Err(AuthorityError::Capacity);
        }
        if self.reservations.values().any(|item| {
            item.binding.target.conflicts(&binding.target)
                && match &item.state {
                    ReservationState::Issued { expires_at, .. } => *expires_at > now,
                    other => is_barrier(other),
                }
        }) {
            return Err(AuthorityError::Conflict);
        }
        for _ in 0..4 {
            let token = amaterasu_storage::random_identifier()
                .map_err(|_| AuthorityError::RandomUnavailable)?;
            if !self.retired_token(&token)
                && !self.reservations.values().any(|item| {
                    item.permit_token == token
                        || item
                            .rebind
                            .as_ref()
                            .is_some_and(|old| old.old_token == token)
                })
            {
                self.reservations.insert(
                    operation_key,
                    Reservation {
                        action_service: caller.into(),
                        permit_token: token.clone(),
                        rebind: None,
                        transition_id: None,
                        transition_ready: false,
                        execution: None,
                        binding: binding.clone(),
                        state: ReservationState::Issued {
                            token: token.clone(),
                            expires_at,
                        },
                    },
                );
                return Ok(token);
            }
        }
        Err(AuthorityError::RandomUnavailable)
    }

    /// Resolve immutable binding from the original action service and permit.
    /// The caller must authenticate the broker identity before calling this.
    pub fn binding_for_permit(
        &self,
        caller: &str,
        token: &str,
    ) -> Result<ActionBinding, AuthorityError> {
        self.validate()?;
        self.reservations
            .values()
            .find(|entry| entry.action_service == caller && entry.permit_token == token)
            .map(|entry| entry.binding.clone())
            .ok_or(AuthorityError::NotAuthorized)
    }

    /// Resolve only for the provider frozen by the trusted action service.
    pub fn binding_for_provider(
        &self,
        caller: &str,
        token: &str,
    ) -> Result<ActionBinding, AuthorityError> {
        self.validate()?;
        self.reservations
            .values()
            .find(|entry| {
                entry.permit_token == token
                    && entry
                        .execution
                        .as_ref()
                        .is_some_and(|execution| execution.provider == caller)
            })
            .map(|entry| entry.binding.clone())
            .ok_or(AuthorityError::NotAuthorized)
    }

    /// Re-resolve the canonical active target and persist Running before invoking
    /// a provider. A duplicate consume never authorizes a second effect.
    pub fn consume_action_permit(
        &mut self,
        caller: &str,
        binding: &ActionBinding,
        canonical: &CanonicalAction<'_>,
        token: &str,
        now: u64,
    ) -> Result<(), AuthorityError> {
        self.validate()?;
        check_canonical(caller, binding, canonical)?;
        let item = self
            .reservations
            .get_mut(&key(binding))
            .ok_or(AuthorityError::NotAuthorized)?;
        if item.binding != *binding || item.action_service != caller {
            return Err(AuthorityError::NotAuthorized);
        }
        match &item.state {
            ReservationState::Issued {
                token: expected,
                expires_at,
            } if token == expected && now < *expires_at => {
                item.state = ReservationState::Running;
                Ok(())
            }
            _ => Err(AuthorityError::NotAuthorized),
        }
    }

    /// Caller must authenticate reconciliation independently before setting
    /// `reconciled`; an ordinary completion cannot clear an unknown outcome.
    pub fn complete_action(
        &mut self,
        caller: &str,
        binding: &ActionBinding,
        outcome: ActionOutcome,
        receipt: Value,
        reconciled: bool,
    ) -> Result<(), AuthorityError> {
        self.validate()?;
        if !valid_binding(binding) || !receipt_valid(&receipt) {
            return Err(AuthorityError::Invalid);
        }
        let item = self
            .reservations
            .get_mut(&key(binding))
            .ok_or(AuthorityError::NotAuthorized)?;
        if item.binding != *binding || caller != item.action_service {
            return Err(AuthorityError::NotAuthorized);
        }
        if !matches!(outcome, ActionOutcome::RecoveryRequired)
            && item
                .execution
                .as_ref()
                .is_some_and(|execution| !execution.claimed)
        {
            return Err(AuthorityError::InvalidState);
        }
        let next = match outcome {
            ActionOutcome::Succeeded => ReservationState::Succeeded { receipt },
            ActionOutcome::Failed => ReservationState::Failed { receipt },
            ActionOutcome::RecoveryRequired => ReservationState::RecoveryRequired { receipt },
        };
        if item.state == next {
            return Ok(());
        }
        if !matches!(item.state, ReservationState::Running)
            && !(reconciled && matches!(item.state, ReservationState::RecoveryRequired { .. }))
        {
            return Err(AuthorityError::InvalidState);
        }
        item.state = next;
        Ok(())
    }

    /// Freeze the selected provider/method supplied by the trusted installed
    /// action service. Persist with the issued permit before returning it.
    pub fn bind_execution(
        &mut self,
        caller: &str,
        token: &str,
        binding: &ActionBinding,
        provider: &str,
        method: &amaterasu_extension_api::MethodRef,
        plan: &amaterasu_extension_api::ResourcePlan,
    ) -> Result<(), AuthorityError> {
        self.validate()?;
        if amaterasu_extension_api::validate_resource_plan(plan).is_err() {
            return Err(AuthorityError::Invalid);
        }
        if !valid_id(provider)
            || !valid_id(&method.service)
            || !valid_id(&method.method)
            || method.interface_major == 0
            || method.schema_revision == 0
        {
            return Err(AuthorityError::Invalid);
        }
        let item = self
            .reservations
            .get_mut(&key(binding))
            .ok_or(AuthorityError::NotAuthorized)?;
        if item.action_service != caller
            || item.permit_token != token
            || item.binding != *binding
            || !matches!(item.state, ReservationState::Issued { .. })
        {
            return Err(AuthorityError::NotAuthorized);
        }
        let selected = ActionExecution {
            provider: provider.into(),
            method: method.clone(),
            plan: plan.clone(),
            approval_snapshot: None,
            claimed: false,
            guard_nonces: Default::default(),
        };
        if item.execution.as_ref().is_some_and(|old| old != &selected) {
            return Err(AuthorityError::Conflict);
        }
        item.execution = Some(selected);
        Ok(())
    }

    /// Freeze the service-attested descriptor and complete broker approvals with the permit.
    pub fn bind_approval_snapshot(
        &mut self,
        caller: &str,
        token: &str,
        binding: &ActionBinding,
        snapshot: ActionApprovalSnapshot,
    ) -> Result<(), AuthorityError> {
        self.validate()?;
        let item = self
            .reservations
            .get_mut(&key(binding))
            .ok_or(AuthorityError::NotAuthorized)?;
        if item.action_service != caller
            || item.permit_token != token
            || item.binding != *binding
            || !matches!(item.state, ReservationState::Issued { .. })
        {
            return Err(AuthorityError::NotAuthorized);
        }
        let execution = item
            .execution
            .as_mut()
            .ok_or(AuthorityError::NotAuthorized)?;
        if !valid_approval_snapshot(
            &snapshot,
            &execution.provider,
            &execution.method,
            binding,
            caller,
        ) {
            return Err(AuthorityError::Invalid);
        }
        if execution
            .approval_snapshot
            .as_ref()
            .is_some_and(|old| old != &snapshot)
        {
            return Err(AuthorityError::Conflict);
        }
        execution.approval_snapshot = Some(snapshot);
        Ok(())
    }

    /// Check the retained terminal receipt without granting any execution right.
    pub fn terminal_succeeded_for(
        &self,
        caller: &str,
        token: &str,
        binding: &ActionBinding,
        receipt: &Value,
    ) -> bool {
        self.validate().is_ok() && self.reservations.get(&key(binding)).is_some_and(|item| {
            item.action_service == caller && item.permit_token == token && item.binding == *binding
                && item.execution.as_ref().is_some_and(|execution| execution.claimed)
                && matches!(&item.state, ReservationState::Succeeded { receipt: saved } if saved == receipt)
        })
    }

    /// Read-only proof that this exact selected provider has already claimed
    /// the still-running transferred action. This grants no additional effect.
    pub(crate) fn claimed_effect_for(
        &self,
        service: &str,
        token: &str,
        binding: &ActionBinding,
        provider: &str,
    ) -> bool {
        self.validate().is_ok()
            && self.reservations.get(&key(binding)).is_some_and(|item| {
                item.action_service == service
                    && item.permit_token == token
                    && item.binding == *binding
                    && item.transition_id.is_some()
                    && item.transition_ready
                    && matches!(item.state, ReservationState::Running)
                    && item.execution.as_ref().is_some_and(|execution| {
                        execution.claimed && execution.provider == provider
                    })
            })
    }

    /// Read the approvals frozen for this service and permit. This does not
    /// grant effect rights; callers must separately verify the active transfer.
    pub fn approval_snapshot_for_permit(
        &self,
        caller: &str,
        token: &str,
    ) -> Result<ActionApprovalSnapshot, AuthorityError> {
        self.validate()?;
        self.reservations
            .values()
            .find(|entry| entry.action_service == caller && entry.permit_token == token)
            .and_then(|entry| entry.execution.as_ref())
            .and_then(|execution| execution.approval_snapshot.clone())
            .ok_or(AuthorityError::NotAuthorized)
    }

    pub fn execution_for_permit(
        &self,
        caller: &str,
        token: &str,
    ) -> Result<
        (
            String,
            amaterasu_extension_api::MethodRef,
            amaterasu_extension_api::ResourcePlan,
        ),
        AuthorityError,
    > {
        self.validate()?;
        let item = self
            .reservations
            .values()
            .find(|entry| entry.action_service == caller && entry.permit_token == token)
            .ok_or(AuthorityError::NotAuthorized)?;
        let execution = item
            .execution
            .as_ref()
            .ok_or(AuthorityError::NotAuthorized)?;
        Ok((
            execution.provider.clone(),
            execution.method.clone(),
            execution.plan.clone(),
        ))
    }

    /// Resolve an old token for an idempotent rebind retry. This is available
    /// only to the configured action service; it never authorizes execution.
    pub fn binding_for_rebind(
        &self,
        caller: &str,
        old_token: &str,
    ) -> Result<ActionBinding, AuthorityError> {
        self.validate()?;
        let item = self
            .reservations
            .values()
            .find(|item| {
                item.action_service == caller
                    && (item.permit_token == old_token
                        || item
                            .rebind
                            .as_ref()
                            .is_some_and(|old| old.old_token == old_token))
            })
            .ok_or(AuthorityError::NotAuthorized)?;
        Ok(item
            .rebind
            .as_ref()
            .filter(|old| old.old_token == old_token)
            .map_or_else(|| item.binding.clone(), |old| old.old_binding.clone()))
    }

    /// Replace a transferred action's plan and permit after the coordinator has
    /// verified durable dependent cleanup and fresh installed policy/rights.
    /// Persist this mutation together with the matching Work transfer before
    /// returning the new token. Old-token retries return the identical result.
    pub fn rebind_transferred(
        &mut self,
        caller: &str,
        old_token: &str,
        old_binding: &ActionBinding,
        transition_id: &str,
        plan: &amaterasu_extension_api::ResourcePlan,
        digest: &str,
    ) -> Result<(String, ActionBinding), AuthorityError> {
        self.validate()?;
        if !valid_id(transition_id)
            || !valid_digest(digest)
            || amaterasu_extension_api::validate_resource_plan(plan).is_err()
        {
            return Err(AuthorityError::Invalid);
        }
        if canonical_plan_digest(plan).as_deref() != Some(digest) {
            return Err(AuthorityError::Invalid);
        }
        let reservation_key = key(old_binding);
        let item = self
            .reservations
            .get(&reservation_key)
            .ok_or(AuthorityError::NotAuthorized)?;
        if item.action_service != caller || item.transition_id.as_deref() != Some(transition_id) {
            return Err(AuthorityError::NotAuthorized);
        }
        if let Some(old) = &item.rebind {
            let execution = item
                .execution
                .as_ref()
                .ok_or(AuthorityError::InvalidState)?;
            if old.old_token == old_token
                && old.old_binding == *old_binding
                && item.binding.plan_digest == digest
                && execution.plan == *plan
            {
                return Ok((item.permit_token.clone(), item.binding.clone()));
            }
            return Err(AuthorityError::Conflict);
        }
        let execution = item
            .execution
            .as_ref()
            .ok_or(AuthorityError::InvalidState)?;
        if item.permit_token != old_token
            || item.binding != *old_binding
            || !matches!(item.state, ReservationState::Running)
            || item.transition_ready
            || execution.claimed
        {
            return Err(AuthorityError::NotAuthorized);
        }
        let mut token = None;
        for _ in 0..8 {
            let candidate = amaterasu_storage::random_identifier()
                .map_err(|_| AuthorityError::RandomUnavailable)?;
            if !self.retired_token(&candidate)
                && !self.reservations.values().any(|other| {
                    other.permit_token == candidate
                        || other
                            .rebind
                            .as_ref()
                            .is_some_and(|old| old.old_token == candidate)
                })
            {
                token = Some(candidate);
                break;
            }
        }
        let token = token.ok_or(AuthorityError::RandomUnavailable)?;
        let item = self
            .reservations
            .get_mut(&reservation_key)
            .expect("checked reservation");
        item.rebind = Some(ActionRebind {
            old_token: old_token.into(),
            old_binding: old_binding.clone(),
        });
        item.permit_token = token.clone();
        item.binding.plan_digest = digest.into();
        item.execution.as_mut().expect("checked execution").plan = plan.clone();
        Ok((token, item.binding.clone()))
    }

    /// Only the coordinator may call this after independently confirming all
    /// affected consumer retirements, other work and required mutation rights.
    pub fn mark_transition_ready(
        &mut self,
        caller: &str,
        token: &str,
        binding: &ActionBinding,
        transition_id: &str,
    ) -> Result<(), AuthorityError> {
        self.validate()?;
        let item = self
            .reservations
            .get_mut(&key(binding))
            .ok_or(AuthorityError::NotAuthorized)?;
        if item.action_service != caller
            || item.permit_token != token
            || item.binding != *binding
            || item.transition_id.as_deref() != Some(transition_id)
            || item.execution.is_none()
            || !matches!(item.state, ReservationState::Running)
        {
            return Err(AuthorityError::NotAuthorized);
        }
        item.transition_ready = true;
        Ok(())
    }

    /// Exactly one effect claim for the bound installed provider and method.
    /// Persist the claimed state before replying; an uncertain reply requires
    /// recovery, never another claim or native-effect replay.
    pub fn claim_effect(
        &mut self,
        provider_caller: &str,
        token: &str,
        binding: &ActionBinding,
        method: &amaterasu_extension_api::MethodRef,
    ) -> Result<(), AuthorityError> {
        self.validate()?;
        let item = self
            .reservations
            .get_mut(&key(binding))
            .ok_or(AuthorityError::NotAuthorized)?;
        if item.permit_token != token
            || item.binding != *binding
            || !item.transition_ready
            || !matches!(item.state, ReservationState::Running)
        {
            return Err(AuthorityError::NotAuthorized);
        }
        let execution = item
            .execution
            .as_mut()
            .ok_or(AuthorityError::NotAuthorized)?;
        if execution.provider != provider_caller || &execution.method != method || execution.claimed
        {
            return Err(AuthorityError::NotAuthorized);
        }
        execution.claimed = true;
        Ok(())
    }

    /// Inspect the exact running execution before a provider advances its own
    /// durable effect floor. The coordinator also rechecks current rights.
    pub fn inspect_effect(
        &self,
        provider_caller: &str,
        token: &str,
        binding: &ActionBinding,
        method: &amaterasu_extension_api::MethodRef,
    ) -> Result<(), AuthorityError> {
        self.validate()?;
        let item = self
            .reservations
            .get(&key(binding))
            .ok_or(AuthorityError::NotAuthorized)?;
        let execution = item
            .execution
            .as_ref()
            .ok_or(AuthorityError::NotAuthorized)?;
        if item.permit_token != token
            || item.binding != *binding
            || !item.transition_ready
            || !matches!(item.state, ReservationState::Running)
            || execution.provider != provider_caller
            || &execution.method != method
            || execution.claimed
        {
            return Err(AuthorityError::NotAuthorized);
        }
        Ok(())
    }

    /// A fresh, one-use gap guard for the already-claimed execute operation.
    /// Caller must also recheck the durable domain admission and quiescence.
    pub fn check_effect(
        &mut self,
        provider_caller: &str,
        token: &str,
        binding: &ActionBinding,
        method: &amaterasu_extension_api::MethodRef,
        nonce: &str,
    ) -> Result<(), AuthorityError> {
        self.validate()?;
        if !valid_id(nonce) {
            return Err(AuthorityError::Invalid);
        }
        let item = self
            .reservations
            .get_mut(&key(binding))
            .ok_or(AuthorityError::NotAuthorized)?;
        if item.permit_token != token
            || item.binding != *binding
            || !item.transition_ready
            || !matches!(item.state, ReservationState::Running)
        {
            return Err(AuthorityError::NotAuthorized);
        }
        let execution = item
            .execution
            .as_mut()
            .ok_or(AuthorityError::NotAuthorized)?;
        if execution.provider != provider_caller
            || &execution.method != method
            || !execution.claimed
            || execution.guard_nonces.len() >= 128
            || !execution.guard_nonces.insert(nonce.into())
        {
            return Err(AuthorityError::NotAuthorized);
        }
        Ok(())
    }

    /// Transfer a consumed action to one coordinator-owned teardown. The caller
    /// must atomically persist this change with the transition work record.
    pub fn transfer_to_transition(
        &mut self,
        caller: &str,
        token: &str,
        binding: &ActionBinding,
        transition_id: &str,
    ) -> Result<(), AuthorityError> {
        self.validate()?;
        if !valid_id(transition_id) {
            return Err(AuthorityError::Invalid);
        }
        let item = self
            .reservations
            .get_mut(&key(binding))
            .ok_or(AuthorityError::NotAuthorized)?;
        if item.action_service != caller
            || item.permit_token != token
            || item.binding != *binding
            || !matches!(item.state, ReservationState::Running)
        {
            return Err(AuthorityError::NotAuthorized);
        }
        if item
            .transition_id
            .as_deref()
            .is_some_and(|old| old != transition_id)
        {
            return Err(AuthorityError::Conflict);
        }
        item.transition_id = Some(transition_id.into());
        Ok(())
    }

    /// Fence all conflicting actions while excluding only the exact running
    /// reservation transferred to this persisted transition. Never expose this
    /// exclusion based on a requester-supplied transition ID alone.
    pub fn fence_target_except_transfer(
        &mut self,
        target: &ActionTarget,
        caller: &str,
        token: &str,
        binding: &ActionBinding,
        transition_id: &str,
    ) -> Result<bool, AuthorityError> {
        self.validate()?;
        let own = self
            .reservations
            .get(&key(binding))
            .ok_or(AuthorityError::NotAuthorized)?;
        if own.action_service != caller
            || own.permit_token != token
            || own.binding != *binding
            || own.transition_id.as_deref() != Some(transition_id)
            || !matches!(own.state, ReservationState::Running)
            || !own.binding.target.conflicts(target)
        {
            return Err(AuthorityError::NotAuthorized);
        }
        let own_key = key(binding);
        let mut blocked = false;
        for (id, item) in &mut self.reservations {
            if id == &own_key || !item.binding.target.conflicts(target) {
                continue;
            }
            match item.state {
                ReservationState::Issued { .. } => item.state = ReservationState::Cancelled,
                ReservationState::Running | ReservationState::RecoveryRequired { .. } => {
                    blocked = true
                }
                _ => {}
            }
        }
        Ok(blocked)
    }

    /// Fence new execution before teardown. Returns true while running or
    /// unknown effects still require retirement/reconciliation.
    pub fn fence_target(&mut self, target: &ActionTarget) -> Result<bool, AuthorityError> {
        self.validate()?;
        if !target.valid() {
            return Err(AuthorityError::Invalid);
        }
        let mut blocked = false;
        for item in self
            .reservations
            .values_mut()
            .filter(|item| item.binding.target.conflicts(target))
        {
            match item.state {
                ReservationState::Issued { .. } => item.state = ReservationState::Cancelled,
                ReservationState::Running | ReservationState::RecoveryRequired { .. } => {
                    blocked = true
                }
                _ => {}
            }
        }
        Ok(blocked)
    }

    /// Retire only an exact successful action after the coordinator has
    /// independently verified its terminal certificate. The caller must persist
    /// this mutation atomically with that certificate; exact completion retries
    /// are served by the certificate, never by this replay fence.
    #[allow(dead_code)] // Used by the checkpoint writer when integrated.
    pub(crate) fn retire_certified_success(
        &mut self,
        service: &str,
        token: &str,
        binding: &ActionBinding,
    ) -> Result<(), AuthorityError> {
        self.validate()?;
        let operation = key(binding);
        let item = self
            .reservations
            .get(&operation)
            .ok_or(AuthorityError::NotAuthorized)?;
        if item.action_service != service
            || item.permit_token != token
            || item.binding != *binding
            || !matches!(item.state, ReservationState::Succeeded { .. })
        {
            return Err(AuthorityError::InvalidState);
        }
        if self.retired.len() >= MAX_RETIRED_OPERATIONS {
            return Err(AuthorityError::Capacity);
        }
        let retired = RetiredAction {
            service_operation_digest: service_operation_fence(
                service,
                &binding.operation_id,
                &binding.action_id,
            ),
            permit_digest: permit_fence(token),
            old_permit_digest: item.rebind.as_ref().map(|old| permit_fence(&old.old_token)),
        };
        let digest = operation_fence(binding);
        if self.retired.contains_key(&digest) {
            return Err(AuthorityError::Conflict);
        }
        self.reservations.remove(&operation);
        self.retired.insert(digest, retired);
        Ok(())
    }

    /// Conservatively detects any prior permit for this action operation.
    pub fn has_operation(&self, service: &str, operation_id: &str, action_id: &str) -> bool {
        self.reservations.values().any(|entry| {
            entry.action_service == service
                && entry.binding.operation_id == operation_id
                && entry.binding.action_id == action_id
        }) || self.retired.values().any(|entry| {
            entry.service_operation_digest
                == service_operation_fence(service, operation_id, action_id)
        })
    }

    /// Terminal reservations are references too; a host target aliases all
    /// sessions. Compact replay fences hold no resource or session reference.
    #[allow(dead_code)] // Consumed by the terminal session checkpoint preflight.
    pub(crate) fn has_any_session_reference(&self, session_id: &str) -> bool {
        self.reservations.values().any(|entry| {
            entry
                .binding
                .target
                .session()
                .is_none_or(|id| id == session_id)
        })
    }

    /// Terminal and cross-session reservations can retain resource or domain
    /// references until their independently certified retirement.
    #[allow(dead_code)] // Used by terminal-root checkpoint preflight.
    pub(crate) fn has_any_reservation(&self) -> bool {
        !self.reservations.is_empty()
    }

    /// Any live or uncertain action for this logical target blocks retirement,
    /// including an action from a previous host epoch after restart.
    pub fn has_unresolved_session(&self, _host_epoch: &str, session_id: &str) -> bool {
        self.reservations.values().any(|entry| {
            is_barrier(&entry.state)
                && entry
                    .binding
                    .target
                    .session()
                    .is_none_or(|id| id == session_id)
        })
    }

    pub fn has_unresolved_stream(
        &self,
        _host_epoch: &str,
        session_id: &str,
        stream_id: &str,
    ) -> bool {
        self.reservations.values().any(|entry| {
            is_barrier(&entry.state)
                && match &entry.binding.target {
                    ActionTarget::Host { .. } => true,
                    ActionTarget::Session { session_id: id, .. } => id == session_id,
                    ActionTarget::Stream {
                        session_id: id,
                        stream_id: stream,
                        ..
                    } => id == session_id && stream == stream_id,
                }
        })
    }

    /// After a crash, outstanding permits are invalid and running work is
    /// unknown. Persist this transition before admitting new actions.
    pub fn recover_after_restart(&mut self) -> Result<(), AuthorityError> {
        self.validate()?;
        for item in self.reservations.values_mut() {
            item.state = match item.state {
                ReservationState::Issued { .. } => ReservationState::Cancelled,
                ReservationState::Running => ReservationState::RecoveryRequired {
                    receipt: serde_json::json!({"reason":"coordinator_restarted"}),
                },
                _ => continue,
            };
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn approval_snapshot_requires_selected_owner_methods_and_service() {
        let method = json!({"service":"display/1","method":"execute","interface_major":1,"interface_minor":0,"schema_revision":1});
        let descriptor = json!({"id":"display/remove","label":"Remove","description":"Remove","execution":"session_mutation","arguments":{},"allowed_targets":["stream"],"required_capabilities":[],"plan":null,"execute":method,"status":null});
        let approval = json!({"provider":"display","callers":["action-service"],"method":{"service":"display/1","method":"execute","interface_major":1,"interface_minor":0,"request":{"required":{}},"response":{"required":{}},"privilege":"user"}});
        let snapshot: ActionApprovalSnapshot = serde_json::from_value(
            json!({"descriptor":descriptor,"execute_approval":approval,"status_approval":null}),
        )
        .unwrap();
        assert!(valid_approval_snapshot(
            &snapshot,
            "display",
            &snapshot.descriptor.execute,
            &binding(),
            "action-service"
        ));
        assert!(!valid_approval_snapshot(
            &snapshot,
            "foreign",
            &snapshot.descriptor.execute,
            &binding(),
            "action-service"
        ));
        assert!(!valid_approval_snapshot(
            &snapshot,
            "display",
            &snapshot.descriptor.execute,
            &binding(),
            "other-service"
        ));
        assert!(!valid_approval_snapshot(
            &snapshot,
            "display",
            &snapshot.descriptor.execute,
            &ActionBinding {
                action_id: "display/other".into(),
                ..binding()
            },
            "action-service"
        ));
        let mut invoked = snapshot.clone();
        invoked.invoke = Some(serde_json::from_value(json!({"epoch":"epoch","sequence":1,"operation_id":"op","action_id":"display/remove","target":binding().target,"arguments":{},"expected_settings_revision":null,"input_reservation_id":null})).unwrap());
        assert!(valid_approval_snapshot(
            &invoked,
            "display",
            &snapshot.descriptor.execute,
            &binding(),
            "action-service"
        ));
        invoked.invoke.as_mut().unwrap().operation_id = "other".into();
        assert!(!valid_approval_snapshot(
            &invoked,
            "display",
            &snapshot.descriptor.execute,
            &binding(),
            "action-service"
        ));
        let mut changed = snapshot.clone();
        changed.descriptor.execute.interface_minor += 1;
        assert!(!valid_approval_snapshot(
            &changed,
            "display",
            &snapshot.descriptor.execute,
            &binding(),
            "action-service"
        ));
    }

    fn binding() -> ActionBinding {
        ActionBinding {
            operation_id: "op".into(),
            target: ActionTarget::Stream {
                host_epoch: "epoch".into(),
                session_id: "session".into(),
                stream_id: "stream".into(),
                expected_generation: 4,
            },
            action_id: "display/remove".into(),
            plan_digest: "a".repeat(64),
            source: Principal {
                caller: "source".into(),
                initiator: "user".into(),
            },
        }
    }
    fn canonical(binding: &ActionBinding) -> CanonicalAction<'_> {
        CanonicalAction {
            action_service: "action-service",
            target: &binding.target,
            owner: &binding.source,
            action_id: &binding.action_id,
            plan_digest: &binding.plan_digest,
            active: true,
        }
    }

    #[test]
    fn certified_terminal_retirement_fences_replay_and_preserves_capacity_boundary() {
        let mut authority = ActionAuthority::default();
        let binding = binding();
        let token = authority
            .authorize_action("action-service", &binding, &canonical(&binding), 10, 20)
            .unwrap();
        assert!(authority.has_any_reservation());
        assert!(authority.has_any_session_reference("session"));
        assert_eq!(
            authority.retire_certified_success("action-service", &token, &binding),
            Err(AuthorityError::InvalidState)
        );
        authority
            .reservations
            .get_mut(&key(&binding))
            .unwrap()
            .state = ReservationState::Succeeded {
            receipt: json!({"status":"ok"}),
        };
        let mut changed = binding.clone();
        changed.target = ActionTarget::Host {
            host_epoch: "epoch".into(),
        };
        assert_eq!(
            authority.retire_certified_success("action-service", &token, &changed),
            Err(AuthorityError::InvalidState)
        );
        assert_eq!(
            authority.retire_certified_success("wrong-service", &token, &binding),
            Err(AuthorityError::InvalidState)
        );
        authority
            .retire_certified_success("action-service", &token, &binding)
            .unwrap();
        assert!(!authority.has_any_session_reference("session"));
        assert!(!authority.has_any_reservation());
        let mut recovered: ActionAuthority =
            serde_json::from_slice(&serde_json::to_vec(&authority).unwrap()).unwrap();
        assert!(recovered.validate().is_ok());
        assert!(recovered.has_operation("action-service", "op", "display/remove"));
        assert_eq!(
            recovered.authorize_action("action-service", &binding, &canonical(&binding), 10, 20),
            Err(AuthorityError::Conflict)
        );
        assert!(recovered.retired_token(&token));
        assert_eq!(
            recovered.retire_certified_success("action-service", &token, &binding),
            Err(AuthorityError::NotAuthorized)
        );
        for index in 1..MAX_RETIRED_OPERATIONS {
            let mut other = binding.clone();
            other.operation_id = format!("operation-{index}");
            recovered.retired.insert(
                operation_fence(&other),
                RetiredAction {
                    service_operation_digest: service_operation_fence(
                        "action-service",
                        &other.operation_id,
                        &other.action_id,
                    ),
                    permit_digest: permit_fence(&format!("token-{index}")),
                    old_permit_digest: None,
                },
            );
        }
        let mut next = binding.clone();
        next.operation_id = "next".into();
        let next_token = recovered
            .authorize_action("action-service", &next, &canonical(&next), 10, 20)
            .unwrap();
        recovered.reservations.get_mut(&key(&next)).unwrap().state = ReservationState::Succeeded {
            receipt: json!({"status":"ok"}),
        };
        assert_eq!(
            recovered.retire_certified_success("action-service", &next_token, &next),
            Err(AuthorityError::Capacity)
        );
        assert!(recovered.has_any_session_reference("session"));
    }

    #[test]
    fn canonical_owner_plan_and_one_use_are_required() {
        let mut authority = ActionAuthority::default();
        let binding = binding();
        let mut untrusted = canonical(&binding);
        untrusted.action_service = "other";
        assert_eq!(
            authority.authorize_action("action-service", &binding, &untrusted, 10, 20),
            Err(AuthorityError::NotAuthorized)
        );
        let token = authority
            .authorize_action("action-service", &binding, &canonical(&binding), 10, 20)
            .unwrap();
        assert!(authority.has_operation("action-service", "op", "display/remove"));
        assert!(!authority.has_operation("other", "op", "display/remove"));
        assert_eq!(
            authority.authorize_action("action-service", &binding, &canonical(&binding), 11, 20),
            Ok(token.clone())
        );
        let mut changed = binding.clone();
        changed.plan_digest = "b".repeat(64);
        assert_eq!(
            authority.consume_action_permit(
                "action-service",
                &changed,
                &canonical(&binding),
                &token,
                11
            ),
            Err(AuthorityError::NotAuthorized)
        );
        authority
            .consume_action_permit("action-service", &binding, &canonical(&binding), &token, 11)
            .unwrap();
        assert_eq!(
            authority.consume_action_permit(
                "action-service",
                &binding,
                &canonical(&binding),
                &token,
                11
            ),
            Err(AuthorityError::NotAuthorized)
        );
        assert!(authority.fence_target(&binding.target).unwrap());
        authority
            .complete_action(
                "action-service",
                &binding,
                ActionOutcome::Succeeded,
                json!({"id":"receipt"}),
                false,
            )
            .unwrap();
        assert!(!authority.fence_target(&binding.target).unwrap());
    }

    #[test]
    fn restart_and_unknown_outcomes_keep_barrier_until_reconciled() {
        let mut authority = ActionAuthority::default();
        let binding = binding();
        let token = authority
            .authorize_action("action-service", &binding, &canonical(&binding), 1, 10)
            .unwrap();
        authority
            .consume_action_permit("action-service", &binding, &canonical(&binding), &token, 2)
            .unwrap();
        let mut restored: ActionAuthority =
            serde_json::from_slice(&serde_json::to_vec(&authority).unwrap()).unwrap();
        restored.recover_after_restart().unwrap();
        let mut new_target = binding.target.clone();
        if let ActionTarget::Stream { host_epoch, .. } = &mut new_target {
            *host_epoch = "new-epoch".into();
        }
        assert!(restored.fence_target(&new_target).unwrap());
        assert_eq!(
            restored.complete_action(
                "action-service",
                &binding,
                ActionOutcome::Failed,
                json!({}),
                false
            ),
            Err(AuthorityError::InvalidState)
        );
        restored
            .complete_action(
                "action-service",
                &binding,
                ActionOutcome::Failed,
                json!({}),
                true,
            )
            .unwrap();
        assert!(!restored.fence_target(&new_target).unwrap());
    }

    #[test]
    fn transfer_excludes_only_own_running_reservation() {
        let mut authority = ActionAuthority::default();
        let binding = binding();
        let token = authority
            .authorize_action("action-service", &binding, &canonical(&binding), 1, 10)
            .unwrap();
        authority
            .consume_action_permit("action-service", &binding, &canonical(&binding), &token, 2)
            .unwrap();
        assert!(
            authority
                .fence_target_except_transfer(
                    &binding.target,
                    "action-service",
                    &token,
                    &binding,
                    "transition"
                )
                .is_err()
        );
        authority
            .transfer_to_transition("action-service", &token, &binding, "transition")
            .unwrap();
        assert!(authority.fence_target(&binding.target).unwrap());
        assert_eq!(
            authority.fence_target_except_transfer(
                &binding.target,
                "action-service",
                &token,
                &binding,
                "transition"
            ),
            Ok(false)
        );
        assert!(
            authority
                .fence_target_except_transfer(
                    &binding.target,
                    "action-service",
                    &token,
                    &binding,
                    "other"
                )
                .is_err()
        );
        assert!(
            authority
                .transfer_to_transition("action-service", &token, &binding, "other")
                .is_err()
        );
    }

    #[test]
    fn transferred_rebind_rotates_token_and_replays_exact_result_after_restart() {
        let mut authority = ActionAuthority::default();
        let binding = binding();
        let token = authority
            .authorize_action("action-service", &binding, &canonical(&binding), 1, 10)
            .unwrap();
        let method = amaterasu_extension_api::MethodRef {
            service: "display/1".into(),
            method: "execute".into(),
            interface_major: 1,
            interface_minor: 0,
            schema_revision: 1,
        };
        let mut plan: amaterasu_extension_api::ResourcePlan = serde_json::from_value(json!({"plan_id":"p","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]})).unwrap();
        authority
            .bind_execution(
                "action-service",
                &token,
                &binding,
                "display",
                &method,
                &plan,
            )
            .unwrap();
        authority
            .consume_action_permit("action-service", &binding, &canonical(&binding), &token, 2)
            .unwrap();
        authority
            .transfer_to_transition("action-service", &token, &binding, "transition")
            .unwrap();
        plan.revision = 2;
        let digest = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&serde_json::to_value(&plan).unwrap()).unwrap())
        );
        assert!(
            authority
                .rebind_transferred("other", &token, &binding, "transition", &plan, &digest)
                .is_err()
        );
        let (new_token, new_binding) = authority
            .rebind_transferred(
                "action-service",
                &token,
                &binding,
                "transition",
                &plan,
                &digest,
            )
            .unwrap();
        assert_ne!(token, new_token);
        assert_eq!(new_binding.plan_digest, digest);
        assert!(
            authority
                .binding_for_permit("action-service", &token)
                .is_err()
        );
        assert_eq!(
            authority.binding_for_rebind("action-service", &token),
            Ok(binding.clone())
        );
        let mut recovered: ActionAuthority =
            serde_json::from_slice(&serde_json::to_vec(&authority).unwrap()).unwrap();
        assert_eq!(
            recovered.rebind_transferred(
                "action-service",
                &token,
                &binding,
                "transition",
                &plan,
                &digest
            ),
            Ok((new_token.clone(), new_binding.clone()))
        );
        let mut changed = plan.clone();
        changed.revision = 3;
        let changed_digest = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&serde_json::to_value(&changed).unwrap()).unwrap())
        );
        assert_eq!(
            recovered.rebind_transferred(
                "action-service",
                &token,
                &binding,
                "transition",
                &changed,
                &changed_digest
            ),
            Err(AuthorityError::Conflict)
        );
        assert!(
            recovered
                .claim_effect("display", &new_token, &new_binding, &method)
                .is_err()
        );
        recovered
            .mark_transition_ready("action-service", &new_token, &new_binding, "transition")
            .unwrap();
        recovered
            .claim_effect("display", &new_token, &new_binding, &method)
            .unwrap();
    }

    #[test]
    fn provider_effect_claim_requires_retirement_and_is_one_use() {
        let mut authority = ActionAuthority::default();
        let binding = binding();
        let token = authority
            .authorize_action("action-service", &binding, &canonical(&binding), 1, 10)
            .unwrap();
        let method = amaterasu_extension_api::MethodRef {
            service: "display/1".into(),
            method: "execute".into(),
            interface_major: 1,
            interface_minor: 0,
            schema_revision: 1,
        };
        authority
            .bind_execution("action-service", &token, &binding, "display", &method, &serde_json::from_value(serde_json::json!({"plan_id":"p","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]})).unwrap())
            .unwrap();
        authority
            .consume_action_permit("action-service", &binding, &canonical(&binding), &token, 2)
            .unwrap();
        assert!(
            authority
                .inspect_effect("display", &token, &binding, &method)
                .is_err()
        );
        assert!(
            authority
                .claim_effect("display", &token, &binding, &method)
                .is_err()
        );
        authority
            .transfer_to_transition("action-service", &token, &binding, "transition")
            .unwrap();
        authority
            .mark_transition_ready("action-service", &token, &binding, "transition")
            .unwrap();
        assert!(
            authority
                .inspect_effect("display", &token, &binding, &method)
                .is_ok()
        );
        assert!(
            authority
                .inspect_effect("display", &token, &binding, &method)
                .is_ok()
        );
        assert!(
            authority
                .inspect_effect("other", &token, &binding, &method)
                .is_err()
        );
        let mut wrong_method = method.clone();
        wrong_method.method = "other".into();
        assert!(
            authority
                .inspect_effect("display", &token, &binding, &wrong_method)
                .is_err()
        );
        assert!(
            authority
                .claim_effect("other", &token, &binding, &method)
                .is_err()
        );
        assert_eq!(
            authority.binding_for_provider("display", &token),
            Ok(binding.clone())
        );
        assert!(!authority.claimed_effect_for("action-service", &token, &binding, "display"));
        assert!(
            authority
                .claim_effect("display", &token, &binding, &method)
                .is_ok()
        );
        assert!(authority.claimed_effect_for("action-service", &token, &binding, "display"));
        assert!(!authority.claimed_effect_for("action-service", &token, &binding, "foreign"));
        assert!(!authority.claimed_effect_for("action-service", "wrong", &binding, "display"));
        let mut changed_binding = binding.clone();
        changed_binding.target = ActionTarget::Host {
            host_epoch: "epoch".into(),
        };
        assert!(!authority.claimed_effect_for(
            "action-service",
            &token,
            &changed_binding,
            "display"
        ));
        assert!(
            authority
                .inspect_effect("display", &token, &binding, &method)
                .is_err()
        );
        assert!(
            authority
                .claim_effect("display", &token, &binding, &method)
                .is_err()
        );
        assert!(
            authority
                .check_effect("display", &token, &binding, &method, "gap-1")
                .is_ok()
        );
        assert!(
            authority
                .check_effect("display", &token, &binding, &method, "gap-1")
                .is_err()
        );
        assert!(
            authority
                .check_effect("other", &token, &binding, &method, "gap-2")
                .is_err()
        );
        let receipt = json!({"removed":true});
        assert!(!authority.terminal_succeeded_for("action-service", &token, &binding, &receipt));
        authority
            .complete_action(
                "action-service",
                &binding,
                ActionOutcome::Succeeded,
                receipt.clone(),
                false,
            )
            .unwrap();
        let mut recovered: ActionAuthority =
            serde_json::from_value(serde_json::to_value(&authority).unwrap()).unwrap();
        assert!(recovered.terminal_succeeded_for("action-service", &token, &binding, &receipt));
        assert!(
            recovered
                .complete_action(
                    "action-service",
                    &binding,
                    ActionOutcome::Succeeded,
                    receipt.clone(),
                    false
                )
                .is_ok()
        );
        assert!(!recovered.terminal_succeeded_for(
            "action-service",
            &token,
            &binding,
            &json!({"removed":false})
        ));
        assert!(
            recovered
                .complete_action(
                    "action-service",
                    &binding,
                    ActionOutcome::Succeeded,
                    json!({"removed":false}),
                    false
                )
                .is_err()
        );
    }

    #[test]
    fn quiesce_revokes_issued_permits_and_wrong_source_is_rejected() {
        let mut authority = ActionAuthority::default();
        let binding = binding();
        let wrong_owner = Principal {
            caller: "other".into(),
            initiator: "user".into(),
        };
        let mut context = canonical(&binding);
        context.owner = &wrong_owner;
        assert_eq!(
            authority.authorize_action("action-service", &binding, &context, 1, 10),
            Err(AuthorityError::NotAuthorized)
        );
        let token = authority
            .authorize_action("action-service", &binding, &canonical(&binding), 1, 10)
            .unwrap();
        assert!(!authority.fence_target(&binding.target).unwrap());
        assert_eq!(
            authority.consume_action_permit(
                "action-service",
                &binding,
                &canonical(&binding),
                &token,
                2
            ),
            Err(AuthorityError::NotAuthorized)
        );
    }
}
