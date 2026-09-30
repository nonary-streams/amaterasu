//! Durable input policy and asynchronous configuration RPC lifecycle.
use crate::controller::Controller;
use crate::gamepad_profile::GamepadProfile;
use crate::model::DevicePolicy;
use crate::{CONFIGURATION_SERVICE, backend::Backend};
use amaterasu_input_policy::KeyBindings;
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const PRODUCT_SETTINGS_VERSION: u32 = 14;
const CONFIGURATION_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_KEY_REPEAT_DELAY_MS: u32 = 500;
const DEFAULT_KEY_REPEAT_FREQUENCY_HZ: f64 = 24.9;
type Sender<'a> = &'a mut dyn FnMut(Command) -> ModuleResult<Response>;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InputPolicy {
    #[serde(default = "default_enabled")]
    pub keyboard: bool,
    #[serde(default = "default_enabled")]
    pub mouse: bool,
    #[serde(default = "default_enabled")]
    pub controller: bool,
    #[serde(default = "default_enabled")]
    pub high_resolution_scrolling: bool,
    #[serde(default = "default_enabled")]
    pub always_send_scancodes: bool,
    #[serde(default)]
    pub key_rightalt_to_key_win: bool,
    #[serde(default)]
    pub keybindings: KeyBindings,
    #[serde(default = "default_key_repeat_delay")]
    pub key_repeat_delay: u32,
    #[serde(default = "default_key_repeat_frequency")]
    pub key_repeat_frequency: f64,
    #[serde(default = "default_back_button_timeout")]
    pub back_button_timeout: i32,
    #[serde(default)]
    pub gamepad: GamepadProfile,
    #[serde(default = "default_enabled")]
    pub ds4_back_as_touchpad_click: bool,
    #[serde(default = "default_enabled")]
    pub motion_as_ds4: bool,
    #[serde(default = "default_enabled")]
    pub touchpad_as_ds4: bool,
    #[serde(default = "default_enabled")]
    pub proton_dualsense_compatibility: bool,
    #[serde(default = "default_enabled")]
    pub ds5_inputtino_randomize_mac: bool,
    #[serde(default = "default_enabled")]
    pub native_pen_touch: bool,
}

#[derive(Clone, Debug, Default)]
pub struct InputPolicyUpdate {
    pub keyboard: bool,
    pub mouse: bool,
    pub controller: Option<bool>,
    pub high_resolution_scrolling: Option<bool>,
    pub always_send_scancodes: Option<bool>,
    pub key_rightalt_to_key_win: Option<bool>,
    pub keybindings: Option<KeyBindings>,
    pub key_repeat_delay: Option<u32>,
    pub key_repeat_frequency: Option<f64>,
    pub back_button_timeout: Option<i32>,
    pub gamepad: Option<GamepadProfile>,
    pub ds4_back_as_touchpad_click: Option<bool>,
    pub motion_as_ds4: Option<bool>,
    pub touchpad_as_ds4: Option<bool>,
    pub proton_dualsense_compatibility: Option<bool>,
    pub ds5_inputtino_randomize_mac: Option<bool>,
    pub native_pen_touch: Option<bool>,
}

const fn default_enabled() -> bool {
    true
}

impl InputPolicy {
    fn device_policy(&self) -> DevicePolicy {
        DevicePolicy {
            keyboard: self.keyboard,
            mouse: self.mouse,
        }
    }

    fn values(&self) -> Value {
        json!({
            "keyboard":self.keyboard,
            "mouse":self.mouse,
            "controller":self.controller,
            "high_resolution_scrolling":self.high_resolution_scrolling,
            "always_send_scancodes":self.always_send_scancodes,
            "key_rightalt_to_key_win":self.key_rightalt_to_key_win,
            "keybindings":self.keybindings,
            "key_repeat_delay":self.key_repeat_delay,
            "key_repeat_frequency":self.key_repeat_frequency,
            "back_button_timeout":self.back_button_timeout,
            "gamepad":self.gamepad.as_str(),
            "ds4_back_as_touchpad_click":self.ds4_back_as_touchpad_click,
            "motion_as_ds4":self.motion_as_ds4,
            "touchpad_as_ds4":self.touchpad_as_ds4,
            "proton_dualsense_compatibility":self.proton_dualsense_compatibility,
            "ds5_inputtino_randomize_mac":self.ds5_inputtino_randomize_mac,
            "native_pen_touch":self.native_pen_touch,
        })
    }

