//! Broker-local serialization of protected calls and exact-effect fences.
//! Durable recovery and actual worker retirement remain owned by the selected authority.
use super::errors::{capacity_exhausted, payload_fits, permission_denied};
use amaterasu_contracts::{
    Response, RuntimeConfig,
    effect_fence::{
        EffectFencePolicy, EffectScope, GuardedEffect, GuardedEffectIdentity, ProtectedMethod,
        SourceSlotLease,
    },
    valid_name,
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

const MAX_SOURCES: usize = 64;
const WINDOW: u64 = 32;

struct Record {
    effect: Option<GuardedEffect>,
    identity: GuardedEffectIdentity,
    request: Option<u64>,
    provider: String,
    delivered: bool,
    worker_retired: bool,
    fenced: bool,
}
struct SourceState {
    initial_retired: u64,
    retired: u64,
    records: BTreeMap<u64, Record>,
    closing: bool,
    recovery_issued: Option<u64>,
    issuer_retired: bool,
    host_retired: bool,
}
pub(super) struct EffectFences {
    policy: Option<EffectFencePolicy>,
    provider_services: BTreeSet<String>,
    slots: Option<super::source_slots::SourceSlots>,
    sources: BTreeMap<EffectScope, SourceState>,
}

impl EffectFences {
    pub fn new(config: &RuntimeConfig) -> Result<Self, String> {
        if let Some(policy) = &config.effect_fence {
            validate_policy(config, policy)?;
        }
        let mut provider_services = BTreeSet::new();
        if let Some(policy) = &config.effect_fence {
            for method in &policy.methods {
                if let Some(approval) = config.api_methods.iter().find(|approval| {
                    approval.method.service == method.service
                        && approval.method.method == method.method
                        && approval.method.interface_major == method.interface_major
                }) {
                    provider_services.extend(
                        config.principals[&approval.provider]
                            .provide
                            .iter()
                            .cloned(),
                    );
                }
            }
        }
        let slots = if let Some(policy) = config
            .effect_fence
            .as_ref()
            .filter(|policy| policy.source_slots)
        {
            let mut required = BTreeSet::from([
                policy.issuer.clone(),
                amaterasu_contracts::HOST_LIFECYCLE_CONTROL_PRINCIPAL.to_owned(),
            ]);
            for method in &policy.methods {
                let approval = config
                    .api_methods
                    .iter()
                    .find(|approval| {
                        approval.method.service == method.service
                            && approval.method.method == method.method
                            && approval.method.interface_major == method.interface_major
                    })
                    .expect("validated protected method");
                required.insert(approval.provider.clone());
            }
            Some(super::source_slots::SourceSlots::new(required)?)
        } else {
            None
        };
        Ok(Self {
            policy: config.effect_fence.clone(),
            provider_services,
            slots,
            sources: BTreeMap::new(),
        })
    }
    pub fn protects(&self, service: &str, method: &str, major: u32) -> bool {
        self.policy.as_ref().is_some_and(|policy| {
            policy.methods.iter().any(|item| {
                item.service == service && item.method == method && item.interface_major == major
            })
        })
    }
    /// A protected provider cannot be reached by another broker alias to bypass
    /// the effect gate. Only explicitly approved cleanup API methods are direct.
    pub fn direct_service_denied(&self, service: &str) -> bool {
        self.provider_services.contains(service)
    }
    pub fn direct_call_denied(&self, service: &str, method: &str, major: u32) -> bool {
        self.direct_service_denied(service)
            && !self.policy.as_ref().is_some_and(|policy| {
                policy.cleanup_methods.iter().any(|cleanup| {
                    cleanup.service == service
                        && cleanup.method == method
                        && cleanup.interface_major == major
                })
            })
    }
    pub fn authority(&self, caller: &str) -> bool {
        self.policy
            .as_ref()
            .is_some_and(|policy| policy.authority == caller)
    }
    fn issuer(&self, caller: &str) -> bool {
        self.policy
            .as_ref()
            .is_some_and(|policy| policy.issuer == caller)
    }
    pub fn issuer_name(&self) -> Option<&str> {
        self.policy.as_ref().map(|policy| policy.issuer.as_str())
    }
    pub fn uses_slots(&self) -> bool {
        self.slots.is_some()
    }
    pub fn broker_epoch(&self) -> Response {
        match &self.slots {
            Some(slots) => Response::Result {
                payload: json!({"broker_epoch":slots.epoch()}),
            },
            None => Response::error("unsupported", "source slots are not configured"),
        }
    }
    pub fn valid_epoch(&self, epoch: &str) -> bool {
        self.slots
            .as_ref()
            .is_some_and(|slots| slots.epoch() == epoch)
    }
    pub fn slot_status(&self, caller: &str, slot: u8) -> Response {
        if !self.authority(caller) {
            return permission_denied();
        }
        match &self.slots {
            Some(slots) => slots.status(slot),
            None => Response::error("unsupported", "source slots are not configured"),
        }
    }
    pub fn recovery_retired(&mut self, caller: &str, epoch: &str) -> Response {
        match &mut self.slots {
            Some(slots) => slots.confirm_retired(caller, epoch),
            None => Response::error("unsupported", "source slots are not configured"),
        }
    }
    pub fn recover_slots(&mut self, caller: &str, epoch: &str, retired: Vec<u64>) -> Response {
        if !self.authority(caller) {
            return permission_denied();
        }
        match &mut self.slots {
            Some(slots) => slots.recover(epoch, retired),
            None => Response::error("unsupported", "source slots are not configured"),
        }
    }
    pub fn slot_valid(&self, lease: &SourceSlotLease, scope: &EffectScope) -> bool {
        self.slots
            .as_ref()
            .is_some_and(|slots| slots.valid_for(lease, scope))
    }
    pub fn open_slot(
        &mut self,
        caller: &str,
        lease: SourceSlotLease,
        scope: EffectScope,
        retired: u64,
        recovery_issued: Option<u64>,
    ) -> Response {
        if !self.authority(caller) {
            return permission_denied();
        }
        let recovery = recovery_issued.is_some();
        if recovery_issued
            .is_some_and(|issued| issued < retired || issued.saturating_sub(retired) > WINDOW)
        {
            return invalid("invalid recovery watermarks");
        }
        if !valid_scope(&scope) {
            return invalid("invalid source scope");
        }
        let Some(slots) = &self.slots else {
            return Response::error("unsupported", "source slots are not configured");
        };
        let new = match slots.can_open(&lease, &scope) {
            Ok(new) => new,
            Err(error) => return error,
        };
        if !new
            && self.sources.get(&scope).is_some_and(|source| {
                source.closing != recovery || source.recovery_issued != recovery_issued
            })
        {
            return Response::error("effect_fenced", "source mode differs");
        }
        let response = self.open(caller, scope.clone(), retired);
        if matches!(response, Response::Ok) && new {
            if recovery {
                let source = self.sources.get_mut(&scope).expect("opened source");
                source.closing = true;
                source.recovery_issued = recovery_issued;
            }
            self.slots
                .as_mut()
                .expect("slot mode")
                .opened(&lease, scope);
        }
        response
    }
    /// Block new work and fence all records under the broker lock. The returned
    /// request IDs were never delivered and must be cancelled before unlocking.
    pub fn begin_close(
        &mut self,
        caller: &str,
        lease: &SourceSlotLease,
        scope: &EffectScope,
    ) -> Result<Vec<u64>, Response> {
        if !self.authority(caller) {
            return Err(permission_denied());
        }
        if !self.slot_valid(lease, scope) {
            return Err(Response::error(
                "stale_source_slot",
                "source slot incarnation is stale",
            ));
        }
        let source = self
            .sources
            .get_mut(scope)
            .ok_or_else(|| Response::error("recovery_required", "source is not open"))?;
        source.closing = true;
        let mut cancel = Vec::new();
        for record in source.records.values_mut() {
            record.fenced = true;
            if !record.delivered {
                record.worker_retired = true;
                if let Some(request) = record.request {
                    cancel.push(request);
                }
            }
        }
        Ok(cancel)
    }
    pub fn source_retired(
        &mut self,
        caller: &str,
        lease: &SourceSlotLease,
        scope: &EffectScope,
    ) -> Response {
        let issuer = self.issuer(caller);
        let host = caller == amaterasu_contracts::HOST_LIFECYCLE_CONTROL_PRINCIPAL;
        if !issuer && !host {
            return permission_denied();
        }
        if !self.slot_valid(lease, scope) {
            return Response::error("stale_source_slot", "source slot incarnation is stale");
        }
        let Some(source) = self.sources.get_mut(scope) else {
            return Response::error("recovery_required", "source is not open");
        };
        if !source.closing {
            return Response::error("effect_pending", "source is not fenced");
        }
        if issuer {
            source.issuer_retired = true;
        }
        if host {
            source.host_retired = true;
        }
        Response::Ok
    }
    pub fn finish_close(
        &mut self,
        caller: &str,
        lease: &SourceSlotLease,
        scope: &EffectScope,
        retired: u64,
    ) -> Response {
        if !self.authority(caller) {
            return permission_denied();
        }
        if !self.slot_valid(lease, scope) {
            return Response::error("stale_source_slot", "source slot incarnation is stale");
        }
        let Some(source) = self.sources.get(scope) else {
            return Response::error("recovery_required", "source is not open");
        };
        if !source.closing
            || !source.issuer_retired
            || !source.host_retired
            || !source.records.is_empty()
            || source.retired != retired
            || source
                .recovery_issued
                .is_some_and(|issued| source.retired != issued)
        {
            return Response::error(
                "effect_pending",
                "source retirement or compaction is incomplete",
            );
        }
        if let Err(error) = self.slots.as_mut().expect("slot mode").close(lease, scope) {
            return error;
        }
        self.sources.remove(scope);
        Response::Ok
    }
    pub fn open(&mut self, caller: &str, scope: EffectScope, retired: u64) -> Response {
        if !self.authority(caller) {
            return permission_denied();
        }
        if !valid_scope(&scope) {
            return invalid("invalid source scope");
        }
        if let Some(existing) = self.sources.get(&scope) {
            return if existing.initial_retired == retired {
                Response::Ok
            } else {
                conflict()
            };
        }
        // A live source may never be reopened under another incarnation in the
        // same broker. Restart recovery must reconcile and retire old workers.
        if self
            .sources
            .keys()
            .any(|old| old.source.id == scope.source.id)
        {
            return conflict();
        }
        if self.sources.len() >= MAX_SOURCES {
            return capacity_exhausted();
        }
        self.sources.insert(
            scope,
            SourceState {
                initial_retired: retired,
                retired,
                records: BTreeMap::new(),
                closing: false,
                recovery_issued: None,
                issuer_retired: false,
                host_retired: false,
            },
        );
        Response::Ok
    }
    fn validate_effect(&self, effect: &GuardedEffect) -> Result<(), Response> {
        if !valid_scope(&effect.scope)
            || effect.sequence == 0
            || effect.schema_revision == 0
            || effect.payload_sha256.len() != 64
            || !effect
                .payload_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !valid_name(&effect.capability)
            || !valid_name(&effect.operation_id)
            || !(1..=30_000).contains(&effect.timeout_ms)
            || !payload_fits(&effect.payload)
            || !self.protects(&effect.service, &effect.method, effect.interface_major)
        {
            return Err(invalid("invalid protected effect"));
        }
        Ok(())
    }
    fn source_mut(&mut self, effect: &GuardedEffect) -> Result<&mut SourceState, Response> {
        self.validate_effect(effect)?;
        let source = self
            .sources
            .get_mut(&effect.scope)
            .ok_or_else(|| Response::error("recovery_required", "source is not open"))?;
        if effect.sequence <= source.retired {
            return Err(Response::error("effect_retired", "sequence is retired"));
        }
        if effect.sequence.saturating_sub(source.retired) > WINDOW {
            return Err(capacity_exhausted());
        }
        Ok(source)
    }
    pub fn arm(&mut self, caller: &str, effect: GuardedEffect, provider: &str) -> Response {
        if !self.authority(caller) {
            return permission_denied();
        }
        let source = match self.source_mut(&effect) {
            Ok(source) => source,
            Err(error) => return error,
        };
        if source.closing {
            return Response::error("effect_fenced", "source is closing");
        }
        if let Some(record) = source.records.get(&effect.sequence) {
            return if record.identity != effect.identity(provider)
                || record
                    .effect
                    .as_ref()
                    .is_some_and(|existing| existing != &effect)
            {
                conflict()
            } else if record.fenced {
                Response::error("effect_fenced", "effect is fenced")
            } else {
                Response::Ok
            };
        }
        source.records.insert(
            effect.sequence,
            Record {
                identity: effect.identity(provider),
                effect: Some(effect),
                request: None,
                provider: provider.into(),
                delivered: false,
                worker_retired: false,
                fenced: false,
            },
        );
        Response::Ok
    }
    pub fn may_dispatch(
        &mut self,
        caller: &str,
        effect: &GuardedEffect,
    ) -> Result<Option<u64>, Response> {
        if !self.issuer(caller) {
            return Err(permission_denied());
        }
        let source = self.source_mut(effect)?;
        if source.closing {
            return Err(Response::error("effect_fenced", "source is closing"));
        }
        let record = source
            .records
            .get(&effect.sequence)
            .ok_or_else(|| Response::error("effect_not_armed", "effect is not armed"))?;
        if record.identity != effect.identity(&record.provider)
            || record
                .effect
                .as_ref()
                .is_some_and(|existing| existing != effect)
        {
            return Err(conflict());
        }
        if record.fenced {
            return Err(Response::error("effect_fenced", "effect is fenced"));
        }
        Ok(record.request)
    }
    pub fn admitted(&mut self, effect: &GuardedEffect, request: u64) {
        // Called within the same broker dispatch after successful enqueue.
        if let Some(record) = self
            .sources
            .get_mut(&effect.scope)
            .and_then(|s| s.records.get_mut(&effect.sequence))
        {
            record.request = Some(request);
        }
    }
    pub fn delivered(&mut self, request: u64) {
        for source in self.sources.values_mut() {
            for record in source.records.values_mut() {
                if record.request == Some(request) {
                    record.delivered = true;
                    return;
                }
            }
        }
    }
    /// Returns an undelivered request to cancel while the broker lock is held.
    pub fn fence(
        &mut self,
        caller: &str,
        effect: GuardedEffect,
        provider: &str,
    ) -> Result<Option<u64>, Response> {
        if !self.authority(caller) {
            return Err(permission_denied());
        }
        let source = self.source_mut(&effect)?;
        let record = source
            .records
            .entry(effect.sequence)
            .or_insert_with(|| Record {
                identity: effect.identity(provider),
                effect: Some(effect.clone()),
                request: None,
                provider: provider.into(),
                delivered: false,
                worker_retired: false,
                fenced: false,
            });
        if record.identity != effect.identity(provider)
            || record
                .effect
                .as_ref()
                .is_some_and(|existing| existing != &effect)
        {
            return Err(conflict());
        }
        record.fenced = true;
        if !record.delivered {
            record.worker_retired = true;
            Ok(record.request)
        } else {
            Ok(None)
        }
    }
    pub fn provider_retired(&mut self, caller: &str, request: u64) -> Response {
        // Request IDs are unique for the broker lifetime. The provider need not
        // reconstruct or learn the authority's private source/operation context.
        for source in self.sources.values_mut() {
            for record in source.records.values_mut() {
                if record.request == Some(request) {
                    if record.provider != caller || !record.delivered {
                        return permission_denied();
                    }
                    record.worker_retired = true;
                    return Response::Ok;
                }
            }
        }
        permission_denied()
    }
    pub fn lookup(&mut self, caller: &str, effect: &GuardedEffect) -> Response {
        if !self.authority(caller) && !self.issuer(caller) {
            return permission_denied();
        }
        if let Err(error) = self.validate_effect(effect) {
            return error;
        }
        let Some(source) = self.sources.get_mut(&effect.scope) else {
            return Response::error("recovery_required", "source is not open");
        };
        if effect.sequence <= source.retired {
            return Response::Result {
                payload: json!({"status":"retired_sequence", "retired_through":source.retired}),
            };
        }
        let Some(record) = source.records.get(&effect.sequence) else {
            return Response::Result {
                payload: json!({"status":"unknown"}),
            };
        };
        if record.identity != effect.identity(&record.provider) {
            return conflict();
        }
        let Some(original) = &record.effect else {
            return Response::error("payload_unavailable", "use payload-free identity lookup");
        };
        if original != effect {
            return conflict();
        }
        let state = if record.fenced && record.worker_retired {
            "retired"
        } else if record.fenced {
            "fenced"
        } else if record.request.is_some() {
            "admitted"
        } else {
            "armed"
        };
        Response::Result {
            payload: json!({"status":state,"request":record.request}),
        }
    }
    fn validate_identity(&self, identity: &GuardedEffectIdentity) -> Result<(), Response> {
        if !valid_scope(&identity.scope)
            || identity.sequence == 0
            || identity.schema_revision == 0
            || identity.payload_sha256.len() != 64
            || !identity
                .payload_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !valid_name(&identity.capability)
            || !valid_name(&identity.provider)
            || !valid_name(&identity.operation_id)
            || !(1..=30_000).contains(&identity.timeout_ms)
            || !self.protects(
                &identity.service,
                &identity.method,
                identity.interface_major,
            )
        {
            return Err(invalid("invalid protected effect identity"));
        }
        Ok(())
    }
    fn identity_source_mut(
        &mut self,
        identity: &GuardedEffectIdentity,
    ) -> Result<&mut SourceState, Response> {
        self.validate_identity(identity)?;
        let source = self
            .sources
            .get_mut(&identity.scope)
            .ok_or_else(|| Response::error("recovery_required", "source is not open"))?;
        if identity.sequence <= source.retired {
            return Err(Response::error("effect_retired", "sequence is retired"));
        }
        if source
            .recovery_issued
            .is_some_and(|issued| identity.sequence > issued)
        {
            return Err(invalid("sequence exceeds recovered issued watermark"));
        }
        if identity.sequence.saturating_sub(source.retired) > WINDOW {
            return Err(capacity_exhausted());
        }
        Ok(source)
    }
    /// Fences a journal identity with no payload; it can never authorize work.
    pub fn recover_fence(
        &mut self,
        caller: &str,
        identity: GuardedEffectIdentity,
    ) -> Result<Option<u64>, Response> {
        if !self.authority(caller) {
            return Err(permission_denied());
        }
        let source = self.identity_source_mut(&identity)?;
        let record = source
            .records
            .entry(identity.sequence)
            .or_insert_with(|| Record {
                effect: None,
                provider: identity.provider.clone(),
                identity: identity.clone(),
                request: None,
                delivered: false,
                worker_retired: false,
                fenced: false,
            });
        if record.identity != identity {
            return Err(conflict());
        }
        record.fenced = true;
        if !record.delivered {
            record.worker_retired = true;
            Ok(record.request)
        } else {
            Ok(None)
        }
    }
    pub fn lookup_identity(&mut self, caller: &str, identity: &GuardedEffectIdentity) -> Response {
        if !self.authority(caller) {
            return permission_denied();
        }
        if let Err(error) = self.validate_identity(identity) {
            return error;
        }
        let Some(source) = self.sources.get(&identity.scope) else {
            return Response::error("recovery_required", "source is not open");
        };
        if identity.sequence <= source.retired {
            return Response::Result {
                payload: json!({"status":"retired_sequence", "retired_through":source.retired}),
            };
        }
        let Some(record) = source.records.get(&identity.sequence) else {
            return Response::Result {
                payload: json!({"status":"unknown"}),
            };
        };
        if record.identity != *identity {
            return conflict();
        }
        let status = if record.fenced && record.worker_retired {
            "retired"
        } else if record.fenced {
            "fenced"
        } else if record.request.is_some() {
            "admitted"
        } else {
            "armed"
        };
        Response::Result {
            payload: json!({"status":status,"request":record.request}),
        }
    }

    /// Forget only a consecutive fenced and retired prefix after the authority
    /// has durably recorded exact-effect evidence. A watermark is not a result.
    pub fn compact(&mut self, caller: &str, scope: &EffectScope, through: u64) -> Response {
        if !self.authority(caller) {
            return permission_denied();
        }
        let Some(source) = self.sources.get_mut(scope) else {
            return Response::error("recovery_required", "source is not open");
        };
        if through <= source.retired {
            return Response::Ok;
        }
        if through.saturating_sub(source.retired) > WINDOW {
            return invalid("invalid retirement watermark");
        }
        for sequence in source.retired.saturating_add(1)..=through {
            if !source
                .records
                .get(&sequence)
                .is_some_and(|record| record.fenced && record.worker_retired)
            {
                return Response::error("effect_pending", "retirement is not confirmed");
            }
        }
        source.records.retain(|sequence, _| *sequence > through);
        source.retired = through;
        Response::Ok
    }
}

fn valid_scope(scope: &EffectScope) -> bool {
    let source = &scope.source;
    let token = |value: &str| {
        !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
    };
    source.version == 1
        && source.id.len() == 32
        && source
            .id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && source.generation != 0
        && source.generation == source.stream_generation
        && source.device_generation != 0
        && [
            &source.host_epoch,
            &source.session_id,
            &source.stream_id,
            &source.device,
            &source.effect_owner_epoch,
            &scope.authority_epoch,
            &scope.issuer_epoch,
        ]
        .iter()
        .all(|value| token(value))
}
fn invalid(message: &str) -> Response {
    Response::error("invalid_request", message)
}
fn conflict() -> Response {
    Response::error("effect_conflict", "effect identity differs")
}
fn validate_policy(config: &RuntimeConfig, policy: &EffectFencePolicy) -> Result<(), String> {
    if !valid_name(&policy.authority)
        || !valid_name(&policy.issuer)
        || policy.authority == policy.issuer
        || (policy.source_slots && !config.host_lifecycle_control)
        || !config.principals.contains_key(&policy.authority)
        || !config.principals.contains_key(&policy.issuer)
        || policy.methods.is_empty()
        || policy.methods.len() > 64
        || policy.cleanup_methods.len() > 64
    {
        return Err("invalid effect fence policy".into());
    }
    let mut keys = BTreeSet::new();
    for ProtectedMethod {
        service,
        method,
        interface_major,
    } in &policy.methods
    {
        let Some(approval) = config.api_methods.iter().find(|approval| {
            approval.method.service == *service
                && approval.method.method == *method
                && approval.method.interface_major == *interface_major
        }) else {
            return Err("protected effect method is not approved".into());
        };
        if approval.provider == policy.issuer
            || approval.provider == policy.authority
            || !approval.callers.contains(&policy.issuer)
            || !keys.insert((service, method, interface_major))
        {
            return Err("invalid protected effect method".into());
        }
    }
    for cleanup in &policy.cleanup_methods {
        if !keys.insert((&cleanup.service, &cleanup.method, &cleanup.interface_major))
            || !config.api_methods.iter().any(|approval| {
                approval.method.service == cleanup.service
                    && approval.method.method == cleanup.method
                    && approval.method.interface_major == cleanup.interface_major
                    && policy.methods.iter().any(|item| {
                        config.api_methods.iter().any(|protected| {
                            protected.provider == approval.provider
                                && protected.method.service == item.service
                                && protected.method.method == item.method
                                && protected.method.interface_major == item.interface_major
                        })
                    })
            })
        {
            return Err("invalid effect cleanup method".into());
        }
    }
    Ok(())
}
