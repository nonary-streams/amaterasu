use super::*;
use crate::{
    backend::Backend,
    controller::Controller,
    gamepad_profile::GamepadProfile,
    model::{Event, InputError, InputResult, Key, KeyState, Operation},
};
use amaterasu_module_sdk::{Command, Response};
use std::{cell::RefCell, collections::VecDeque, rc::Rc, time::Instant};

#[derive(Default)]
struct TestBackend {
    events: Rc<RefCell<Vec<Event>>>,
    fail_releases: bool,
}

impl Backend for TestBackend {
    fn name(&self) -> &'static str {
        "settings_test"
    }
    fn supported(&self) -> bool {
        true
    }
    fn absolute_pointer(&self) -> bool {
        true
    }
    fn key_encoding(&self) -> &'static str {
        "windows_set1"
    }
    fn validate(&self, event: &Event) -> InputResult<()> {
        event.validate()
    }
    fn start(&mut self) -> InputResult<()> {
        Ok(())
    }
    fn inject(&mut self, event: &Event) -> InputResult<()> {
        self.events.borrow_mut().push(event.clone());
        if self.fail_releases && event.transition().is_some_and(|(_, down)| !down) {
            Err(InputError::new(
                "release_failed",
                "test native release failure",
            ))
        } else {
            Ok(())
        }
    }
    fn stop(&mut self) -> InputResult<()> {
        Ok(())
    }
}

struct Script {
    responses: VecDeque<Response>,
    commands: Vec<Command>,
}

impl Script {
    fn new(responses: impl IntoIterator<Item = Response>) -> Self {
        Self {
            responses: responses.into_iter().collect(),
            commands: Vec::new(),
        }
    }

    fn send(&mut self, command: Command) -> ModuleResult<Response> {
        self.commands.push(command);
        Ok(self.responses.pop_front().expect("scripted response"))
    }
}

#[test]
fn absent_product_document_uses_enabled_bootstrap_defaults() {
    let (policy, revision) = read_document(&json!({
        "ok":true,
        "found":false,
        "version":null,
        "revision":0,
        "values":{},
    }))
    .unwrap();
    assert_eq!(policy, InputPolicy::default());
    assert_eq!(
        policy.values(),
        json!({"keyboard":true,"mouse":true,"controller":true,"high_resolution_scrolling":true,"always_send_scancodes":true,"key_rightalt_to_key_win":false,"key_repeat_delay":500,"key_repeat_frequency":24.9,"back_button_timeout":-1,"gamepad":"auto","ds4_back_as_touchpad_click":true,"motion_as_ds4":true,"touchpad_as_ds4":true,"proton_dualsense_compatibility":true,"ds5_inputtino_randomize_mac":true,"native_pen_touch":true,"keybindings":[16,160,17,162,18,164]})
    );
    assert_eq!(revision, 0);
}

#[test]
fn settings_read_does_not_report_bootstrap_defaults_as_persisted_before_load() {
    let settings = ProductSettings::new();
    let controller = Controller::new(TestBackend::default(), false);

    assert_eq!(
        settings.get_response(&controller),
        json!({"ok":false,"error":{"code":"settings_unavailable"}}),
    );
}

#[test]
fn product_v1_load_preserves_values_and_defaults_a_missing_legacy_field() {
    let (policy, revision) = read_document(&json!({
        "ok":true,
        "found":true,
        "version":1,
        "revision":9,
        "values":{"keyboard":false},
    }))
    .unwrap();
    assert_eq!(
        policy,
        InputPolicy {
            keyboard: false,
            mouse: true,
            high_resolution_scrolling: true,
            always_send_scancodes: true,
            key_rightalt_to_key_win: false,
            ..InputPolicy::default()
        }
    );
    assert_eq!(revision, 9);
}

#[test]
fn product_v2_reads_high_resolution_value_and_defaults_a_missing_field() {
    let (disabled, _) = read_document(&json!({
        "ok":true,
        "found":true,
        "version":2,
        "revision":10,
        "values":{"keyboard":false,"mouse":false,"high_resolution_scrolling":false},
    }))
    .unwrap();
    assert!(!disabled.high_resolution_scrolling);
    assert!(!disabled.keyboard);
    assert!(!disabled.mouse);

    let (defaulted, _) = read_document(&json!({
        "ok":true,
        "found":true,
        "version":2,
        "revision":11,
        "values":{"keyboard":false,"mouse":true},
    }))
    .unwrap();
    assert!(defaulted.high_resolution_scrolling);
    assert!(!defaulted.keyboard);
    assert!(defaulted.mouse);
    assert!(defaulted.always_send_scancodes);
    assert!(!defaulted.key_rightalt_to_key_win);
}