    fn validate(&self) -> Result<(), &'static str> {
        if self.key_repeat_delay > i32::MAX as u32 {
            return Err("key_repeat_delay must not exceed 2147483647 milliseconds");
        }
        if !self.key_repeat_frequency.is_finite() || self.key_repeat_frequency < 0.0 {
            return Err("key_repeat_frequency must be a finite nonnegative number");
        }
        Ok(())
    }

    fn update_payload(&self, expected_revision: u64) -> Value {
        json!({
            "action":"product_update",
            "expected_revision":expected_revision,
            "version":PRODUCT_SETTINGS_VERSION,
            "values":self.values(),
        })
    }
}

const fn default_key_repeat_delay() -> u32 {
    DEFAULT_KEY_REPEAT_DELAY_MS
}

const fn default_key_repeat_frequency() -> f64 {
    DEFAULT_KEY_REPEAT_FREQUENCY_HZ
}

const fn default_back_button_timeout() -> i32 {
    -1
}

impl Default for InputPolicy {
    fn default() -> Self {
        Self {
            keyboard: true,
            mouse: true,
            controller: true,
            high_resolution_scrolling: true,
            always_send_scancodes: true,
            key_rightalt_to_key_win: false,
            keybindings: KeyBindings::default(),
            key_repeat_delay: DEFAULT_KEY_REPEAT_DELAY_MS,
            key_repeat_frequency: DEFAULT_KEY_REPEAT_FREQUENCY_HZ,
            back_button_timeout: default_back_button_timeout(),
            gamepad: GamepadProfile::Auto,
            ds4_back_as_touchpad_click: true,
            motion_as_ds4: true,
            touchpad_as_ds4: true,
            proton_dualsense_compatibility: true,
            ds5_inputtino_randomize_mac: true,
            native_pen_touch: true,
        }
    }
}

enum Work {
    StartupLoad,
    Update {
        management_request: u64,
        expected_revision: u64,
        desired: InputPolicy,
    },
    ReconcileUpdate {
        management_request: u64,
        expected_revision: u64,
        desired: InputPolicy,
    },
}

enum ChildOutcome {
    Payload(Value),
    Failure,
}

enum Call {
    Submit {
        payload: Value,
        operation_id: String,
        parent: Option<u64>,
        retry_ticks: u8,
    },
    Waiting {
        request: u64,
    },
    Releasing {
        request: u64,
        outcome: ChildOutcome,
    },
}

struct Pending {
    work: Work,
    call: Call,
}

pub struct ProductSettings {
    policy: InputPolicy,
    revision: u64,
    ready: bool,
    pending: Option<Pending>,
}

impl ProductSettings {
    pub fn new() -> Self {
        Self {
            policy: InputPolicy::default(),
            revision: 0,
            ready: false,
            pending: Some(Pending {
                work: Work::StartupLoad,
                call: Call::Submit {
                    payload: json!({"action":"product_get"}),
                    operation_id: "input.product-settings.startup".into(),
                    parent: None,
                    retry_ticks: 0,
                },
            }),
        }
    }

