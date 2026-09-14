use crate::backend::Backend;
use crate::model::{
    Event, InputError, InputResult, KeyEncoding, KeyState, MAX_EVENTS, MAX_PRESSED, Operation,
    Pressed, lease_duration, validate_operation_id,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

const MAX_ACQUIRE_OPERATIONS: usize = 64;
const OPERATION_RETENTION: Duration = Duration::from_secs(300);

struct Lease {
    owner: String,
    token: String,
    expires: Instant,
    cleanup_required: bool,
    operation_id: Option<String>,
    requested_ttl_ms: Option<u64>,
    termination_reason: Option<&'static str>,
    key_encoding: Option<KeyEncoding>,
}

struct TerminalAcquire {
    owner: String,
    operation_id: String,
    requested_ttl_ms: Option<u64>,
    reason: &'static str,
    retained_until: Instant,
}
pub struct Controller<B: Backend> {
    backend: B,
    enabled: bool,
    lease: Option<Lease>,
    // Preserve press order so cleanup releases chords in reverse order.
    pressed: Vec<Pressed>,
    retry_at: Instant,
    terminal_acquires: VecDeque<TerminalAcquire>,
}
impl<B: Backend> Controller<B> {
    pub fn new(backend: B, enabled: bool) -> Self {
        Self {
            backend,
            enabled,
            lease: None,
            pressed: Vec::new(),
            retry_at: Instant::now(),
            terminal_acquires: VecDeque::new(),
        }
    }
    pub fn handle(
        &mut self,
        caller: &str,
        operation: Operation,
        now: Instant,
        guard: &mut impl FnMut() -> InputResult<()>,
    ) -> Value {
        self.expire(now);
        let result = match operation {
            Operation::Status => Ok(self.status(caller, now)),
            Operation::Acquire {
                operation_id,
                ttl_ms,
            } => self.acquire(caller, operation_id, ttl_ms, now, guard),
            Operation::Lookup { operation_id } => self.lookup(caller, &operation_id, now),
            Operation::Renew { lease, ttl_ms } => self.renew(caller, &lease, ttl_ms, now, guard),
            Operation::Batch { lease, events } => return self.batch(caller, &lease, events, guard),
            Operation::Release { lease } => self.authorize(caller, &lease, true).and_then(|()| {
                self.finish_lease(now, "released")?;
                Ok(json!({"ok":true,"released":true}))
            }),
        };
        result.unwrap_or_else(|error| self.failure(error))
    }
    pub fn expire(&mut self, now: Instant) {
        self.terminal_acquires
            .retain(|operation| now < operation.retained_until);
        let due = self
            .lease
            .as_ref()
            .is_some_and(|lease| lease.cleanup_required || now >= lease.expires);
        if due && now >= self.retry_at {
            let reason = self
                .lease
                .as_ref()
                .and_then(|lease| lease.termination_reason)
                .unwrap_or("expired");
            let _ = self.finish_lease(now, reason);
        }
    }
    pub fn cancel_owner(&mut self, caller: &str) {
        if self
            .lease
            .as_ref()
            .is_some_and(|lease| lease.owner == caller)
        {
            let _ = self.finish_lease(Instant::now(), "cancelled");
        }
    }
    fn acquire(
        &mut self,
        caller: &str,
        operation_id: Option<String>,
        ttl_ms: Option<u64>,
        now: Instant,
        guard: &mut impl FnMut() -> InputResult<()>,
    ) -> InputResult<Value> {
        if let Some(operation_id) = operation_id.as_deref() {
            validate_operation_id(operation_id)?;
            if let Some(lease) = self.lease.as_ref().filter(|lease| {
                lease.owner == caller && lease.operation_id.as_deref() == Some(operation_id)
            }) {
                if lease.requested_ttl_ms != ttl_ms {
                    return Err(operation_conflict());
                }
                return Ok(active_lease(
                    lease,
                    now,
                    self.backend.name(),
                    self.backend.absolute_pointer(),
                ));
            }
            if let Some(terminal) = self
                .terminal_acquires
                .iter()
                .find(|terminal| terminal.owner == caller && terminal.operation_id == operation_id)
            {
                if terminal.requested_ttl_ms != ttl_ms {
                    return Err(operation_conflict());
                }
                return Ok(terminal_acquire(terminal));
            }
        }
        if !self.enabled {
            return Err(InputError::new(
                "injection_disabled",
                "input injection requires allow_injection startup policy",
            ));
        }
        if !self.backend.supported() {
            return Err(InputError::new(
                "unsupported_platform",
                "no native input backend for this platform",
            ));
        }
        if self.lease.is_some() {
            return Err(InputError::new(
                "input_busy",
                "another input lease or unresolved release is active",
            ));
        }
        if operation_id.is_some() && self.terminal_acquires.len() >= MAX_ACQUIRE_OPERATIONS {
            return Err(InputError::new(
                "resource_exhausted",
                "acquire operation history is full",
            ));
        }
        let duration = lease_duration(ttl_ms)?;
        let mut random = [0u8; 16];
        getrandom::fill(&mut random)
            .map_err(|error| InputError::new("entropy_error", error.to_string()))?;
        let token: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        guard()?;
        self.backend.start()?;
        self.lease = Some(Lease {
            owner: caller.into(),
            token: token.clone(),
            expires: now + Duration::from_millis(duration),
            cleanup_required: false,
            operation_id,
            requested_ttl_ms: ttl_ms,
            termination_reason: None,
            key_encoding: None,
        });
        Ok(active_lease(
            self.lease.as_ref().expect("new input lease"),
            now,
            self.backend.name(),
            self.backend.absolute_pointer(),
        ))
    }

    fn lookup(&self, caller: &str, operation_id: &str, now: Instant) -> InputResult<Value> {
        validate_operation_id(operation_id)?;
        if let Some(lease) = self.lease.as_ref().filter(|lease| {
            lease.owner == caller && lease.operation_id.as_deref() == Some(operation_id)
        }) {
            return Ok(active_lease(
                lease,
                now,
                self.backend.name(),
                self.backend.absolute_pointer(),
            ));
        }
        if let Some(terminal) = self
            .terminal_acquires
            .iter()
            .find(|terminal| terminal.owner == caller && terminal.operation_id == operation_id)
        {
            return Ok(terminal_acquire(terminal));
        }
        Err(InputError::new(
            "not_found",
            "no input acquire operation exists for this caller",
        ))
    }
    fn renew(
        &mut self,
        caller: &str,
        token: &str,
        ttl_ms: Option<u64>,
        now: Instant,
        guard: &mut impl FnMut() -> InputResult<()>,
    ) -> InputResult<Value> {
        self.authorize(caller, token, false)?;
        let duration = lease_duration(ttl_ms)?;
        if let Err(error) = guard() {
            let _ = self.finish_lease(now, "failed");
            return Err(error);
        }
        self.lease.as_mut().expect("authorized lease").expires =
            now + Duration::from_millis(duration);
        Ok(json!({"ok":true,"lease":token,"ttl_ms":duration}))
    }
    fn authorize(&self, caller: &str, token: &str, cleanup: bool) -> InputResult<()> {
        let lease = self
            .lease
            .as_ref()
            .ok_or_else(|| InputError::new("not_found", "no input lease exists"))?;
        if lease.owner != caller || lease.token != token {
            return Err(InputError::new(
                "permission_denied",
                "input lease belongs to a different caller or generation",
            ));
        }
        if lease.cleanup_required && !cleanup {
            return Err(InputError::new(
                "cleanup_required",
                "unreleased keys or buttons must be released before more input",
            ));
        }
        Ok(())
    }
    fn batch(
        &mut self,
        caller: &str,
        token: &str,
        events: Vec<Event>,
        guard: &mut impl FnMut() -> InputResult<()>,
    ) -> Value {
        if let Err(error) = self
            .authorize(caller, token, false)
            .and_then(|()| self.validate_batch(&events))
        {
            return self.failure(error);
        }
        for (accepted, event) in events.iter().enumerate() {
            let result = guard().and_then(|()| {
                if self
                    .lease
                    .as_ref()
                    .is_some_and(|lease| Instant::now() >= lease.expires)
                {
                    return Err(InputError::new(
                        "lease_expired",
                        "input lease expired during batch",
                    ));
                }
                self.inject(event)
            });
            if let Err(error) = result {
                let _ = self.finish_lease(Instant::now(), "failed");
                let mut reply = self.failure(error);
                reply["accepted"] = accepted.into();
                reply["partial_event_possible"] = true.into();
                return reply;
            }
        }
        json!({"ok":true,"accepted":events.len(),"lease":token})
    }
    fn validate_batch(&self, events: &[Event]) -> InputResult<()> {
        if events.is_empty() || events.len() > MAX_EVENTS {
            return Err(InputError::new(
                "invalid_request",
                "input batch must contain 1..64 events",
            ));
        }
        let mut pressed = self.pressed.clone();
        let mut key_encoding = self.lease.as_ref().and_then(|lease| lease.key_encoding);
        for event in events {
            event.validate()?;
            self.backend.validate(event)?;
            if let Event::Key { key, .. } = event {
                let event_encoding = key.encoding();
                if key_encoding.is_some_and(|encoding| encoding != event_encoding) {
                    return Err(InputError::new(
                        "invalid_sequence",
                        "keyboard encodings cannot be mixed within one lease",
                    ));
                }
                key_encoding = Some(event_encoding);
            }
            if let Event::Key {
                key,
                state: KeyState::Repeat,
            } = event
                && !pressed.contains(&Pressed::Key(key.canonical()))
            {
                return Err(InputError::new(
                    "invalid_sequence",
                    "repeat requires a held key owned by this lease",
                ));
            }
            if let Some((value, down)) = event.transition() {
                if pressed.contains(&value) == down {
                    return Err(InputError::new(
                        "invalid_sequence",
                        "duplicate press or release of an unowned input",
                    ));
                }
                if down {
                    pressed.push(value);
                } else {
                    pressed.retain(|held| held != &value);
                }
                if pressed.len() > MAX_PRESSED {
                    return Err(InputError::new(
                        "resource_exhausted",
                        "at most 128 keys/buttons may be held",
                    ));
                }
            }
        }
        Ok(())
    }
    fn inject(&mut self, event: &Event) -> InputResult<()> {
        if let Event::Key { key, .. } = event
            && let Some(lease) = &mut self.lease
        {
            lease.key_encoding.get_or_insert(key.encoding());
        }
        let transition = event.transition();
        // A down event can reach the OS even when the backend subsequently fails
        // (for example uinput SYN_REPORT fails). Track it conservatively first.
        if let Some((value, true)) = &transition {
            self.pressed.push(value.clone());
        }
        self.backend.inject(event)?;
        if let Some((value, false)) = transition {
            self.pressed.retain(|held| held != &value);
        }
        Ok(())
    }
    fn finish_lease(&mut self, now: Instant, reason: &'static str) -> InputResult<()> {
        if let Some(lease) = &mut self.lease {
            lease.termination_reason.get_or_insert(reason);
        }
        let mut failure = None;
        for index in (0..self.pressed.len()).rev() {
            match self.backend.inject(&self.pressed[index].release_event()) {
                Ok(()) => {
                    self.pressed.remove(index);
                }
                Err(error) => {
                    failure = Some(error);
                }
            }
        }
        if let Some(error) = failure {
            if let Some(lease) = &mut self.lease {
                lease.cleanup_required = true;
            }
            self.retry_at = now + Duration::from_millis(100);
            return Err(InputError::new("release_failed", error.message));
        }
        let Some(lease) = self.lease.take() else {
            self.retry_at = now;
            return Ok(());
        };
        self.backend.stop();
        if let Some(operation_id) = lease.operation_id {
            self.terminal_acquires.push_back(TerminalAcquire {
                owner: lease.owner,
                operation_id,
                requested_ttl_ms: lease.requested_ttl_ms,
                reason: lease.termination_reason.unwrap_or(reason),
                retained_until: now + OPERATION_RETENTION,
            });
        }
        self.retry_at = now;
        Ok(())
    }
    fn status(&self, caller: &str, now: Instant) -> Value {
        let lease = self.lease.as_ref().map(|lease| {
            let owned = lease.owner == caller;
            json!({
                "owned_by_caller":owned,
                "operation_id":if owned { lease.operation_id.as_deref() } else { None },
                "token":owned.then_some(&lease.token),
                "expires_in_ms":lease.expires.saturating_duration_since(now).as_millis() as u64,
                "pressed_count":self.pressed.len(),"cleanup_required":lease.cleanup_required,
            })
        });
        json!({"ok":true,"enabled":self.enabled,"backend":self.backend.name(),"native_supported":self.backend.supported(),"key_encoding":self.backend.key_encoding(),"absolute_pointer":self.backend.absolute_pointer(),"lease":lease})
    }
    fn failure(&self, error: InputError) -> Value {
        let mut value = error.payload();
        value["cleanup_required"] = self
            .lease
            .as_ref()
            .is_some_and(|lease| lease.cleanup_required)
            .into();
        value
    }
}
impl<B: Backend> Drop for Controller<B> {
    fn drop(&mut self) {
        // Graceful unload gets a final best-effort release pass. Linux also
        // destroys its owned uinput devices; Windows cannot undo OS rejection.
        let _ = self.finish_lease(Instant::now(), "cancelled");
        if self.lease.is_some() {
            self.backend.stop();
        }
    }
}

fn active_lease(lease: &Lease, now: Instant, backend: &str, absolute_pointer: bool) -> Value {
    json!({
        "ok": true,
        "state": "active",
        "operation_id": lease.operation_id,
        "lease": lease.token,
        "ttl_ms": lease.requested_ttl_ms.unwrap_or(5000),
        "expires_in_ms": lease.expires.saturating_duration_since(now).as_millis() as u64,
        "backend": backend,
        "absolute_pointer": absolute_pointer,
        "cleanup_required": lease.cleanup_required,
    })
}

fn terminal_acquire(operation: &TerminalAcquire) -> Value {
    json!({
        "ok": true,
        "state": "released",
        "operation_id": operation.operation_id,
        "lease": Value::Null,
        "reason": operation.reason,
    })
}

fn operation_conflict() -> InputError {
    InputError::new(
        "operation_conflict",
        "operation_id was already used with different acquire parameters",
    )
}
#[cfg(test)]
mod tests;