#[test]
fn product_v4_reads_right_alt_mapping_and_legacy_versions_default_it_off() {
    let (enabled, _) = read_document(&json!({
        "ok":true,
        "found":true,
        "version":4,
        "revision":14,
        "values":{"keyboard":true,"mouse":true,"key_rightalt_to_key_win":true},
    }))
    .unwrap();
    assert!(enabled.key_rightalt_to_key_win);
    assert!(enabled.always_send_scancodes);
    assert_eq!(enabled.key_repeat_delay, DEFAULT_KEY_REPEAT_DELAY_MS);
    assert_eq!(
        enabled.key_repeat_frequency,
        DEFAULT_KEY_REPEAT_FREQUENCY_HZ
    );

    let (legacy, _) = read_document(&json!({
        "ok":true,
        "found":true,
        "version":3,
        "revision":15,
        "values":{"keyboard":true,"mouse":true,"always_send_scancodes":false},
    }))
    .unwrap();
    assert!(!legacy.key_rightalt_to_key_win);
    assert_eq!(legacy.key_repeat_delay, DEFAULT_KEY_REPEAT_DELAY_MS);
    assert_eq!(legacy.key_repeat_frequency, DEFAULT_KEY_REPEAT_FREQUENCY_HZ);
}

#[test]
fn product_v5_preserves_repeat_settings_and_zero_frequency_uses_default() {
    let (policy, revision) = read_document(&json!({
        "ok":true,
        "found":true,
        "version":5,
        "revision":16,
        "values":{
            "keyboard":true,
            "mouse":true,
            "high_resolution_scrolling":true,
            "always_send_scancodes":true,
            "key_rightalt_to_key_win":false,
            "key_repeat_delay":0,
            "key_repeat_frequency":0.0,
        },
    }))
    .unwrap();
    assert_eq!(revision, 16);
    assert_eq!(policy.key_repeat_delay, 0);
    assert_eq!(policy.key_repeat_frequency, 0.0);
}

#[test]
fn product_v6_reads_disabled_and_explicit_back_button_timeouts() {
    let (disabled, _) = read_document(&json!({
        "ok":true,
        "found":true,
        "version":6,
        "revision":17,
        "values":{"keyboard":true,"mouse":true,"back_button_timeout":-1},
    }))
    .unwrap();
    assert_eq!(disabled.back_button_timeout, -1);

    let (enabled, _) = read_document(&json!({
        "ok":true,
        "found":true,
        "version":6,
        "revision":18,
        "values":{"keyboard":true,"mouse":true,"back_button_timeout":0},
    }))
    .unwrap();
    assert_eq!(enabled.back_button_timeout, 0);
    assert_eq!(enabled.gamepad, GamepadProfile::Auto);
}

#[test]
#[cfg(any(windows, target_os = "linux"))]
fn product_v7_persists_typed_platform_profile_and_rejects_unknown_choice() {
    let (profile, _) = read_document(&json!({
        "ok":true,
        "found":true,
        "version":7,
        "revision":19,
        "values":{"keyboard":true,"mouse":true,"gamepad":"ds4"},
    }))
    .unwrap();
    assert_eq!(profile.gamepad, GamepadProfile::Ds4);

    let malformed = json!({
        "ok":true,
        "found":true,
        "version":7,
        "revision":20,
        "values":{"keyboard":true,"mouse":true,"gamepad":"xbox_invented"},
    });
    assert!(read_document(&malformed).is_err());
}

#[test]
fn product_v7_migrates_controller_policy_to_enabled() {
    let (policy, revision) = read_document(&json!({
        "ok":true,
        "found":true,
        "version":7,
        "revision":21,
        "values":{"keyboard":false,"mouse":true,"gamepad":"auto"},
    }))
    .unwrap();
    assert_eq!(revision, 21);
    assert!(!policy.keyboard);
    assert!(policy.mouse);
    assert!(policy.controller);
}

#[test]
fn product_v8_preserves_an_explicitly_disabled_controller_policy() {
    let (policy, revision) = read_document(&json!({
        "ok":true,
        "found":true,
        "version":8,
        "revision":22,
        "values":{"keyboard":true,"mouse":false,"controller":false},
    }))
    .unwrap();
    assert_eq!(revision, 22);
    assert!(policy.keyboard);
    assert!(!policy.mouse);
    assert!(!policy.controller);
    assert!(policy.ds4_back_as_touchpad_click);
}

#[test]
fn product_v10_migrates_v9_native_pen_touch_to_true_and_reads_v10_values() {
    let (legacy, _) = read_document(&json!({
        "ok":true,
        "found":true,
        "version":8,
        "revision":23,
        "values":{"keyboard":true,"mouse":true,"controller":true},
    }))
    .unwrap();
    assert!(legacy.ds4_back_as_touchpad_click);
    assert!(legacy.native_pen_touch);

    let (disabled, _) = read_document(&json!({
        "ok":true,
        "found":true,
        "version":9,
        "revision":24,
        "values":{"keyboard":true,"mouse":true,"controller":true,"ds4_back_as_touchpad_click":false},
    }))
    .unwrap();
    assert!(!disabled.ds4_back_as_touchpad_click);
    assert!(disabled.native_pen_touch);

    for native_pen_touch in [true, false] {
        let (policy, _) = read_document(&json!({
            "ok":true,
            "found":true,
            "version":10,
            "revision":25,
            "values":{"keyboard":true,"mouse":true,"native_pen_touch":native_pen_touch},
        }))
        .unwrap();
        assert_eq!(policy.native_pen_touch, native_pen_touch);
        assert_eq!(policy.values()["native_pen_touch"], native_pen_touch);
    }

    let (missing, _) = read_document(&json!({
        "ok":true,
        "found":true,
        "version":10,
        "revision":26,
        "values":{"keyboard":true,"mouse":true},
    }))
    .unwrap();
    assert!(missing.native_pen_touch);
}