    pub fn ready(&self) -> bool {
        self.ready
    }

    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    pub fn merge_update(&self, update: InputPolicyUpdate) -> InputPolicy {
        InputPolicy {
            keyboard: update.keyboard,
            mouse: update.mouse,
            controller: update.controller.unwrap_or(self.policy.controller),
            high_resolution_scrolling: update
                .high_resolution_scrolling
                .unwrap_or(self.policy.high_resolution_scrolling),
            always_send_scancodes: update
                .always_send_scancodes
                .unwrap_or(self.policy.always_send_scancodes),
            key_rightalt_to_key_win: update
                .key_rightalt_to_key_win
                .unwrap_or(self.policy.key_rightalt_to_key_win),
            keybindings: update
                .keybindings
                .unwrap_or_else(|| self.policy.keybindings.clone()),
            key_repeat_delay: update
                .key_repeat_delay
                .unwrap_or(self.policy.key_repeat_delay),
            key_repeat_frequency: update
                .key_repeat_frequency
                .unwrap_or(self.policy.key_repeat_frequency),
            back_button_timeout: update
                .back_button_timeout
                .unwrap_or(self.policy.back_button_timeout),
            gamepad: update.gamepad.unwrap_or(self.policy.gamepad),
            ds4_back_as_touchpad_click: update
                .ds4_back_as_touchpad_click
                .unwrap_or(self.policy.ds4_back_as_touchpad_click),
            motion_as_ds4: update.motion_as_ds4.unwrap_or(self.policy.motion_as_ds4),
            touchpad_as_ds4: update
                .touchpad_as_ds4
                .unwrap_or(self.policy.touchpad_as_ds4),
            proton_dualsense_compatibility: update
                .proton_dualsense_compatibility
                .unwrap_or(self.policy.proton_dualsense_compatibility),
            ds5_inputtino_randomize_mac: update
                .ds5_inputtino_randomize_mac
                .unwrap_or(self.policy.ds5_inputtino_randomize_mac),
            native_pen_touch: update
                .native_pen_touch
                .unwrap_or(self.policy.native_pen_touch),
        }
    }

    pub fn get_response<B: Backend>(&self, controller: &Controller<B>) -> Value {
        if !self.ready {
            return error("settings_unavailable");
        }
        self.response(controller, true)
    }

    pub fn begin_update(
        &mut self,
        request: u64,
        expected_revision: u64,
        desired: InputPolicy,
        remaining_ms: u64,
    ) -> Result<(), Value> {
        if !self.ready {
            return Err(error("settings_unavailable"));
        }
        if self.pending.is_some() {
            return Err(error("settings_busy"));
        }
        if expected_revision != self.revision {
            return Err(json!({
                "ok":false,
                "error":{"code":"revision_conflict","current_revision":self.revision}
            }));
        }
        if let Err(message) = desired.validate() {
            return Err(json!({"ok":false,"error":{"code":"invalid_settings","message":message}}));
        }
        let timeout_ms = remaining_ms.min(CONFIGURATION_TIMEOUT_MS);
        if timeout_ms == 0 {
            return Err(error("deadline_exceeded"));
        }
        self.pending = Some(Pending {
            work: Work::Update {
                management_request: request,
                expected_revision,
                desired: desired.clone(),
            },
            call: Call::Submit {
                payload: desired.update_payload(expected_revision),
                operation_id: format!("input.product-settings.update.{request}"),
                parent: Some(request),
                retry_ticks: 0,
            },
        });
        Ok(())
    }

    pub fn advance<B: Backend>(
        &mut self,
        send: Sender<'_>,
        controller: &mut Controller<B>,
    ) -> ModuleResult<()> {
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        match pending.call {
            Call::Submit {
                payload,
                operation_id,
                parent,
                retry_ticks,
            } => {
                if retry_ticks > 0 {
                    self.pending = Some(Pending {
                        work: pending.work,
                        call: Call::Submit {
                            payload,
                            operation_id,
                            parent,
                            retry_ticks: retry_ticks - 1,
                        },
                    });
                    return Ok(());
                }
                let timeout_ms = CONFIGURATION_TIMEOUT_MS;
                match send(Command::Request {
                    service: CONFIGURATION_SERVICE.into(),
                    payload: payload.clone(),
                    timeout_ms,
                    operation_id: operation_id.clone(),
                    parent,
                }) {
                    Ok(Response::Accepted { id }) => {
                        self.pending = Some(Pending {
                            work: pending.work,
                            call: Call::Waiting { request: id },
                        });
                    }
                    Ok(Response::Error { code, .. }) => {
                        self.on_submit_error(
                            send,
                            pending.work,
                            payload,
                            operation_id,
                            parent,
                            code,
                        )?;
                    }
                    Ok(_response) => {
                        self.on_terminal(send, controller, pending.work, ChildOutcome::Failure)?;
                    }
                    Err(_) => {
                        // Repeating the same operation id retrieves the existing request if the
                        // transport accepted it before the local acknowledgement was lost.
                        self.pending = Some(Pending {
                            work: pending.work,
                            call: Call::Submit {
                                payload,
                                operation_id,
                                parent,
                                retry_ticks: 1,
                            },
                        });
                    }
                }
            }
            Call::Waiting { request } => match send(Command::TakeResult { request }) {
                Ok(Response::Pending) => {
                    self.pending = Some(Pending {
                        work: pending.work,
                        call: Call::Waiting { request },
                    });
                }
                Err(_) => {
                    self.pending = Some(Pending {
                        work: pending.work,
                        call: Call::Waiting { request },
                    });
                }
                Ok(Response::Error { code, .. }) if code == "not_found" => {
                    self.on_child_missing(send, pending.work)?;
                }
                Ok(Response::Result { payload }) => {
                    self.pending = Some(Pending {
                        work: pending.work,
                        call: Call::Releasing {
                            request,
                            outcome: ChildOutcome::Payload(payload),
                        },
                    });
                }
                Ok(Response::Error { .. }) => {
                    self.pending = Some(Pending {
                        work: pending.work,
                        call: Call::Releasing {
                            request,
                            outcome: ChildOutcome::Failure,
                        },
                    });
                }
                Ok(_response) => {
                    self.pending = Some(Pending {
                        work: pending.work,
                        call: Call::Releasing {
                            request,
                            outcome: ChildOutcome::Failure,
                        },
                    });
                }
            },
            Call::Releasing { request, outcome } => {
                match send(Command::ReleaseResult { request }) {
                    Ok(Response::Ok) => {
                        self.on_terminal(send, controller, pending.work, outcome)?;
                    }
                    Ok(Response::Error { code, .. }) if code == "not_found" => {
                        self.on_terminal(send, controller, pending.work, outcome)?;
                    }
                    _ => {
                        self.pending = Some(Pending {
                            work: pending.work,
                            call: Call::Releasing { request, outcome },
                        });
                    }
                }
            }
        }
        Ok(())
    }

