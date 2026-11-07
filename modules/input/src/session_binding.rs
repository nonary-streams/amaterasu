//! Lifecycle-issued input bindings. Tokens identify selected resources; only
//! broker-attested lifecycle responses grant a lease or revoke its admission.
use amaterasu_extension_api::LifecycleTarget;
use amaterasu_module_sdk::input::{INPUT_BINDING_RETIREMENT_VERSION, InputBindingRetirement};
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Instant;

const MAX_BINDINGS: usize = 128;
const LIFECYCLE_SERVICE: &str = "amaterasu.session-lifecycle/1";
const LIFECYCLE_PRINCIPAL: &str = "amaterasu.session-lifecycle";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RevokeRequest {
    pub action: String,
    pub operation_id: String,
    pub token: String,
    pub target: LifecycleTarget,
    #[serde(default)]
    pub transfer: Option<Transfer>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Transfer {
    pub operation_id: String,
    pub reservation_id: String,
}

#[derive(Clone)]
pub(crate) struct Authority {
    pub target: LifecycleTarget,
    pub geometry_revision: u64,
    pub batch_epoch: String,
    pub active: bool,
}
struct Binding {
    owner: String,
    lease: Option<String>,
    target: Option<LifecycleTarget>,
    geometry_revision: Option<u64>,
    batch_epoch: Option<String>,
    active: bool,
    next_sequence: u64,
    revoked: bool,
    revoke_id: Option<String>,
    retired: bool,
}
#[derive(Default)]
pub(crate) struct Bindings {
    entries: BTreeMap<String, Binding>,
}

fn valid_text(text: &str) -> bool {
    !text.is_empty() && text.len() <= 256 && !text.chars().any(char::is_control)
}
fn valid_target(target: &LifecycleTarget) -> bool {
    match target {
        LifecycleTarget::Stream {
            host_epoch,
            session_id,
            stream_id,
            generation,
        } => {
            valid_text(host_epoch)
                && valid_text(session_id)
                && valid_text(stream_id)
                && *generation > 0
        }
        _ => false,
    }
}
impl Bindings {
    pub fn begin_acquire(&mut self, token: &str, owner: &str) -> Result<(), &'static str> {
        if !valid_text(token) || !valid_text(owner) {
            return Err("invalid_binding");
        }
        if self.entries.contains_key(token) {
            return Err("binding_already_used");
        }
        if self.entries.len() >= MAX_BINDINGS {
            return Err("binding_capacity");
        }
        self.entries.insert(
            token.into(),
            Binding {
                owner: owner.into(),
                lease: None,
                target: None,
                geometry_revision: None,
                batch_epoch: None,
                active: false,
                next_sequence: 1,
                revoked: false,
                revoke_id: None,
                retired: false,
            },
        );
        Ok(())
    }
    pub fn abandon_acquire(&mut self, token: &str) {
        if self
            .entries
            .get(token)
            .is_some_and(|entry| entry.lease.is_none() && !entry.revoked)
        {
            self.entries.remove(token);
        }
    }
    pub fn is_revoked(&self, token: &str) -> bool {
        self.entries.get(token).is_none_or(|entry| entry.revoked)
    }
    pub fn bind(
        &mut self,
        token: &str,
        owner: &str,
        lease: &str,
        authority: &Authority,
    ) -> Result<(), &'static str> {
        if !valid_text(lease)
            || !valid_text(&authority.batch_epoch)
            || authority.geometry_revision == 0
            || !valid_target(&authority.target)
        {
            return Err("invalid_binding");
        }
        let entry = self.entries.get_mut(token).ok_or("binding_unavailable")?;
        if entry.revoked || entry.owner != owner || entry.lease.is_some() {
            return Err("binding_unavailable");
        }
        entry.lease = Some(lease.into());
        entry.target = Some(authority.target.clone());
        entry.geometry_revision = Some(authority.geometry_revision);
        entry.batch_epoch = Some(authority.batch_epoch.clone());
        entry.active = authority.active;
        Ok(())
    }
    /// Reject bare legacy batches on bound leases and reject token/lease mismatch.
    pub fn authorize_batch(
        &self,
        owner: &str,
        lease: &str,
        token: Option<&str>,
    ) -> Result<(), &'static str> {
        if let Some((bound_token, binding)) = self
            .entries
            .iter()
            .find(|(_, binding)| binding.lease.as_deref() == Some(lease))
        {
            if binding.revoked || binding.owner != owner || token != Some(bound_token.as_str()) {
                return Err("binding_retired");
            }
            Ok(())
        } else if token.is_some() {
            Err("binding_unavailable")
        } else {
            Ok(())
        }
    }
    /// A pending binding may be acquired but cannot deliver until lifecycle validates it active.
    pub fn batch_active(
        &self,
        owner: &str,
        lease: &str,
        token: &str,
    ) -> Result<bool, &'static str> {
        self.authorize_batch(owner, lease, Some(token))?;
        Ok(self.entries.get(token).ok_or("binding_unavailable")?.active)
    }
    pub fn activate(
        &mut self,
        owner: &str,
        lease: &str,
        token: &str,
        authority: &Authority,
    ) -> Result<(), &'static str> {
        self.authorize_batch(owner, lease, Some(token))?;
        let binding = self.entries.get_mut(token).ok_or("binding_unavailable")?;
        if !authority.active
            || binding.target.as_ref() != Some(&authority.target)
            || binding.geometry_revision != Some(authority.geometry_revision)
            || binding.batch_epoch.as_deref() != Some(authority.batch_epoch.as_str())
        {
            return Err("binding_stale");
        }
        binding.active = true;
        Ok(())
    }
    /// Advance the source edge before native work. A lost response cannot be replayed.
    pub fn admit_sequence(
        &mut self,
        owner: &str,
        lease: &str,
        token: &str,
        operation_id: &str,
    ) -> Result<(), &'static str> {
        self.authorize_batch(owner, lease, Some(token))?;
        let binding = self.entries.get_mut(token).ok_or("binding_unavailable")?;
        if !binding.active {
            return Err("binding_inactive");
        }
        let (epoch, suffix) = operation_id
            .rsplit_once(':')
            .ok_or("invalid_operation_id")?;
        let sequence = suffix.parse::<u64>().map_err(|_| "invalid_operation_id")?;
        if binding.batch_epoch.as_deref() != Some(epoch)
            || sequence == 0
            || suffix != sequence.to_string()
        {
            return Err("invalid_operation_id");
        }
        if sequence < binding.next_sequence {
            return Err("receipt_expired");
        }
        if sequence != binding.next_sequence {
            return Err("sequence_conflict");
        }
        binding.next_sequence = sequence.checked_add(1).ok_or("sequence_exhausted")?;
        Ok(())
    }
    pub fn batch_target(
        &self,
        owner: &str,
        lease: &str,
        token: &str,
    ) -> Result<LifecycleTarget, &'static str> {
        self.authorize_batch(owner, lease, Some(token))?;
        self.entries
            .get(token)
            .and_then(|entry| entry.target.clone())
            .ok_or("binding_unavailable")
    }
    pub fn status(
        &self,
        caller: &str,
        token: &str,
        target: &LifecycleTarget,
    ) -> Result<(&str, &str, u64), &'static str> {
        if caller != LIFECYCLE_PRINCIPAL {
            return Err("permission_denied");
        }
        let entry = self.entries.get(token).ok_or("binding_unknown")?;
        if entry.revoked || entry.target.as_ref() != Some(target) {
            return Err("binding_unavailable");
        }
        Ok((
            &entry.owner,
            entry.lease.as_deref().ok_or("binding_unavailable")?,
            entry.geometry_revision.ok_or("binding_unavailable")?,
        ))
    }
    /// Close admission before queued work can run. Unknown tokens after restart
    /// remain unknown, so native cleanup is never assumed complete.
    pub fn revoke(
        &mut self,
        caller: &str,
        operation_id: &str,
        token: &str,
        target: LifecycleTarget,
    ) -> Result<(String, Option<String>), &'static str> {
        if caller != LIFECYCLE_PRINCIPAL || !valid_text(operation_id) || !valid_target(&target) {
            return Err("permission_denied");
        }
        if self.entries.iter().any(|(other, entry)| {
            other != token && entry.revoke_id.as_deref() == Some(operation_id)
        }) {
            return Err("operation_conflict");
        }
        let entry = self.entries.get_mut(token).ok_or("binding_unknown")?;
        if entry
            .target
            .as_ref()
            .is_some_and(|existing| existing != &target)
            || entry
                .revoke_id
                .as_deref()
                .is_some_and(|existing| existing != operation_id)
        {
            return Err("operation_conflict");
        }
        entry.target = Some(target);
        entry.revoked = true;
        entry.revoke_id = Some(operation_id.into());
        Ok((entry.owner.clone(), entry.lease.clone()))
    }

    pub fn record_retirement(
        &mut self,
        revoke: &RevokeRequest,
        input_boot_epoch: &str,
    ) -> Result<InputBindingRetirement, &'static str> {
        let entry = self
            .entries
            .get_mut(&revoke.token)
            .ok_or("binding_unknown")?;
        if !entry.revoked
            || entry.revoke_id.as_deref() != Some(&revoke.operation_id)
            || entry.target.as_ref() != Some(&revoke.target)
        {
            return Err("binding_retirement_mismatch");
        }
        let receipt = InputBindingRetirement {
            version: INPUT_BINDING_RETIREMENT_VERSION,
            token: revoke.token.clone(),
            target: revoke.target.clone(),
            revoke_operation_id: revoke.operation_id.clone(),
            input_boot_epoch: input_boot_epoch.to_owned(),
        };
        receipt.validate()?;
        entry.retired = true;
        Ok(receipt)
    }

    fn retirement_owner(
        &self,
        receipt: &InputBindingRetirement,
        input_boot_epoch: &str,
    ) -> Result<Option<(String, Option<String>)>, &'static str> {
        let Some(entry) = self.entries.get(&receipt.token) else {
            // Absence can acknowledge bookkeeping only. It never supplies a
            // native retirement receipt, including after provider restart.
            return Ok(None);
        };
        if receipt.input_boot_epoch != input_boot_epoch
            || !entry.retired
            || !entry.revoked
            || entry.revoke_id.as_deref() != Some(&receipt.revoke_operation_id)
            || entry.target.as_ref() != Some(&receipt.target)
        {
            return Err("binding_retirement_mismatch");
        }
        Ok(Some((entry.owner.clone(), entry.lease.clone())))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AcknowledgeRetirement {
    action: String,
    receipt: InputBindingRetirement,
}