#[test]
fn product_v3_reads_scancode_policy_and_defaults_a_missing_field_on() {
    let (disabled, _) = read_document(&json!({
        "ok":true,
        "found":true,
        "version":3,
        "revision":12,
        "values":{"keyboard":false,"mouse":true,"high_resolution_scrolling":false,"always_send_scancodes":false},
    }))
    .unwrap();
    assert!(!disabled.always_send_scancodes);
    assert!(!disabled.high_resolution_scrolling);

    let (defaulted, _) = read_document(&json!({
        "ok":true,
        "found":true,
        "version":3,
        "revision":13,
        "values":{"keyboard":true,"mouse":true,"high_resolution_scrolling":true},
    }))
    .unwrap();
    assert!(defaulted.always_send_scancodes);
    assert!(!defaulted.key_rightalt_to_key_win);
}

#[test]
fn product_documents_reject_bad_versions_revisions_types_and_unknown_keys() {
    for document in [
        json!({"ok":true,"found":true,"version":15,"revision":1,"values":{"keyboard":true,"mouse":true}}),
        json!({"ok":true,"found":true,"version":9,"revision":1,"values":{"keyboard":true,"mouse":true,"native_pen_touch":false}}),
        json!({"ok":true,"found":true,"version":10,"revision":1,"values":{"keyboard":true,"mouse":true,"native_pen_touch":"false"}}),
        json!({"ok":true,"found":true,"version":10,"revision":1,"values":{"keyboard":true,"mouse":true,"unexpected":true}}),
        json!({"ok":true,"found":true,"version":1,"revision":0,"values":{"keyboard":true,"mouse":true}}),
        json!({"ok":true,"found":true,"version":1,"revision":1,"values":{"keyboard":"true","mouse":true}}),
        json!({"ok":true,"found":true,"version":1,"revision":1,"values":{"keyboard":true,"mouse":true,"controller":true}}),
        json!({"ok":true,"found":true,"version":2,"revision":1,"values":{"keyboard":true,"mouse":true,"controller":true}}),
        json!({"ok":true,"found":true,"version":2,"revision":1,"values":{"keyboard":true,"mouse":true,"high_resolution_scrolling":"true"}}),
        json!({"ok":true,"found":true,"version":2,"revision":1,"values":{"keyboard":true,"mouse":true,"always_send_scancodes":false}}),
        json!({"ok":true,"found":true,"version":3,"revision":1,"values":{"keyboard":true,"mouse":true,"always_send_scancodes":"true"}}),
        json!({"ok":true,"found":true,"version":4,"revision":1,"values":{"keyboard":true,"mouse":true,"key_rightalt_to_key_win":"true"}}),
        json!({"ok":true,"found":true,"version":4,"revision":1,"values":{"keyboard":true,"mouse":true,"key_repeat_delay":100}}),
        json!({"ok":true,"found":true,"version":5,"revision":1,"values":{"keyboard":true,"mouse":true,"controller":true}}),
        json!({"ok":true,"found":true,"version":6,"revision":1,"values":{"keyboard":true,"mouse":true,"back_button_timeout":2147483648i64}}),
        json!({"ok":true,"found":true,"version":6,"revision":1,"values":{"keyboard":true,"mouse":true,"back_button_timeout":-2147483649i64}}),
        json!({"ok":true,"found":true,"version":6,"revision":1,"values":{"keyboard":true,"mouse":true,"back_button_timeout":1.5}}),
        json!({"ok":true,"found":true,"version":6,"revision":1,"values":{"keyboard":true,"mouse":true,"back_button_timeout":"100"}}),
        json!({"ok":true,"found":true,"version":6,"revision":1,"values":{"keyboard":true,"mouse":true,"gamepad":"auto"}}),
        json!({"ok":true,"found":true,"version":8,"revision":1,"values":{"keyboard":true,"mouse":true,"controller":"true"}}),
        json!({"ok":true,"found":true,"version":5,"revision":1,"values":{"keyboard":true,"mouse":true,"key_repeat_delay":-1}}),
        json!({"ok":true,"found":true,"version":5,"revision":1,"values":{"keyboard":true,"mouse":true,"key_repeat_frequency":-0.1}}),
        json!({"ok":true,"found":true,"version":3,"revision":1,"values":{"keyboard":true,"mouse":true,"key_rightalt_to_key_win":true}}),
        json!({"ok":true,"found":false,"version":null,"revision":1,"values":{}}),
    ] {
        assert!(read_document(&document).is_err(), "accepted {document}");
    }
}

