//! Caller-owned keyboard, mouse, and capability-gated gamepad input with leases.
mod backend;
pub mod companion_client;
mod controller;
mod feedback;
#[cfg(any(target_os = "linux", windows, test))]
mod gamepad;
mod gamepad_profile;
mod model;
pub mod shortcut;
mod shortcut_bridge;
mod shortcut_discovery;
mod shortcut_dispatch;
mod shortcut_job;
mod shortcut_journal;
mod shortcut_reservation;
mod shortcut_store;
pub use model::{Key as ShortcutKey, KeyState as ShortcutKeyState};
mod product_settings;
mod session_binding;

use amaterasu_input_policy::KeyBindings;
use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use backend::NativeBackend;
use controller::Controller;
use model::{InputError, MAX_QUEUED_REQUESTS, Operation};
use product_settings::{InputPolicyUpdate, ProductSettings};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

const SERVICE: &str = "amaterasu.input/1";
const CONFIGURATION_SERVICE: &str = "amaterasu.configuration/1";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    #[serde(default)]
    allow_injection: bool,
    #[serde(default = "default_uinput_path")]
    uinput_path: String,
    #[serde(default = "default_linux_companion_socket_path")]
    linux_companion_socket_path: String,
    #[serde(default)]
    windows_backend: Option<String>,
}
fn default_uinput_path() -> String {
    "/dev/uinput".into()
}
#[cfg(target_os = "linux")]
fn default_linux_companion_socket_path() -> String {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(|runtime| std::path::PathBuf::from(runtime).join("amaterasu-input.sock"))
        .unwrap_or_else(|| {
            let uid = unsafe { libc::geteuid() };
            std::path::PathBuf::from(format!("/run/user/{uid}/amaterasu-input.sock"))
        })
        .to_string_lossy()
        .into_owned()
}
#[cfg(not(target_os = "linux"))]
fn default_linux_companion_socket_path() -> String {
    String::new()
}
struct InputModule {
    runtime: RuntimeClient,
    controller: Controller<NativeBackend>,
    product_settings: ProductSettings,
    queued: VecDeque<QueuedRequest>,
    shortcut_job: Option<(String, shortcut_job::ShortcutJob)>,
    bindings: session_binding::Bindings,
    acquire_job: Option<session_binding::AcquireJob>,
    pending_revoke: Option<(QueuedRequest, session_binding::RevokeRequest)>,
}

