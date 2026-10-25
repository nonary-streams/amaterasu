//! Caller-owned keyboard, mouse, and capability-gated gamepad input with leases.
mod backend;
pub mod companion_client;
mod controller;
mod feedback;
#[cfg(any(target_os = "linux", windows, test))]
mod gamepad;
mod gamepad_profile;
mod model;
mod product_settings;

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
}

struct QueuedRequest {
    id: u64,
    caller: String,
    service: String,
    payload: Value,
    deadline: Instant,
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
        })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        self.controller.expire(Instant::now());
        let runtime = &self.runtime;
        self.product_settings
            .advance(&mut |command| runtime.send(command), &mut self.controller)?;
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
        let defer = should_defer_settings_request(
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
        while self.product_settings.ready() && !self.product_settings.busy() {
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
        Some("status" | "lookup" | "settings_get")
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