#[test]
fn update_requires_current_revision_and_sends_whole_v14_document() {
    let mut settings = ProductSettings::new();
    settings.ready = true;
    settings.revision = 4;
    settings.pending = None;

    let conflict = settings
        .begin_update(
            71,
            3,
            InputPolicy {
                keyboard: false,
                mouse: true,
                high_resolution_scrolling: true,
                always_send_scancodes: true,
                key_rightalt_to_key_win: false,
                ..InputPolicy::default()
            },
            10_000,
        )
        .unwrap_err();
    assert_eq!(conflict["error"]["code"], "revision_conflict");
    assert_eq!(conflict["error"]["current_revision"], 4);
    assert!(!settings.busy());

    settings
        .begin_update(
            72,
            4,
            InputPolicy {
                keyboard: false,
                mouse: false,
                high_resolution_scrolling: false,
                always_send_scancodes: false,
                key_rightalt_to_key_win: false,
                ..InputPolicy::default()
            },
            10_000,
        )
        .unwrap();
    let pending = settings.pending.as_ref().unwrap();
    assert!(matches!(
        pending.work,
        Work::Update {
            management_request: 72,
            expected_revision: 4,
            ..
        }
    ));
    let Call::Submit {
        payload, parent, ..
    } = &pending.call
    else {
        panic!("update did not begin with a product request");
    };
    assert_eq!(parent, &Some(72));
    assert_eq!(
        payload,
        &json!({
            "action":"product_update",
            "expected_revision":4,
            "version":14,
            "values":{"keyboard":false,"mouse":false,"controller":true,"high_resolution_scrolling":false,"always_send_scancodes":false,"key_rightalt_to_key_win":false,"key_repeat_delay":500,"key_repeat_frequency":24.9,"back_button_timeout":-1,"gamepad":"auto","ds4_back_as_touchpad_click":true,"motion_as_ds4":true,"touchpad_as_ds4":true,"proton_dualsense_compatibility":true,"ds5_inputtino_randomize_mac":true,"native_pen_touch":true,"keybindings":[16,160,17,162,18,164]},
        })
    );
}

#[test]
fn v14_update_serializes_controller_proton_and_random_mac_preferences() {
    let policy = InputPolicy {
        motion_as_ds4: false,
        touchpad_as_ds4: false,
        ds5_inputtino_randomize_mac: false,
        ..InputPolicy::default()
    };

    let payload = policy.update_payload(12);

    assert_eq!(payload["version"], 14);
    assert_eq!(payload["values"]["motion_as_ds4"], false);
    assert_eq!(payload["values"]["touchpad_as_ds4"], false);
    assert_eq!(payload["values"]["proton_dualsense_compatibility"], true);
    assert_eq!(payload["values"]["ds5_inputtino_randomize_mac"], false);
}

#[test]
fn v12_documents_migrate_proton_compatibility_to_its_enabled_default() {
    let old = json!({
        "ok":true,
        "found":true,
        "version":12,
        "revision":9,
        "values":InputPolicy::default().values(),
    });
    let mut values = old["values"].as_object().unwrap().clone();
    values.remove("proton_dualsense_compatibility");
    values.remove("ds5_inputtino_randomize_mac");
    let (policy, revision) = read_document(&json!({
        "ok":true,
        "found":true,
        "version":12,
        "revision":9,
        "values":values,
    }))
    .unwrap();
    assert_eq!(revision, 9);
    assert!(policy.proton_dualsense_compatibility);
    assert!(policy.ds5_inputtino_randomize_mac);
}

#[test]
fn v13_documents_preserve_proton_choice_when_random_mac_is_added() {
    let mut values = InputPolicy::default().values();
    values["proton_dualsense_compatibility"] = json!(false);
    values
        .as_object_mut()
        .unwrap()
        .remove("ds5_inputtino_randomize_mac");
    let (policy, revision) = read_document(&json!({
        "ok":true,
        "found":true,
        "version":13,
        "revision":11,
        "values":values,
    }))
    .unwrap();

    assert_eq!(revision, 11);
    assert!(!policy.proton_dualsense_compatibility);
    assert!(policy.ds5_inputtino_randomize_mac);
    let updated = policy.update_payload(revision);
    assert_eq!(updated["version"], 14);
    assert_eq!(updated["values"]["proton_dualsense_compatibility"], false);
}

