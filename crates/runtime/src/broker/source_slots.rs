//! Fixed broker-owned anti-replay slots for source lifetimes.
//! Pairing must durably reserve and recover the per-slot watermarks.
use amaterasu_contracts::{
    Response,
    effect_fence::{EffectScope, SOURCE_SLOT_COUNT, SourceSlotLease},
};
use serde_json::json;
use std::collections::BTreeSet;

const SLOT_COUNT: usize = SOURCE_SLOT_COUNT;

struct Slot {
    retired: u64,
    token: String,
    active: Option<EffectScope>,
}

pub(super) struct SourceSlots {
    epoch: String,
    initial: Option<Vec<u64>>,
    required: BTreeSet<String>,
    retired: BTreeSet<String>,
    slots: Vec<Slot>,
}

impl SourceSlots {
    pub fn new(required: BTreeSet<String>) -> Result<Self, String> {
        Ok(Self {
            epoch: random_token()?,
            initial: None,
            required,
            retired: BTreeSet::new(),
            slots: Vec::new(),
        })
    }
    pub fn confirm_retired(&mut self, caller: &str, epoch: &str) -> Response {
        if epoch != self.epoch {
            return stale();
        }
        if !self.required.contains(caller) {
            return Response::error("permission_denied", "recovery participant is not selected");
        }
        self.retired.insert(caller.to_owned());
        Response::Ok
    }
    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// The selected authority verifies durable state and prior worker retirement.
    /// A request from a previous broker epoch cannot initialize this broker.
    pub fn recover(&mut self, epoch: &str, retired: Vec<u64>) -> Response {
        if epoch != self.epoch {
            return stale();
        }
        if retired.len() != SLOT_COUNT {
            return invalid();
        }
        if !self.required.is_subset(&self.retired) {
            return Response::error(
                "recovery_required",
                "old worker retirement is not confirmed",
            );
        }
        if let Some(initial) = &self.initial {
            return if *initial == retired {
                Response::Ok
            } else {
                conflict()
            };
        }
        let mut slots = Vec::with_capacity(SLOT_COUNT);
        for &floor in &retired {
            let Ok(token) = random_token() else {
                return unavailable();
            };
            slots.push(Slot {
                retired: floor,
                token,
                active: None,
            });
        }
        self.slots = slots;
        self.initial = Some(retired);
        Response::Ok
    }

    pub fn status(&self, index: u8) -> Response {
        if usize::from(index) >= SLOT_COUNT {
            return invalid();
        }
        let Some(slot) = self.slots.get(usize::from(index)) else {
            return Response::Result {
                payload: json!({"broker_epoch":self.epoch,"ready":false,"pending_retirements":self.required.difference(&self.retired).collect::<Vec<_>>()}),
            };
        };
        let Some(incarnation) = slot.retired.checked_add(1) else {
            return unavailable();
        };
        let lease = SourceSlotLease {
            slot: index,
            incarnation,
            broker_epoch: self.epoch.clone(),
            token: slot.token.clone(),
        };
        Response::Result {
            payload: json!({"broker_epoch":self.epoch,"ready":true,"lease":lease,"active":slot.active}),
        }
    }

    pub fn valid_lease(&self, lease: &SourceSlotLease) -> bool {
        lease.broker_epoch == self.epoch
            && self.slots.get(usize::from(lease.slot)).is_some_and(|slot| {
                slot.retired.checked_add(1) == Some(lease.incarnation) && slot.token == lease.token
            })
    }
    pub fn valid_for(&self, lease: &SourceSlotLease, scope: &EffectScope) -> bool {
        self.valid_lease(lease)
            && slot_for(scope) == Some(lease.slot)
            && self.slots[usize::from(lease.slot)].active.as_ref() == Some(scope)
    }
    pub fn lease_for(&self, scope: &EffectScope) -> Option<SourceSlotLease> {
        let index = slot_for(scope)?;
        let slot = self.slots.get(usize::from(index))?;
        if slot.active.as_ref() != Some(scope) {
            return None;
        }
        Some(SourceSlotLease {
            slot: index,
            incarnation: slot.retired.checked_add(1)?,
            broker_epoch: self.epoch.clone(),
            token: slot.token.clone(),
        })
    }
    pub fn can_open(&self, lease: &SourceSlotLease, scope: &EffectScope) -> Result<bool, Response> {
        if !self.valid_lease(lease) || slot_for(scope) != Some(lease.slot) {
            return Err(stale());
        }
        match &self.slots[usize::from(lease.slot)].active {
            None => Ok(true),
            Some(active) if active == scope => Ok(false),
            Some(_) => Err(conflict()),
        }
    }
    pub fn opened(&mut self, lease: &SourceSlotLease, scope: EffectScope) {
        self.slots[usize::from(lease.slot)].active = Some(scope);
    }
    /// Obtain entropy before changing state so entropy failure leaves the old
    /// source closed and the slot unavailable rather than reusing its token.
    pub fn close(&mut self, lease: &SourceSlotLease, scope: &EffectScope) -> Result<(), Response> {
        if !self.valid_for(lease, scope) {
            return Err(stale());
        }
        let next = random_token().map_err(|_| unavailable())?;
        let slot = &mut self.slots[usize::from(lease.slot)];
        slot.retired = lease.incarnation;
        slot.token = next;
        slot.active = None;
        Ok(())
    }
}

/// Lifecycle source IDs are authority-authenticated random 128-bit hex IDs.
/// A stable slot prevents a delayed old open being replayed on another slot.
/// Collisions apply backpressure; no existing source is evicted.
fn slot_for(scope: &EffectScope) -> Option<u8> {
    scope.source.slot()
}
fn random_token() -> Result<String, String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| "effect slot entropy unavailable".to_string())?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
fn invalid() -> Response {
    Response::error("invalid_request", "invalid source slot request")
}
fn stale() -> Response {
    Response::error("stale_source_slot", "source slot incarnation is stale")
}
fn conflict() -> Response {
    Response::error(
        "effect_conflict",
        "source slot is already occupied or recovery differs",
    )
}
fn unavailable() -> Response {
    Response::error("unavailable", "source slot token unavailable")
}