pub(super) struct PendingRetirementAcknowledgement {
    request: super::QueuedRequest,
    reply: Value,
}

impl super::InputModule {
    pub(super) fn acknowledge_binding_retirement(
        &mut self,
        request: super::QueuedRequest,
    ) -> ModuleResult<()> {
        if self.pending_retirement_acknowledgement.is_some() {
            return self.complete(
                request.id,
                super::InputError::new("binding_retirement_busy", "an acknowledgement is settling")
                    .payload(),
                &request.caller,
                false,
            );
        }
        let response = self.release_retired_binding(&request);
        let reply = match response {
            Ok(receipt) => json!({"ok":true,"status":"acknowledged","receipt":receipt}),
            Err(code) => {
                super::InputError::new(code, "input retirement acknowledgement rejected").payload()
            }
        };
        self.pending_retirement_acknowledgement =
            Some(PendingRetirementAcknowledgement { request, reply });
        self.advance_retirement_acknowledgement()
    }

    fn release_retired_binding(
        &mut self,
        request: &super::QueuedRequest,
    ) -> Result<InputBindingRetirement, &'static str> {
        if request.caller != LIFECYCLE_PRINCIPAL {
            return Err("permission_denied");
        }
        let acknowledgement: AcknowledgeRetirement =
            serde_json::from_value(request.payload.clone()).map_err(|_| "invalid_request")?;
        if acknowledgement.action != "acknowledge_binding_retirement" {
            return Err("invalid_request");
        }
        let receipt = acknowledgement.receipt;
        receipt.validate()?;
        if self.binding_retirement_work_pending(&receipt.token) {
            return Err("binding_retirement_busy");
        }
        if let Some((owner, lease)) = self
            .bindings
            .retirement_owner(&receipt, &self.input_boot_epoch)?
        {
            self.controller
                .retire_binding_context(&receipt.token, &owner, lease.as_deref())
                .map_err(|_| "binding_retirement_busy")?;
            self.bindings.entries.remove(&receipt.token);
            self.shortcut_cache.invalidate(&receipt.token);
        }
        Ok(receipt)
    }

    fn binding_retirement_work_pending(&self, token: &str) -> bool {
        self.acquire_job
            .as_ref()
            .is_some_and(|job| job.token == token)
            || self
                .activation_job
                .as_ref()
                .is_some_and(|(request, _)| request.payload["input_binding"] == token)
            || self
                .shortcut_job
                .as_ref()
                .is_some_and(|(_, job)| job.binding_identity().1 == token)
            || self
                .pending_revoke
                .as_ref()
                .is_some_and(|(_, revoke)| revoke.token == token)
            || self.queued.iter().any(|request| {
                ["input_binding", "lifecycle_binding_token", "token"]
                    .into_iter()
                    .any(|field| request.payload[field] == token)
            })
    }

    pub(super) fn advance_retirement_acknowledgement(&mut self) -> ModuleResult<()> {
        let Some(pending) = self.pending_retirement_acknowledgement.take() else {
            return Ok(());
        };
        if let Err(error) = self.complete(
            pending.request.id,
            pending.reply.clone(),
            &pending.request.caller,
            false,
        ) {
            self.pending_retirement_acknowledgement = Some(pending);
            return Err(error);
        }
        Ok(())
    }
}