#[test]
fn old_updates_preserve_new_scancode_and_high_resolution_values() {
    let mut settings = ProductSettings::new();
    settings.ready = true;
    settings.revision = 9;
    settings.pending = None;
    settings.policy.high_resolution_scrolling = false;
    settings.policy.always_send_scancodes = false;
    settings.policy.key_rightalt_to_key_win = true;
    settings.policy.key_repeat_delay = 230;
    settings.policy.key_repeat_frequency = 40.0;
    settings.policy.gamepad = selected_supported_profile();
    settings.policy.controller = false;
    settings.policy.ds4_back_as_touchpad_click = false;
    settings.policy.native_pen_touch = false;
    let legacy_desired = settings.merge_update(InputPolicyUpdate {
        keyboard: false,
        mouse: true,
        ..InputPolicyUpdate::default()
    });
    assert!(!legacy_desired.high_resolution_scrolling);
    assert!(!legacy_desired.always_send_scancodes);
    assert!(legacy_desired.key_rightalt_to_key_win);
    assert_eq!(legacy_desired.gamepad, selected_supported_profile());
    assert!(!legacy_desired.ds4_back_as_touchpad_click);
    assert!(!legacy_desired.controller);
    assert!(!legacy_desired.native_pen_touch);
    settings
        .begin_update(90, 9, legacy_desired, 10_000)
        .unwrap();
    let Call::Submit { payload, .. } = &settings.pending.as_ref().unwrap().call else {
        panic!("legacy settings update was not submitted");
    };
    assert_eq!(payload["version"], 14);
    assert_eq!(
        payload["values"],
        json!({"keyboard":false,"mouse":true,"controller":false,"high_resolution_scrolling":false,"always_send_scancodes":false,"key_rightalt_to_key_win":true,"key_repeat_delay":230,"key_repeat_frequency":40.0,"back_button_timeout":-1,"gamepad":selected_supported_profile().as_str(),"ds4_back_as_touchpad_click":false,"motion_as_ds4":true,"touchpad_as_ds4":true,"proton_dualsense_compatibility":true,"ds5_inputtino_randomize_mac":true,"native_pen_touch":false,"keybindings":[16,160,17,162,18,164]})
    );
    assert!(
        !settings
            .merge_update(InputPolicyUpdate {
                keyboard: false,
                mouse: true,
                high_resolution_scrolling: Some(true),
                ..InputPolicyUpdate::default()
            })
            .always_send_scancodes
    );
    assert!(
        settings
            .merge_update(InputPolicyUpdate {
                keyboard: false,
                mouse: true,
                high_resolution_scrolling: Some(true),
                always_send_scancodes: Some(true),
                ..InputPolicyUpdate::default()
            })
            .high_resolution_scrolling
    );
    assert!(
        settings
            .merge_update(InputPolicyUpdate {
                keyboard: false,
                mouse: true,
                always_send_scancodes: Some(true),
                ..InputPolicyUpdate::default()
            })
            .always_send_scancodes
    );
    assert!(
        settings
            .merge_update(InputPolicyUpdate {
                keyboard: false,
                mouse: true,
                ..InputPolicyUpdate::default()
            })
            .key_rightalt_to_key_win
    );
    assert!(
        !settings
            .merge_update(InputPolicyUpdate {
                keyboard: false,
                mouse: true,
                key_rightalt_to_key_win: Some(false),
                ..InputPolicyUpdate::default()
            })
            .key_rightalt_to_key_win
    );
    assert_eq!(
        settings
            .merge_update(InputPolicyUpdate {
                keyboard: false,
                mouse: true,
                gamepad: Some(GamepadProfile::Auto),
                ..InputPolicyUpdate::default()
            })
            .gamepad,
        GamepadProfile::Auto
    );
    assert!(
        !settings
            .merge_update(InputPolicyUpdate {
                keyboard: true,
                mouse: true,
                controller: None,
                ..InputPolicyUpdate::default()
            })
            .controller
    );
}

fn selected_supported_profile() -> GamepadProfile {
    #[cfg(windows)]
    {
        GamepadProfile::X360
    }
    #[cfg(target_os = "linux")]
    {
        GamepadProfile::Xone
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        GamepadProfile::Auto
    }
}

#[test]
fn applying_durable_scroll_policy_updates_controller_before_reporting_applied() {
    let mut settings = ProductSettings::new();
    let mut controller = Controller::new(TestBackend::default(), true);
    let mut script = Script::new([Response::Ok]);
    let policy = InputPolicy {
        keyboard: true,
        mouse: true,
        controller: false,
        high_resolution_scrolling: false,
        always_send_scancodes: false,
        key_rightalt_to_key_win: true,
        key_repeat_delay: 250,
        key_repeat_frequency: 30.5,
        back_button_timeout: 1_250,
        gamepad: selected_supported_profile(),
        ds4_back_as_touchpad_click: false,
        motion_as_ds4: true,
        touchpad_as_ds4: true,
        proton_dualsense_compatibility: true,
        ds5_inputtino_randomize_mac: true,
        native_pen_touch: false,
        keybindings: KeyBindings::default(),
    };

    settings
        .apply_update(
            &mut |command| script.send(command),
            &mut controller,
            93,
            policy,
            1,
        )
        .unwrap();

    assert!(!controller.high_resolution_scrolling());
    assert!(!controller.always_send_scancodes());
    assert!(controller.key_rightalt_to_key_win());
    assert_eq!(controller.back_button_timeout_ms(), 1_250);
    assert_eq!(controller.gamepad_profile(), selected_supported_profile());
    assert!(!controller.ds4_back_as_touchpad_click());
    assert!(!controller.native_pen_touch());
    assert!(!controller.controller_enabled());
    assert_eq!(settings.get_response(&controller)["applied"], true);
    assert_eq!(
        settings.get_response(&controller)["values"]["high_resolution_scrolling"],
        false
    );
    assert_eq!(
        settings.get_response(&controller)["values"]["key_rightalt_to_key_win"],
        true
    );
    assert_eq!(
        settings.get_response(&controller)["values"]["native_pen_touch"],
        false
    );
    assert_eq!(
        settings.get_response(&controller)["values"]["back_button_timeout"],
        1_250
    );
    assert_eq!(
        settings.get_response(&controller)["values"]["gamepad"],
        selected_supported_profile().as_str()
    );
    assert_eq!(
        settings.get_response(&controller)["values"]["ds4_back_as_touchpad_click"],
        false
    );
}