struct QueuedRequest {
    id: u64,
    caller: String,
    service: String,
    payload: Value,
    deadline: Instant,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeliveryProofRequest {
    action: String,
    epoch: String,
    sequence: u64,
    operation_id: String,
    reservation_id: String,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum SettingsOperation {
    SettingsGet,
    SettingsUpdate {
        keyboard: bool,
        mouse: bool,
        #[serde(default)]
        controller: Option<bool>,
        #[serde(default)]
        high_resolution_scrolling: Option<bool>,
        #[serde(default)]
        always_send_scancodes: Option<bool>,
        #[serde(default)]
        key_rightalt_to_key_win: Option<bool>,
        #[serde(default, deserialize_with = "deserialize_keybindings_update")]
        keybindings: Option<KeyBindings>,
        #[serde(default)]
        key_repeat_delay: Option<u32>,
        #[serde(default)]
        key_repeat_frequency: Option<f64>,
        #[serde(default)]
        back_button_timeout: Option<i32>,
        #[serde(default)]
        gamepad: Option<gamepad_profile::GamepadProfile>,
        #[serde(default)]
        ds4_back_as_touchpad_click: Option<bool>,
        #[serde(default)]
        motion_as_ds4: Option<bool>,
        #[serde(default)]
        touchpad_as_ds4: Option<bool>,
        #[serde(default)]
        proton_dualsense_compatibility: Option<bool>,
        #[serde(default)]
        ds5_inputtino_randomize_mac: Option<bool>,
        #[serde(default)]
        native_pen_touch: Option<bool>,
        expected_revision: u64,
    },
}
// Only omission preserves the saved global list. A present field must be a
// validated flat byte list, including an explicit empty list; null is invalid.
fn deserialize_keybindings_update<'de, D>(deserializer: D) -> Result<Option<KeyBindings>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    KeyBindings::deserialize(deserializer).map(Some)
}

impl ManagedModule for InputModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("input module settings unavailable".into());
        };
        let settings: Settings = serde_json::from_value(payload)
            .map_err(|error| format!("invalid input settings: {error}"))?;
        #[cfg(target_os = "linux")]
        if !std::path::Path::new(&settings.linux_companion_socket_path).is_absolute() {
            return Err("linux_companion_socket_path must be absolute".into());
        }
        #[cfg(windows)]
        let backend = match settings.windows_backend.as_deref() {
            None | Some("native") => NativeBackend::new(settings.uinput_path),
            Some("vhf_companion") => NativeBackend::new_with_companion(true),
            Some(_) => return Err("unknown Windows input backend".into()),
        };
        #[cfg(not(windows))]
        let backend = {
            if settings.windows_backend.is_some() {
                return Err(
                    "Windows input backend selection is unavailable on this platform".into(),
                );
            }
            #[cfg(target_os = "linux")]
            {
                NativeBackend::new(settings.linux_companion_socket_path, settings.uinput_path)
            }
            #[cfg(not(target_os = "linux"))]
            {
                NativeBackend::new(settings.uinput_path)
            }
        };
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            controller: Controller::new(backend, settings.allow_injection),
            product_settings: ProductSettings::new(),
            queued: VecDeque::new(),
            shortcut_job: None,
            bindings: session_binding::Bindings::default(),
            acquire_job: None,
            pending_revoke: None,
        })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        self.controller.expire(Instant::now());
        let runtime = &self.runtime;
        self.product_settings
            .advance(&mut |command| runtime.send(command), &mut self.controller)?;
        self.advance_acquire()?;
        self.advance_shortcut()?;
        self.advance_revoke()?;
        self.drain_queued()?;
        for _ in 0..8 {
            let (id, caller, service, payload, remaining_ms) =
                match self.runtime.send(Command::Poll {})? {
                    Response::Pending => break,
                    Response::Request {
                        id,
                        caller,
                        service,
                        payload,
                        remaining_ms,
                        ..
                    } => (id, caller, service, payload, remaining_ms),
                    response => return Err(format!("input poll failed: {response:?}")),
                };
            self.accept_request(id, caller, service, payload, remaining_ms)?;
        }
        self.advance_acquire()?;
        self.advance_shortcut()?;
        self.advance_revoke()?;
        self.drain_queued()?;
        Ok(())
    }
}