    fn on_submit_error(
        &mut self,
        send: Sender<'_>,
        work: Work,
        payload: Value,
        operation_id: String,
        parent: Option<u64>,
        code: String,
    ) -> ModuleResult<()> {
        match work {
            Work::StartupLoad => {
                self.pending = Some(Pending {
                    work: Work::StartupLoad,
                    call: Call::Submit {
                        payload,
                        operation_id,
                        parent,
                        retry_ticks: 30,
                    },
                });
            }
            Work::Update {
                management_request, ..
            } => complete(send, management_request, error(&safe_code(&code)))?,
            Work::ReconcileUpdate {
                management_request,
                expected_revision,
                desired,
            } => self.retry_reconciliation(management_request, expected_revision, desired, 10),
        }
        Ok(())
    }

    fn on_child_missing(&mut self, _send: Sender<'_>, work: Work) -> ModuleResult<()> {
        match work {
            Work::StartupLoad => self.retry_startup_load(10),
            Work::Update {
                management_request,
                expected_revision,
                desired,
            }
            | Work::ReconcileUpdate {
                management_request,
                expected_revision,
                desired,
            } => self.start_reconciliation(management_request, expected_revision, desired),
        }
        Ok(())
    }

    fn on_terminal<B: Backend>(
        &mut self,
        send: Sender<'_>,
        controller: &mut Controller<B>,
        work: Work,
        outcome: ChildOutcome,
    ) -> ModuleResult<()> {
        match work {
            Work::StartupLoad => match outcome {
                ChildOutcome::Payload(payload) => match read_document(&payload) {
                    Ok((policy, revision)) => {
                        self.policy = policy.clone();
                        self.revision = revision;
                        self.ready = true;
                        controller.set_high_resolution_scrolling(policy.high_resolution_scrolling);
                        controller.set_always_send_scancodes(policy.always_send_scancodes);
                        controller.set_key_rightalt_to_key_win(policy.key_rightalt_to_key_win);
                        controller.set_keybindings(&policy.keybindings);
                        controller.set_key_repeat_policy(
                            policy.key_repeat_delay,
                            policy.key_repeat_frequency,
                        );
                        controller.set_back_button_timeout_ms(policy.back_button_timeout);
                        controller.set_gamepad_profile(policy.gamepad);
                        controller
                            .set_ds4_back_as_touchpad_click(policy.ds4_back_as_touchpad_click);
                        controller.set_motion_as_ds4(policy.motion_as_ds4);
                        controller.set_touchpad_as_ds4(policy.touchpad_as_ds4);
                        controller
                            .set_ds5_inputtino_randomize_mac(policy.ds5_inputtino_randomize_mac);
                        controller.set_native_pen_touch(policy.native_pen_touch);
                        let _ = controller
                            .set_controller_enabled(policy.controller, std::time::Instant::now());
                        let _ = controller
                            .set_device_policy(policy.device_policy(), std::time::Instant::now());
                    }
                    Err(_) => self.retry_startup_load(30),
                },
                ChildOutcome::Failure => self.retry_startup_load(30),
            },
            Work::Update {
                management_request,
                expected_revision,
                desired,
            } => match outcome {
                ChildOutcome::Payload(payload) if payload["ok"] == true => {
                    match read_document(&payload) {
                        Ok((actual, revision))
                            if expected_revision.checked_add(1) == Some(revision)
                                && actual == desired =>
                        {
                            self.apply_update(
                                send,
                                controller,
                                management_request,
                                actual,
                                revision,
                            )?;
                        }
                        _ => self.start_reconciliation(
                            management_request,
                            expected_revision,
                            desired,
                        ),
                    }
                }
                ChildOutcome::Payload(payload) => {
                    if let Some(failure) = product_failure(&payload) {
                        complete(send, management_request, failure)?;
                    } else {
                        self.start_reconciliation(management_request, expected_revision, desired);
                    }
                }
                ChildOutcome::Failure => {
                    self.start_reconciliation(management_request, expected_revision, desired)
                }
            },
            Work::ReconcileUpdate {
                management_request,
                expected_revision,
                desired,
            } => match outcome {
                ChildOutcome::Payload(payload) => match read_document(&payload) {
                    Ok((actual, revision))
                        if expected_revision.checked_add(1) == Some(revision)
                            && actual == desired =>
                    {
                        self.apply_update(send, controller, management_request, actual, revision)?;
                    }
                    Ok((actual, revision)) => {
                        // Reconciliation establishes the currently durable policy even when the
                        // write outcome is uncertain, but never reports the attempted update as
                        // successful unless its exact next revision is present.
                        self.policy = actual.clone();
                        self.revision = revision;
                        self.ready = true;
                        controller.set_high_resolution_scrolling(actual.high_resolution_scrolling);
                        controller.set_always_send_scancodes(actual.always_send_scancodes);
                        controller.set_key_rightalt_to_key_win(actual.key_rightalt_to_key_win);
                        controller.set_keybindings(&actual.keybindings);
                        controller.set_key_repeat_policy(
                            actual.key_repeat_delay,
                            actual.key_repeat_frequency,
                        );
                        controller.set_back_button_timeout_ms(actual.back_button_timeout);
                        controller.set_gamepad_profile(actual.gamepad);
                        controller
                            .set_ds4_back_as_touchpad_click(actual.ds4_back_as_touchpad_click);
                        controller.set_motion_as_ds4(actual.motion_as_ds4);
                        controller.set_touchpad_as_ds4(actual.touchpad_as_ds4);
                        controller
                            .set_ds5_inputtino_randomize_mac(actual.ds5_inputtino_randomize_mac);
                        controller.set_native_pen_touch(actual.native_pen_touch);
                        let now = std::time::Instant::now();
                        let controller_cleanup = controller
                            .set_controller_enabled(actual.controller, now)
                            .is_err();
                        let device_cleanup = controller
                            .set_device_policy(actual.device_policy(), std::time::Instant::now())
                            .is_err();
                        let cleanup = controller_cleanup || device_cleanup;
                        complete(
                            send,
                            management_request,
                            json!({"ok":false,"error":{"code":"write_unresolved"},"persisted":false,"revision":revision,"values":actual.values(),"applied":!cleanup,"cleanup_required":cleanup}),
                        )?;
                    }
                    Err(_) => self.retry_reconciliation(
                        management_request,
                        expected_revision,
                        desired,
                        10,
                    ),
                },
                ChildOutcome::Failure => {
                    self.retry_reconciliation(management_request, expected_revision, desired, 10)
                }
            },
        }
        Ok(())
    }