#[test]
fn management_readback_distinguishes_persisted_and_applied_policy() {
    let cleanup_pending = settings_response_with_state(
        InputPolicy {
            keyboard: false,
            mouse: true,
            high_resolution_scrolling: true,
            always_send_scancodes: true,
            key_rightalt_to_key_win: false,
            ..InputPolicy::default()
        },
        12,
        true,
        false,
        true,
    );
    assert_eq!(cleanup_pending["ok"], true);
    assert_eq!(cleanup_pending["persisted"], true);
    assert_eq!(cleanup_pending["applied"], false);
    assert_eq!(cleanup_pending["cleanup_required"], true);
    assert_eq!(cleanup_pending["revision"], 12);
}

#[test]
fn product_errors_forward_only_bounded_codes_and_revision() {
    let conflict = product_failure(&json!({
        "ok":false,
        "error":{"code":"revision_conflict","message":"private details","current_revision":8},
    }))
    .unwrap();
    assert_eq!(
        conflict,
        json!({"ok":false,"error":{"code":"revision_conflict","current_revision":8}})
    );
    let unsafe_response = product_failure(&json!({
        "ok":false,
        "error":{"code":"Bad code","message":"detail"},
    }))
    .unwrap();
    assert_eq!(
        unsafe_response["error"]["code"],
        "configuration_unavailable"
    );
}

#[test]
fn terminal_configuration_result_is_retained_until_release_then_applied_once() {
    let absent = json!({
        "ok":true,"found":false,"version":null,"revision":0,"values":{}
    });
    let updated = json!({
        "ok":true,
        "found":true,
        "version":4,
        "revision":1,
        "values":{"keyboard":false,"mouse":true,"high_resolution_scrolling":true,"always_send_scancodes":true,"key_rightalt_to_key_win":false},
    });
    let mut script = Script::new([
        Response::Accepted { id: 11 },
        Response::Result { payload: absent },
        Response::Ok,
        Response::Accepted { id: 12 },
        Response::Result { payload: updated },
        Response::Error {
            code: "temporarily_unavailable".into(),
            message: "retry".into(),
        },
        Response::Ok,
        Response::Ok,
    ]);
    let mut settings = ProductSettings::new();
    let mut controller = Controller::new(TestBackend::default(), true);

    settings
        .advance(&mut |command| script.send(command), &mut controller)
        .unwrap();
    settings
        .advance(&mut |command| script.send(command), &mut controller)
        .unwrap();
    settings
        .advance(&mut |command| script.send(command), &mut controller)
        .unwrap();
    assert!(settings.ready());

    settings
        .begin_update(
            72,
            0,
            InputPolicy {
                keyboard: false,
                mouse: true,
                high_resolution_scrolling: true,
                always_send_scancodes: true,
                key_rightalt_to_key_win: false,
                ..InputPolicy::default()
            },
            10_000,
        )
        .unwrap();
    settings
        .advance(&mut |command| script.send(command), &mut controller)
        .unwrap();
    settings
        .advance(&mut |command| script.send(command), &mut controller)
        .unwrap();
    assert!(settings.busy());
    settings
        .advance(&mut |command| script.send(command), &mut controller)
        .unwrap();
    assert!(
        settings.busy(),
        "failed ReleaseResult must retain the payload outcome"
    );

    settings
        .advance(&mut |command| script.send(command), &mut controller)
        .unwrap();
    assert!(!settings.busy());
    assert_eq!(settings.revision, 1);
    assert!(!controller.device_policy().keyboard);
    let request_count = script
        .commands
        .iter()
        .filter(|command| matches!(command, Command::Request { .. }))
        .count();
    assert_eq!(
        request_count, 2,
        "release retry must not resubmit the write"
    );
    assert!(matches!(
        script.commands[5],
        Command::ReleaseResult { request: 12 }
    ));
    assert!(matches!(
        script.commands[6],
        Command::ReleaseResult { request: 12 }
    ));
    let Command::Complete {
        request: 72,
        payload,
    } = &script.commands[7]
    else {
        panic!("management request was not completed after result release");
    };
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["persisted"], true);
    assert_eq!(payload["applied"], true);
}