impl InputModule {
    fn accept_request(
        &mut self,
        id: u64,
        caller: String,
        service: String,
        payload: Value,
        remaining_ms: u64,
    ) -> ModuleResult<()> {
        let request = QueuedRequest {
            id,
            caller,
            service,
            payload,
            deadline: Instant::now() + Duration::from_millis(remaining_ms),
        };
        // Fence immediately, even when a previously admitted batch is draining.
        if request.service == SERVICE
            && request.payload.get("action").and_then(Value::as_str) == Some("revoke_binding")
        {
            let revoke: Result<session_binding::RevokeRequest, _> =
                serde_json::from_value(request.payload.clone());
            let Ok(revoke) = revoke else {
                return self.complete(
                    request.id,
                    InputError::new("invalid_request", "invalid binding revocation").payload(),
                    &request.caller,
                    false,
                );
            };
            if revoke.action != "revoke_binding" {
                return self.complete(
                    request.id,
                    InputError::new("invalid_request", "invalid binding action").payload(),
                    &request.caller,
                    false,
                );
            }
            let transferred = if let Some(transfer) = &revoke.transfer {
                if request.caller != "amaterasu.session-lifecycle" {
                    return self.complete(
                        request.id,
                        InputError::new(
                            "permission_denied",
                            "transfer requires lifecycle coordinator",
                        )
                        .payload(),
                        &request.caller,
                        false,
                    );
                }
                let valid = self.shortcut_job.as_ref().is_some_and(|(_, job)| {
                    job.can_transfer(
                        &revoke.token,
                        &revoke.target,
                        &transfer.operation_id,
                        &transfer.reservation_id,
                    )
                });
                if !valid {
                    return self.complete(
                        request.id,
                        InputError::new(
                            "invalid_transfer",
                            "no matching delivered action reservation",
                        )
                        .payload(),
                        &request.caller,
                        false,
                    );
                }
                true
            } else {
                false
            };
            if let Err(code) = self.bindings.revoke(
                &request.caller,
                &revoke.operation_id,
                &revoke.token,
                revoke.target.clone(),
            ) {
                return self.complete(
                    request.id,
                    InputError::new(code, "binding revocation rejected").payload(),
                    &request.caller,
                    false,
                );
            }
            if transferred {
                self.shortcut_job
                    .as_mut()
                    .expect("validated transfer")
                    .1
                    .activate_transfer();
                return self.dispatch(request);
            }
            if self.queued.len() >= MAX_QUEUED_REQUESTS {
                return self.complete(
                    request.id,
                    InputError::new("resource_exhausted", "input queue full").payload(),
                    &request.caller,
                    false,
                );
            }
            self.queued.push_front(request);
            return self.drain_queued();
        }
        let defer = ((self.shortcut_job.is_some()
            || self.acquire_job.is_some()
            || self.pending_revoke.is_some())
            && !is_read_only_input(&request.payload))
            || should_defer_settings_request(
                &self.product_settings,
                &request.service,
                &request.payload,
            );
        if defer {
            if self.queued.len() >= MAX_QUEUED_REQUESTS {
                return self.complete(
                    request.id,
                    InputError::new("resource_exhausted", "input settings queue is full").payload(),
                    &request.caller,
                    false,
                );
            }
            self.queued.push_back(request);
            Ok(())
        } else {
            self.dispatch(request)
        }
    }

    fn drain_queued(&mut self) -> ModuleResult<()> {
        while self.product_settings.ready()
            && !self.product_settings.busy()
            && self.shortcut_job.is_none()
            && self.acquire_job.is_none()
            && self.pending_revoke.is_none()
        {
            let Some(request) = self.queued.pop_front() else {
                break;
            };
            if request.deadline <= Instant::now() {
                self.complete(
                    request.id,
                    InputError::new("deadline_exceeded", "request expired while queued").payload(),
                    &request.caller,
                    false,
                )?;
                continue;
            }
            self.dispatch(request)?;
        }
        Ok(())
    }

