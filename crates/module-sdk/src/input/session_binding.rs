//! Lifecycle-issued input bindings. Tokens identify selected resources; only
//! broker-attested lifecycle responses grant a lease or revoke its admission.
//! `authorize_batch` retains legacy unbound behavior; providers advertising
//! lifecycle-bound feedback must require the binding on that route.
use crate::{Command, ModuleResult, Response};
use amaterasu_extension_api::LifecycleTarget;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Instant;

const MAX_BINDINGS: usize = 128;
const LIFECYCLE_SERVICE: &str = "amaterasu.session-lifecycle/1";
const LIFECYCLE_PRINCIPAL: &str = "amaterasu.session-lifecycle";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevokeRequest {
    pub action: String,
    pub operation_id: String,
    pub token: String,
    pub target: LifecycleTarget,
    #[serde(default)]
    pub transfer: Option<Transfer>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transfer {
    pub operation_id: String,
    pub reservation_id: String,
}

#[derive(Clone)]
pub struct Authority {
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
}
#[derive(Default)]
pub struct Bindings {
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
}

pub struct AcquireJob {
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
pub struct BatchActivationJob(AcquireJob);
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