#[test]
fn native_release_failure_reports_saved_policy_as_pending() {
    let now = Instant::now();
    let mut controller = Controller::new(
        TestBackend {
            fail_releases: true,
            ..TestBackend::default()
        },
        true,
    );
    let acquired = controller.handle(
        "owner",
        Operation::Acquire {
            operation_id: None,
            ttl_ms: None,
            back_button_timeout: None,
            controller: None,
            gamepad: None,
            ds4_back_as_touchpad_click: None,
            motion_as_ds4: None,
            touchpad_as_ds4: None,
            ds5_inputtino_randomize_mac: None,
            native_pen_touch: None,
            keyboard: None,
            mouse: None,
            high_resolution_scrolling: None,
            always_send_scancodes: None,
            key_rightalt_to_key_win: None,
            keybindings: None,
            key_repeat_delay: None,
            key_repeat_frequency: None,
        },
        now,
        &mut || Ok(()),
    );
    let token = acquired["lease"].as_str().unwrap().to_owned();
    let key_down = Event::Key {
        key: Key::WindowsScan {
            code: 30,
            extended: false,
        },
        state: KeyState::Down,
    };
    assert_eq!(
        controller.handle(
            "owner",
            Operation::Batch {
                lease: token,
                events: vec![key_down]
            },
            now,
            &mut || Ok(()),
        )["ok"],
        true
    );

    let product_result = json!({
        "ok":true,"found":true,"version":4,"revision":1,
        "values":{"keyboard":false,"mouse":true,"high_resolution_scrolling":true,"always_send_scancodes":true,"key_rightalt_to_key_win":false}
    });
    let mut script = Script::new([Response::Ok, Response::Ok]);
    let mut settings = ProductSettings::new();
    settings.ready = true;
    settings.pending = Some(Pending {
        work: Work::Update {
            management_request: 88,
            expected_revision: 0,
            desired: InputPolicy {
                keyboard: false,
                mouse: true,
                high_resolution_scrolling: true,
                always_send_scancodes: true,
                key_rightalt_to_key_win: false,
                ..InputPolicy::default()
            },
        },
        call: Call::Releasing {
            request: 18,
            outcome: ChildOutcome::Payload(product_result),
        },
    });
    settings
        .advance(&mut |command| script.send(command), &mut controller)
        .unwrap();
    let Command::Complete {
        request: 88,
        payload,
    } = &script.commands[1]
    else {
        panic!("pending settings update was not completed");
    };
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "cleanup_pending");
    assert_eq!(payload["persisted"], true);
    assert_eq!(payload["applied"], false);
    assert_eq!(payload["cleanup_required"], true);
    assert_eq!(
        settings.get_response(&controller)["values"]["keyboard"],
        false
    );
    assert_eq!(settings.get_response(&controller)["applied"], false);
}

#[test]
fn legacy_documents_keep_saved_values_and_default_only_the_new_keybindings() {
    for version in 1..=10 {
        let (policy, revision) = read_document(&json!({
            "ok":true, "found":true, "version":version, "revision":19,
            "values":{"keyboard":false,"mouse":false}
        }))
        .unwrap();
        assert!(!policy.keyboard);
        assert!(!policy.mouse);
        assert_eq!(policy.keybindings, KeyBindings::default());
        assert_eq!(revision, 19);
        let updated = policy.update_payload(revision);
        assert_eq!(updated["version"], 14);
        assert_eq!(updated["values"]["keyboard"], false);
        assert_eq!(updated["values"]["mouse"], false);
        assert_eq!(
            updated["values"]["keybindings"],
            json!([16, 160, 17, 162, 18, 164])
        );
    }
}

#[test]
fn v11_documents_preserve_empty_order_duplicates_and_exact_byte_boundaries() {
    for bindings in [json!([]), json!([0, 255, 255, 0]), json!([65, 66, 65, 67])] {
        let (policy, _) = read_document(&json!({
            "ok":true, "found":true, "version":11, "revision":2,
            "values":{"keybindings":bindings}
        }))
        .unwrap();
        assert_eq!(policy.values()["keybindings"], bindings);
    }
    for invalid in [
        json!(null),
        json!([0]),
        json!([0, 256]),
        json!([0.0, 1]),
        json!(vec![0_u8; 514]),
    ] {
        assert!(
            read_document(&json!({
                "ok":true, "found":true, "version":11, "revision":2,
                "values":{"keybindings":invalid}
            }))
            .is_err()
        );
    }
    assert!(
        read_document(&json!({
            "ok":true, "found":true, "version":10, "revision":2,
            "values":{"keybindings":[]}
        }))
        .is_err()
    );
}

#[test]
fn omitted_keybinding_updates_retain_the_list_and_explicit_empty_replaces_it() {
    let mut settings = ProductSettings::new();
    settings.policy.keybindings = KeyBindings::from_flat(vec![65, 66, 65, 67]).unwrap();
    let legacy = settings.merge_update(InputPolicyUpdate {
        keyboard: false,
        mouse: true,
        ..InputPolicyUpdate::default()
    });
    assert_eq!(legacy.keybindings, settings.policy.keybindings);
    assert_eq!(legacy.values()["keybindings"], json!([65, 66, 65, 67]));
    let explicit = settings.merge_update(InputPolicyUpdate {
        keyboard: false,
        mouse: true,
        keybindings: Some(KeyBindings::empty()),
        ..InputPolicyUpdate::default()
    });
    assert_eq!(explicit.values()["keybindings"], json!([]));
}

