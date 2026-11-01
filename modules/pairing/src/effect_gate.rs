//! Pairing-owned admission ledger. A source is bound only after the protocol
//! owner has joined a verified TLS peer to the lifecycle-issued source.
use std::collections::BTreeMap;

const MAX_SOURCES: usize = 64;
const MAX_OPERATIONS: usize = 256;
const MAX_RETIRED_SOURCES: usize = 4096;

pub use amaterasu_module_sdk::effect_authority::{Request, Source, StreamRetirement, StreamScope};

fn token(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

fn same_transport(left: &StreamScope, right: &StreamScope) -> bool {
    left.transport_service == right.transport_service
        && left.transport_stream == right.transport_stream
        && left.transport_epoch == right.transport_epoch
}

fn receipt_matches_transport(receipt: &StreamRetirement, scope: &StreamScope) -> bool {
    receipt.service == scope.transport_service
        && receipt.stream == scope.transport_stream
        && receipt.provider_epoch == scope.transport_epoch
}

#[derive(Clone)]
struct Admission {
    source: Source,
    ticket: String,
    sequence: u64,
    complete: bool,
    scope: Option<StreamScope>,
}

#[derive(Clone)]
struct SourceRetirement {
    scope: StreamScope,
    operation_id: String,
    complete: bool,
    receipt: Option<StreamRetirement>,
}

/// An admission is kept until its source is retired. A lost reply can be
/// reconciled by repeating the same operation ID; a ticket is never expired
/// merely because a deadline elapsed while an effect could still be running.
pub struct EffectGate {
    epoch: String,
    owner_epoch: Option<String>,
    sources: BTreeMap<Source, Option<StreamScope>>,
    operations: BTreeMap<String, Admission>,
    lease_admissions: BTreeMap<Source, String>,
    lease_receipts: BTreeMap<Source, StreamRetirement>,
    retired_sources: BTreeMap<Source, SourceRetirement>,
    highwater: u64,
    fenced: bool,
    fence_generation: u64,
    drain_required: bool,
    startup_recovery_required: bool,
}

impl EffectGate {
    pub fn new() -> Result<Self, &'static str> {
        let mut bytes = [0_u8; 16];
        getrandom::fill(&mut bytes).map_err(|_| "entropy_unavailable")?;
        Ok(Self {
            epoch: bytes.iter().map(|b| format!("{b:02x}")).collect(),
            owner_epoch: None,
            sources: BTreeMap::new(),
            operations: BTreeMap::new(),
            lease_admissions: BTreeMap::new(),
            lease_receipts: BTreeMap::new(),
            retired_sources: BTreeMap::new(),
            highwater: 0,
            fenced: true,
            fence_generation: 0,
            drain_required: false,
            startup_recovery_required: true,
        })
    }

    pub fn epoch(&self) -> &str {
        &self.epoch
    }
    pub fn startup_recovery_required(&self) -> bool {
        self.startup_recovery_required
    }
    pub fn owner_epoch(&self) -> Option<&str> {
        self.owner_epoch.as_deref()
    }
    pub fn require_epoch(&self, epoch: &str) -> Result<(), &'static str> {
        if epoch == self.epoch {
            Ok(())
        } else {
            Err("stale_epoch")
        }
    }

    /// The selected effect owner must first retire all workers from any prior
    /// authority epoch. This is a trusted owner's assertion, not a timer.
    pub fn recover(&mut self, owner_epoch: &str) -> Result<(), &'static str> {
        if !token(owner_epoch)
            || self.in_flight() != 0
            || self.drain_required
            || self
                .retired_sources
                .values()
                .any(|retirement| !retirement.complete)
            || (!self.fenced && !self.sources.is_empty())
        {
            return Err("recovery_required");
        }
        let mut bytes = [0_u8; 16];
        getrandom::fill(&mut bytes).map_err(|_| "entropy_unavailable")?;
        self.epoch = bytes.iter().map(|b| format!("{b:02x}")).collect();
        self.sources.clear();
        self.operations.clear();
        self.lease_admissions.clear();
        self.lease_receipts.clear();
        self.retired_sources.clear();
        self.highwater = 0;
        self.fence_generation = 0;
        self.drain_required = false;
        self.owner_epoch = Some(owner_epoch.into());
        self.startup_recovery_required = false;
        self.fenced = false;
        Ok(())
    }

    /// Called only by the selected protocol owner after checking live pairing
    /// and joining its TLS session to lifecycle's independently issued source.
    pub fn bind(&mut self, source: Source) -> Result<(), &'static str> {
        self.bind_scoped(source, None)
    }

    pub fn bind_scoped(
        &mut self,
        source: Source,
        scope: Option<StreamScope>,
    ) -> Result<(), &'static str> {
        if scope.as_ref().is_some_and(|scope| !scope.valid()) {
            return Err("invalid_stream_scope");
        }
        if self.fenced
            || !source.valid()
            || self.owner_epoch.as_deref() != Some(&source.effect_owner_epoch)
            || self.retired_sources.contains_key(&source)
        {
            return Err("source_denied");
        }
        if !self.sources.contains_key(&source) && self.sources.len() >= MAX_SOURCES {
            return Err("source_capacity");
        }
        if self
            .sources
            .get(&source)
            .is_some_and(|previous| previous != &scope)
        {
            return Err("source_scope_conflict");
        }
        if let Some(requested) = &scope {
            if self
                .retired_sources
                .values()
                .any(|retirement| same_transport(requested, &retirement.scope))
                || self
                    .lease_receipts
                    .values()
                    .any(|receipt| receipt_matches_transport(receipt, requested))
            {
                return Err("transport_retired");
            }
            let same_slot = |other: &Source, existing: &StreamScope| {
                other.host_epoch == source.host_epoch
                    && other.session_id == source.session_id
                    && existing.controller_slot == requested.controller_slot
            };
            if self.sources.iter().any(|(other, existing)| {
                other != &source
                    && existing
                        .as_ref()
                        .is_some_and(|scope| same_slot(other, scope))
            }) {
                return Err("controller_slot_in_use");
            }
            if self.retired_sources.iter().any(|(other, retirement)| {
                !retirement.complete && same_slot(other, &retirement.scope)
            }) {
                return Err("controller_retirement_pending");
            }
        }
        self.sources.insert(source, scope);
        Ok(())
    }

    /// Fence even an as-yet-unbound source, so a delayed bind cannot resurrect
    /// it. Tombstones survive until a fully drained authority epoch rotates.
    pub fn fence_source(
        &mut self,
        epoch: &str,
        source: &Source,
        scope: &StreamScope,
        operation_id: &str,
    ) -> Result<bool, &'static str> {
        self.require_epoch(epoch)?;
        if !source.valid()
            || !scope.valid()
            || !token(operation_id)
            || self.owner_epoch.as_deref() != Some(&source.effect_owner_epoch)
        {
            return Err("invalid_source_retirement");
        }
        if let Some(previous) = self.retired_sources.get(source) {
            return if previous.operation_id == operation_id && previous.scope == *scope {
                Ok(previous.complete)
            } else {
                Err("source_retirement_conflict")
            };
        }
        if self
            .retired_sources
            .values()
            .any(|previous| previous.operation_id == operation_id)
        {
            return Err("source_retirement_conflict");
        }
        if self.retired_sources.len() >= MAX_RETIRED_SOURCES {
            return Err("source_retirement_capacity");
        }
        if self
            .sources
            .get(source)
            .is_some_and(|bound| bound.as_ref() != Some(scope))
            || self.operations.values().any(|admission| {
                admission.source == *source && admission.scope.as_ref() != Some(scope)
            })
        {
            return Err("stream_scope_denied");
        }
        self.retired_sources.insert(
            source.clone(),
            SourceRetirement {
                scope: scope.clone(),
                operation_id: operation_id.into(),
                complete: false,
                receipt: None,
            },
        );
        self.sources.remove(source);
        Ok(false)
    }

    pub fn lookup_source_retirement(
        &self,
        epoch: &str,
        source: &Source,
        scope: &StreamScope,
        operation_id: &str,
    ) -> Result<bool, &'static str> {
        self.require_epoch(epoch)?;
        let retirement = self
            .retired_sources
            .get(source)
            .ok_or("unknown_source_retirement")?;
        if retirement.scope != *scope || retirement.operation_id != operation_id {
            return Err("source_retirement_conflict");
        }
        Ok(retirement.complete)
    }

    pub fn lookup_current_source_retirement(
        &self,
        epoch: &str,
        source: &Source,
        scope: &StreamScope,
    ) -> Result<(&str, bool), &'static str> {
        self.require_epoch(epoch)?;
        let retirement = self
            .retired_sources
            .get(source)
            .ok_or("unknown_source_retirement")?;
        if retirement.scope != *scope {
            return Err("source_retirement_conflict");
        }
        Ok((&retirement.operation_id, retirement.complete))
    }

    /// The selected effect owner must first complete every admitted lease and
    /// attest the exact transport retirement if any lease was ever admitted.
    pub fn complete_source_retirement(
        &mut self,
        epoch: &str,
        source: &Source,
        scope: &StreamScope,
        operation_id: &str,
        receipt: Option<&StreamRetirement>,
    ) -> Result<(), &'static str> {
        let complete = self.lookup_source_retirement(epoch, source, scope, operation_id)?;
        if complete {
            return if self
                .retired_sources
                .get(source)
                .and_then(|item| item.receipt.as_ref())
                == receipt
            {
                Ok(())
            } else {
                Err("source_retirement_conflict")
            };
        }
        if self.sources.iter().any(|(other, bound)| {
            other != source
                && bound
                    .as_ref()
                    .is_some_and(|other_scope| same_transport(scope, other_scope))
        }) {
            return Err("shared_transport_active");
        }
        if self.operations.values().any(|admission| {
            admission
                .scope
                .as_ref()
                .is_some_and(|other_scope| same_transport(scope, other_scope))
                && !admission.complete
        }) {
            return Err("effects_in_flight");
        }
        if self.operations.values().any(|admission| {
            admission
                .scope
                .as_ref()
                .is_some_and(|other_scope| same_transport(scope, other_scope))
        }) {
            return Err("lease_acknowledgment_pending");
        }
        if (self.lease_admissions.contains_key(source)
            && self
                .lease_receipts
                .get(source)
                .is_none_or(|expected| Some(expected) != receipt))
            || receipt.is_some_and(|retirement| !retirement.matches(scope))
        {
            return Err("retirement_mismatch");
        }
        if let Some(receipt) = receipt
            && self.lease_receipts.values().any(|previous| {
                previous.service == receipt.service
                    && previous.stream == receipt.stream
                    && previous.provider_epoch == receipt.provider_epoch
                    && previous != receipt
            })
        {
            return Err("retirement_mismatch");
        }
        if self.lease_receipts.values().any(|previous| {
            previous.service == scope.transport_service
                && previous.stream == scope.transport_stream
                && previous.provider_epoch == scope.transport_epoch
                && Some(previous) != receipt
        }) {
            return Err("retirement_mismatch");
        }
        let record = self
            .retired_sources
            .get_mut(source)
            .expect("validated retirement");
        record.receipt = receipt.cloned();
        record.complete = true;
        Ok(())
    }

    pub fn admit(
        &mut self,
        source: &Source,
        operation: &str,
        sequence: u64,
    ) -> Result<&str, &'static str> {
        self.admit_with_scope(source, operation, sequence, None)
    }

    pub fn admit_stream_lease(
        &mut self,
        source: &Source,
        operation: &str,
        sequence: u64,
        scope: StreamScope,
    ) -> Result<&str, &'static str> {
        if !scope.valid() || self.sources.get(source) != Some(&Some(scope.clone())) {
            return Err("stream_scope_denied");
        }
        if self
            .lease_admissions
            .get(source)
            .is_some_and(|id| id != operation)
        {
            return Err("stream_lease_in_use");
        }
        if self.lease_receipts.contains_key(source) {
            return Err("operation_complete");
        }
        if self
            .lease_receipts
            .values()
            .any(|receipt| receipt_matches_transport(receipt, &scope))
        {
            return Err("transport_retired");
        }
        self.admit_with_scope(source, operation, sequence, Some(scope))?;
        self.lease_admissions
            .entry(source.clone())
            .or_insert_with(|| operation.to_owned());
        Ok(&self
            .operations
            .get(operation)
            .expect("admitted lease")
            .ticket)
    }

    fn admit_with_scope(
        &mut self,
        source: &Source,
        operation: &str,
        sequence: u64,
        scope: Option<StreamScope>,
    ) -> Result<&str, &'static str> {
        if self.fenced || !self.sources.contains_key(source) || !token(operation) {
            return Err("admission_denied");
        }
        if let Some(prior) = self.operations.get(operation) {
            if &prior.source != source
                || prior.complete
                || prior.sequence != sequence
                || prior.scope != scope
            {
                return Err("operation_conflict");
            }
        } else {
            if sequence == 0 || sequence <= self.highwater {
                return Err("operation_conflict");
            }
            if self.operations.len() >= MAX_OPERATIONS {
                return Err("admission_capacity");
            }
            let mut bytes = [0_u8; 16];
            getrandom::fill(&mut bytes).map_err(|_| "entropy_unavailable")?;
            self.operations.insert(
                operation.into(),
                Admission {
                    source: source.clone(),
                    ticket: bytes.iter().map(|b| format!("{b:02x}")).collect(),
                    sequence,
                    complete: false,
                    scope,
                },
            );
            self.highwater = sequence;
        }
        Ok(&self
            .operations
            .get(operation)
            .expect("inserted admission")
            .ticket)
    }

    pub fn lookup(&self, operation: &str) -> Result<(&str, bool), &'static str> {
        let admission = self.operations.get(operation).ok_or("unknown_admission")?;
        if admission.complete {
            return Err("operation_complete");
        }
        if admission.scope.is_some() {
            return Err("lease_lookup_required");
        }
        Ok((
            &admission.ticket,
            self.fenced || self.retired_sources.contains_key(&admission.source),
        ))
    }

    pub fn lookup_stream_lease(
        &self,
        source: &Source,
        operation: &str,
        scope: &StreamScope,
    ) -> Result<(&str, bool), &'static str> {
        let admission = self.operations.get(operation).ok_or("unknown_admission")?;
        if admission.complete {
            return Err("operation_complete");
        }
        if &admission.source != source || admission.scope.as_ref() != Some(scope) {
            return Err("stream_scope_denied");
        }
        Ok((
            &admission.ticket,
            self.fenced || self.retired_sources.contains_key(source),
        ))
    }

    pub fn complete(
        &mut self,
        epoch: &str,
        operation: &str,
        ticket: &str,
    ) -> Result<(), &'static str> {
        if epoch != self.epoch {
            return Err("stale_epoch");
        }
        let admission = self
            .operations
            .get_mut(operation)
            .ok_or("unknown_admission")?;
        if admission.ticket != ticket || admission.scope.is_some() {
            return Err("unknown_admission");
        }
        admission.complete = true;
        Ok(())
    }

    /// The owner must first verify the matching broker receipt and stop its
    /// local producer. A payload is an owner assertion, not a transport proof.
    pub fn complete_stream_lease(
        &mut self,
        epoch: &str,
        operation: &str,
        ticket: &str,
        retirement: &StreamRetirement,
    ) -> Result<(), &'static str> {
        self.require_epoch(epoch)?;
        let admission = self
            .operations
            .get_mut(operation)
            .ok_or("unknown_admission")?;
        if admission.ticket != ticket
            || admission
                .scope
                .as_ref()
                .is_none_or(|scope| !retirement.matches(scope))
        {
            return Err("retirement_mismatch");
        }
        if self.lease_receipts.values().any(|previous| {
            previous.service == retirement.service
                && previous.stream == retirement.stream
                && previous.provider_epoch == retirement.provider_epoch
                && previous != retirement
        }) {
            return Err("retirement_mismatch");
        }
        self.lease_receipts
            .insert(admission.source.clone(), retirement.clone());
        admission.complete = true;
        Ok(())
    }

    /// The owner acknowledges the completion receipt; the monotonic sequence
    /// remains fenced even after the completed record is freed.
    pub fn acknowledge(
        &mut self,
        epoch: &str,
        operation: &str,
        ticket: &str,
    ) -> Result<(), &'static str> {
        if epoch != self.epoch {
            return Err("stale_epoch");
        }
        if let Some(admission) = self.operations.get(operation) {
            if admission.ticket != ticket || !admission.complete {
                return Err("unknown_admission");
            }
            self.operations.remove(operation);
        }
        Ok(())
    }

    /// Close new admissions synchronously before beginning a trust mutation.
    /// Existing effects remain counted until their owner explicitly completes.
    pub fn fence(&mut self) {
        if !self.fenced {
            self.drain_required = !self.sources.is_empty() || self.in_flight() != 0;
            self.fence_generation = self.fence_generation.saturating_add(1);
        }
        self.fenced = true;
        self.sources.clear();
    }
    pub fn fence_generation(&self) -> u64 {
        self.fence_generation
    }
    pub fn drain_required(&self) -> bool {
        self.drain_required
    }
    pub fn fenced(&self) -> bool {
        self.fenced
    }
    pub fn bound_sources(&self) -> usize {
        self.sources.len()
    }
    pub fn pending_source_retirements(&self) -> usize {
        self.retired_sources
            .values()
            .filter(|retirement| !retirement.complete)
            .count()
    }
    /// The selected effect owner must first retire its actual stream/transport
    /// queues, not just local API calls. Already delivered packets cannot be recalled.
    pub fn acknowledge_drain(&mut self, epoch: &str, generation: u64) -> Result<(), &'static str> {
        if epoch != self.epoch || !self.fenced || generation != self.fence_generation {
            return Err("stale_fence");
        }
        if self.in_flight() != 0 {
            return Err("effects_in_flight");
        }
        self.drain_required = false;
        Ok(())
    }
    pub fn in_flight(&self) -> usize {
        self.operations.values().filter(|a| !a.complete).count()
    }
    pub fn drained(&self) -> bool {
        self.fenced
            && self.in_flight() == 0
            && !self.drain_required
            && !self.startup_recovery_required
            && self
                .retired_sources
                .values()
                .all(|retirement| retirement.complete)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source() -> Source {
        Source {
            version: 1,
            id: "a".repeat(32),
            generation: 3,
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            stream_generation: 3,
            device: "device".into(),
            device_generation: 4,
            effect_owner_epoch: "owner".into(),
        }
    }
    #[test]
    fn fence_denies_new_work_and_waits_for_effect_completion() {
        let mut gate = EffectGate::new().unwrap();
        gate.recover("owner").unwrap();
        let source = source();
        gate.bind(source.clone()).unwrap();
        let ticket = gate.admit(&source, "operation", 1).unwrap().to_owned();
        assert_eq!(gate.admit(&source, "operation", 1).unwrap(), ticket);
        let epoch = gate.epoch().to_owned();
        gate.fence();
        assert_eq!(gate.admit(&source, "next", 2), Err("admission_denied"));
        assert!(!gate.drained());
        assert_eq!(
            gate.complete(&epoch, "operation", "wrong"),
            Err("unknown_admission")
        );
        gate.complete(&epoch, "operation", &ticket).unwrap();
        gate.complete(&epoch, "operation", &ticket).unwrap();
        assert!(!gate.drained());
        gate.acknowledge_drain(&epoch, gate.fence_generation())
            .unwrap();
        assert!(gate.drained());
    }
    #[test]
    fn transport_drain_waits_for_final_effect_and_rejects_stale_fences() {
        let mut gate = EffectGate::new().unwrap();
        gate.recover("owner").unwrap();
        let source = source();
        gate.bind(source.clone()).unwrap();
        let ticket = gate.admit(&source, "send", 1).unwrap().to_owned();
        let epoch = gate.epoch().to_owned();
        gate.fence();
        let generation = gate.fence_generation();
        assert_eq!(
            gate.acknowledge_drain(&epoch, generation),
            Err("effects_in_flight")
        );
        assert_eq!(
            gate.acknowledge_drain(&epoch, generation + 1),
            Err("stale_fence")
        );
        gate.complete(&epoch, "send", &ticket).unwrap();
        assert!(!gate.drained());
        gate.acknowledge_drain(&epoch, generation).unwrap();
        assert!(gate.drained());
    }

    #[test]
    fn completion_receipt_releases_bounded_space_without_allowing_sequence_replay() {
        let mut gate = EffectGate::new().unwrap();
        gate.recover("owner").unwrap();
        let source = source();
        gate.bind(source.clone()).unwrap();
        let epoch = gate.epoch().to_owned();
        let ticket = gate.admit(&source, "first", 1).unwrap().to_owned();
        assert_eq!(
            gate.acknowledge(&epoch, "first", &ticket),
            Err("unknown_admission")
        );
        gate.complete(&epoch, "first", &ticket).unwrap();
        gate.acknowledge(&epoch, "first", &ticket).unwrap();
        gate.acknowledge(&epoch, "first", &ticket).unwrap();
        assert_eq!(gate.admit(&source, "first", 1), Err("operation_conflict"));
        assert!(gate.admit(&source, "second", 2).is_ok());
    }

    fn stream_scope() -> StreamScope {
        StreamScope {
            capabilities: [amaterasu_module_sdk::effect_authority::FeedbackCapability {
                schema: "controller.speaker.pcm".into(),
                version: 1,
                direction: amaterasu_module_sdk::effect_authority::FeedbackDirection::ToClient,
            }]
            .into(),
            controller_slot: 3,
            transport_service: "amaterasu.control-transport/1".into(),
            transport_stream: 7,
            transport_epoch: "c".repeat(32),
        }
    }

    #[test]
    fn source_fence_blocks_late_binds_and_requires_exact_owner_retirement() {
        let mut gate = EffectGate::new().unwrap();
        gate.recover("owner").unwrap();
        let source = source();
        let scope = stream_scope();
        let epoch = gate.epoch().to_owned();
        gate.bind_scoped(source.clone(), Some(scope.clone()))
            .unwrap();
        let ticket = gate
            .admit_stream_lease(&source, "lease", 1, scope.clone())
            .unwrap()
            .to_owned();
        assert!(
            !gate
                .fence_source(&epoch, &source, &scope, "retire-source")
                .unwrap()
        );
        assert_eq!(
            gate.bind_scoped(source.clone(), Some(scope.clone())),
            Err("source_denied")
        );
        assert_eq!(
            gate.lookup_stream_lease(&source, "lease", &scope).unwrap(),
            (ticket.as_str(), true)
        );
        assert_eq!(
            gate.complete_source_retirement(&epoch, &source, &scope, "retire-source", None),
            Err("effects_in_flight")
        );
        let receipt = StreamRetirement {
            ok: true,
            service: scope.transport_service.clone(),
            stream: scope.transport_stream,
            provider_epoch: scope.transport_epoch.clone(),
            operation_id: "transport-retire".into(),
            scope: "whole_control_stream".into(),
            state: "retired".into(),
        };
        gate.complete_stream_lease(&epoch, "lease", &ticket, &receipt)
            .unwrap();
        assert_eq!(
            gate.complete_source_retirement(&epoch, &source, &scope, "retire-source", None),
            Err("lease_acknowledgment_pending")
        );
        gate.acknowledge(&epoch, "lease", &ticket).unwrap();
        assert_eq!(
            gate.complete_source_retirement(&epoch, &source, &scope, "retire-source", None),
            Err("retirement_mismatch")
        );
        let mut wrong = receipt.clone();
        wrong.provider_epoch = "d".repeat(32);
        assert_eq!(
            gate.complete_source_retirement(&epoch, &source, &scope, "retire-source", Some(&wrong)),
            Err("retirement_mismatch")
        );
        gate.complete_source_retirement(&epoch, &source, &scope, "retire-source", Some(&receipt))
            .unwrap();
        gate.complete_source_retirement(&epoch, &source, &scope, "retire-source", Some(&receipt))
            .unwrap();
        assert_eq!(
            gate.complete_source_retirement(&epoch, &source, &scope, "retire-source", Some(&wrong)),
            Err("source_retirement_conflict")
        );
        assert!(
            gate.lookup_source_retirement(&epoch, &source, &scope, "retire-source")
                .unwrap()
        );
        assert_eq!(
            gate.lookup_source_retirement(&epoch, &source, &scope, "other"),
            Err("source_retirement_conflict")
        );
    }

    #[test]
    fn source_retirement_leaves_other_sources_live_and_fences_unknown_late_bind() {
        let mut gate = EffectGate::new().unwrap();
        gate.recover("owner").unwrap();
        let first = source();
        let mut second = first.clone();
        second.id = "b".repeat(32);
        second.stream_id = "other".into();
        let scope = stream_scope();
        let mut other_scope = scope.clone();
        other_scope.transport_stream += 1;
        let epoch = gate.epoch().to_owned();
        gate.bind_scoped(second.clone(), Some(other_scope.clone()))
            .unwrap();
        assert!(
            !gate
                .fence_source(&epoch, &first, &scope, "unknown-source")
                .unwrap()
        );
        assert_eq!(
            gate.bind_scoped(first.clone(), Some(scope.clone())),
            Err("source_denied")
        );
        assert!(
            gate.admit_stream_lease(&second, "other-lease", 1, other_scope)
                .is_ok()
        );
        gate.complete_source_retirement(&epoch, &first, &scope, "unknown-source", None)
            .unwrap();
        assert_eq!(gate.bind_scoped(first, Some(scope)), Err("source_denied"));
    }

    #[test]
    fn completed_sources_do_not_consume_live_slots_and_restart_fences_old_epoch() {
        let mut gate = EffectGate::new().unwrap();
        gate.recover("owner").unwrap();
        let scope = stream_scope();
        let epoch = gate.epoch().to_owned();
        for index in 0..80 {
            let mut source = source();
            source.id = format!("{index:032x}");
            let mut source_scope = scope.clone();
            source_scope.transport_stream = index + 1;
            gate.bind_scoped(source.clone(), Some(source_scope.clone()))
                .unwrap();
            let operation = format!("retire-{index}");
            gate.fence_source(&epoch, &source, &source_scope, &operation)
                .unwrap();
            gate.complete_source_retirement(&epoch, &source, &source_scope, &operation, None)
                .unwrap();
        }
        let mut pending = source();
        pending.id = "f".repeat(32);
        gate.fence_source(&epoch, &pending, &scope, "pending")
            .unwrap();
        assert_eq!(gate.recover("next-owner"), Err("recovery_required"));
        gate.complete_source_retirement(&epoch, &pending, &scope, "pending", None)
            .unwrap();
        gate.fence();
        assert!(gate.drained());
        gate.recover("next-owner").unwrap();
        assert_eq!(
            gate.lookup_source_retirement(&epoch, &pending, &scope, "pending"),
            Err("stale_epoch")
        );
        let replacement = EffectGate::new().unwrap();
        assert!(replacement.startup_recovery_required());
        assert_eq!(
            replacement.lookup_source_retirement(&epoch, &pending, &scope, "pending"),
            Err("stale_epoch")
        );
        assert!(!replacement.drained());
    }

    #[test]
    fn shared_transport_retirement_waits_for_all_sources_and_blocks_reuse() {
        let mut gate = EffectGate::new().unwrap();
        gate.recover("owner").unwrap();
        let first = source();
        let mut second = first.clone();
        second.id = "b".repeat(32);
        second.device = "other".into();
        let scope = stream_scope();
        let mut other_scope = scope.clone();
        other_scope.controller_slot += 1;
        let epoch = gate.epoch().to_owned();
        gate.bind_scoped(first.clone(), Some(scope.clone()))
            .unwrap();
        gate.bind_scoped(second.clone(), Some(other_scope.clone()))
            .unwrap();
        let first_ticket = gate
            .admit_stream_lease(&first, "one", 1, scope.clone())
            .unwrap()
            .to_owned();
        let second_ticket = gate
            .admit_stream_lease(&second, "two", 2, other_scope.clone())
            .unwrap()
            .to_owned();
        gate.fence_source(&epoch, &first, &scope, "retire-one")
            .unwrap();
        let receipt = StreamRetirement {
            ok: true,
            service: scope.transport_service.clone(),
            stream: scope.transport_stream,
            provider_epoch: scope.transport_epoch.clone(),
            operation_id: "transport-retire".into(),
            scope: "whole_control_stream".into(),
            state: "retired".into(),
        };
        assert_eq!(
            gate.complete_source_retirement(&epoch, &first, &scope, "retire-one", Some(&receipt)),
            Err("shared_transport_active")
        );
        gate.fence_source(&epoch, &second, &other_scope, "retire-two")
            .unwrap();
        assert_eq!(
            gate.complete_source_retirement(&epoch, &first, &scope, "retire-one", Some(&receipt)),
            Err("effects_in_flight")
        );
        gate.complete_stream_lease(&epoch, "one", &first_ticket, &receipt)
            .unwrap();
        let mut conflicting = receipt.clone();
        conflicting.operation_id = "different-transport-receipt".into();
        assert_eq!(
            gate.complete_stream_lease(&epoch, "two", &second_ticket, &conflicting),
            Err("retirement_mismatch")
        );
        assert_eq!(gate.in_flight(), 1);
        gate.complete_stream_lease(&epoch, "two", &second_ticket, &receipt)
            .unwrap();
        gate.acknowledge(&epoch, "one", &first_ticket).unwrap();
        assert_eq!(
            gate.complete_source_retirement(&epoch, &first, &scope, "retire-one", Some(&receipt)),
            Err("lease_acknowledgment_pending")
        );
        gate.acknowledge(&epoch, "two", &second_ticket).unwrap();
        gate.complete_source_retirement(&epoch, &first, &scope, "retire-one", Some(&receipt))
            .unwrap();
        gate.complete_source_retirement(
            &epoch,
            &second,
            &other_scope,
            "retire-two",
            Some(&receipt),
        )
        .unwrap();
        let mut late = first.clone();
        late.id = "c".repeat(32);
        late.device_generation += 1;
        assert_eq!(
            gate.bind_scoped(late, Some(scope)),
            Err("transport_retired")
        );
    }

    #[test]
    fn replacement_controller_waits_for_previous_slot_retirement() {
        let mut gate = EffectGate::new().unwrap();
        gate.recover("owner").unwrap();
        let original = source();
        let mut replacement = original.clone();
        replacement.id = "c".repeat(32);
        replacement.device_generation += 1;
        replacement.stream_id = "next-stream".into();
        replacement.stream_generation += 1;
        replacement.generation += 1;
        let scope = stream_scope();
        let mut replacement_scope = scope.clone();
        replacement_scope.transport_stream += 1;
        let epoch = gate.epoch().to_owned();
        gate.bind_scoped(original.clone(), Some(scope.clone()))
            .unwrap();
        assert_eq!(
            gate.bind_scoped(replacement.clone(), Some(replacement_scope.clone())),
            Err("controller_slot_in_use")
        );
        gate.fence_source(&epoch, &original, &scope, "old-slot")
            .unwrap();
        assert_eq!(
            gate.bind_scoped(replacement.clone(), Some(replacement_scope.clone())),
            Err("controller_retirement_pending")
        );
        gate.complete_source_retirement(&epoch, &original, &scope, "old-slot", None)
            .unwrap();
        gate.bind_scoped(replacement, Some(replacement_scope))
            .unwrap();
    }

    #[test]
    fn retirement_tombstones_are_bounded_and_reclaimed_only_by_epoch_rotation() {
        let mut gate = EffectGate::new().unwrap();
        gate.recover("owner").unwrap();
        let epoch = gate.epoch().to_owned();
        let scope = stream_scope();
        for index in 0..MAX_RETIRED_SOURCES {
            let mut item = source();
            item.id = format!("{index:032x}");
            let operation = format!("retire-{index}");
            gate.fence_source(&epoch, &item, &scope, &operation)
                .unwrap();
            gate.complete_source_retirement(&epoch, &item, &scope, &operation, None)
                .unwrap();
        }
        let mut next = source();
        next.id = format!("{:032x}", MAX_RETIRED_SOURCES);
        assert_eq!(
            gate.fence_source(&epoch, &next, &scope, "overflow"),
            Err("source_retirement_capacity")
        );
        gate.fence();
        assert!(gate.drained());
        gate.recover("owner-next").unwrap();
        next.effect_owner_epoch = "owner-next".into();
        let new_epoch = gate.epoch().to_owned();
        assert!(
            !gate
                .fence_source(&new_epoch, &next, &scope, "overflow")
                .unwrap()
        );
    }

    #[test]
    fn stream_lease_stays_bounded_and_fence_waits_for_exact_transport_retirement() {
        let mut gate = EffectGate::new().unwrap();
        gate.recover("owner").unwrap();
        let source = source();
        let scope = stream_scope();
        gate.bind_scoped(source.clone(), Some(scope.clone()))
            .unwrap();
        let ticket = gate
            .admit_stream_lease(&source, "lease", 1, scope.clone())
            .unwrap()
            .to_owned();
        for _ in 0..1000 {
            assert_eq!(
                gate.admit_stream_lease(&source, "lease", 1, scope.clone())
                    .unwrap(),
                ticket
            );
        }
        assert_eq!(gate.in_flight(), 1);
        assert_eq!(gate.lookup("lease"), Err("lease_lookup_required"));
        assert_eq!(
            gate.lookup_stream_lease(&source, "lease", &scope).unwrap(),
            (ticket.as_str(), false)
        );
        assert_eq!(
            gate.admit_stream_lease(&source, "other", 2, scope.clone()),
            Err("stream_lease_in_use")
        );
        let mut wrong_session = source.clone();
        wrong_session.session_id = "other-session".into();
        assert_eq!(
            gate.admit_stream_lease(&wrong_session, "other", 2, scope.clone()),
            Err("stream_scope_denied")
        );
        let mut wrong_owner = source.clone();
        wrong_owner.effect_owner_epoch = "other-owner".into();
        assert_eq!(
            gate.admit_stream_lease(&wrong_owner, "other", 2, scope.clone()),
            Err("stream_scope_denied")
        );
        let mut wrong_source = source.clone();
        wrong_source.device_generation += 1;
        assert_eq!(
            gate.admit_stream_lease(&wrong_source, "other", 2, scope.clone()),
            Err("stream_scope_denied")
        );
        let mut wrong_scope = scope.clone();
        wrong_scope.controller_slot += 1;
        assert_eq!(
            gate.admit_stream_lease(&source, "lease", 1, wrong_scope),
            Err("stream_scope_denied")
        );
        let epoch = gate.epoch().to_owned();
        gate.fence();
        assert_eq!(
            gate.lookup_stream_lease(&source, "lease", &scope).unwrap(),
            (ticket.as_str(), true)
        );
        assert!(
            gate.admit_stream_lease(&source, "late", 2, scope.clone())
                .is_err()
        );
        assert_eq!(
            gate.complete(&epoch, "lease", &ticket),
            Err("unknown_admission")
        );
        assert_eq!(
            gate.acknowledge_drain(&epoch, gate.fence_generation()),
            Err("effects_in_flight")
        );
        let mut retirement = StreamRetirement {
            ok: true,
            service: scope.transport_service.clone(),
            stream: 8,
            provider_epoch: scope.transport_epoch.clone(),
            operation_id: "retire".into(),
            scope: "whole_control_stream".into(),
            state: "retired".into(),
        };
        assert_eq!(
            gate.complete_stream_lease(&epoch, "lease", &ticket, &retirement),
            Err("retirement_mismatch")
        );
        retirement.stream = scope.transport_stream;
        assert_eq!(
            gate.complete_stream_lease("stale-epoch", "lease", &ticket, &retirement),
            Err("stale_epoch")
        );
        gate.complete_stream_lease(&epoch, "lease", &ticket, &retirement)
            .unwrap();
        assert!(!gate.drained());
        gate.acknowledge_drain(&epoch, gate.fence_generation())
            .unwrap();
        assert!(gate.drained());
    }

    #[test]
    fn a_retired_lease_cannot_be_replaced_on_the_same_source_generation() {
        let mut gate = EffectGate::new().unwrap();
        gate.recover("owner").unwrap();
        let source = source();
        let scope = stream_scope();
        gate.bind_scoped(source.clone(), Some(scope.clone()))
            .unwrap();
        let ticket = gate
            .admit_stream_lease(&source, "first", 1, scope.clone())
            .unwrap()
            .to_owned();
        let epoch = gate.epoch().to_owned();
        let retirement = StreamRetirement {
            ok: true,
            service: scope.transport_service.clone(),
            stream: scope.transport_stream,
            provider_epoch: scope.transport_epoch.clone(),
            operation_id: "retire".into(),
            scope: "whole_control_stream".into(),
            state: "retired".into(),
        };
        gate.complete_stream_lease(&epoch, "first", &ticket, &retirement)
            .unwrap();
        gate.acknowledge(&epoch, "first", &ticket).unwrap();
        assert_eq!(
            gate.admit_stream_lease(&source, "first", 2, scope.clone()),
            Err("operation_complete")
        );
        assert_eq!(gate.in_flight(), 0);
        assert_eq!(
            gate.admit_stream_lease(&source, "second", 2, scope),
            Err("stream_lease_in_use")
        );
    }

    #[test]
    fn restart_and_recovery_cannot_reuse_old_ticket_or_owner_epoch() {
        let mut old = EffectGate::new().unwrap();
        old.recover("owner").unwrap();
        let source = source();
        old.bind(source.clone()).unwrap();
        let ticket = old.admit(&source, "op", 1).unwrap().to_owned();
        let mut replacement = EffectGate::new().unwrap();
        assert_eq!(replacement.bind(source.clone()), Err("source_denied"));
        assert_eq!(
            replacement.complete(old.epoch(), "op", &ticket),
            Err("stale_epoch")
        );
        replacement.recover("new-owner").unwrap();
        assert_eq!(replacement.bind(source), Err("source_denied"));
    }
}