    fn dispatch(&mut self, request: QueuedRequest) -> ModuleResult<()> {
        if request.service != SERVICE {
            return self.complete(
                request.id,
                InputError::new("invalid_service", "unsupported input service").payload(),
                &request.caller,
                false,
            );
        }
        if request.payload.get("action").and_then(Value::as_str) == Some("confirm_delivery") {
            let proof: Result<DeliveryProofRequest, _> =
                serde_json::from_value(request.payload.clone());
            let response = if request.caller != "amaterasu.extension-actions" {
                InputError::new(
                    "permission_denied",
                    "delivery confirmation requires action service",
                )
                .payload()
            } else if let Ok(proof) = proof {
                if proof.action != "confirm_delivery" {
                    InputError::new("invalid_request", "invalid delivery confirmation").payload()
                } else {
                    self.shortcut_job
                        .as_ref()
                        .and_then(|(_, job)| {
                            job.confirm_delivery(
                                &proof.epoch,
                                proof.sequence,
                                &proof.operation_id,
                                &proof.reservation_id,
                            )
                        })
                        .unwrap_or_else(|| {
                            InputError::new("not_found", "confirmed delivery is unavailable")
                                .payload()
                        })
                }
            } else {
                InputError::new("invalid_request", "invalid delivery confirmation").payload()
            };
            return self.complete(request.id, response, &request.caller, false);
        }
        if request.payload.get("action").and_then(Value::as_str) == Some("binding_status") {
            let mut object = request.payload.clone();
            if let Some(fields) = object.as_object_mut() {
                fields.insert("action".into(), "revoke_binding".into());
            }
            let parsed: Result<session_binding::RevokeRequest, _> = serde_json::from_value(object);
            let reply = match parsed {
                Ok(status) => {
                    match self
                        .bindings
                        .status(&request.caller, &status.token, &status.target)
                    {
                        Ok((owner, lease, geometry))
                            if self.controller.bound_lease_active(owner, lease) =>
                        {
                            serde_json::json!({"ok":true,"status":"bound","token":status.token,"target":status.target,"lease":lease,"geometry_revision":geometry})
                        }
                        _ => InputError::new("binding_unavailable", "no active bound lease")
                            .payload(),
                    }
                }
                Err(_) => {
                    InputError::new("invalid_request", "invalid binding status request").payload()
                }
            };
            return self.complete(request.id, reply, &request.caller, false);
        }
        if request.payload.get("action").and_then(Value::as_str) == Some("revoke_binding") {
            let revoke: session_binding::RevokeRequest =
                serde_json::from_value(request.payload.clone()).map_err(|e| e.to_string())?;
            let (owner, lease) = self
                .bindings
                .revoke(
                    &request.caller,
                    &revoke.operation_id,
                    &revoke.token,
                    revoke.target.clone(),
                )
                .map_err(str::to_owned)?;
            if let Some(lease) = lease {
                let _ = self.controller.cancel_bound_lease(&owner, &lease);
            }
            self.pending_revoke = Some((request, revoke));
            return self.advance_revoke();
        }
        if request.payload.get("action").and_then(Value::as_str) == Some("acquire")
            && request.payload.get("lifecycle_binding_token").is_some()
        {
            let mut payload = request.payload.clone();
            let token = payload
                .as_object_mut()
                .and_then(|object| object.remove("lifecycle_binding_token"))
                .and_then(|v| v.as_str().map(str::to_owned));
            let Some(token) = token else {
                return self.complete(
                    request.id,
                    InputError::new("invalid_request", "invalid binding token").payload(),
                    &request.caller,
                    false,
                );
            };
            if let Err(code) = self.bindings.begin_acquire(&token, &request.caller) {
                return self.complete(
                    request.id,
                    InputError::new(code, "binding unavailable").payload(),
                    &request.caller,
                    false,
                );
            }
            match session_binding::AcquireJob::new(
                request.id,
                request.caller.clone(),
                token.clone(),
                payload,
                request.deadline,
            ) {
                Ok(job) => self.acquire_job = Some(job),
                Err(error) => {
                    self.bindings.abandon_acquire(&token);
                    return self.complete(
                        request.id,
                        InputError::new("binding_unavailable", error).payload(),
                        &request.caller,
                        false,
                    );
                }
            }
            return Ok(());
        }
        if request.payload.get("action").and_then(Value::as_str) == Some("batch") {
            let lease = request
                .payload
                .get("lease")
                .and_then(Value::as_str)
                .unwrap_or("");
            let token = request.payload.get("input_binding").and_then(Value::as_str);
            if let Err(code) = self.bindings.authorize_batch(&request.caller, lease, token) {
                return self.complete(
                    request.id,
                    InputError::new(code, "input binding is unavailable").payload(),
                    &request.caller,
                    false,
                );
            }
        }
        if let Some(action) = request.payload.get("action").and_then(Value::as_str)
            && matches!(action, "settings_get" | "settings_update")
        {
            if !authorized_settings_operation(&request.caller, action) {
                return self.complete(
                    request.id,
                    InputError::new(
                        "permission_denied",
                        "input settings require the management principal",
                    )
                    .payload(),
                    &request.caller,
                    false,
                );
            }
            let operation: Result<SettingsOperation, _> =
                serde_json::from_value(request.payload.clone());
            return match operation {
                Ok(SettingsOperation::SettingsGet) => {
                    let response = self.product_settings.get_response(&self.controller);
                    self.complete(request.id, response, &request.caller, false)
                }
                Ok(SettingsOperation::SettingsUpdate {
                    keyboard,
                    mouse,
                    controller,
                    high_resolution_scrolling,
                    always_send_scancodes,
                    key_rightalt_to_key_win,
                    keybindings,
                    key_repeat_delay,
                    key_repeat_frequency,
                    back_button_timeout,
                    gamepad,
                    ds4_back_as_touchpad_click,
                    motion_as_ds4,
                    touchpad_as_ds4,
                    proton_dualsense_compatibility,
                    ds5_inputtino_randomize_mac,
                    native_pen_touch,
                    expected_revision,
                }) => {
                    if let Err(error) = self.check_request(request.id) {
                        return self.complete(request.id, error.payload(), &request.caller, false);
                    }
                    let remaining_ms = request
                        .deadline
                        .saturating_duration_since(Instant::now())
                        .as_millis() as u64;
                    let desired = self.product_settings.merge_update(InputPolicyUpdate {
                        keyboard,
                        mouse,
                        controller,
                        high_resolution_scrolling,
                        always_send_scancodes,
                        key_rightalt_to_key_win,
                        keybindings,
                        key_repeat_delay,
                        key_repeat_frequency,
                        back_button_timeout,
                        gamepad,
                        ds4_back_as_touchpad_click,
                        motion_as_ds4,
                        touchpad_as_ds4,
                        proton_dualsense_compatibility,
                        ds5_inputtino_randomize_mac,
                        native_pen_touch,
                    });
                    match self.product_settings.begin_update(
                        request.id,
                        expected_revision,
                        desired,
                        remaining_ms,
                    ) {
                        Ok(()) => Ok(()),
                        Err(response) => {
                            self.complete(request.id, response, &request.caller, false)
                        }
                    }
                }
                Err(error) => self.complete(
                    request.id,
                    InputError::new("invalid_request", error.to_string()).payload(),
                    &request.caller,
                    false,
                ),
            };
        }
        if request.payload.get("action").and_then(Value::as_str) == Some("batch")
            && (request.payload.get("operation_id").is_some()
                || request.payload.get("input_binding").is_some())
        {
            let lease = request
                .payload
                .get("lease")
                .and_then(Value::as_str)
                .unwrap_or("");
            let token = request
                .payload
                .get("input_binding")
                .and_then(Value::as_str)
                .unwrap_or("");
            let target = match self.bindings.batch_target(&request.caller, lease, token) {
                Ok(target) => target,
                Err(code) => {
                    return self.complete(
                        request.id,
                        InputError::new(code, "input binding unavailable").payload(),
                        &request.caller,
                        false,
                    );
                }
            };
            return match shortcut_job::ShortcutJob::new(
                request.id,
                request.caller.clone(),
                request.payload,
                target,
                request.deadline,
            ) {
                Ok(job) => {
                    self.shortcut_job = Some((request.caller, job));
                    Ok(())
                }
                Err(message) => self.complete(
                    request.id,
                    InputError::new("invalid_request", message).payload(),
                    &request.caller,
                    false,
                ),
            };
        }
        let operation: Result<Operation, _> = serde_json::from_value(request.payload);
        match operation {
            Ok(operation) => {
                let runtime = &self.runtime;
                let mut check = || match runtime.send(Command::CheckRequest {
                    request: request.id,
                }) {
                    Ok(Response::Ok) => Ok(()),
                    Ok(Response::Error { message, .. }) => {
                        Err(InputError::new("cancelled", message))
                    }
                    Ok(_) => Err(InputError::new(
                        "runtime_error",
                        "unexpected request guard reply",
                    )),
                    Err(error) => Err(InputError::new("runtime_error", error)),
                };
                let cleanup_if_completion_is_lost = !matches!(
                    &operation,
                    Operation::Status
                        | Operation::Lookup { .. }
                        | Operation::FeedbackPeek { .. }
                        | Operation::FeedbackAck { .. }
                        | Operation::Acquire {
                            operation_id: Some(_),
                            ..
                        }
                );
                let reply =
                    self.controller
                        .handle(&request.caller, operation, Instant::now(), &mut check);
                let cleanup = completion_requires_cleanup(cleanup_if_completion_is_lost, &reply);
                self.complete(request.id, reply, &request.caller, cleanup)
            }
            Err(error) => self.complete(
                request.id,
                InputError::new("invalid_request", error.to_string()).payload(),
                &request.caller,
                false,
            ),
        }
    }