#[test]
fn keybinding_write_uses_cas_and_verified_readback_before_applying_or_completing() {
    let mut settings = ProductSettings::new();
    settings.ready = true;
    settings.pending = None;
    settings.revision = 7;
    let desired = InputPolicy {
        keybindings: KeyBindings::empty(),
        ..InputPolicy::default()
    };
    let mut controller = Controller::new(TestBackend::default(), true);
    let mut script = Script::new([
        Response::Accepted { id: 51 },
        Response::Result {
            payload: json!({"ok":true,"found":true,"version":14,
            "revision":8,"values":desired.values()}),
        },
        Response::Ok,
        Response::Ok,
    ]);
    let conflict = settings
        .begin_update(70, 6, desired.clone(), 10_000)
        .unwrap_err();
    assert_eq!(conflict["error"]["code"], "revision_conflict");
    assert_eq!(controller.keybindings(), &KeyBindings::default());
    settings.begin_update(71, 7, desired, 10_000).unwrap();
    for _ in 0..2 {
        settings
            .advance(&mut |command| script.send(command), &mut controller)
            .unwrap();
        assert_eq!(controller.keybindings(), &KeyBindings::default());
    }
    settings
        .advance(&mut |command| script.send(command), &mut controller)
        .unwrap();
    let Command::Request { payload: write, .. } = &script.commands[0] else {
        panic!("missing write");
    };
    assert_eq!(write["expected_revision"], 7);
    assert_eq!(write["version"], 14);
    assert_eq!(write["values"]["keybindings"], json!([]));
    let Command::Complete {
        request: 71,
        payload: complete,
    } = &script.commands[3]
    else {
        panic!("missing completion");
    };
    assert_eq!(complete["ok"], true);
    assert_eq!(complete["persisted"], true);
    assert_eq!(complete["applied"], true);
    assert_eq!(controller.keybindings(), &KeyBindings::empty());
    assert_eq!(
        settings.get_response(&controller)["values"]["keybindings"],
        json!([])
    );

    let mut restarted = ProductSettings::new();
    let mut restarted_controller = Controller::new(TestBackend::default(), true);
    let mut load = Script::new([
        Response::Accepted { id: 61 },
        Response::Result {
            payload: json!({"ok":true,"found":true,"version":14,
            "revision":8,"values":settings.policy.values()}),
        },
        Response::Ok,
    ]);
    for _ in 0..3 {
        restarted
            .advance(&mut |command| load.send(command), &mut restarted_controller)
            .unwrap();
    }
    assert_eq!(restarted_controller.keybindings(), &KeyBindings::empty());
    assert_eq!(restarted.get_response(&restarted_controller)["revision"], 8);
    assert_eq!(
        restarted.get_response(&restarted_controller)["values"]["keybindings"],
        json!([])
    );
}

#[test]
fn reordered_keybinding_readback_is_reconciled_and_never_reported_as_the_attempted_write() {
    let mut settings = ProductSettings::new();
    let mut controller = Controller::new(TestBackend::default(), true);
    let desired = InputPolicy {
        keybindings: KeyBindings::from_flat(vec![65, 66, 65, 67]).unwrap(),
        ..InputPolicy::default()
    };
    let actual = InputPolicy {
        keybindings: KeyBindings::from_flat(vec![65, 67, 65, 66]).unwrap(),
        ..InputPolicy::default()
    };
    let mut script = Script::new([]);
    settings
        .on_terminal(
            &mut |command| script.send(command),
            &mut controller,
            Work::Update {
                management_request: 72,
                expected_revision: 7,
                desired: desired.clone(),
            },
            ChildOutcome::Payload(
                json!({"ok":true,"found":true,"version":14,"revision":8,"values":actual.values()}),
            ),
        )
        .unwrap();
    assert!(matches!(
        settings.pending.as_ref().unwrap().work,
        Work::ReconcileUpdate { .. }
    ));
    assert_eq!(controller.keybindings(), &KeyBindings::default());
    let mut script = Script::new([Response::Ok]);
    settings
        .on_terminal(
            &mut |command| script.send(command),
            &mut controller,
            Work::ReconcileUpdate {
                management_request: 72,
                expected_revision: 7,
                desired,
            },
            ChildOutcome::Payload(
                json!({"ok":true,"found":true,"version":14,"revision":8,"values":actual.values()}),
            ),
        )
        .unwrap();
    let Command::Complete { payload, .. } = &script.commands[0] else {
        panic!("missing completion");
    };
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "write_unresolved");
    assert_eq!(payload["values"]["keybindings"], json!([65, 67, 65, 66]));
    assert_eq!(controller.keybindings(), &actual.keybindings);
}
