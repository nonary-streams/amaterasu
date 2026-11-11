//! Fixed, versioned owner-retirement contracts for retained session sources.
//!
//! Trusted composition selects the provider of each service. Recovery metadata
//! supplies only an operation and admission epoch; it cannot choose a service,
//! method, payload, caller identity or native resource. A read-only observer's
//! broker grant never authorizes the owner-only mutation operations.
use crate::requests::RequestOptions;
use crate::{Command, ModuleResult};
use serde::Deserialize;
use serde_json::{Value, json};

pub const MAX_RESPONSE_BYTES: usize = 4_096;
pub const MAX_OPERATION_BYTES: usize = 128;
pub const MAX_TIMEOUT_MS: u64 = 30_000;

/// The selected public service, independent of its provider's identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Contract {
    Transport,
    Application,
}

impl Contract {
    pub const fn service(self) -> &'static str {
        match self {
            Self::Transport => "amaterasu.rtsp/1",
            Self::Application => "amaterasu.launch/1",
        }
    }
}

/// An exact operation admitted under a provider's authoritative runtime epoch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationRef {
    pub operation_id: String,
    pub epoch: String,
}

impl OperationRef {
    pub fn validate(&self, contract: Contract) -> ModuleResult<()> {
        if !valid_epoch(&self.epoch)
            || !valid_operation(&self.operation_id)
            || (contract == Contract::Application
                && application_operation_epoch(&self.operation_id) != Some(self.epoch.as_str()))
        {
            return Err("invalid retirement operation reference".into());
        }
        Ok(())
    }
}

/// Provider-owned lifecycle facts, never inferred from a missing list entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Active,
    Retired,
    Unknown,
    EpochFenced,
    OperationFenced,
    RecoveryPending,
    RecoveryBlocked,
}

/// A validated observation of the exact requested operation and epoch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Status {
    pub runtime_epoch: String,
    pub startup_reconciled: bool,
    pub state: State,
    /// True only for the contract's explicit native retirement or admission
    /// fence proof. A restart alone cannot establish this fact.
    pub retired: bool,
    /// Optional application generation evidence must also match the consumer's
    /// saved generation before it releases generation-scoped resources.
    pub application_generation: Option<u64>,
}

/// Read the admission epoch before effects. This operation does not prove any
/// earlier operation retired. Launch exposes this read only to its owner.
pub fn epoch_command(options: RequestOptions<'_>, contract: Contract) -> ModuleResult<Command> {
    let action = match contract {
        Contract::Transport => "retirement_status",
        Contract::Application => "operation_epoch",
    };
    command(options, contract, json!({"action":action}))
}

/// Read exact retirement evidence. Providers authenticate the caller and map
/// configured observers to the original owner without granting mutation rights.
pub fn status_command(
    options: RequestOptions<'_>,
    contract: Contract,
    reference: &OperationRef,
) -> ModuleResult<Command> {
    reference.validate(contract)?;
    let action = match contract {
        Contract::Transport => "retirement_status",
        Contract::Application => "application_retirement_status",
    };
    command(options, contract, operation_payload(action, reference))
}

/// Ask the original owner to fence admission and retire its exact operation.
/// Admission or a successful acknowledgment is not retirement evidence: follow
/// with `status_command` and `parse_status` before releasing retained resources.
pub fn retire_command(
    options: RequestOptions<'_>,
    contract: Contract,
    reference: &OperationRef,
) -> ModuleResult<Command> {
    reference.validate(contract)?;
    command(
        options,
        contract,
        operation_payload("retire_operation", reference),
    )
}

/// Retry the transport provider's supported startup cleanup as the owner.
/// Observers can inspect recovery state but cannot invoke this mutation.
pub fn reconcile_transport_command(options: RequestOptions<'_>) -> ModuleResult<Command> {
    command(
        options,
        Contract::Transport,
        json!({"action":"reconcile_startup"}),
    )
}

fn operation_payload(action: &str, reference: &OperationRef) -> Value {
    json!({"action":action,"operation_id":reference.operation_id,
        "expected_runtime_epoch":reference.epoch})
}

fn command(
    options: RequestOptions<'_>,
    contract: Contract,
    payload: Value,
) -> ModuleResult<Command> {
    if options.service != contract.service()
        || !(1..=MAX_TIMEOUT_MS).contains(&options.timeout_ms)
        || !valid_operation(options.operation_id)
        || options.parent == Some(0)
    {
        return Err("invalid fixed retirement request options".into());
    }
    Ok(Command::Request {
        service: contract.service().into(),
        payload,
        timeout_ms: options.timeout_ms,
        operation_id: options.operation_id.into(),
        parent: options.parent,
    })
}

