use crate::model::{
    Event, FixtureError, FixtureResult, HeldInput, MAX_BATCH_EVENTS, Operation, RequestedPolicy,
    lease_duration, validate_operation_id,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

const BACKEND: &str = "synthetic-input-probe";
const MAX_LIFETIME_LEASES: u64 = 128;
const MAX_OPERATION_RECORDS: usize = 128;
const MAX_RECORDED_EVENTS: usize = 256;
const MAX_HELD_INPUTS: usize = 128;

struct Lease {
    owner: String,
    token: String,
    operation_id: Option<String>,
    acquire_ttl_ms: u64,
    requested_policy: RequestedPolicy,
    expires: Instant,
    held: Vec<HeldInput>,
}

#[derive(Clone, Copy)]
enum RecordState {
    Active,
    Terminal(TerminalReason),
}

#[derive(Clone, Copy)]
enum TerminalReason {
    Released,
    Expired,
    Cancelled,
}

impl TerminalReason {
    fn name(self) -> &'static str {
        match self {
            Self::Released => "released",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
        }
    }
}

struct OperationRecord {
    requested_ttl_ms: u64,
    requested_policy: RequestedPolicy,
    state: RecordState,
}

pub struct Controller {
    enabled: bool,
    lease: Option<Lease>,
    operations: BTreeMap<(String, String), OperationRecord>,
    issued_tokens: BTreeSet<String>,
    events: VecDeque<Event>,
    events_truncated: bool,
    acquired: u64,
    renewed: u64,
    released: u64,
    expired: u64,
}