    fn advance_acquire(&mut self) -> ModuleResult<()> {
        let Some(mut job) = self.acquire_job.take() else {
            return Ok(());
        };
        if self.bindings.is_revoked(&job.token) {
            let runtime = &self.runtime;
            job.cancel(&mut |command| runtime.send(command));
            return self.complete(
                job.parent,
                InputError::new("binding_retired", "binding was revoked").payload(),
                &job.owner,
                false,
            );
        }
        let runtime = &self.runtime;
        match job.advance(&mut |command| runtime.send(command)) {
            Ok(None) => {
                self.acquire_job = Some(job);
                Ok(())
            }
            Err(error) => {
                self.bindings.abandon_acquire(&job.token);
                self.complete(
                    job.parent,
                    InputError::new("binding_unavailable", error).payload(),
                    &job.owner,
                    false,
                )
            }
            Ok(Some(authority)) => {
                if let Err(error) = self.check_request(job.parent) {
                    self.bindings.abandon_acquire(&job.token);
                    return self.complete(job.parent, error.payload(), &job.owner, false);
                }
                let operation: Operation = match serde_json::from_value(job.payload.clone()) {
                    Ok(operation @ Operation::Acquire { .. }) => operation,
                    _ => {
                        self.bindings.abandon_acquire(&job.token);
                        return self.complete(
                            job.parent,
                            InputError::new("invalid_request", "invalid bound acquisition")
                                .payload(),
                            &job.owner,
                            false,
                        );
                    }
                };
                let runtime = &self.runtime;
                let mut guard = || match runtime.send(Command::CheckRequest {
                    request: job.parent,
                }) {
                    Ok(Response::Ok) => Ok(()),
                    _ => Err(InputError::new("cancelled", "acquisition cancelled")),
                };
                let mut reply =
                    self.controller
                        .handle(&job.owner, operation, Instant::now(), &mut guard);
                if reply.get("ok").and_then(Value::as_bool) == Some(true) {
                    let lease = reply
                        .get("lease")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    if self
                        .bindings
                        .bind(
                            &job.token,
                            &job.owner,
                            &lease,
                            authority.target.clone(),
                            authority.geometry_revision,
                        )
                        .is_err()
                    {
                        self.controller.cancel_owner(&job.owner);
                        return self.complete(
                            job.parent,
                            InputError::new("binding_unavailable", "could not bind acquired lease")
                                .payload(),
                            &job.owner,
                            false,
                        );
                    }
                    reply["input_binding"] = job.token.clone().into();
                    reply["target"] =
                        serde_json::to_value(authority.target).map_err(|e| e.to_string())?;
                    reply["geometry_revision"] = authority.geometry_revision.into();
                    reply["batch_epoch"] = authority.batch_epoch.into();
                    reply["input_acknowledged"] = true.into();
                    self.complete(job.parent, reply, &job.owner, true)
                } else {
                    self.bindings.abandon_acquire(&job.token);
                    self.complete(job.parent, reply, &job.owner, false)
                }
            }
        }
    }