pub(crate) struct AcquireJob {
    pub parent: u64,
    pub owner: String,
    pub token: String,
    pub payload: Value,
    pub deadline: Instant,
    request: Option<u64>,
    operation_id: String,
    input_boot_epoch: String,
}
impl AcquireJob {
    pub fn new(
        parent: u64,
        owner: String,
        token: String,
        payload: Value,
        deadline: Instant,
        input_boot_epoch: String,
    ) -> Result<Self, String> {
        if !valid_text(&input_boot_epoch) {
            return Err("invalid input boot epoch".into());
        }
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|_| "random unavailable")?;
        let operation_id = format!(
            "input-binding-{}",
            bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        Ok(Self {
            parent,
            owner,
            token,
            payload,
            deadline,
            request: None,
            operation_id,
            input_boot_epoch,
        })
    }
    pub fn cancel(&mut self, send: &mut impl FnMut(Command) -> ModuleResult<Response>) {
        if let Some(request) = self.request.take() {
            let _ = send(Command::Cancel { request });
            let _ = send(Command::ReleaseResult { request });
        }
    }
    pub fn advance(
        &mut self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> Result<Option<Authority>, String> {
        if Instant::now() >= self.deadline {
            if let Some(request) = self.request.take() {
                let _ = send(Command::Cancel { request });
                let _ = send(Command::ReleaseResult { request });
            }
            return Err("binding validation expired".into());
        }
        if let Some(request) = self.request {
            return match send(Command::TakeResult { request })? {
                Response::Pending => Ok(None),
                Response::Result { payload } => {
                    if !matches!(send(Command::ReleaseResult { request })?, Response::Ok) {
                        return Err("binding result release failed".into());
                    }
                    self.request = None;
                    let target: LifecycleTarget = serde_json::from_value(
                        payload.get("target").cloned().unwrap_or(Value::Null),
                    )
                    .map_err(|_| "invalid binding target")?;
                    let geometry_revision = payload
                        .get("geometry_revision")
                        .and_then(Value::as_u64)
                        .filter(|n| *n > 0)
                        .ok_or("invalid binding geometry")?;
                    let batch_epoch = payload
                        .get("batch_epoch")
                        .and_then(Value::as_str)
                        .filter(|v| valid_text(v))
                        .ok_or("invalid binding epoch")?
                        .to_owned();
                    if payload.get("status").and_then(Value::as_str) != Some("succeeded")
                        || !valid_target(&target)
                    {
                        return Err("binding authorization denied".into());
                    }
                    let active = payload
                        .get("active")
                        .and_then(Value::as_bool)
                        .ok_or("missing binding active state")?;
                    Ok(Some(Authority {
                        target,
                        geometry_revision,
                        batch_epoch,
                        active,
                    }))
                }
                _ => {
                    let _ = send(Command::ReleaseResult { request });
                    Err("binding validation failed".into())
                }
            };
        }
        let timeout_ms = self
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u64;
        match send(Command::Request {
            service: LIFECYCLE_SERVICE.into(),
            payload: json!({"operation":"validate_input_binding","operation_id":self.operation_id,"token":self.token,"input_boot_epoch":self.input_boot_epoch}),
            timeout_ms: timeout_ms.max(1),
            operation_id: self.operation_id.clone(),
            parent: Some(self.parent),
        })? {
            Response::Accepted { id } => {
                self.request = Some(id);
                Ok(None)
            }
            _ => Err("binding validation unavailable".into()),
        }
    }
}

/// One lightweight lifecycle validation used when the acquired generation is not yet active.
pub(crate) struct BatchActivationJob(AcquireJob);
impl BatchActivationJob {
    pub fn new(
        parent: u64,
        token: String,
        deadline: Instant,
        input_boot_epoch: String,
    ) -> Result<Self, String> {
        Ok(Self(AcquireJob::new(
            parent,
            String::new(),
            token,
            Value::Null,
            deadline,
            input_boot_epoch,
        )?))
    }
    pub fn advance(
        &mut self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> Result<Option<Authority>, String> {
        self.0.advance(send)
    }
    pub fn cancel(&mut self, send: &mut impl FnMut(Command) -> ModuleResult<Response>) {
        self.0.cancel(send);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;
    fn target() -> LifecycleTarget {
        LifecycleTarget::Stream {
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            generation: 1,
        }
    }
    #[test]
    fn binding_enforces_exact_lease_and_revocation_idempotence() {
        let mut bindings = Bindings::default();
        bindings.begin_acquire("token", "owner").unwrap();
        bindings
            .bind(
                "token",
                "owner",
                "lease",
                &Authority {
                    target: target(),
                    geometry_revision: 1,
                    batch_epoch: "epoch".into(),
                    active: true,
                },
            )
            .unwrap();
        assert!(
            bindings
                .authorize_batch("owner", "lease", Some("token"))
                .is_ok()
        );
        assert_eq!(
            bindings.status(LIFECYCLE_PRINCIPAL, "token", &target()),
            Ok(("owner", "lease", 1))
        );
        assert!(
            bindings
                .status(LIFECYCLE_SERVICE, "token", &target())
                .is_err()
        );
        assert!(
            bindings
                .revoke(LIFECYCLE_SERVICE, "forged", "token", target())
                .is_err()
        );
        assert!(bindings.authorize_batch("owner", "lease", None).is_err());
        assert!(
            bindings
                .authorize_batch("owner", "lease", Some("wrong"))
                .is_err()
        );
        assert!(
            bindings
                .revoke("attacker", "revoke", "token", target())
                .is_err()
        );
        assert!(
            bindings
                .authorize_batch("owner", "lease", Some("token"))
                .is_ok()
        );
        assert_eq!(
            bindings
                .revoke(LIFECYCLE_PRINCIPAL, "revoke", "token", target())
                .unwrap(),
            ("owner".into(), Some("lease".into()))
        );
        assert!(
            bindings
                .authorize_batch("owner", "lease", Some("token"))
                .is_err()
        );
        assert!(
            bindings
                .revoke(LIFECYCLE_PRINCIPAL, "revoke", "token", target())
                .is_ok()
        );
        assert!(
            bindings
                .revoke(LIFECYCLE_PRINCIPAL, "other", "token", target())
                .is_err()
        );
        assert!(bindings.authorize_batch("owner", "legacy", None).is_ok());
    }
    #[test]
    fn active_validation_and_source_sequence_are_fenced_by_target_and_revoke() {
        let mut bindings = Bindings::default();
        bindings.begin_acquire("token", "owner").unwrap();
        bindings
            .bind(
                "token",
                "owner",
                "lease",
                &Authority {
                    target: target(),
                    geometry_revision: 4,
                    batch_epoch: "epoch".into(),
                    active: false,
                },
            )
            .unwrap();
        assert!(!bindings.batch_active("owner", "lease", "token").unwrap());
        assert_eq!(
            bindings.admit_sequence("owner", "lease", "token", "epoch:1"),
            Err("binding_inactive")
        );
        let mut authority = Authority {
            target: target(),
            geometry_revision: 5,
            batch_epoch: "epoch".into(),
            active: true,
        };
        assert!(
            bindings
                .activate("owner", "lease", "token", &authority)
                .is_err()
        );
        authority.geometry_revision = 4;
        bindings
            .activate("owner", "lease", "token", &authority)
            .unwrap();
        assert!(bindings.batch_active("owner", "lease", "token").unwrap());
        bindings
            .admit_sequence("owner", "lease", "token", "epoch:1")
            .unwrap();
        assert_eq!(
            bindings.admit_sequence("owner", "lease", "token", "epoch:1"),
            Err("receipt_expired")
        );
        assert_eq!(
            bindings.admit_sequence("owner", "lease", "token", "epoch:3"),
            Err("sequence_conflict")
        );
        bindings
            .revoke(LIFECYCLE_PRINCIPAL, "revoke", "token", target())
            .unwrap();
        assert!(
            bindings
                .activate("owner", "lease", "token", &authority)
                .is_err()
        );
        assert!(
            bindings
                .admit_sequence("owner", "lease", "token", "epoch:2")
                .is_err()
        );
    }

    #[test]
    fn revoke_before_authority_cannot_bind_and_unknown_token_never_claims_retirement() {
        let mut bindings = Bindings::default();
        assert!(
            bindings
                .revoke(LIFECYCLE_PRINCIPAL, "r", "unknown", target())
                .is_err()
        );
        bindings.begin_acquire("token", "owner").unwrap();
        bindings
            .revoke(LIFECYCLE_PRINCIPAL, "r", "token", target())
            .unwrap();
        assert!(
            bindings
                .bind(
                    "token",
                    "owner",
                    "lease",
                    &Authority {
                        target: target(),
                        geometry_revision: 1,
                        batch_epoch: "epoch".into(),
                        active: true
                    }
                )
                .is_err()
        );
    }
    #[test]
    fn acquire_uses_parent_bound_authority_and_rejects_forged_response() {
        let mut job = AcquireJob::new(
            7,
            "owner".into(),
            "token".into(),
            json!({}),
            Instant::now() + Duration::from_secs(1),
            "boot".into(),
        )
        .unwrap();
        let mut send = |command| -> ModuleResult<Response> {
            match command {
                Command::Request {
                    payload, parent, ..
                } => {
                    assert_eq!(parent, Some(7));
                    assert_eq!(payload["token"], "token");
                    assert_eq!(payload["input_boot_epoch"], "boot");
                    Ok(Response::Accepted { id: 1 })
                }
                Command::TakeResult { .. } => Ok(Response::Result {
                    payload: json!({"status":"succeeded","target":target(),"geometry_revision":1,"batch_epoch":"epoch","active":true,"allowed_actions":[]}),
                }),
                Command::ReleaseResult { .. } => Ok(Response::Ok),
                _ => panic!("unexpected command"),
            }
        };
        assert!(job.advance(&mut send).unwrap().is_none());
        assert_eq!(job.advance(&mut send).unwrap().unwrap().target, target());
        let mut denied = AcquireJob::new(
            7,
            "owner".into(),
            "token".into(),
            json!({}),
            Instant::now() + Duration::from_secs(1),
            "boot".into(),
        )
        .unwrap();
        let mut send = |command| -> ModuleResult<Response> {
            match command {
                Command::Request { .. } => Ok(Response::Accepted { id: 1 }),
                Command::TakeResult { .. } => Ok(Response::Result {
                    payload: json!({"status":"succeeded","target":{"kind":"host","host_epoch":"forged"},"geometry_revision":1,"batch_epoch":"epoch","active":true}),
                }),
                Command::ReleaseResult { .. } => Ok(Response::Ok),
                _ => panic!("unexpected command"),
            }
        };
        denied.advance(&mut send).unwrap();
        assert!(denied.advance(&mut send).is_err());
    }
}
