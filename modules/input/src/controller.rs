use crate::backend::Backend;
use crate::gamepad_profile::GamepadProfile;
use crate::model::{
    DevicePolicy, Event, InputError, InputResult, KeyEncoding, KeyState, MAX_EVENTS, MAX_PRESSED,
    Operation, Pressed, is_temporary_modifier, lease_duration, repeat_modifiers,
    validate_operation_id,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

const MAX_ACQUIRE_OPERATIONS: usize = 64;
const OPERATION_RETENTION: Duration = Duration::from_secs(300);
const WHEEL_DELTA: i32 = 120;
const MAX_SCROLL_STEPS_PER_EVENT: i32 = 100;
const DEFAULT_KEY_REPEAT_DELAY_MS: u32 = 500;
const DEFAULT_KEY_REPEAT_FREQUENCY_HZ: f64 = 24.9;
const DEFAULT_BACK_BUTTON_TIMEOUT_MS: i32 = -1;

struct AcquireRequest {
    operation_id: Option<String>,
    ttl_ms: Option<u64>,
    back_button_timeout: Option<i32>,
    controller: Option<bool>,
    gamepad: Option<GamepadProfile>,
    ds4_back_as_touchpad_click: Option<bool>,
    native_pen_touch: Option<bool>,
    keyboard: Option<bool>,
    mouse: Option<bool>,
    high_resolution_scrolling: Option<bool>,
    always_send_scancodes: Option<bool>,
    key_rightalt_to_key_win: Option<bool>,
    key_repeat_delay: Option<u32>,
    key_repeat_frequency: Option<f64>,
}

struct Lease {
    owner: String,
    token: String,
    expires: Instant,
    cleanup_required: bool,
    operation_id: Option<String>,
    requested_ttl_ms: Option<u64>,
    requested_back_button_timeout: Option<i32>,
    requested_controller: Option<bool>,
    requested_gamepad: Option<GamepadProfile>,
    requested_ds4_back_as_touchpad_click: Option<bool>,
    ds4_back_as_touchpad_click: bool,
    requested_native_pen_touch: Option<bool>,
    native_pen_touch: bool,
    requested_keyboard: Option<bool>,
    requested_mouse: Option<bool>,
    requested_high_resolution_scrolling: Option<bool>,
    requested_always_send_scancodes: Option<bool>,
    requested_key_rightalt_to_key_win: Option<bool>,
    requested_key_repeat_delay: Option<u32>,
    requested_key_repeat_frequency: Option<f64>,
    back_button_timeout_ms: i32,
    termination_reason: Option<&'static str>,
    key_encoding: Option<KeyEncoding>,
    gamepad_profile: GamepadProfile,
    effective_gamepad_profile: Option<&'static str>,
    gamepad_unavailable_reason: Option<String>,
}

struct TerminalAcquire {
    owner: String,
    operation_id: String,
    requested_ttl_ms: Option<u64>,
    requested_back_button_timeout: Option<i32>,
    requested_controller: Option<bool>,
    requested_gamepad: Option<GamepadProfile>,
    requested_ds4_back_as_touchpad_click: Option<bool>,
    requested_native_pen_touch: Option<bool>,
    native_pen_touch: bool,
    requested_keyboard: Option<bool>,
    requested_mouse: Option<bool>,
    requested_high_resolution_scrolling: Option<bool>,
    requested_always_send_scancodes: Option<bool>,
    requested_key_rightalt_to_key_win: Option<bool>,
    requested_key_repeat_delay: Option<u32>,
    requested_key_repeat_frequency: Option<f64>,
    reason: &'static str,
    retained_until: Instant,
}

struct RepeatingKey {
    key: crate::model::Key,
    modifiers: u8,
    next_due: Instant,
    period: Duration,
}

pub struct Controller<B: Backend> {
    backend: B,
    enabled: bool,
    device_policy: DevicePolicy,
    high_resolution_scrolling: bool,
    always_send_scancodes: bool,
    key_rightalt_to_key_win: bool,
    key_repeat_delay_ms: u32,
    key_repeat_frequency_hz: f64,
    back_button_timeout_ms: i32,
    gamepad_profile: GamepadProfile,
    ds4_back_as_touchpad_click: bool,
    native_pen_touch: bool,
    controller_enabled: bool,
    controller_cleanup_pending: bool,
    repeating_key: Option<RepeatingKey>,
    vertical_scroll_remainder: i32,
    horizontal_scroll_remainder: i32,
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
            device_policy: DevicePolicy::default(),
            high_resolution_scrolling: true,
            always_send_scancodes: true,
            key_rightalt_to_key_win: false,
            key_repeat_delay_ms: DEFAULT_KEY_REPEAT_DELAY_MS,
            key_repeat_frequency_hz: DEFAULT_KEY_REPEAT_FREQUENCY_HZ,
            back_button_timeout_ms: DEFAULT_BACK_BUTTON_TIMEOUT_MS,
            gamepad_profile: GamepadProfile::Auto,
            ds4_back_as_touchpad_click: true,
            native_pen_touch: true,
            controller_enabled: true,
            controller_cleanup_pending: false,
            repeating_key: None,
            vertical_scroll_remainder: 0,
            horizontal_scroll_remainder: 0,
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
                caller,
                AcquireRequest {
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
                },
                now,
                guard,
            ),
            Operation::Lookup { operation_id } => self.lookup(caller, &operation_id, now),
            Operation::Renew { lease, ttl_ms } => self.renew(caller, &lease, ttl_ms, now, guard),
            Operation::Batch { lease, events } => {
                return self.batch(caller, &lease, events, now, guard);
            }
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
            return;
        }
        if self.controller_cleanup_pending && now >= self.retry_at {
            let _ = self.retry_controller_cleanup(now);
        }
        let lease_cleanup_required = self
            .lease
            .as_ref()
            .is_some_and(|lease| lease.cleanup_required);
        if self.lease.is_some()
            && !lease_cleanup_required
            && !self.controller_cleanup_pending
            && self.backend.advance(now).is_err()
        {
            let _ = self.finish_lease(now, "failed");
            return;
        }
        if self.lease.is_some() && !lease_cleanup_required && self.repeat_if_due(now).is_err() {
            let _ = self.finish_lease(now, "failed");
        }
    }

    pub fn device_policy(&self) -> DevicePolicy {
        self.device_policy
    }

    fn effective_device_policy(&self) -> DevicePolicy {
        let lease = self.lease.as_ref();
        DevicePolicy {
            keyboard: lease
                .and_then(|lease| lease.requested_keyboard)
                .unwrap_or(self.device_policy.keyboard),
            mouse: lease
                .and_then(|lease| lease.requested_mouse)
                .unwrap_or(self.device_policy.mouse),
        }
    }

    fn effective_controller_enabled(&self) -> bool {
        self.lease
            .as_ref()
            .and_then(|lease| lease.requested_controller)
            .unwrap_or(self.controller_enabled)
    }

    fn effective_gamepad_profile(&self) -> GamepadProfile {
        self.lease
            .as_ref()
            .map(|lease| lease.gamepad_profile)
            .unwrap_or(self.gamepad_profile)
    }

    fn effective_ds4_back_as_touchpad_click(&self) -> bool {
        self.lease
            .as_ref()
            .map(|lease| lease.ds4_back_as_touchpad_click)
            .unwrap_or(self.ds4_back_as_touchpad_click)
    }

    fn effective_high_resolution_scrolling(&self) -> bool {
        self.lease
            .as_ref()
            .and_then(|lease| lease.requested_high_resolution_scrolling)
            .unwrap_or(self.high_resolution_scrolling)
    }

    fn effective_always_send_scancodes(&self) -> bool {
        self.lease
            .as_ref()
            .and_then(|lease| lease.requested_always_send_scancodes)
            .unwrap_or(self.always_send_scancodes)
    }

    fn effective_key_rightalt_to_key_win(&self) -> bool {
        self.lease
            .as_ref()
            .and_then(|lease| lease.requested_key_rightalt_to_key_win)
            .unwrap_or(self.key_rightalt_to_key_win)
    }

    fn effective_key_repeat_policy(&self) -> (u32, f64) {
        let lease = self.lease.as_ref();
        (
            lease
                .and_then(|lease| lease.requested_key_repeat_delay)
                .unwrap_or(self.key_repeat_delay_ms),
            lease
                .and_then(|lease| lease.requested_key_repeat_frequency)
                .unwrap_or(self.key_repeat_frequency_hz),
        )
    }

    pub fn high_resolution_scrolling(&self) -> bool {
        self.high_resolution_scrolling
    }

    pub fn set_high_resolution_scrolling(&mut self, enabled: bool) {
        let previous = self.effective_high_resolution_scrolling();
        self.high_resolution_scrolling = enabled;
        if previous != self.effective_high_resolution_scrolling() {
            self.reset_scroll_remainders();
        }
    }

    pub fn always_send_scancodes(&self) -> bool {
        self.always_send_scancodes
    }

    pub fn set_always_send_scancodes(&mut self, enabled: bool) {
        let previous = self.effective_always_send_scancodes();
        self.always_send_scancodes = enabled;
        let effective = self.effective_always_send_scancodes();
        if previous != effective {
            self.backend.set_always_send_scancodes(effective);
        }
    }

    pub fn key_rightalt_to_key_win(&self) -> bool {
        self.key_rightalt_to_key_win
    }

    pub fn set_key_rightalt_to_key_win(&mut self, enabled: bool) {
        let previous = self.effective_key_rightalt_to_key_win();
        self.key_rightalt_to_key_win = enabled;
        let effective = self.effective_key_rightalt_to_key_win();
        if previous != effective {
            self.backend.set_key_rightalt_to_key_win(effective);
        }
    }

    pub fn key_repeat_policy(&self) -> (u32, f64) {
        (self.key_repeat_delay_ms, self.key_repeat_frequency_hz)
    }

    pub fn set_key_repeat_policy(&mut self, delay_ms: u32, frequency_hz: f64) {
        self.key_repeat_delay_ms = delay_ms;
        self.key_repeat_frequency_hz = frequency_hz;
    }

    pub fn back_button_timeout_ms(&self) -> i32 {
        self.back_button_timeout_ms
    }

    pub fn set_back_button_timeout_ms(&mut self, timeout_ms: i32) {
        self.back_button_timeout_ms = timeout_ms;
        if self.lease.is_none() {
            self.backend.set_back_button_timeout(timeout_ms);
        }
    }

    pub fn gamepad_profile(&self) -> GamepadProfile {
        self.gamepad_profile
    }

    pub fn set_gamepad_profile(&mut self, profile: GamepadProfile) {
        self.gamepad_profile = profile;
        self.backend
            .set_gamepad_profile(self.effective_gamepad_profile());
    }

    pub fn ds4_back_as_touchpad_click(&self) -> bool {
        self.ds4_back_as_touchpad_click
    }

    pub fn set_ds4_back_as_touchpad_click(&mut self, enabled: bool) {
        let previous = self.effective_ds4_back_as_touchpad_click();
        self.ds4_back_as_touchpad_click = enabled;
        let effective = self.effective_ds4_back_as_touchpad_click();
        if previous != effective {
            self.backend.set_ds4_back_as_touchpad_click(effective);
        }
    }

    pub fn native_pen_touch(&self) -> bool {
        self.native_pen_touch
    }

    pub fn set_native_pen_touch(&mut self, enabled: bool) {
        self.native_pen_touch = enabled;
    }

    pub fn controller_enabled(&self) -> bool {
        self.controller_enabled
    }

    /// Updates the live controller gate before trying to remove virtual slots.
    /// A failed removal is retried without disabling keyboard or pointer input.
    pub fn set_controller_enabled(&mut self, enabled: bool, now: Instant) -> InputResult<()> {
        self.controller_enabled = enabled;
        let effective_enabled = self.effective_controller_enabled();
        if effective_enabled && !self.controller_cleanup_pending {
            return Ok(());
        }
        self.controller_cleanup_pending = true;
        self.retry_controller_cleanup(now)
    }

    fn retry_controller_cleanup(&mut self, now: Instant) -> InputResult<()> {
        match self.backend.disable_controller_input() {
            Ok(()) => {
                self.controller_cleanup_pending = false;
                if self.lease.is_none() {
                    self.retry_at = now;
                }
                Ok(())
            }
            Err(error) => {
                self.controller_cleanup_pending = true;
                self.retry_at = now + Duration::from_millis(100);
                Err(InputError::new("release_failed", error.message))
            }
        }
    }

    fn reset_scroll_remainders(&mut self) {
        self.vertical_scroll_remainder = 0;
        self.horizontal_scroll_remainder = 0;
    }

    /// Switches policy gates before attempting native cleanup. Requests later
    /// in the module queue therefore cannot inject disabled categories even if
    /// releasing an already-held input needs a retry.
    pub fn set_device_policy(&mut self, policy: DevicePolicy, now: Instant) -> InputResult<()> {
        self.device_policy = policy;
        let effective_policy = self.effective_device_policy();
        if !effective_policy.keyboard {
            self.repeating_key = None;
        }
        if !effective_policy.mouse {
            self.reset_scroll_remainders();
        }
        let mut failure = None;
        for index in (0..self.pressed.len()).rev() {
            let pressed = &self.pressed[index];
            let should_release = match pressed {
                Pressed::Key(_) => !effective_policy.keyboard,
                Pressed::Button(_) => !effective_policy.mouse,
            };
            if !should_release {
                continue;
            }
            match self.backend.inject(&pressed.release_event()) {
                Ok(()) => {
                    self.pressed.remove(index);
                }
                Err(error) => failure = Some(error),
            }
        }
        if let Some(error) = failure {
            if let Some(lease) = &mut self.lease {
                lease.cleanup_required = true;
                lease.termination_reason.get_or_insert("policy_changed");
            }
            self.retry_at = now + Duration::from_millis(100);
            return Err(InputError::new("release_failed", error.message));
        }
        if self
            .lease
            .as_ref()
            .is_some_and(|lease| lease.cleanup_required)
        {
            self.finish_lease(now, "policy_changed")?;
        }
        Ok(())
    }

    pub fn cleanup_required(&self) -> bool {
        self.controller_cleanup_pending
            || self
                .lease
                .as_ref()
                .is_some_and(|lease| lease.cleanup_required)
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
        request: AcquireRequest,
        now: Instant,
        guard: &mut impl FnMut() -> InputResult<()>,
    ) -> InputResult<Value> {
        let AcquireRequest {
            operation_id,
            ttl_ms,
            back_button_timeout: requested_back_button_timeout,
            controller: requested_controller,
            gamepad: requested_gamepad,
            ds4_back_as_touchpad_click: requested_ds4_back_as_touchpad_click,
            native_pen_touch: requested_native_pen_touch,
            keyboard: requested_keyboard,
            mouse: requested_mouse,
            high_resolution_scrolling: requested_high_resolution_scrolling,
            always_send_scancodes: requested_always_send_scancodes,
            key_rightalt_to_key_win: requested_key_rightalt_to_key_win,
            key_repeat_delay: requested_key_repeat_delay,
            key_repeat_frequency: requested_key_repeat_frequency,
        } = request;
        if requested_key_repeat_delay.is_some_and(|delay| delay > i32::MAX as u32)
            || requested_key_repeat_frequency
                .is_some_and(|frequency| !frequency.is_finite() || frequency < 0.0)
        {
            return Err(InputError::new(
                "invalid_request",
                "scoped key repeat values are outside their supported range",
            ));
        }
        if let Some(operation_id) = operation_id.as_deref() {
            validate_operation_id(operation_id)?;
            if let Some(lease) = self.lease.as_ref().filter(|lease| {
                lease.owner == caller && lease.operation_id.as_deref() == Some(operation_id)
            }) {
                if lease.requested_ttl_ms != ttl_ms
                    || lease.requested_back_button_timeout != requested_back_button_timeout
                    || lease.requested_controller != requested_controller
                    || lease.requested_gamepad != requested_gamepad
                    || lease.requested_ds4_back_as_touchpad_click
                        != requested_ds4_back_as_touchpad_click
                    || lease.requested_native_pen_touch != requested_native_pen_touch
                    || lease.requested_keyboard != requested_keyboard
                    || lease.requested_mouse != requested_mouse
                    || lease.requested_high_resolution_scrolling
                        != requested_high_resolution_scrolling
                    || lease.requested_always_send_scancodes != requested_always_send_scancodes
                    || lease.requested_key_rightalt_to_key_win != requested_key_rightalt_to_key_win
                    || lease.requested_key_repeat_delay != requested_key_repeat_delay
                    || lease.requested_key_repeat_frequency != requested_key_repeat_frequency
                {
                    return Err(operation_conflict());
                }
                return Ok(active_lease(
                    lease,
                    now,
                    self.backend.name(),
                    self.backend.absolute_pointer(),
                    self.backend.controller_input_supported(),
                ));
            }
            if let Some(terminal) = self
                .terminal_acquires
                .iter()
                .find(|terminal| terminal.owner == caller && terminal.operation_id == operation_id)
            {
                if terminal.requested_ttl_ms != ttl_ms
                    || terminal.requested_back_button_timeout != requested_back_button_timeout
                    || terminal.requested_controller != requested_controller
                    || terminal.requested_gamepad != requested_gamepad
                    || terminal.requested_ds4_back_as_touchpad_click
                        != requested_ds4_back_as_touchpad_click
                    || terminal.requested_native_pen_touch != requested_native_pen_touch
                    || terminal.requested_keyboard != requested_keyboard
                    || terminal.requested_mouse != requested_mouse
                    || terminal.requested_high_resolution_scrolling
                        != requested_high_resolution_scrolling
                    || terminal.requested_always_send_scancodes != requested_always_send_scancodes
                    || terminal.requested_key_rightalt_to_key_win
                        != requested_key_rightalt_to_key_win
                    || terminal.requested_key_repeat_delay != requested_key_repeat_delay
                    || terminal.requested_key_repeat_frequency != requested_key_repeat_frequency
                {
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
        if self.controller_cleanup_pending {
            return Err(InputError::new(
                "cleanup_required",
                "virtual controller cleanup must complete before acquiring a lease",
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
        let back_button_timeout_ms =
            requested_back_button_timeout.unwrap_or(self.back_button_timeout_ms);
        let gamepad_profile = requested_gamepad.unwrap_or(self.gamepad_profile);
        let ds4_back_as_touchpad_click =
            requested_ds4_back_as_touchpad_click.unwrap_or(self.ds4_back_as_touchpad_click);
        let native_pen_touch = requested_native_pen_touch.unwrap_or(self.native_pen_touch);
        let high_resolution_scrolling =
            requested_high_resolution_scrolling.unwrap_or(self.high_resolution_scrolling);
        let always_send_scancodes =
            requested_always_send_scancodes.unwrap_or(self.always_send_scancodes);
        let key_rightalt_to_key_win =
            requested_key_rightalt_to_key_win.unwrap_or(self.key_rightalt_to_key_win);
        if high_resolution_scrolling != self.high_resolution_scrolling {
            self.reset_scroll_remainders();
        }
        self.backend.set_back_button_timeout(back_button_timeout_ms);
        self.backend.set_gamepad_profile(gamepad_profile);
        self.backend
            .set_ds4_back_as_touchpad_click(ds4_back_as_touchpad_click);
        self.backend
            .set_always_send_scancodes(always_send_scancodes);
        self.backend
            .set_key_rightalt_to_key_win(key_rightalt_to_key_win);
        if let Err(error) = self.backend.start() {
            self.backend
                .set_back_button_timeout(self.back_button_timeout_ms);
            self.backend.set_gamepad_profile(self.gamepad_profile);
            self.backend
                .set_ds4_back_as_touchpad_click(self.ds4_back_as_touchpad_click);
            self.backend
                .set_always_send_scancodes(self.always_send_scancodes);
            self.backend
                .set_key_rightalt_to_key_win(self.key_rightalt_to_key_win);
            return Err(error);
        }
        self.reset_scroll_remainders();
        self.lease = Some(Lease {
            owner: caller.into(),
            token: token.clone(),
            expires: now + Duration::from_millis(duration),
            cleanup_required: false,
            operation_id,
            requested_ttl_ms: ttl_ms,
            requested_back_button_timeout,
            requested_controller,
            requested_gamepad,
            requested_ds4_back_as_touchpad_click,
            requested_native_pen_touch,
            native_pen_touch,
            requested_keyboard,
            requested_mouse,
            requested_high_resolution_scrolling,
            requested_always_send_scancodes,
            requested_key_rightalt_to_key_win,
            requested_key_repeat_delay,
            requested_key_repeat_frequency,
            back_button_timeout_ms,
            termination_reason: None,
            key_encoding: None,
            gamepad_profile,
            ds4_back_as_touchpad_click,
            effective_gamepad_profile: self.backend.effective_gamepad_profile(),
            gamepad_unavailable_reason: self
                .backend
                .gamepad_unavailable_reason()
                .map(str::to_owned),
        });
        Ok(active_lease(
            self.lease.as_ref().expect("new input lease"),
            now,
            self.backend.name(),
            self.backend.absolute_pointer(),
            self.backend.controller_input_supported(),
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
                self.backend.controller_input_supported(),
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
        now: Instant,
        guard: &mut impl FnMut() -> InputResult<()>,
    ) -> Value {
        if let Err(error) = self
            .authorize(caller, token, false)
            .and_then(|()| self.validate_batch(&events))
        {
            return self.failure(error);
        }
        let policy = self.effective_device_policy();
        let mut suppressed = 0;
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
                if self.event_enabled(event, policy) {
                    self.inject_batch_event(event)
                        .and_then(|()| self.update_repeat_after_event(event, now))
                } else {
                    suppressed += 1;
                    Ok(())
                }
            });
            if let Err(error) = result {
                if matches!(event, Event::Controller { .. })
                    && error.code == "controller_unavailable"
                {
                    // Keep the active lease and let its owner decide whether to
                    // drop this gamepad packet. The backend retains any virtual
                    // device whose neutral release or destroy still needs retry.
                    let mut reply = self.failure(error);
                    reply["lease"] = json!(token);
                    reply["accepted"] = accepted.into();
                    reply["partial_event_possible"] = true.into();
                    return reply;
                }
                let _ = self.finish_lease(Instant::now(), "failed");
                let mut reply = self.failure(error);
                reply["accepted"] = accepted.into();
                reply["partial_event_possible"] = true.into();
                return reply;
            }
        }
        json!({"ok":true,"accepted":events.len(),"suppressed":suppressed,"lease":token})
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
            if !self.event_enabled(event, self.effective_device_policy()) {
                continue;
            }
            self.backend.validate(event)?;
            if let Event::Key { key, state } = event {
                match state {
                    KeyState::Down => {
                        let required = repeat_modifiers(key);
                        let available = held_temporary_modifier_mask(&pressed);
                        if required & !available != 0 {
                            return Err(InputError::new(
                                "invalid_sequence",
                                "repeat modifiers must be temporary modifiers held before the key-down",
                            ));
                        }
                    }
                    KeyState::Up => {
                        let identity = Pressed::Key(key.canonical());
                        let Some(Pressed::Key(held_key)) =
                            pressed.iter().find(|held| **held == identity)
                        else {
                            return Err(InputError::new(
                                "invalid_sequence",
                                "key-up does not match a held key",
                            ));
                        };
                        if is_temporary_modifier(held_key) != is_temporary_modifier(key) {
                            return Err(InputError::new(
                                "invalid_sequence",
                                "key-up temporary modifier marker does not match its key-down",
                            ));
                        }
                    }
                    KeyState::Repeat => {}
                }
            }
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

    fn inject_batch_event(&mut self, event: &Event) -> InputResult<()> {
        let Event::ScrollUnits { axis, amount } = event else {
            return self.inject(event);
        };
        if self.effective_high_resolution_scrolling() {
            return self.inject(event);
        }

        let previous_remainder = match axis {
            crate::model::Axis::Horizontal => self.horizontal_scroll_remainder,
            crate::model::Axis::Vertical => self.vertical_scroll_remainder,
        };
        let total = previous_remainder + i32::from(*amount);
        let mut steps = total / WHEEL_DELTA;
        let next_remainder = total % WHEEL_DELTA;
        while steps != 0 {
            let chunk = steps.clamp(-MAX_SCROLL_STEPS_PER_EVENT, MAX_SCROLL_STEPS_PER_EVENT);
            self.inject(&Event::Scroll {
                axis: *axis,
                steps: chunk,
            })?;
            steps -= chunk;
        }
        match axis {
            crate::model::Axis::Horizontal => self.horizontal_scroll_remainder = next_remainder,
            crate::model::Axis::Vertical => self.vertical_scroll_remainder = next_remainder,
        }
        Ok(())
    }

    fn event_enabled(&self, event: &Event, policy: DevicePolicy) -> bool {
        event.enabled_by(policy)
            && (!matches!(event, Event::Controller { .. })
                || (self.effective_controller_enabled() && !self.controller_cleanup_pending))
    }

    fn update_repeat_after_event(&mut self, event: &Event, now: Instant) -> InputResult<()> {
        let Event::Key { key, state } = event else {
            return Ok(());
        };
        if is_temporary_modifier(key) {
            return Ok(());
        }
        match state {
            KeyState::Down => {
                let (delay_ms, frequency_hz) = self.effective_key_repeat_policy();
                if delay_ms == 0 {
                    self.repeating_key = None;
                    return Ok(());
                }
                let period = repeat_period(frequency_hz);
                let Some(next_due) = now.checked_add(Duration::from_millis(u64::from(delay_ms)))
                else {
                    self.repeating_key = None;
                    return Ok(());
                };
                let mut repeating = key.clone();
                repeating.clear_repeat_modifiers();
                self.repeating_key = Some(RepeatingKey {
                    key: repeating,
                    modifiers: repeat_modifiers(key),
                    next_due,
                    period,
                });
            }
            KeyState::Up => {
                let released = Pressed::Key(key.canonical());
                if self
                    .repeating_key
                    .as_ref()
                    .is_some_and(|repeat| Pressed::Key(repeat.key.canonical()) == released)
                {
                    self.repeating_key = None;
                }
            }
            KeyState::Repeat => {}
        }
        Ok(())
    }

    fn repeat_if_due(&mut self, now: Instant) -> InputResult<()> {
        let Some(repeating) = self.repeating_key.as_ref() else {
            return Ok(());
        };
        if now < repeating.next_due {
            return Ok(());
        }
        let repeat = RepeatingKey {
            key: repeating.key.clone(),
            modifiers: repeating.modifiers,
            next_due: repeating.next_due,
            period: repeating.period,
        };
        if !self.pressed.contains(&Pressed::Key(repeat.key.canonical())) {
            self.repeating_key = None;
            return Ok(());
        }

        let requested_modifiers = [(1, 0x10), (2, 0x11), (4, 0x12)];
        let synthetic: Vec<(u8, u8)> = requested_modifiers
            .into_iter()
            .filter(|(bit, _)| repeat.modifiers & bit != 0)
            .filter(|(bit, _)| !self.modifier_is_held(*bit))
            .collect();
        for (_, code) in &synthetic {
            self.inject(&temporary_modifier_event(
                repeat.key.clone(),
                *code,
                KeyState::Down,
            ))?;
        }
        let mut repeat_key = repeat.key.clone();
        repeat_key.clear_repeat_modifiers();
        self.inject(&Event::Key {
            key: repeat_key,
            state: KeyState::Repeat,
        })?;
        for (_, code) in synthetic.iter().rev() {
            self.inject(&temporary_modifier_event(
                repeat.key.clone(),
                *code,
                KeyState::Up,
            ))?;
        }
        let period = repeat_period(self.effective_key_repeat_policy().1);
        match next_repeat_deadline(repeat.next_due, now, period) {
            Some(next_due) => {
                if let Some(repeating) = &mut self.repeating_key {
                    repeating.next_due = next_due;
                    repeating.period = period;
                }
            }
            None => self.repeating_key = None,
        }
        Ok(())
    }

    fn modifier_is_held(&self, bit: u8) -> bool {
        self.pressed.iter().any(|pressed| match pressed {
            Pressed::Key(key) => modifier_key_bit(key).is_some_and(|held_bit| held_bit == bit),
            Pressed::Button(_) => false,
        })
    }
    fn finish_lease(&mut self, now: Instant, reason: &'static str) -> InputResult<()> {
        self.repeating_key = None;
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
        if self.lease.is_some()
            && let Err(error) = self.backend.stop()
        {
            if let Some(lease) = &mut self.lease {
                lease.cleanup_required = true;
            }
            self.retry_at = now + Duration::from_millis(100);
            return Err(InputError::new("release_failed", error.message));
        }
        self.controller_cleanup_pending = false;
        self.backend
            .set_back_button_timeout(self.back_button_timeout_ms);
        self.backend.set_gamepad_profile(self.gamepad_profile);
        self.backend
            .set_ds4_back_as_touchpad_click(self.ds4_back_as_touchpad_click);
        self.backend
            .set_always_send_scancodes(self.always_send_scancodes);
        self.backend
            .set_key_rightalt_to_key_win(self.key_rightalt_to_key_win);
        let Some(lease) = self.lease.take() else {
            self.reset_scroll_remainders();
            self.retry_at = now;
            return Ok(());
        };
        self.reset_scroll_remainders();
        if let Some(operation_id) = lease.operation_id {
            self.terminal_acquires.push_back(TerminalAcquire {
                owner: lease.owner,
                operation_id,
                requested_ttl_ms: lease.requested_ttl_ms,
                requested_back_button_timeout: lease.requested_back_button_timeout,
                requested_controller: lease.requested_controller,
                requested_gamepad: lease.requested_gamepad,
                requested_ds4_back_as_touchpad_click: lease.requested_ds4_back_as_touchpad_click,
                requested_native_pen_touch: lease.requested_native_pen_touch,
                native_pen_touch: lease.native_pen_touch,
                requested_keyboard: lease.requested_keyboard,
                requested_mouse: lease.requested_mouse,
                requested_high_resolution_scrolling: lease.requested_high_resolution_scrolling,
                requested_always_send_scancodes: lease.requested_always_send_scancodes,
                requested_key_rightalt_to_key_win: lease.requested_key_rightalt_to_key_win,
                requested_key_repeat_delay: lease.requested_key_repeat_delay,
                requested_key_repeat_frequency: lease.requested_key_repeat_frequency,
                reason: lease.termination_reason.unwrap_or(reason),
                retained_until: now + OPERATION_RETENTION,
            });
        }
        self.retry_at = now;
        Ok(())
    }
    fn status(&self, caller: &str, now: Instant) -> Value {
        let effective_gamepad_profile = match self.lease.as_ref() {
            Some(lease) => lease.effective_gamepad_profile,
            None => self.backend.effective_gamepad_profile(),
        };
        let gamepad_unavailable_reason = match self.lease.as_ref() {
            Some(lease) => lease.gamepad_unavailable_reason.as_deref(),
            None => self.backend.gamepad_unavailable_reason(),
        };
        let lease = self.lease.as_ref().map(|lease| {
            let owned = lease.owner == caller;
            json!({
                "owned_by_caller":owned,
                "operation_id":if owned { lease.operation_id.as_deref() } else { None },
                "token":owned.then_some(&lease.token),
                "expires_in_ms":lease.expires.saturating_duration_since(now).as_millis() as u64,
                "pressed_count":self.pressed.len(),"cleanup_required":lease.cleanup_required,
                "back_button_timeout":lease.back_button_timeout_ms,
                "controller_enabled":lease.requested_controller.unwrap_or(self.controller_enabled),
                "keyboard":lease.requested_keyboard.unwrap_or(self.device_policy.keyboard),
                "mouse":lease.requested_mouse.unwrap_or(self.device_policy.mouse),
                "high_resolution_scrolling":lease.requested_high_resolution_scrolling.unwrap_or(self.high_resolution_scrolling),
                "always_send_scancodes":lease.requested_always_send_scancodes.unwrap_or(self.always_send_scancodes),
                "key_rightalt_to_key_win":lease.requested_key_rightalt_to_key_win.unwrap_or(self.key_rightalt_to_key_win),
                "key_repeat_delay":lease.requested_key_repeat_delay.unwrap_or(self.key_repeat_delay_ms),
                "key_repeat_frequency":lease.requested_key_repeat_frequency.unwrap_or(self.key_repeat_frequency_hz),
                "gamepad_profile":lease.gamepad_profile.as_str(),
                "ds4_back_as_touchpad_click":lease.requested_ds4_back_as_touchpad_click.unwrap_or(lease.ds4_back_as_touchpad_click),
                "native_pen_touch":lease.native_pen_touch,
                "effective_gamepad_profile":lease.effective_gamepad_profile,
                "gamepad_unavailable_reason":lease.gamepad_unavailable_reason,
            })
        });
        json!({"ok":true,"enabled":self.enabled,"keyboard":self.device_policy.keyboard,"mouse":self.device_policy.mouse,"controller_enabled":self.controller_enabled,"controller_cleanup_required":self.controller_cleanup_pending,"high_resolution_scrolling":self.high_resolution_scrolling,"always_send_scancodes":self.always_send_scancodes,"key_rightalt_to_key_win":self.key_rightalt_to_key_win,"key_repeat_delay":self.key_repeat_delay_ms,"key_repeat_frequency":self.key_repeat_frequency_hz,"back_button_timeout":self.back_button_timeout_ms,"gamepad":self.gamepad_profile.as_str(),"ds4_back_as_touchpad_click":self.ds4_back_as_touchpad_click,"native_pen_touch":self.native_pen_touch,"supported_profiles":self.backend.supported_gamepad_profiles(),"recognized_profiles":GamepadProfile::recognized_profiles(),"effective_gamepad_profile":effective_gamepad_profile,"gamepad_unavailable_reason":gamepad_unavailable_reason,"backend":self.backend.name(),"native_supported":self.backend.supported(),"key_encoding":self.backend.key_encoding(),"absolute_pointer":self.backend.absolute_pointer(),"controller_input":self.backend.controller_input_supported(),"lease":lease})
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
            let _ = self.backend.stop();
        }
    }
}

fn active_lease(
    lease: &Lease,
    now: Instant,
    backend: &str,
    absolute_pointer: bool,
    controller_input: bool,
) -> Value {
    json!({
        "ok": true,
        "state": "active",
        "operation_id": lease.operation_id,
        "lease": lease.token,
        "ttl_ms": lease.requested_ttl_ms.unwrap_or(5000),
        "back_button_timeout": lease.back_button_timeout_ms,
        "high_resolution_scrolling": lease.requested_high_resolution_scrolling,
        "always_send_scancodes": lease.requested_always_send_scancodes,
        "key_rightalt_to_key_win": lease.requested_key_rightalt_to_key_win,
        "key_repeat_delay": lease.requested_key_repeat_delay,
        "key_repeat_frequency": lease.requested_key_repeat_frequency,
        "expires_in_ms": lease.expires.saturating_duration_since(now).as_millis() as u64,
        "backend": backend,
        "absolute_pointer": absolute_pointer,
        "controller_input": controller_input,
        "gamepad_profile": lease.gamepad_profile.as_str(),
        "ds4_back_as_touchpad_click": lease.ds4_back_as_touchpad_click,
        "native_pen_touch": lease.native_pen_touch,
        "effective_gamepad_profile": lease.effective_gamepad_profile,
        "gamepad_unavailable_reason": lease.gamepad_unavailable_reason,
        "cleanup_required": lease.cleanup_required,
    })
}

fn repeat_period(frequency_hz: f64) -> Duration {
    let frequency_hz = if frequency_hz == 0.0 {
        DEFAULT_KEY_REPEAT_FREQUENCY_HZ
    } else {
        frequency_hz
    };
    // Vibeshine schedules the initial delay before its periodic timer. Preserve
    // that first repeat when a valid, very small frequency exceeds Duration's
    // range; a later interval cannot be represented then.
    Duration::try_from_secs_f64(1.0 / frequency_hz).unwrap_or(Duration::MAX)
}

fn next_repeat_deadline(previous: Instant, now: Instant, period: Duration) -> Option<Instant> {
    if period.is_zero() {
        return Some(now);
    }
    let elapsed_periods =
        now.saturating_duration_since(previous).as_nanos() / period.as_nanos() + 1;
    let Ok(periods) = u32::try_from(elapsed_periods) else {
        return now.checked_add(period);
    };
    period
        .checked_mul(periods)
        .and_then(|advance| previous.checked_add(advance))
        .or_else(|| now.checked_add(period))
}

fn temporary_modifier_event(template: crate::model::Key, code: u8, state: KeyState) -> Event {
    let normalized = match template {
        crate::model::Key::MoonlightVk { normalized, .. } => normalized,
        _ => true,
    };
    Event::Key {
        key: crate::model::Key::MoonlightVk {
            code,
            normalized,
            repeat_modifiers: 0,
            temporary_modifier: true,
        },
        state,
    }
}

fn modifier_key_bit(key: &crate::model::Key) -> Option<u8> {
    match key {
        crate::model::Key::MoonlightVk { code, .. } => match code {
            0x10 | 0xa0 | 0xa1 => Some(1),
            0x11 | 0xa2 | 0xa3 => Some(2),
            0x12 | 0xa4 | 0xa5 => Some(4),
            _ => None,
        },
        crate::model::Key::WindowsScan { code, extended } => match (*code, *extended) {
            (0x2a | 0x36, false) => Some(1),
            (0x1d, _) => Some(2),
            (0x38, _) => Some(4),
            _ => None,
        },
        crate::model::Key::LinuxEvdev { code } => match code {
            42 | 54 => Some(1),
            29 | 97 => Some(2),
            56 | 100 => Some(4),
            _ => None,
        },
    }
}

fn held_temporary_modifier_mask(pressed: &[Pressed]) -> u8 {
    pressed.iter().fold(0, |mask, value| match value {
        Pressed::Key(key) if is_temporary_modifier(key) => {
            mask | modifier_key_bit(key).unwrap_or(0)
        }
        _ => mask,
    })
}

fn terminal_acquire(operation: &TerminalAcquire) -> Value {
    json!({
        "ok": true,
        "state": "released",
        "operation_id": operation.operation_id,
        "lease": Value::Null,
        "reason": operation.reason,
        "native_pen_touch": operation.native_pen_touch,
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