impl Controller {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            lease: None,
            operations: BTreeMap::new(),
            issued_tokens: BTreeSet::new(),
            events: VecDeque::new(),
            events_truncated: false,
            acquired: 0,
            renewed: 0,
            released: 0,
            expired: 0,
        }
    }

    pub fn handle(&mut self, owner: &str, operation: Operation, now: Instant) -> Value {
        self.expire(now);
        let result = match operation {
            Operation::Status => Ok(self.status(owner, now)),
            Operation::Acquire {
                operation_id,
                ttl_ms,
                back_button_timeout,
                controller,
                gamepad,
                ds4_back_as_touchpad_click,
                native_pen_touch,
                keyboard,
                mouse,
                high_resolution_scrolling,
                always_send_scancodes,
                key_rightalt_to_key_win,
                key_repeat_delay,
                key_repeat_frequency,
            } => self.acquire(
                owner,
                operation_id,
                ttl_ms,
                RequestedPolicy {
                    back_button_timeout,
                    controller,
                    gamepad,
                    ds4_back_as_touchpad_click,
                    native_pen_touch,
                    keyboard,
                    mouse,
                    high_resolution_scrolling,
                    always_send_scancodes,
                    key_rightalt_to_key_win,
                    key_repeat_delay,
                    key_repeat_frequency,
                },
                now,
            ),
            Operation::Lookup { operation_id } => self.lookup(owner, &operation_id, now),
            Operation::Renew { lease, ttl_ms } => self.renew(owner, &lease, ttl_ms, now),
            Operation::Batch { lease, events } => self.batch(owner, &lease, events),
            Operation::Release { lease } => self.release(owner, &lease),
        };
        result.unwrap_or_else(FixtureError::payload)
    }

    pub fn metrics(&mut self, now: Instant) -> Value {
        self.expire(now);
        let (held_keys, held_buttons) = self
            .lease
            .as_ref()
            .map(|lease| {
                lease
                    .held
                    .iter()
                    .fold((0usize, 0usize), |(keys, buttons), held| match held {
                        HeldInput::Key(_) => (keys + 1, buttons),
                        HeldInput::Button(_) => (keys, buttons + 1),
                    })
            })
            .unwrap_or_default();
        json!({
            "ok": true,
            "fixture": true,
            "enabled": self.enabled,
            "active": self.lease.is_some(),
            "acquired": self.acquired,
            "renewed": self.renewed,
            "released": self.released,
            "expired": self.expired,
            "events": self.events.iter().cloned().collect::<Vec<_>>(),
            "events_truncated": self.events_truncated,
            "held_keys": held_keys,
            "held_buttons": held_buttons,
            "requested_policy": self.lease.as_ref().map(|lease| &lease.requested_policy),
        })
    }

    pub fn expire(&mut self, now: Instant) {
        if self
            .lease
            .as_ref()
            .is_some_and(|lease| now >= lease.expires)
        {
            self.finish(TerminalReason::Expired);
        }
    }

    pub fn cancel_owner(&mut self, owner: &str) {
        if self
            .lease
            .as_ref()
            .is_some_and(|lease| lease.owner == owner)
        {
            self.finish(TerminalReason::Cancelled);
        }
    }

    fn acquire(
        &mut self,
        owner: &str,
        operation_id: Option<String>,
        ttl_ms: Option<u64>,
        requested_policy: RequestedPolicy,
        now: Instant,
    ) -> FixtureResult<Value> {
        requested_policy.validate()?;
        if !self.enabled {
            return Err(FixtureError::new(
                "injection_disabled",
                "synthetic input requires enabled startup policy",
            ));
        }
        let duration = lease_duration(ttl_ms)?;
        if let Some(operation_id) = operation_id.as_deref() {
            validate_operation_id(operation_id)?;
            let key = (owner.to_owned(), operation_id.to_owned());
            if let Some(record) = self.operations.get(&key) {
                if record.requested_ttl_ms != duration
                    || record.requested_policy != requested_policy
                {
                    return Err(FixtureError::new(
                        "operation_conflict",
                        "operation_id was already used with a different lease duration or input policy",
                    ));
                }
                return self.operation_response(owner, operation_id, now);
            }
        }
        if self.lease.is_some() {
            return Err(FixtureError::new(
                "input_busy",
                "another synthetic input lease is active",
            ));
        }
        if self.acquired >= MAX_LIFETIME_LEASES {
            return Err(FixtureError::new(
                "capacity_exceeded",
                "synthetic input lease history is full",
            ));
        }
        if operation_id.is_some() && self.operations.len() >= MAX_OPERATION_RECORDS {
            return Err(FixtureError::new(
                "capacity_exceeded",
                "synthetic input operation history is full",
            ));
        }
        let token = self.fresh_token()?;
        self.issued_tokens.insert(token.clone());
        self.lease = Some(Lease {
            owner: owner.to_owned(),
            token: token.clone(),
            operation_id: operation_id.clone(),
            acquire_ttl_ms: duration,
            requested_policy: requested_policy.clone(),
            expires: now + Duration::from_millis(duration),
            held: Vec::new(),
        });
        if let Some(operation_id) = &operation_id {
            self.operations.insert(
                (owner.to_owned(), operation_id.clone()),
                OperationRecord {
                    requested_ttl_ms: duration,
                    requested_policy,
                    state: RecordState::Active,
                },
            );
        }
        self.acquired = self.acquired.saturating_add(1);
        Ok(active_response(
            operation_id.as_deref(),
            &token,
            duration,
            duration,
        ))
    }

    fn lookup(&self, owner: &str, operation_id: &str, now: Instant) -> FixtureResult<Value> {
        validate_operation_id(operation_id)?;
        if !self
            .operations
            .contains_key(&(owner.to_owned(), operation_id.to_owned()))
        {
            return Err(FixtureError::new(
                "not_found",
                "synthetic input operation was not found",
            ));
        }
        self.operation_response(owner, operation_id, now)
    }

    fn operation_response(
        &self,
        owner: &str,
        operation_id: &str,
        now: Instant,
    ) -> FixtureResult<Value> {
        let record = self
            .operations
            .get(&(owner.to_owned(), operation_id.to_owned()))
            .ok_or_else(|| FixtureError::new("not_found", "input operation was not found"))?;
        match record.state {
            RecordState::Terminal(reason) => Ok(json!({
                "ok": true,
                "state": "released",
                "operation_id": operation_id,
                "lease": null,
                "reason": reason.name(),
            })),
            RecordState::Active => {
                let lease = self
                    .lease
                    .as_ref()
                    .filter(|lease| {
                        lease.owner == owner && lease.operation_id.as_deref() == Some(operation_id)
                    })
                    .ok_or_else(|| {
                        FixtureError::new("fixture_state", "active input operation lost its lease")
                    })?;
                Ok(active_response(
                    Some(operation_id),
                    &lease.token,
                    lease.acquire_ttl_ms,
                    remaining_ms(lease.expires, now),
                ))
            }
        }
    }

    fn renew(
        &mut self,
        owner: &str,
        token: &str,
        ttl_ms: Option<u64>,
        now: Instant,
    ) -> FixtureResult<Value> {
        self.authorize(owner, token)?;
        let duration = lease_duration(ttl_ms)?;
        self.lease.as_mut().expect("authorized lease").expires =
            now + Duration::from_millis(duration);
        self.renewed = self.renewed.saturating_add(1);
        Ok(json!({
            "ok": true,
            "lease": token,
            "ttl_ms": duration,
            "expires_in_ms": duration,
        }))
    }

    fn batch(&mut self, owner: &str, token: &str, events: Vec<Event>) -> FixtureResult<Value> {
        self.authorize(owner, token)?;
        if events.is_empty() || events.len() > MAX_BATCH_EVENTS {
            return Err(FixtureError::new(
                "invalid_request",
                "input batch must contain 1..64 events",
            ));
        }
        let mut held = self.lease.as_ref().expect("authorized lease").held.clone();
        for event in &events {
            event.validate()?;
            if let Some(key) = event.repeated_key()
                && !held.contains(&HeldInput::Key(key.clone()))
            {
                return Err(FixtureError::new(
                    "invalid_sequence",
                    "repeat requires a held key owned by this lease",
                ));
            }
            if let Some((input, pressed)) = event.transition() {
                if held.contains(&input) == pressed {
                    return Err(FixtureError::new(
                        "invalid_sequence",
                        "duplicate press or release of an unowned input",
                    ));
                }
                if pressed {
                    held.push(input);
                } else {
                    held.retain(|existing| existing != &input);
                }
                if held.len() > MAX_HELD_INPUTS {
                    return Err(FixtureError::new(
                        "resource_exhausted",
                        "at most 128 keys and buttons may be held",
                    ));
                }
            }
        }
        self.lease.as_mut().expect("authorized lease").held = held;
        for event in events.iter().cloned() {
            if self.events.len() == MAX_RECORDED_EVENTS {
                self.events.pop_front();
                self.events_truncated = true;
            }
            self.events.push_back(event);
        }
        Ok(json!({"ok": true, "accepted": events.len(), "lease": token}))
    }

    fn release(&mut self, owner: &str, token: &str) -> FixtureResult<Value> {
        self.authorize(owner, token)?;
        self.finish(TerminalReason::Released);
        Ok(json!({"ok": true, "released": true}))
    }

    fn authorize(&self, owner: &str, token: &str) -> FixtureResult<()> {
        if token.len() != 32 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(FixtureError::new(
                "invalid_request",
                "lease must be a 32-character hexadecimal token",
            ));
        }
        let lease = self
            .lease
            .as_ref()
            .ok_or_else(|| FixtureError::new("not_found", "no synthetic input lease exists"))?;
        if lease.owner != owner || lease.token != token {
            return Err(FixtureError::new(
                "permission_denied",
                "input lease belongs to a different caller or generation",
            ));
        }
        Ok(())
    }

    fn finish(&mut self, reason: TerminalReason) {
        let Some(lease) = self.lease.take() else {
            return;
        };
        if let Some(operation_id) = lease.operation_id
            && let Some(record) = self.operations.get_mut(&(lease.owner, operation_id))
        {
            record.state = RecordState::Terminal(reason);
        }
        match reason {
            TerminalReason::Released => self.released = self.released.saturating_add(1),
            TerminalReason::Expired => self.expired = self.expired.saturating_add(1),
            TerminalReason::Cancelled => {}
        }
    }

    fn fresh_token(&self) -> FixtureResult<String> {
        for _ in 0..8 {
            let mut bytes = [0u8; 16];
            getrandom::fill(&mut bytes)
                .map_err(|error| FixtureError::new("entropy_error", error.to_string()))?;
            let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
            if !self.issued_tokens.contains(&token) {
                return Ok(token);
            }
        }
        Err(FixtureError::new(
            "identifier_exhausted",
            "could not allocate a fresh synthetic input lease",
        ))
    }

    fn status(&self, owner: &str, now: Instant) -> Value {
        let lease = self.lease.as_ref().map(|lease| {
            json!({
                "owned_by_caller": lease.owner == owner,
                "token": (lease.owner == owner).then_some(&lease.token),
                "operation_id": if lease.owner == owner {
                    lease.operation_id.as_deref()
                } else {
                    None
                },
                "expires_in_ms": remaining_ms(lease.expires, now),
                "pressed_count": lease.held.len(),
            })
        });
        json!({
            "ok": true,
            "enabled": self.enabled,
            "backend": BACKEND,
            "native_supported": false,
            "key_encoding": "moonlight_vk",
            "absolute_pointer": true,
            "lease": lease,
        })
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        self.finish(TerminalReason::Cancelled);
    }
}

fn active_response(
    operation_id: Option<&str>,
    token: &str,
    ttl_ms: u64,
    expires_in_ms: u64,
) -> Value {
    json!({
        "ok": true,
        "state": "active",
        "operation_id": operation_id,
        "lease": token,
        "ttl_ms": ttl_ms,
        "expires_in_ms": expires_in_ms,
        "backend": BACKEND,
        "cleanup_required": false,
        "absolute_pointer": true,
    })
}

fn remaining_ms(expires: Instant, now: Instant) -> u64 {
    expires
        .saturating_duration_since(now)
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests;