    fn apply_update<B: Backend>(
        &mut self,
        send: Sender<'_>,
        controller: &mut Controller<B>,
        request: u64,
        policy: InputPolicy,
        revision: u64,
    ) -> ModuleResult<()> {
        self.policy = policy.clone();
        self.revision = revision;
        self.ready = true;
        controller.set_high_resolution_scrolling(policy.high_resolution_scrolling);
        controller.set_always_send_scancodes(policy.always_send_scancodes);
        controller.set_key_rightalt_to_key_win(policy.key_rightalt_to_key_win);
        controller.set_keybindings(&policy.keybindings);
        controller.set_key_repeat_policy(policy.key_repeat_delay, policy.key_repeat_frequency);
        controller.set_back_button_timeout_ms(policy.back_button_timeout);
        controller.set_gamepad_profile(policy.gamepad);
        controller.set_ds4_back_as_touchpad_click(policy.ds4_back_as_touchpad_click);
        controller.set_motion_as_ds4(policy.motion_as_ds4);
        controller.set_touchpad_as_ds4(policy.touchpad_as_ds4);
        controller.set_ds5_inputtino_randomize_mac(policy.ds5_inputtino_randomize_mac);
        controller.set_native_pen_touch(policy.native_pen_touch);
        let now = std::time::Instant::now();
        let controller_cleanup = controller
            .set_controller_enabled(policy.controller, now)
            .is_err();
        let device_cleanup = controller
            .set_device_policy(policy.device_policy(), now)
            .is_err();
        let cleanup = controller_cleanup || device_cleanup;
        let response = if cleanup {
            json!({
                "ok":false,
                "error":{"code":"cleanup_pending"},
                "persisted":true,
                "applied":false,
                "cleanup_required":true,
                "revision":revision,
                "values":policy.values(),
            })
        } else {
            settings_response(policy, revision, true)
        };
        complete(send, request, response)
    }