    fn advance_revoke(&mut self) -> ModuleResult<()> {
        if self.controller.cleanup_required() {
            return Ok(());
        }
        let Some((request, revoke)) = self.pending_revoke.take() else {
            return Ok(());
        };
        self.complete(request.id, serde_json::json!({"ok":true,"status":"retired","token":revoke.token,"target":revoke.target}), &request.caller, false)
    }

    fn advance_shortcut(&mut self) -> ModuleResult<()> {
        let Some((caller, mut job)) = self.shortcut_job.take() else {
            return Ok(());
        };
        let request = job.request_id();
        let (lease, binding) = job.binding_identity();
        let lease = lease.to_owned();
        let binding = binding.to_owned();
        if self
            .bindings
            .authorize_batch(&caller, &lease, Some(&binding))
            .is_err()
            && !job.is_transferred()
        {
            let armed = job.armed();
            if armed {
                self.controller.cancel_owner(&caller);
            }
            let mut payload =
                InputError::new("binding_retired", "input binding was revoked").payload();
            payload["recovery_required"] = armed.into();
            return self.complete(request, payload, &caller, false);
        }
        let runtime = &self.runtime;
        let bindings = &self.bindings;
        let mut send = |command| runtime.send(command);
        let mut guard = || match runtime.send(Command::CheckRequest { request }) {
            Ok(Response::Ok) => bindings
                .authorize_batch(&caller, &lease, Some(&binding))
                .map_err(|code| InputError::new(code, "input binding revoked")),
            Ok(Response::Error { message, .. }) => Err(InputError::new("cancelled", message)),
            _ => Err(InputError::new(
                "runtime_error",
                "shortcut request guard failed",
            )),
        };
        match job.advance(&mut self.controller, &mut send, &mut guard) {
            Ok(None) => {
                self.shortcut_job = Some((caller, job));
                Ok(())
            }
            Ok(Some(payload)) => self.complete(request, payload, &caller, false),
            Err(message) => {
                let armed = job.armed();
                if armed {
                    self.controller.cancel_owner(&caller);
                }
                let code = if message == "shortcut receipt expired" {
                    "receipt_expired"
                } else {
                    "shortcut_unavailable"
                };
                let mut payload = InputError::new(code, message).payload();
                payload["recovery_required"] = armed.into();
                self.complete(request, payload, &caller, false)
            }
        }
    }

