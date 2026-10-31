//! Lifecycle-issued input bindings. Tokens identify selected resources; only
//! broker-attested lifecycle responses grant a lease or revoke its admission.
use amaterasu_extension_api::LifecycleTarget;
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Instant;

const MAX_BINDINGS: usize = 128;
const LIFECYCLE: &str = "amaterasu.session-lifecycle/1";

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
}
struct Binding {
    owner: String,
    lease: Option<String>,
    target: Option<LifecycleTarget>,
    geometry_revision: Option<u64>,
    revoked: bool,
    revoke_id: Option<String>,
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
        target: LifecycleTarget,
        geometry_revision: u64,
    ) -> Result<(), &'static str> {
        if !valid_text(lease) || geometry_revision == 0 || !valid_target(&target) {
            return Err("invalid_binding");
        }
        let entry = self.entries.get_mut(token).ok_or("binding_unavailable")?;
        if entry.revoked || entry.owner != owner || entry.lease.is_some() {
            return Err("binding_unavailable");
        }
        entry.lease = Some(lease.into());
        entry.target = Some(target);
        entry.geometry_revision = Some(geometry_revision);
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
        if caller != LIFECYCLE {
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
        if caller != LIFECYCLE || !valid_text(operation_id) || !valid_target(&target) {
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

pub(crate) struct AcquireJob {
    pub parent: u64,
    pub owner: String,
    pub token: String,
    pub payload: Value,
    pub deadline: Instant,
    request: Option<u64>,
    operation_id: String,
}
impl AcquireJob {
    pub fn new(
        parent: u64,
        owner: String,
        token: String,
        payload: Value,
        deadline: Instant,
    ) -> Result<Self, String> {
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
                    Ok(Some(Authority {
                        target,
                        geometry_revision,
                        batch_epoch,
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
            service: LIFECYCLE.into(),
            payload: json!({"operation":"validate_input_binding","operation_id":self.operation_id,"token":self.token}),
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
            .bind("token", "owner", "lease", target(), 1)
            .unwrap();
        assert!(
            bindings
                .authorize_batch("owner", "lease", Some("token"))
                .is_ok()
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
                .revoke(LIFECYCLE, "revoke", "token", target())
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
                .revoke(LIFECYCLE, "revoke", "token", target())
                .is_ok()
        );
        assert!(
            bindings
                .revoke(LIFECYCLE, "other", "token", target())
                .is_err()
        );
        assert!(bindings.authorize_batch("owner", "legacy", None).is_ok());
    }
    #[test]
    fn revoke_before_authority_cannot_bind_and_unknown_token_never_claims_retirement() {
        let mut bindings = Bindings::default();
        assert!(
            bindings
                .revoke(LIFECYCLE, "r", "unknown", target())
                .is_err()
        );
        bindings.begin_acquire("token", "owner").unwrap();
        bindings.revoke(LIFECYCLE, "r", "token", target()).unwrap();
        assert!(
            bindings
                .bind("token", "owner", "lease", target(), 1)
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
        )
        .unwrap();
        let mut send = |command| -> ModuleResult<Response> {
            match command {
                Command::Request {
                    payload, parent, ..
                } => {
                    assert_eq!(parent, Some(7));
                    assert_eq!(payload["token"], "token");
                    Ok(Response::Accepted { id: 1 })
                }
                Command::TakeResult { .. } => Ok(Response::Result {
                    payload: json!({"status":"succeeded","target":target(),"geometry_revision":1,"batch_epoch":"epoch","allowed_actions":[]}),
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
        )
        .unwrap();
        let mut send = |command| -> ModuleResult<Response> {
            match command {
                Command::Request { .. } => Ok(Response::Accepted { id: 1 }),
                Command::TakeResult { .. } => Ok(Response::Result {
                    payload: json!({"status":"succeeded","target":{"kind":"host","host_epoch":"forged"},"geometry_revision":1,"batch_epoch":"epoch"}),
                }),
                Command::ReleaseResult { .. } => Ok(Response::Ok),
                _ => panic!("unexpected command"),
            }
        };
        denied.advance(&mut send).unwrap();
        assert!(denied.advance(&mut send).is_err());
    }
}