    fn response<B: Backend>(&self, controller: &Controller<B>, persisted: bool) -> Value {
        let cleanup_required = controller.cleanup_required();
        let applied = controller.device_policy() == self.policy.device_policy()
            && controller.controller_enabled() == self.policy.controller
            && controller.high_resolution_scrolling() == self.policy.high_resolution_scrolling
            && controller.always_send_scancodes() == self.policy.always_send_scancodes
            && controller.key_rightalt_to_key_win() == self.policy.key_rightalt_to_key_win
            && controller.keybindings() == &self.policy.keybindings
            && controller.key_repeat_policy()
                == (
                    self.policy.key_repeat_delay,
                    self.policy.key_repeat_frequency,
                )
            && controller.back_button_timeout_ms() == self.policy.back_button_timeout
            && controller.gamepad_profile() == self.policy.gamepad
            && controller.ds4_back_as_touchpad_click() == self.policy.ds4_back_as_touchpad_click
            && controller.motion_as_ds4() == self.policy.motion_as_ds4
            && controller.touchpad_as_ds4() == self.policy.touchpad_as_ds4
            && controller.ds5_inputtino_randomize_mac() == self.policy.ds5_inputtino_randomize_mac
            && controller.native_pen_touch() == self.policy.native_pen_touch
            && !cleanup_required;
        settings_response_with_state(
            self.policy.clone(),
            self.revision,
            persisted,
            applied,
            cleanup_required,
        )
    }

    fn retry_startup_load(&mut self, retry_ticks: u8) {
        self.pending = Some(Pending {
            work: Work::StartupLoad,
            call: Call::Submit {
                payload: json!({"action":"product_get"}),
                operation_id: "input.product-settings.startup".into(),
                parent: None,
                retry_ticks,
            },
        });
    }

    fn start_reconciliation(&mut self, request: u64, expected_revision: u64, desired: InputPolicy) {
        self.pending = Some(Pending {
            work: Work::ReconcileUpdate {
                management_request: request,
                expected_revision,
                desired,
            },
            call: Call::Submit {
                payload: json!({"action":"product_get"}),
                operation_id: format!("input.product-settings.reconcile.{request}"),
                parent: Some(request),
                retry_ticks: 0,
            },
        });
    }

    fn retry_reconciliation(
        &mut self,
        request: u64,
        expected_revision: u64,
        desired: InputPolicy,
        retry_ticks: u8,
    ) {
        self.pending = Some(Pending {
            work: Work::ReconcileUpdate {
                management_request: request,
                expected_revision,
                desired,
            },
            call: Call::Submit {
                payload: json!({"action":"product_get"}),
                operation_id: format!("input.product-settings.reconcile.{request}"),
                parent: Some(request),
                retry_ticks,
            },
        });
    }
}