    fn check_request(&self, request: u64) -> Result<(), InputError> {
        match self.runtime.send(Command::CheckRequest { request }) {
            Ok(Response::Ok) => Ok(()),
            Ok(Response::Error { message, .. }) => Err(InputError::new("cancelled", message)),
            Ok(_) => Err(InputError::new(
                "runtime_error",
                "unexpected request guard reply",
            )),
            Err(error) => Err(InputError::new("runtime_error", error)),
        }
    }

    fn complete(
        &mut self,
        request: u64,
        payload: Value,
        caller: &str,
        cleanup_on_loss: bool,
    ) -> ModuleResult<()> {
        match self.runtime.send(Command::Complete { request, payload }) {
            Ok(Response::Ok) => Ok(()),
            Ok(Response::Error { code, .. }) if code == "request_ended" || code == "not_found" => {
                if cleanup_on_loss {
                    self.controller.cancel_owner(caller);
                }
                Ok(())
            }
            failure => {
                if cleanup_on_loss {
                    self.controller.cancel_owner(caller);
                }
                Err(format!("input completion failed: {failure:?}"))
            }
        }
    }
}

fn is_read_only_input(payload: &Value) -> bool {
    matches!(
        payload.get("action").and_then(Value::as_str),
        Some("status" | "lookup" | "settings_get" | "confirm_delivery")
    )
}

fn authorized_settings_operation(caller: &str, action: &str) -> bool {
    caller == "amaterasu.management" || (caller == "amaterasu.nvhttp" && action == "settings_get")
}