/// Validate the fixed epoch response. Transport reports `ready` once a new
/// operation may bind it: it waits a bounded time after start for the
/// previous runtime's sessions and is not held back by them after that.
/// `startup_reconciled` is the separate statement that those sessions were
/// retired, and other current-epoch operations may still have admissions or
/// cleanup pending; reading admission identity neither waits for any of this
/// nor proves resources have retired.
/// Launch's epoch is admission identity only, with no cross-restart proof.
pub fn parse_epoch(contract: Contract, value: &Value) -> ModuleResult<String> {
    validate_response_size(value)?;
    if value.get("ok") != Some(&Value::Bool(true)) {
        return Err("retirement epoch provider failed".into());
    }
    let field = match contract {
        Contract::Transport => {
            if value
                .get("startup_reconciled")
                .and_then(Value::as_bool)
                .is_none()
                || value.get("state").and_then(Value::as_str) != Some("ready")
                || value.get("retired") != Some(&Value::Bool(false))
                || value.get("operation_id") != Some(&Value::Null)
                || value.get("expected_runtime_epoch") != Some(&Value::Null)
                || value
                    .get("cleanup_pending")
                    .and_then(Value::as_bool)
                    .is_none()
                || value
                    .get("pending_admissions")
                    .and_then(Value::as_u64)
                    .is_none()
                || value
                    .get("active_operations")
                    .and_then(Value::as_u64)
                    .is_none()
            {
                return Err("transport admission epoch is not ready".into());
            }
            "runtime_epoch"
        }
        Contract::Application => "operation_epoch",
    };
    let epoch = value
        .get(field)
        .and_then(Value::as_str)
        .filter(|epoch| valid_epoch(epoch))
        .ok_or("invalid retirement admission epoch")?;
    Ok(epoch.into())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusResponse {
    ok: bool,
    operation_id: String,
    expected_runtime_epoch: String,
    runtime_epoch: String,
    startup_reconciled: bool,
    state: State,
    retired: bool,
    #[serde(default)]
    active_operations: Option<u64>,
    #[serde(default)]
    pending_admissions: Option<u64>,
    #[serde(default)]
    cleanup_pending: Option<bool>,
    #[serde(default)]
    application_generation: Option<u64>,
}

/// Validate proof against the complete requested tuple. Transport may prove an
/// old epoch retired only after authoritative startup cleanup; Launch v1 has no
/// durable native recovery journal, so its epoch fence never proves retirement.
pub fn parse_status(
    contract: Contract,
    reference: &OperationRef,
    value: &Value,
) -> ModuleResult<Status> {
    reference.validate(contract)?;
    validate_response_size(value)?;
    let response: StatusResponse =
        serde_json::from_value(value.clone()).map_err(|_| "invalid retirement status response")?;
    if !response.ok
        || response.operation_id != reference.operation_id
        || response.expected_runtime_epoch != reference.epoch
        || !valid_epoch(&response.runtime_epoch)
        || response.application_generation == Some(0)
    {
        return Err("retirement status does not match its operation reference".into());
    }
    let same_epoch = response.runtime_epoch == reference.epoch;
    let valid = match contract {
        Contract::Transport => validate_transport_status(&response, same_epoch),
        Contract::Application => validate_application_status(&response, same_epoch),
    };
    if !valid {
        return Err("unsupported or inconsistent retirement evidence".into());
    }
    Ok(Status {
        runtime_epoch: response.runtime_epoch,
        startup_reconciled: response.startup_reconciled,
        state: response.state,
        retired: response.retired,
        application_generation: response.application_generation,
    })
}

fn validate_transport_status(response: &StatusResponse, same_epoch: bool) -> bool {
    if response.active_operations.is_none()
        || response.pending_admissions.is_none()
        || response.cleanup_pending.is_none()
        || response.application_generation.is_some()
    {
        return false;
    }
    match response.state {
        State::Retired | State::OperationFenced => same_epoch && response.retired,
        State::EpochFenced => {
            !same_epoch
                && response.retired
                && response.startup_reconciled
                && response.cleanup_pending == Some(false)
                && response.pending_admissions == Some(0)
        }
        State::Active | State::Unknown => same_epoch && !response.retired,
        State::RecoveryPending | State::RecoveryBlocked => {
            !response.retired && !response.startup_reconciled
        }
    }
}

fn validate_application_status(response: &StatusResponse, same_epoch: bool) -> bool {
    if response.startup_reconciled
        || response.active_operations.is_some()
        || response.pending_admissions.is_some()
        || response.cleanup_pending.is_some()
    {
        return false;
    }
    match response.state {
        State::Retired => same_epoch && response.retired,
        State::Active | State::Unknown => same_epoch && !response.retired,
        State::EpochFenced => !same_epoch && !response.retired,
        _ => false,
    }
}

fn validate_response_size(value: &Value) -> ModuleResult<()> {
    if !value.is_object()
        || !serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= MAX_RESPONSE_BYTES)
    {
        return Err("retirement response is invalid or too large".into());
    }
    Ok(())
}

fn valid_epoch(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_operation(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_OPERATION_BYTES && !value.chars().any(char::is_control)
}

fn application_operation_epoch(value: &str) -> Option<&str> {
    let (epoch, nonce) = value.strip_prefix("ale1:")?.split_once(':')?;
    (valid_epoch(epoch)
        && !nonce.is_empty()
        && nonce.len() <= 80
        && nonce
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte)))
    .then_some(epoch)
}