fn read_document(payload: &Value) -> Result<(InputPolicy, u64), &'static str> {
    if payload["ok"] != true {
        return Err("configuration get was not successful");
    }
    let revision = payload["revision"].as_u64().ok_or("missing revision")?;
    match payload["found"].as_bool() {
        Some(false)
            if revision == 0 && payload["version"].is_null() && payload["values"] == json!({}) =>
        {
            Ok((InputPolicy::default(), 0))
        }
        Some(true) if matches!(payload["version"].as_u64(), Some(1..=14)) => {
            let values = payload["values"].as_object().ok_or("values not object")?;
            let version = payload["version"].as_u64().unwrap_or_default();
            let has_unknown_key = values.keys().any(|key| match version {
                1 => !matches!(key.as_str(), "keyboard" | "mouse"),
                2 => !matches!(
                    key.as_str(),
                    "keyboard" | "mouse" | "high_resolution_scrolling"
                ),
                3 => !matches!(
                    key.as_str(),
                    "keyboard" | "mouse" | "high_resolution_scrolling" | "always_send_scancodes"
                ),
                4 => !matches!(
                    key.as_str(),
                    "keyboard"
                        | "mouse"
                        | "high_resolution_scrolling"
                        | "always_send_scancodes"
                        | "key_rightalt_to_key_win"
                ),
                5 => !matches!(
                    key.as_str(),
                    "keyboard"
                        | "mouse"
                        | "high_resolution_scrolling"
                        | "always_send_scancodes"
                        | "key_rightalt_to_key_win"
                        | "key_repeat_delay"
                        | "key_repeat_frequency"
                ),
                6 => !matches!(
                    key.as_str(),
                    "keyboard"
                        | "mouse"
                        | "high_resolution_scrolling"
                        | "always_send_scancodes"
                        | "key_rightalt_to_key_win"
                        | "key_repeat_delay"
                        | "key_repeat_frequency"
                        | "back_button_timeout"
                ),
                7 => !matches!(
                    key.as_str(),
                    "keyboard"
                        | "mouse"
                        | "high_resolution_scrolling"
                        | "always_send_scancodes"
                        | "key_rightalt_to_key_win"
                        | "key_repeat_delay"
                        | "key_repeat_frequency"
                        | "back_button_timeout"
                        | "gamepad"
                ),
                8 => !matches!(
                    key.as_str(),
                    "keyboard"
                        | "mouse"
                        | "controller"
                        | "high_resolution_scrolling"
                        | "always_send_scancodes"
                        | "key_rightalt_to_key_win"
                        | "key_repeat_delay"
                        | "key_repeat_frequency"
                        | "back_button_timeout"
                        | "gamepad"
                ),
                9 => !matches!(
                    key.as_str(),
                    "keyboard"
                        | "mouse"
                        | "controller"
                        | "high_resolution_scrolling"
                        | "always_send_scancodes"
                        | "key_rightalt_to_key_win"
                        | "key_repeat_delay"
                        | "key_repeat_frequency"
                        | "back_button_timeout"
                        | "gamepad"
                        | "ds4_back_as_touchpad_click"
                ),
                10 => !matches!(
                    key.as_str(),
                    "keyboard"
                        | "mouse"
                        | "controller"
                        | "high_resolution_scrolling"
                        | "always_send_scancodes"
                        | "key_rightalt_to_key_win"
                        | "key_repeat_delay"
                        | "key_repeat_frequency"
                        | "back_button_timeout"
                        | "gamepad"
                        | "ds4_back_as_touchpad_click"
                        | "native_pen_touch"
                ),
                11 => !matches!(
                    key.as_str(),
                    "keyboard"
                        | "mouse"
                        | "controller"
                        | "high_resolution_scrolling"
                        | "always_send_scancodes"
                        | "key_rightalt_to_key_win"
                        | "key_repeat_delay"
                        | "key_repeat_frequency"
                        | "back_button_timeout"
                        | "gamepad"
                        | "ds4_back_as_touchpad_click"
                        | "native_pen_touch"
                        | "keybindings"
                ),
                12 => !matches!(
                    key.as_str(),
                    "keyboard"
                        | "mouse"
                        | "controller"
                        | "high_resolution_scrolling"
                        | "always_send_scancodes"
                        | "key_rightalt_to_key_win"
                        | "key_repeat_delay"
                        | "key_repeat_frequency"
                        | "back_button_timeout"
                        | "gamepad"
                        | "ds4_back_as_touchpad_click"
                        | "native_pen_touch"
                        | "keybindings"
                        | "motion_as_ds4"
                        | "touchpad_as_ds4"
                ),
                13 => !matches!(
                    key.as_str(),
                    "keyboard"
                        | "mouse"
                        | "controller"
                        | "high_resolution_scrolling"
                        | "always_send_scancodes"
                        | "key_rightalt_to_key_win"
                        | "key_repeat_delay"
                        | "key_repeat_frequency"
                        | "back_button_timeout"
                        | "gamepad"
                        | "ds4_back_as_touchpad_click"
                        | "native_pen_touch"
                        | "keybindings"
                        | "motion_as_ds4"
                        | "touchpad_as_ds4"
                        | "proton_dualsense_compatibility"
                ),
                14 => !matches!(
                    key.as_str(),
                    "keyboard"
                        | "mouse"
                        | "controller"
                        | "high_resolution_scrolling"
                        | "always_send_scancodes"
                        | "key_rightalt_to_key_win"
                        | "key_repeat_delay"
                        | "key_repeat_frequency"
                        | "back_button_timeout"
                        | "gamepad"
                        | "ds4_back_as_touchpad_click"
                        | "native_pen_touch"
                        | "keybindings"
                        | "motion_as_ds4"
                        | "touchpad_as_ds4"
                        | "proton_dualsense_compatibility"
                        | "ds5_inputtino_randomize_mac"
                ),
                _ => true,
            });
            if has_unknown_key {
                return Err("unknown input policy value");
            }
            let policy: InputPolicy = serde_json::from_value(payload["values"].clone())
                .map_err(|_| "malformed input policy")?;
            policy.validate()?;
            if revision == 0 {
                return Err("found document has no revision");
            }
            Ok((policy, revision))
        }
        _ => Err("malformed configuration document"),
    }
}

