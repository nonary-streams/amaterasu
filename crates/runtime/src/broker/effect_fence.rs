//! Broker-local serialization of protected calls and exact-effect fences.
//! Durable recovery and actual worker retirement remain owned by the selected authority.
use super::errors::{capacity_exhausted, payload_fits, permission_denied};
use amaterasu_contracts::{
    Response, RuntimeConfig,
    effect_fence::{
        EffectFencePolicy, EffectScope, GuardedEffect, GuardedEffectIdentity,
        HostProviderRetirement, ProtectedMethod, ProviderIncarnation, SourceSlotLease,
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
    provider_incarnation: Option<ProviderIncarnation>,
    delivered: bool,
    worker_retired: bool,
    fenced: bool,
    historical: bool,
    cleanup_confirmed: bool,
    historical_entry: Option<serde_json::Value>,
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
    joined_providers: BTreeSet<String>,
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
            if let Some(Some(state)) = config.service_selections.broker.get("amaterasu.state/1") {
                required.insert(state.provider.clone());
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
            joined_providers: BTreeSet::new(),
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
    pub fn authority_name(&self) -> Option<&str> {
        self.policy.as_ref().map(|policy| policy.authority.as_str())
    }
    pub fn provider_joined(&self, provider: &str) -> bool {
        self.joined_providers.contains(provider)
    }
    pub fn host_provider_joined(&mut self, provider: &str, incarnation: &ProviderIncarnation) {
        self.joined_providers.insert(provider.to_owned());
        for source in self.sources.values_mut() {
            for record in source.records.values_mut() {
                if record.provider == provider
                    && record.provider_incarnation.as_ref() == Some(incarnation)
                {
                    record.worker_retired = true;
                }
            }
        }
    }
    pub fn host_retirement_receipt(&self, scope: &EffectScope, sequence: u64) -> Response {
        let Some(record) = self
            .sources
            .get(scope)
            .and_then(|source| source.records.get(&sequence))
        else {
            return Response::error("recovery_required", "effect record is unavailable");
        };
        let Some(incarnation) = &record.provider_incarnation else {
            return Response::error("recovery_required", "provider incarnation is unavailable");
        };
        let Some(slots) = &self.slots else {
            return Response::error("unsupported", "source slots are not configured");
        };
        if !record.fenced || !record.worker_retired || !self.provider_joined(&record.provider) {
            return Response::error("effect_pending", "provider worker has not been joined");
        }
        let receipt = HostProviderRetirement {
            version: 1,
            broker_epoch: slots.epoch().to_owned(),
            issued_lease: match slots.lease_for(scope) {
                Some(lease) => lease,
                None => return Response::error("recovery_required", "source slot is unavailable"),
            },
            provider_incarnation: incarnation.clone(),
            effect: record.identity.clone(),
        };
        Response::Result {
            payload: serde_json::to_value(receipt).expect("receipt is serializable"),
        }
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
    pub fn recovery_only(&self, scope: &EffectScope) -> bool {
        self.sources
            .get(scope)
            .is_some_and(|source| source.recovery_issued.is_some() && source.closing)
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
    pub fn arm(
        &mut self,
        caller: &str,
        effect: GuardedEffect,
        provider: &str,
        incarnation: Option<&ProviderIncarnation>,
    ) -> Response {
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
                || record.provider_incarnation.as_ref() != incarnation
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
                provider_incarnation: incarnation.cloned(),
                delivered: false,
                worker_retired: false,
                fenced: false,
                historical: false,
                cleanup_confirmed: false,
                historical_entry: None,
            },
        );
        Response::Ok
    }
    pub fn may_dispatch(
        &mut self,
        caller: &str,
        effect: &GuardedEffect,
        incarnation: Option<&ProviderIncarnation>,
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
                .provider_incarnation
                .as_ref()
                .is_some_and(|stored| Some(stored) != incarnation)
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
                provider_incarnation: None,
                delivered: false,
                worker_retired: false,
                fenced: false,
                historical: false,
                cleanup_confirmed: false,
                historical_entry: None,
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
    pub fn provider_retired(
        &mut self,
        caller: &str,
        request: u64,
        incarnation: Option<&ProviderIncarnation>,
    ) -> Response {
        // Request IDs are unique for the broker lifetime. The provider need not
        // reconstruct or learn the authority's private source/operation context.
        for source in self.sources.values_mut() {
            for record in source.records.values_mut() {
                if record.request == Some(request) {
                    if record.provider != caller
                        || !record.delivered
                        || record.provider_incarnation.as_ref() != incarnation
                    {
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
    fn validate_identity(
        &self,
        identity: &GuardedEffectIdentity,
        historical: bool,
    ) -> Result<(), Response> {
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
            || !valid_name(&identity.service)
            || !valid_name(&identity.method)
            || identity.interface_major == 0
            || (!historical
                && !self.protects(
                    &identity.service,
                    &identity.method,
                    identity.interface_major,
                ))
        {
            return Err(invalid("invalid protected effect identity"));
        }
        Ok(())
    }
    fn identity_source_mut(
        &mut self,
        identity: &GuardedEffectIdentity,
    ) -> Result<&mut SourceState, Response> {
        self.validate_identity(identity, false)?;
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
    pub fn is_historical(&self, identity: &GuardedEffectIdentity) -> bool {
        self.sources
            .get(&identity.scope)
            .and_then(|source| source.records.get(&identity.sequence))
            .is_some_and(|record| record.historical && record.identity == *identity)
    }
    pub fn recover_historical(
        &mut self,
        identity: GuardedEffectIdentity,
        incarnation: &ProviderIncarnation,
        expected_entry: &serde_json::Value,
    ) -> Response {
        if let Err(error) = self.validate_identity(&identity, true) {
            return error;
        }
        let Some(source) = self.sources.get_mut(&identity.scope) else {
            return Response::error("recovery_required", "source is not open");
        };
        let Some(issued) = source.recovery_issued else {
            return Response::error("recovery_required", "source is not in recovery");
        };
        if identity.sequence <= source.retired
            || identity.sequence > issued
            || identity.sequence.saturating_sub(source.retired) > WINDOW
        {
            return Response::error("effect_conflict", "historical sequence differs");
        }
        let mut original_entry = expected_entry.clone();
        original_entry["cleanup_confirmed"] = serde_json::Value::Bool(false);
        if let Some(record) = source.records.get(&identity.sequence) {
            return if record.historical
                && record.identity == identity
                && record.provider_incarnation.as_ref() == Some(incarnation)
                && record.historical_entry.as_ref() == Some(&original_entry)
            {
                Response::Ok
            } else {
                conflict()
            };
        }
        source.records.insert(
            identity.sequence,
            Record {
                provider: identity.provider.clone(),
                identity,
                effect: None,
                request: None,
                provider_incarnation: Some(incarnation.clone()),
                delivered: false,
                worker_retired: true,
                fenced: true,
                historical: true,
                cleanup_confirmed: false,
                historical_entry: Some(original_entry),
            },
        );
        Response::Ok
    }
    pub fn confirm_historical_cleanup(
        &mut self,
        identity: &GuardedEffectIdentity,
        incarnation: &ProviderIncarnation,
        expected_entry: &serde_json::Value,
    ) -> Response {
        let Some(record) = self
            .sources
            .get_mut(&identity.scope)
            .and_then(|source| source.records.get_mut(&identity.sequence))
        else {
            return Response::error("recovery_required", "historical retirement is unavailable");
        };
        let mut original_entry = expected_entry.clone();
        original_entry["cleanup_confirmed"] = serde_json::Value::Bool(false);
        if !record.historical
            || record.identity != *identity
            || record.provider_incarnation.as_ref() != Some(incarnation)
            || record.historical_entry.as_ref() != Some(&original_entry)
        {
            return conflict();
        }
        record.cleanup_confirmed = true;
        Response::Ok
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
                provider_incarnation: None,
                identity: identity.clone(),
                request: None,
                delivered: false,
                worker_retired: false,
                fenced: false,
                historical: false,
                cleanup_confirmed: false,
                historical_entry: None,
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
        if let Err(error) = self.validate_identity(identity, self.is_historical(identity)) {
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
            if !source.records.get(&sequence).is_some_and(|record| {
                record.fenced
                    && record.worker_retired
                    && (!record.historical || record.cleanup_confirmed)
            }) {
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