fn should_defer_settings_request(
    product_settings: &ProductSettings,
    service: &str,
    payload: &Value,
) -> bool {
    service == SERVICE
        && !is_read_only_input(payload)
        && (product_settings.busy() || !product_settings.ready())
}
// A rejected request may refer to a lease that predates the request. Losing
// its error reply must not revoke that lease. Failed native operations already
// attempt cleanup; unresolved releases still warrant another cleanup pass.
fn completion_requires_cleanup(mutating: bool, reply: &serde_json::Value) -> bool {
    mutating && (reply["ok"] == true || reply["cleanup_required"] == true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn settings_reads_do_not_wait_behind_the_startup_load() {
        let settings = ProductSettings::new();

        assert!(!should_defer_settings_request(
            &settings,
            SERVICE,
            &json!({"action":"settings_get"}),
        ));
        assert!(should_defer_settings_request(
            &settings,
            SERVICE,
            &json!({"action":"settings_update","keyboard":false}),
        ));
        assert!(!should_defer_settings_request(
            &settings,
            SERVICE,
            &json!({"action":"status"}),
        ));
    }

    #[test]
    fn nvhttp_can_only_read_global_input_settings_for_launch_resolution() {
        assert!(authorized_settings_operation(
            "amaterasu.nvhttp",
            "settings_get"
        ));
        assert!(!authorized_settings_operation(
            "amaterasu.nvhttp",
            "settings_update"
        ));
        assert!(!authorized_settings_operation(
            "untrusted.caller",
            "settings_get"
        ));
        assert!(authorized_settings_operation(
            "amaterasu.management",
            "settings_update"
        ));
    }

    #[test]
    fn settings_keybindings_preserve_omission_and_validate_explicit_replacement() {
        for supplied in [None, Some(json!([])), Some(json!([0, 255, 65, 66]))] {
            let mut request = json!({"action":"settings_update", "keyboard":true, "mouse":true, "expected_revision":2});
            if let Some(value) = supplied.clone() {
                request["keybindings"] = value;
            }
            let operation: SettingsOperation = serde_json::from_value(request).unwrap();
            let SettingsOperation::SettingsUpdate { keybindings, .. } = operation else {
                panic!("wrong operation");
            };
            assert_eq!(
                serde_json::to_value(keybindings).unwrap(),
                supplied.unwrap_or(serde_json::Value::Null)
            );
        }
        for invalid in [
            serde_json::Value::Null,
            json!([0]),
            json!([0, 256]),
            json!([-1, 0]),
            json!([0.0, 1]),
            json!(vec![0_u8; 514]),
        ] {
            assert!(serde_json::from_value::<SettingsOperation>(json!({"action":"settings_update", "keyboard":true, "mouse":true, "expected_revision":2, "keybindings":invalid})).is_err());
        }
    }

    #[test]
    fn legacy_settings_update_shape_omits_only_the_new_scroll_field() {
        let legacy: SettingsOperation = serde_json::from_value(json!({
            "action":"settings_update",
            "keyboard":false,
            "mouse":true,
            "expected_revision":4,
        }))
        .unwrap();
        assert!(matches!(
            legacy,
            SettingsOperation::SettingsUpdate {
                high_resolution_scrolling: None,
                always_send_scancodes: None,
                key_rightalt_to_key_win: None,
                key_repeat_delay: None,
                key_repeat_frequency: None,
                back_button_timeout: None,
                gamepad: None,
                controller: None,
                native_pen_touch: None,
                ..
            }
        ));

        let current: SettingsOperation = serde_json::from_value(json!({
            "action":"settings_update",
            "keyboard":false,
            "mouse":true,
            "high_resolution_scrolling":false,
            "always_send_scancodes":false,
            "key_rightalt_to_key_win":false,
            "proton_dualsense_compatibility":false,
            "ds5_inputtino_randomize_mac":false,
            "key_repeat_delay":250,
            "key_repeat_frequency":30.5,
            "back_button_timeout":-1,
            "gamepad":"auto",
            "controller":false,
            "native_pen_touch":false,
            "expected_revision":4,
        }))
        .unwrap();
        assert!(matches!(
            current,
            SettingsOperation::SettingsUpdate {
                high_resolution_scrolling: Some(false),
                always_send_scancodes: Some(false),
                key_rightalt_to_key_win: Some(false),
                proton_dualsense_compatibility: Some(false),
                ds5_inputtino_randomize_mac: Some(false),
                key_repeat_delay: Some(250),
                key_repeat_frequency: Some(30.5),
                back_button_timeout: Some(-1),
                gamepad: Some(gamepad_profile::GamepadProfile::Auto),
                controller: Some(false),
                native_pen_touch: Some(false),
                ..
            }
        ));
    }
}

amaterasu_module_sdk::export_module!(InputModule);