fn product_failure(payload: &Value) -> Option<Value> {
    if payload["ok"] != false {
        return None;
    }
    let failure = payload.get("error")?.as_object()?;
    let code = failure.get("code")?.as_str()?;
    if failure
        .keys()
        .any(|key| !matches!(key.as_str(), "code" | "message" | "current_revision"))
        || code.is_empty()
        || code.len() > 64
        || !code
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Some(error("configuration_unavailable"));
    }
    let current_revision = failure.get("current_revision").and_then(Value::as_u64);
    Some(
        match current_revision.filter(|_| code == "revision_conflict") {
            Some(current_revision) => {
                json!({"ok":false,"error":{"code":code,"current_revision":current_revision}})
            }
            None => json!({"ok":false,"error":{"code":code}}),
        },
    )
}

fn settings_response(policy: InputPolicy, revision: u64, applied: bool) -> Value {
    settings_response_with_state(policy, revision, true, applied, !applied)
}

fn settings_response_with_state(
    policy: InputPolicy,
    revision: u64,
    persisted: bool,
    applied: bool,
    cleanup_required: bool,
) -> Value {
    json!({
        "ok":true,
        "persisted":persisted,
        "applied":applied,
        "cleanup_required":cleanup_required,
        "revision":revision,
        "values":policy.values(),
    })
}

fn complete(send: Sender<'_>, request: u64, payload: Value) -> ModuleResult<()> {
    match send(Command::Complete { request, payload })? {
        Response::Ok => Ok(()),
        Response::Error { code, .. } if code == "request_ended" || code == "not_found" => Ok(()),
        _ => Err("input settings completion failed".into()),
    }
}

fn safe_code(code: &str) -> String {
    if !code.is_empty()
        && code.len() <= 64
        && code
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        code.to_owned()
    } else {
        "configuration_unavailable".into()
    }
}

fn error(code: &str) -> Value {
    json!({"ok":false,"error":{"code":safe_code(code)}})
}

#[cfg(test)]
mod tests;
