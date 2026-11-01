use super::*;

fn enable_input(host: &mut Harness) {
    host.pipeline.settings.enable_input = true;
    host.pipeline.settings.input_lease_ttl_ms = 1_000;
}

fn pcm_feedback(sequence: u32) -> Value {
    serde_json::to_value(amaterasu_gamestream::control::HostFeedback::HapticsPcm {
        controller: 2,
        sequence,
        frames: 1,
        samples_s16le: vec![1, 2, 3, 4],
    })
    .unwrap()
}

#[test]
fn feedback_poll_requires_rtsp_flags_gates_pcm_and_retries_before_ack() {
    for (flags, expected_delivery) in [(3, false), (7, true)] {
        let mut host = Harness::new();
        enable_input(&mut host);
        host.pipeline
            .settings
            .allowed_principals
            .push("amaterasu.rtsp".into());
        let id = host.response_from(1, "amaterasu.rtsp", prepare())["session"]
            .as_u64()
            .unwrap();
        let mut launch = start(id);
        launch["configuration"]["client_feature_flags"] = json!(flags);
        assert_eq!(
            host.response_from(2, "amaterasu.rtsp", launch)["state"],
            "active"
        );

        host.broker.input_feedback_events.push_back(json!({
            "sequence":1,"feedback":pcm_feedback(9)
        }));
        host.plane.borrow_mut().feedback_queue_full = 1;
        host.idle();

        assert_eq!(host.broker.input_feedback_acks, vec![1]);
        assert!(host.broker.input_feedback_events.is_empty());
        assert_eq!(
            host.plane.borrow().feedback.len(),
            usize::from(expected_delivery)
        );
        assert_eq!(
            host.pipeline.records[&id].response()["haptics_pcm_negotiated"],
            expected_delivery
        );
    }
}

#[test]
fn feedback_ack_failure_retries_same_sequence_without_duplicate_plane_delivery() {
    let mut host = Harness::new();
    enable_input(&mut host);
    host.pipeline
        .settings
        .allowed_principals
        .push("amaterasu.rtsp".into());
    let id = host.response_from(1, "amaterasu.rtsp", prepare())["session"]
        .as_u64()
        .unwrap();
    let mut launch = start(id);
    launch["configuration"]["client_feature_flags"] = json!(7);
    assert_eq!(
        host.response_from(2, "amaterasu.rtsp", launch)["state"],
        "active"
    );

    host.broker.input_feedback_events.push_back(json!({
        "sequence":1,"feedback":pcm_feedback(9)
    }));
    host.broker.feedback_ack_failures = 1;
    host.idle();

    assert_eq!(host.broker.input_feedback_acks, vec![1]);
    assert!(host.broker.input_feedback_events.is_empty());
    assert_eq!(host.plane.borrow().feedback.len(), 1);
}

#[test]
fn direct_stream_session_caller_cannot_supply_client_feature_flags() {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    let mut launch = start(id);
    launch["configuration"]["client_feature_flags"] = json!(0);
    assert_eq!(host.response(2, launch)["code"], "permission_denied");
}

fn enable_client_haptics(host: &mut Harness, session: u64) {
    let record = host.pipeline.records.get_mut(&session).unwrap();
    crate::pipeline::input::queue_control_input(
        record,
        amaterasu_gamestream::control::Input::EnableHaptics { enabled: true },
        true,
        host.now,
    )
    .unwrap();
}

fn feedback(slot: u16, strong_motor: u16, weak_motor: u16, duration_ms: u16) -> Value {
    json!({"slot":slot,"strong_motor":strong_motor,"weak_motor":weak_motor,
        "duration_ms":duration_ms})
}

#[test]
fn haptics_require_client_opt_in_and_an_authorized_gamepad_backend() {
    for backend_grant in [false, true] {
        let mut host = Harness::new();
        enable_input(&mut host);
        host.broker.input_controller_supported = Some(json!(backend_grant));
        let session = host.response(1, prepare())["session"].as_u64().unwrap();
        assert_eq!(host.response(2, start(session))["state"], "active");
        let status = host.pipeline.records[&session].response();
        assert_eq!(status["haptics_feedback_available"], backend_grant);
        assert_eq!(status["haptics_feedback_enabled"], false);
        for _ in 0..60 {
            host.tick();
        }
        assert!(host.broker.gamepad_poll_times.is_empty());

        enable_client_haptics(&mut host, session);
        assert_eq!(
            host.pipeline.records[&session].response()["haptics_feedback_enabled"],
            backend_grant
        );
        assert_eq!(
            host.pipeline.records[&session].client_haptics_requested,
            backend_grant
        );
        assert_eq!(
            host.pipeline.records[&session].unsupported_controls,
            u64::from(!backend_grant)
        );
        for _ in 0..60 {
            host.tick();
        }
        assert_eq!(!host.broker.gamepad_poll_times.is_empty(), backend_grant);
    }

    let mut host = Harness::new();
    let session = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(session))["state"], "active");
    let record = host.pipeline.records.get_mut(&session).unwrap();
    crate::pipeline::input::queue_control_input(
        record,
        amaterasu_gamestream::control::Input::EnableHaptics { enabled: true },
        false,
        host.now,
    )
    .unwrap();
    assert!(!record.client_haptics_requested);
    assert_eq!(record.response()["haptics_feedback_available"], false);
}

#[test]
fn disabling_client_haptics_stops_active_motor_without_revoking_input() {
    let mut host = Harness::new();
    enable_input(&mut host);
    host.broker.input_controller_supported = Some(json!(true));
    host.broker
        .gamepad_feedback
        .push_back(feedback(5, 250, 450, 5_000));
    let session = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(session))["state"], "active");
    enable_client_haptics(&mut host, session);
    host.idle();
    assert!(host.plane.borrow().rumble_commands.iter().any(|command| {
        command.slot == 5 && command.low_frequency == 250 && command.high_frequency == 450
    }));

    let record = host.pipeline.records.get_mut(&session).unwrap();
    crate::pipeline::input::queue_control_input(
        record,
        amaterasu_gamestream::control::Input::EnableHaptics { enabled: false },
        true,
        host.now,
    )
    .unwrap();
    assert!(!record.client_haptics_requested);
    assert_eq!(record.response()["haptics_feedback_enabled"], false);
    assert_eq!(record.rumble_shutdown_deadline, Some(host.now + 200));
    host.idle();
    assert!(host.plane.borrow().rumble_commands.iter().any(|command| {
        command.slot == 5 && command.low_frequency == 0 && command.high_frequency == 0
    }));
    assert!(host.pipeline.records[&session].input_lease.is_some());
}

#[test]
fn busy_terminal_zero_is_bounded_and_releases_local_input() {
    let mut host = Harness::new();
    enable_input(&mut host);
    host.broker.input_controller_supported = Some(json!(true));
    host.broker
        .gamepad_feedback
        .push_back(feedback(6, 250, 450, 500));
    let session = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(session))["state"], "active");
    enable_client_haptics(&mut host, session);
    host.idle();
    assert!(host.plane.borrow().rumble_commands.iter().any(|command| {
        command.slot == 6 && command.low_frequency == 250 && command.high_frequency == 450
    }));
    host.plane.borrow_mut().rumble_busy = true;
    for _ in 0..750 {
        host.tick();
    }
    assert!(host.pipeline.records[&session].rumble_stop_unconfirmed);
    assert_eq!(
        host.pipeline.records[&session].error,
        Some("rumble_zero_delivery_timeout")
    );
    assert!(host.broker.input_lease.is_none());
}

#[test]
fn gamepad_feedback_is_polled_at_40ms_and_replacement_expires_with_zero() {
    let mut host = Harness::new();
    enable_input(&mut host);
    host.broker.input_controller_supported = Some(json!(true));
    host.broker.gamepad_feedback.extend([
        feedback(2, 0x1234, 0x5678, 100),
        feedback(2, 0x2222, 0x3333, 60),
    ]);
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");
    enable_client_haptics(&mut host, id);
    host.idle();

    let polls = host
        .broker
        .calls
        .iter()
        .filter(|call| {
            call.service == "amaterasu.input/1" && call.payload["action"] == "poll_gamepad_feedback"
        })
        .count();
    assert!(
        polls >= 3,
        "40 ms cadence should continue while the gamepad lease is active"
    );
    assert!(
        host.broker.gamepad_poll_times.windows(2).all(|pair| {
            let interval = pair[1] - pair[0];
            (40..=50).contains(&interval)
        }),
        "observed poll times: {:?}",
        host.broker.gamepad_poll_times
    );
    let commands = &host.plane.borrow().rumble_commands;
    let first = commands
        .iter()
        .position(|command| {
            command.slot == 2 && command.low_frequency == 0x1234 && command.high_frequency == 0x5678
        })
        .expect("first effect sent");
    let replacement = commands
        .iter()
        .position(|command| {
            command.slot == 2 && command.low_frequency == 0x2222 && command.high_frequency == 0x3333
        })
        .expect("replacement effect sent");
    assert!(first < replacement);
    assert!(
        !commands[first + 1..replacement]
            .iter()
            .any(|command| { command.low_frequency == 0 && command.high_frequency == 0 }),
        "the newer effect replaces the earlier effect before expiry"
    );
    assert!(commands.iter().any(|command| command.slot == 2
        && command.low_frequency == 0
        && command.high_frequency == 0));
}

#[test]
fn stop_attempts_terminal_zero_before_releasing_local_input() {
    let mut host = Harness::new();
    enable_input(&mut host);
    host.broker.input_controller_supported = Some(json!(true));
    host.broker
        .gamepad_feedback
        .push_back(feedback(1, 500, 900, 5_000));
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");
    enable_client_haptics(&mut host, id);
    host.idle();
    assert!(
        host.plane
            .borrow()
            .rumble_commands
            .iter()
            .any(|command| command.low_frequency == 500)
    );

    let response = host.response(3, json!({"action":"stop","session":id}));
    assert_eq!(response["state"], "stopped", "{response}");
    let timeline = host.broker.timeline.borrow();
    let zero_index = timeline
        .iter()
        .position(|event| *event == "rumble_zero")
        .expect("terminal zero attempted");
    let release_index = timeline
        .iter()
        .position(|event| *event == "input_release")
        .expect("local input release");
    let control_close_index = timeline
        .iter()
        .position(|event| *event == "control_close")
        .expect("control resource close");
    assert!(zero_index < release_index && release_index < control_close_index);
    assert_eq!(
        response["remote_rumble_stop_unconfirmed"], true,
        "provider acceptance is not a client acknowledgement"
    );
}

#[test]
fn failed_remote_zero_does_not_retain_local_input_lease() {
    let mut host = Harness::new();
    enable_input(&mut host);
    host.broker.input_controller_supported = Some(json!(true));
    host.broker
        .gamepad_feedback
        .push_back(feedback(0, 10, 20, 5_000));
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");
    enable_client_haptics(&mut host, id);
    host.idle();
    host.plane.borrow_mut().rumble_error = true;
    let result = host.response(3, json!({"action":"stop","session":id}));
    assert_eq!(result["state"], "failed", "{result}");
    assert!(
        host.broker.input_lease.is_none(),
        "local input is always released"
    );
    assert_eq!(result["remote_rumble_stop_unconfirmed"], true);
}

#[test]
fn busy_control_send_times_out_remote_zero_but_completes_local_cleanup() {
    let mut host = Harness::new();
    enable_input(&mut host);
    host.broker.input_controller_supported = Some(json!(true));
    host.broker
        .gamepad_feedback
        .push_back(feedback(4, 100, 200, 5_000));
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");
    enable_client_haptics(&mut host, id);
    host.idle();
    host.plane.borrow_mut().rumble_busy = true;

    let stopped = host.response(3, json!({"action":"stop","session":id}));
    assert_eq!(stopped["state"], "stopped", "{stopped}");
    assert_eq!(stopped["remote_rumble_stop_unconfirmed"], true);
    assert!(
        host.broker.input_lease.is_none(),
        "local lease is released after the bounded send window"
    );
    assert!(
        host.now < 2_000,
        "cleanup must not wait indefinitely: {} ms",
        host.now
    );
}

#[test]
fn reused_controller_slot_does_not_inherit_prior_session_motor_state() {
    let mut host = Harness::new();
    enable_input(&mut host);
    host.broker.input_controller_supported = Some(json!(true));
    host.broker
        .gamepad_feedback
        .push_back(feedback(3, 300, 400, 5_000));
    let first = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(first))["state"], "active");
    enable_client_haptics(&mut host, first);
    host.idle();
    let stopped = host.response(3, json!({"action":"stop","session":first}));
    assert_eq!(stopped["state"], "stopped");

    let mut new_operation = prepare();
    new_operation["operation_id"] = json!("rumble-reuse-second-session");
    let second = host.response(4, new_operation)["session"].as_u64().unwrap();
    assert_eq!(host.response(5, start(second))["state"], "active");
    let record = &host.pipeline.records[&second];
    assert!(record.rumble_active_until.is_empty());
    assert!(record.rumble_pending.is_empty());
}

fn keyboard(pressed: bool, virtual_key: u8) -> crate::plane::ControlRecord {
    keyboard_packet(pressed, u16::from(virtual_key) | 0x8000, 0)
}

fn keyboard_packet(pressed: bool, key_code: u16, flags: u8) -> crate::plane::ControlRecord {
    keyboard_packet_with_modifiers(pressed, key_code, flags, 0)
}

fn keyboard_packet_with_modifiers(
    pressed: bool,
    key_code: u16,
    flags: u8,
    modifiers: u8,
) -> crate::plane::ControlRecord {
    let magic = if pressed { 3 } else { 4 };
    let key_code = key_code.to_le_bytes();
    crate::plane::ControlRecord {
        message_type: 0x0206,
        payload: vec![
            0,
            0,
            0,
            10,
            magic,
            0,
            0,
            0,
            flags,
            key_code[0],
            key_code[1],
            modifiers,
            0,
            0,
        ],
        sequence: 1,
    }
}

#[test]
fn synthetic_repeat_modifiers_survive_the_stream_session_wire_projection() {
    let mut host = Harness::new();
    enable_input(&mut host);
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");

    host.plane
        .borrow_mut()
        .controls
        .push(keyboard_packet_with_modifiers(true, 0x8041, 0, 3));
    host.idle();

    assert_eq!(
        host.broker.input_events,
        [
            json!({"kind":"key","key":{"platform":"moonlight_vk","code":0x10,"temporary_modifier":true},"state":"down"}),
            json!({"kind":"key","key":{"platform":"moonlight_vk","code":0x11,"temporary_modifier":true},"state":"down"}),
            json!({"kind":"key","key":{"platform":"moonlight_vk","code":0x41,"repeat_modifiers":3},"state":"down"}),
            json!({"kind":"key","key":{"platform":"moonlight_vk","code":0x11,"temporary_modifier":true},"state":"up"}),
            json!({"kind":"key","key":{"platform":"moonlight_vk","code":0x10,"temporary_modifier":true},"state":"up"}),
        ]
    );
}

#[test]
fn stream_start_snapshots_scoped_input_policy_into_input_acquire() {
    let mut host = Harness::new();
    enable_input(&mut host);
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    let mut start_request = start(id);
    start_request["configuration"]["back_button_timeout"] = json!(-1);
    start_request["configuration"]["controller"] = json!(false);
    start_request["configuration"]["gamepad"] = json!("auto");
    start_request["configuration"]["keyboard"] = json!(false);
    start_request["configuration"]["mouse"] = json!(true);
    start_request["configuration"]["high_resolution_scrolling"] = json!(false);
    start_request["configuration"]["always_send_scancodes"] = json!(false);
    start_request["configuration"]["key_rightalt_to_key_win"] = json!(true);
    start_request["configuration"]["key_repeat_delay"] = json!(0);
    start_request["configuration"]["key_repeat_frequency"] = json!(0.0);
    start_request["configuration"]["ds4_back_as_touchpad_click"] = json!(true);
    start_request["configuration"]["motion_as_ds4"] = json!(false);
    start_request["configuration"]["touchpad_as_ds4"] = json!(true);
    start_request["configuration"]["ds5_inputtino_randomize_mac"] = json!(false);

    assert_eq!(host.response(2, start_request)["state"], "active");
    let acquire = host
        .broker
        .calls
        .iter()
        .find(|call| call.service == "amaterasu.input/1" && call.payload["action"] == "acquire")
        .expect("input lease acquisition");
    assert_eq!(acquire.payload["back_button_timeout"], -1);
    assert_eq!(acquire.payload["controller"], false);
    assert_eq!(acquire.payload["gamepad"], "auto");
    assert_eq!(acquire.payload["keyboard"], false);
    assert_eq!(acquire.payload["mouse"], true);
    assert_eq!(acquire.payload["high_resolution_scrolling"], false);
    assert_eq!(acquire.payload["always_send_scancodes"], false);
    assert_eq!(acquire.payload["key_rightalt_to_key_win"], true);
    assert_eq!(acquire.payload["key_repeat_delay"], 0);
    assert_eq!(acquire.payload["key_repeat_frequency"], 0.0);
    assert_eq!(acquire.payload["ds4_back_as_touchpad_click"], true);
    assert_eq!(acquire.payload["motion_as_ds4"], false);
    assert_eq!(acquire.payload["touchpad_as_ds4"], true);
    assert_eq!(acquire.payload["ds5_inputtino_randomize_mac"], false);
}

#[test]
fn native_pen_touch_acquire_preserves_explicit_values_and_none() {
    for selected in [None, Some(false), Some(true)] {
        let mut host = Harness::new();
        enable_input(&mut host);
        let id = host.response(1, prepare())["session"].as_u64().unwrap();
        let mut start_request = start(id);
        if let Some(enabled) = selected {
            start_request["configuration"]["native_pen_touch"] = json!(enabled);
        }

        assert_eq!(host.response(2, start_request)["state"], "active");
        let acquire = host
            .broker
            .calls
            .iter()
            .find(|call| call.service == "amaterasu.input/1" && call.payload["action"] == "acquire")
            .expect("input lease acquisition");
        assert_eq!(acquire.payload["native_pen_touch"], json!(selected));
    }
}

#[test]
fn explicit_ds4_profiles_preserve_touchpad_click_option_values() {
    let mut profiles = Vec::new();
    #[cfg(any(windows, target_os = "linux"))]
    profiles.push("ds4");
    #[cfg(windows)]
    profiles.push("vhf_ds4");

    for profile in profiles {
        for setting in [Some(true), Some(false), None] {
            let mut host = Harness::new();
            enable_input(&mut host);
            let id = host.response(1, prepare())["session"].as_u64().unwrap();
            let mut start_request = start(id);
            start_request["configuration"]["gamepad"] = json!(profile);
            if let Some(value) = setting {
                start_request["configuration"]["ds4_back_as_touchpad_click"] = json!(value);
            }

            assert_eq!(host.response(2, start_request)["state"], "active");
            let acquire = host
                .broker
                .calls
                .iter()
                .find(|call| {
                    call.service == "amaterasu.input/1" && call.payload["action"] == "acquire"
                })
                .expect("input lease acquisition");
            match setting {
                Some(value) => {
                    assert_eq!(
                        acquire.payload.get("ds4_back_as_touchpad_click"),
                        Some(&json!(value))
                    );
                }
                None => assert!(acquire.payload.get("ds4_back_as_touchpad_click").is_none()),
            }
        }
    }
}

#[test]
fn input_policy_override_is_forwarded_for_implicitly_selected_gamepad() {
    let mut host = Harness::new();
    enable_input(&mut host);
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    let mut start_request = start(id);
    start_request["configuration"]["ds4_back_as_touchpad_click"] = json!(false);

    assert_eq!(host.response(2, start_request)["state"], "active");
    let acquire = host
        .broker
        .calls
        .iter()
        .find(|call| call.service == "amaterasu.input/1" && call.payload["action"] == "acquire")
        .expect("input lease acquisition");
    assert_eq!(
        acquire.payload.get("ds4_back_as_touchpad_click"),
        Some(&json!(false))
    );
}

fn controller_packet(sequence: u64) -> crate::plane::ControlRecord {
    let mut payload = vec![0, 0, 0, 30, 0x0c, 0, 0, 0];
    payload.extend_from_slice(&0x1au16.to_le_bytes());
    payload.extend_from_slice(&15u16.to_le_bytes());
    payload.extend_from_slice(&0xa55au16.to_le_bytes());
    payload.extend_from_slice(&0x14u16.to_le_bytes());
    payload.extend_from_slice(&0xcdefu16.to_le_bytes());
    payload.extend_from_slice(&[0, 255]);
    payload.extend_from_slice(&i16::MIN.to_le_bytes());
    payload.extend_from_slice(&i16::MAX.to_le_bytes());
    payload.extend_from_slice(&(-1i16).to_le_bytes());
    payload.extend_from_slice(&1i16.to_le_bytes());
    payload.extend_from_slice(&0x9cu16.to_le_bytes());
    payload.extend_from_slice(&0x89abu16.to_le_bytes());
    payload.extend_from_slice(&0x55u16.to_le_bytes());
    crate::plane::ControlRecord {
        message_type: 0x0206,
        payload,
        sequence,
    }
}

fn controller_extension_packet(
    magic: u32,
    body: &[u8],
    sequence: u64,
) -> crate::plane::ControlRecord {
    let mut payload = vec![0u8; 8 + body.len()];
    let declared_size = (payload.len() - 4) as u32;
    payload[..4].copy_from_slice(&declared_size.to_be_bytes());
    payload[4..8].copy_from_slice(&magic.to_le_bytes());
    payload[8..].copy_from_slice(body);
    crate::plane::ControlRecord {
        message_type: 0x0206,
        payload,
        sequence,
    }
}

#[test]
fn controller_input_remains_unsupported_without_failing_an_active_stream() {
    let mut host = Harness::new();
    enable_input(&mut host);
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");

    host.plane.borrow_mut().controls.push(controller_packet(1));
    host.idle();

    assert!(host.broker.input_events.is_empty());
    assert_eq!(host.pipeline.records[&id].unsupported_controls, 1);
    assert_eq!(host.pipeline.records[&id].state, State::Active);
    assert!(host.broker.input_lease.is_some());
}

#[test]
fn controller_state_is_forwarded_when_the_input_lease_advertises_support() {
    let mut host = Harness::new();
    enable_input(&mut host);
    host.broker.input_controller_supported = Some(json!(true));
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    let active = host.response(2, start(id));
    assert_eq!(active["state"], "active");
    assert_eq!(active["controller_input"], true);

    host.plane.borrow_mut().controls.push(controller_packet(1));
    host.idle();

    assert_eq!(
        host.broker.input_events,
        [json!({
            "kind":"controller",
            "number":15,
            "active_mask":0xa55a,
            "buttons":0x89abcdefu32,
            "left_trigger":0,
            "right_trigger":255,
            "left_x":-32768,
            "left_y":32767,
            "right_x":-1,
            "right_y":1,
        })]
    );
    assert_eq!(host.pipeline.records[&id].input_events_sent, 1);
    assert_eq!(host.pipeline.records[&id].unsupported_controls, 0);
}

#[test]
fn authenticated_controller_extension_packets_reach_the_native_input_contract() {
    let mut host = Harness::new();
    enable_input(&mut host);
    host.broker.input_controller_supported = Some(json!(true));
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");

    let mut arrival = [0u8; 8];
    arrival[0] = 3;
    arrival[1] = 2;
    arrival[2..4].copy_from_slice(&0x0123u16.to_le_bytes());
    arrival[4..8].copy_from_slice(&0x89ab_cdefu32.to_le_bytes());
    let mut controls = vec![controller_extension_packet(0x55000004, &arrival, 1)];

    for event_type in 0..=7 {
        let mut touch = [0u8; 20];
        touch[0] = 3;
        touch[1] = event_type;
        touch[3] = 1;
        touch[4..8].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        touch[8..12].copy_from_slice(&1.25f32.to_bits().to_le_bytes());
        touch[12..16].copy_from_slice(&(-0.25f32).to_bits().to_le_bytes());
        touch[16..20].copy_from_slice(&0.75f32.to_bits().to_le_bytes());
        controls.push(controller_extension_packet(
            0x55000005,
            &touch,
            u64::from(event_type) + 2,
        ));
    }

    let mut motion = [0u8; 16];
    motion[0] = 3;
    motion[1] = 1;
    motion[4..8].copy_from_slice(&(-1.25f32).to_bits().to_le_bytes());
    motion[8..12].copy_from_slice(&2.5f32.to_bits().to_le_bytes());
    motion[12..16].copy_from_slice(&3.75f32.to_bits().to_le_bytes());
    controls.push(controller_extension_packet(0x55000006, &motion, 10));
    controls.push(controller_extension_packet(
        0x55000007,
        &[3, 5, 0xff, 0],
        11,
    ));

    host.plane.borrow_mut().controls.extend(controls);
    host.idle();

    assert_eq!(host.broker.input_events.len(), 11);
    assert_eq!(
        host.broker.input_events[0],
        json!({
            "kind":"controller_arrival",
            "number":3,
            "controller_type":2,
            "capabilities":0x0123,
            "supported_buttons":0x89ab_cdefu32,
        })
    );
    let phases = [
        "hover",
        "down",
        "up",
        "move",
        "cancel",
        "button_only",
        "hover_leave",
        "cancel_all",
    ];
    for (index, phase) in phases.into_iter().enumerate() {
        assert_eq!(
            host.broker.input_events[index + 1],
            json!({
                "kind":"controller_touch",
                "number":3,
                "touchpad_index":1,
                "pointer_id":0x1234_5678u32,
                "phase":phase,
                "x":1.0,
                "y":0.0,
                "pressure":0.75,
            })
        );
    }
    assert_eq!(
        host.broker.input_events[9],
        json!({
            "kind":"controller_motion",
            "number":3,
            "motion_type":"accel",
            "x":-1.25,
            "y":2.5,
            "z":3.75,
        })
    );
    assert_eq!(
        host.broker.input_events[10],
        json!({
            "kind":"controller_battery",
            "number":3,
            "state":"full",
            "percentage":null,
        })
    );
    assert_eq!(host.pipeline.records[&id].input_events_sent, 11);
    assert_eq!(host.pipeline.records[&id].input_batches, 1);
    assert_eq!(host.pipeline.records[&id].unsupported_controls, 0);
}

#[test]
fn controller_extension_packets_are_dropped_without_native_controller_support() {
    let mut host = Harness::new();
    enable_input(&mut host);
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");

    let mut motion = [0u8; 16];
    motion[1] = 1;
    host.plane.borrow_mut().controls.extend([
        controller_extension_packet(0x55000004, &[0; 8], 1),
        controller_extension_packet(0x55000005, &[0; 20], 2),
        controller_extension_packet(0x55000006, &motion, 3),
        controller_extension_packet(0x55000007, &[0; 4], 4),
    ]);
    host.idle();

    assert!(host.broker.input_events.is_empty());
    assert_eq!(host.pipeline.records[&id].unsupported_controls, 4);
    assert_eq!(host.pipeline.records[&id].state, State::Active);
}

#[test]
fn controller_extension_batch_failure_drops_only_the_uncertain_native_event() {
    let mut host = Harness::new();
    enable_input(&mut host);
    host.broker.input_controller_supported = Some(json!(true));
    host.broker.fail_controller_input = true;
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");

    let mut motion = [0u8; 16];
    motion[1] = 1;
    host.plane.borrow_mut().controls.extend([
        controller_extension_packet(0x55000004, &[0; 8], 1),
        controller_extension_packet(0x55000005, &[0; 20], 2),
        controller_extension_packet(0x55000006, &motion, 3),
        controller_extension_packet(0x55000007, &[0; 4], 4),
    ]);
    host.idle();

    assert_eq!(host.pipeline.records[&id].state, State::Active);
    assert_eq!(host.pipeline.records[&id].input_events_sent, 3);
    assert_eq!(host.pipeline.records[&id].unsupported_controls, 1);
    assert!(host.pipeline.records[&id].input_events.is_empty());
}

#[test]
fn unavailable_virtual_gamepad_is_dropped_without_stopping_the_stream_or_input_lease() {
    let mut host = Harness::new();
    enable_input(&mut host);
    host.broker.input_controller_supported = Some(json!(true));
    host.broker.fail_controller_input = true;
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");

    host.plane
        .borrow_mut()
        .controls
        .extend([controller_packet(1), controller_packet(2)]);
    host.idle();

    assert_eq!(host.pipeline.records[&id].state, State::Active);
    assert_eq!(host.pipeline.records[&id].unsupported_controls, 1);
    assert_eq!(host.pipeline.records[&id].input_events_sent, 1);
    assert!(host.pipeline.records[&id].input_lease.is_some());
    assert_eq!(host.broker.tray_vigem_notifications, 1);

    host.plane.borrow_mut().controls.push(relative(3));
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Active);
    assert_eq!(host.pipeline.records[&id].input_events_sent, 2);
    assert!(host.pipeline.records[&id].input_lease.is_some());
    assert_eq!(host.broker.tray_vigem_notifications, 1);
}

fn relative(sequence: u64) -> crate::plane::ControlRecord {
    crate::plane::ControlRecord {
        message_type: 0x0206,
        payload: vec![0, 0, 0, 8, 7, 0, 0, 0, 0, 1, 0, 2],
        sequence,
    }
}

fn vertical_scroll(sequence: u64, amount: i16) -> crate::plane::ControlRecord {
    let mut payload = vec![0, 0, 0, 10, 10, 0, 0, 0];
    payload.extend_from_slice(&amount.to_be_bytes());
    payload.extend_from_slice(&amount.to_be_bytes());
    payload.extend_from_slice(&[0, 0]);
    crate::plane::ControlRecord {
        message_type: 0x0206,
        payload,
        sequence,
    }
}

fn horizontal_scroll(sequence: u64, amount: i16) -> crate::plane::ControlRecord {
    let mut payload = vec![0, 0, 0, 6, 1, 0, 0, 0x55];
    payload.extend_from_slice(&amount.to_be_bytes());
    crate::plane::ControlRecord {
        message_type: 0x0206,
        payload,
        sequence,
    }
}

fn absolute(sequence: u64, x: u16, y: u16, width: u16, height: u16) -> crate::plane::ControlRecord {
    let mut payload = vec![0, 0, 0, 14, 5, 0, 0, 0];
    payload.extend_from_slice(&x.to_be_bytes());
    payload.extend_from_slice(&y.to_be_bytes());
    payload.extend_from_slice(&[0, 0]);
    payload.extend_from_slice(&(width - 1).to_be_bytes());
    payload.extend_from_slice(&(height - 1).to_be_bytes());
    crate::plane::ControlRecord {
        message_type: 0x0206,
        payload,
        sequence,
    }
}

fn desktop_geometry() -> Value {
    json!({
        "output": {"left":-1920,"top":0,"width":1920,"height":1080},
        "virtual_desktop": {"left":-1920,"top":-200,"width":3840,"height":1440},
    })
}

fn touch_packet(magic: u32, event: u8, pointer_id: u32) -> crate::plane::ControlRecord {
    let mut payload = vec![0u8; 36];
    payload[..4].copy_from_slice(&32u32.to_be_bytes());
    payload[4..8].copy_from_slice(&magic.to_le_bytes());
    payload[8] = event;
    if magic == 0x55000002 {
        payload[12..16].copy_from_slice(&pointer_id.to_le_bytes());
        for offset in [16, 20, 24, 28, 32] {
            payload[offset..offset + 4].copy_from_slice(&0.5f32.to_bits().to_le_bytes());
        }
    } else {
        payload[9] = 1;
        payload[10] = 3;
        payload[26] = 30;
        for offset in [12, 16, 20, 28, 32] {
            payload[offset..offset + 4].copy_from_slice(&0.5f32.to_bits().to_le_bytes());
        }
    }
    crate::plane::ControlRecord {
        message_type: 0x0206,
        payload,
        sequence: u64::from(event),
    }
}

fn touch_port() -> Value {
    json!({"width":1920,"height":1080,"client_offset_x":0,"client_offset_y":0,
        "logical_width":1920,"logical_height":1080})
}

#[test]
fn large_logical_touchport_caps_contact_area_without_changing_coordinates() {
    let mut host = Harness::new();
    enable_input(&mut host);
    host.broker.desktop_geometry = Some(desktop_geometry());
    let mut port = touch_port();
    port["logical_width"] = json!(1_000_000);
    port["logical_height"] = json!(1_000_000);
    host.broker.touch_port = Some(port);
    host.broker.input_pen_touch_supported = Some(json!(true));
    host.broker.input_native_pen_touch = Some(json!(true));
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");
    host.plane
        .borrow_mut()
        .controls
        .push(touch_packet(0x55000002, 1, 17));
    host.idle();

    let event = &host.broker.input_events[0];
    let rect = serde_json::from_value::<amaterasu_media::CaptureGeometry>(desktop_geometry())
        .unwrap()
        .normalized_output()
        .unwrap();
    assert_eq!(
        event["x"],
        json!(((f32::from(rect.left) + f32::from(rect.right)) / 2.0).round() as u16)
    );
    assert_eq!(
        event["y"],
        json!(((f32::from(rect.top) + f32::from(rect.bottom)) / 2.0).round() as u16)
    );
    assert_eq!(event["contact_area_major"], 1.0);
    assert_eq!(event["contact_area_minor"], 1.0);
}

#[test]
fn inset_touchport_reaches_both_capture_edges_and_bad_metadata_fails_closed() {
    let mut host = Harness::new();
    enable_input(&mut host);
    host.broker.desktop_geometry = Some(desktop_geometry());
    host.broker.touch_port = Some(json!({"width":1920,"height":1080,
        "client_offset_x":120,"client_offset_y":60,
        "logical_width":1920,"logical_height":1080}));
    host.broker.input_pen_touch_supported = Some(json!(true));
    host.broker.input_native_pen_touch = Some(json!(true));
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");
    let mut left = touch_packet(0x55000002, 1, 7);
    left.payload[16..20].copy_from_slice(&0f32.to_bits().to_le_bytes());
    left.payload[20..24].copy_from_slice(&0f32.to_bits().to_le_bytes());
    let mut right = touch_packet(0x55000002, 3, 7);
    right.payload[16..20].copy_from_slice(&1f32.to_bits().to_le_bytes());
    right.payload[20..24].copy_from_slice(&1f32.to_bits().to_le_bytes());
    host.plane.borrow_mut().controls.extend([left, right]);
    host.idle();
    let rect = serde_json::from_value::<amaterasu_media::CaptureGeometry>(desktop_geometry())
        .unwrap()
        .normalized_output()
        .unwrap();
    assert_eq!(
        (
            host.broker.input_events[0]["x"].as_u64(),
            host.broker.input_events[0]["y"].as_u64()
        ),
        (Some(u64::from(rect.left)), Some(u64::from(rect.top)))
    );
    assert_eq!(
        (
            host.broker.input_events[1]["x"].as_u64(),
            host.broker.input_events[1]["y"].as_u64()
        ),
        (Some(u64::from(rect.right)), Some(u64::from(rect.bottom)))
    );

    for bad in [
        json!({"width":1920.5,"height":1080,"client_offset_x":0,"client_offset_y":0,"logical_width":1920,"logical_height":1080}),
        json!({"width":-1,"height":1080,"client_offset_x":0,"client_offset_y":0,"logical_width":1920,"logical_height":1080}),
        json!({"width":1920,"height":1080,"client_offset_x":960,"client_offset_y":0,"logical_width":1920,"logical_height":1080}),
        json!({"width":1920,"height":1080,"client_offset_x":0,"client_offset_y":0,"logical_width":1920,"logical_height":1080,"extra":true}),
    ] {
        let mut host = Harness::new();
        enable_input(&mut host);
        host.broker.desktop_geometry = Some(desktop_geometry());
        host.broker.touch_port = Some(bad);
        host.broker.input_pen_touch_supported = Some(json!(true));
        host.broker.input_native_pen_touch = Some(json!(true));
        let id = host.response(1, prepare())["session"].as_u64().unwrap();
        assert_eq!(host.response(2, start(id))["state"], "active");
        host.plane
            .borrow_mut()
            .controls
            .push(touch_packet(0x55000002, 1, 7));
        host.idle();
        assert!(host.broker.input_events.is_empty());
    }
}

#[test]
fn authenticated_pen_touch_requires_capability_policy_and_authoritative_port() {
    let mut host = Harness::new();
    enable_input(&mut host);
    host.broker.desktop_geometry = Some(desktop_geometry());
    host.broker.touch_port = Some(touch_port());
    host.broker.input_pen_touch_supported = Some(json!(true));
    host.broker.input_native_pen_touch = Some(json!(true));
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");
    host.plane.borrow_mut().controls.extend([
        touch_packet(0x55000002, 1, 42),
        touch_packet(0x55000003, 1, 0),
        touch_packet(0x55000002, 4, 42),
    ]);
    host.idle();
    assert_eq!(host.broker.input_events.len(), 3);
    assert_eq!(host.broker.input_events[0]["pointer_id"], 42);
    assert_eq!(host.broker.input_events[0]["phase"], "down");
    assert_eq!(host.broker.input_events[1]["pointer_id"], 1);
    assert_eq!(host.broker.input_events[1]["device"], "pen");
    assert_eq!(host.broker.input_events[1]["pen_buttons"], 3);
    assert_eq!(host.broker.input_events[2]["phase"], "cancel");
    assert!(host.broker.input_events[0]["x"].as_u64().unwrap() < 32768);
    assert!(
        host.broker.input_events[0]["contact_area_major"]
            .as_f64()
            .unwrap()
            > 0.0
    );
    assert_eq!(
        host.response(3, json!({"action":"stop","session":id}))["state"],
        "stopped"
    );
    assert!(host.broker.input_lease.is_none());

    for blocked in ["capability", "policy", "port"] {
        let mut host = Harness::new();
        enable_input(&mut host);
        host.broker.desktop_geometry = Some(desktop_geometry());
        host.broker.touch_port = (blocked != "port").then(touch_port);
        host.broker.input_pen_touch_supported = Some(json!(blocked != "capability"));
        host.broker.input_native_pen_touch = Some(json!(blocked != "policy"));
        let id = host.response(1, prepare())["session"].as_u64().unwrap();
        assert_eq!(host.response(2, start(id))["state"], "active");
        host.plane
            .borrow_mut()
            .controls
            .push(touch_packet(0x55000002, 1, 42));
        host.idle();
        assert!(host.broker.input_events.is_empty(), "{blocked}");
        assert_eq!(host.pipeline.records[&id].unsupported_controls, 1);
    }
}

#[test]
fn active_input_is_batched_and_released_before_media_drain() {
    let mut host = Harness::new();
    enable_input(&mut host);
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    let active = host.response(2, start(id));
    assert_eq!(active["state"], "active");
    assert_eq!(active["input_active"], true);

    host.plane.borrow_mut().controls.extend([
        keyboard(true, 0x41),
        relative(2),
        keyboard(false, 0x41),
    ]);
    host.idle();
    assert_eq!(host.broker.input_events.len(), 3);
    assert_eq!(host.broker.input_events[0]["kind"], "key");
    assert_eq!(host.broker.input_events[0]["key"]["code"], 0x41);
    assert_eq!(host.broker.input_events[1]["kind"], "relative");
    assert_eq!(host.pipeline.records[&id].input_events_sent, 3);

    let stopped = host.response(3, json!({"action":"stop","session":id}));
    assert_eq!(stopped["state"], "stopped");
    let release = host
        .broker
        .calls
        .iter()
        .position(|call| call.service == "amaterasu.input/1" && call.payload["action"] == "release")
        .unwrap();
    let drain = host
        .broker
        .calls
        .iter()
        .position(|call| {
            call.service == Resource::Video.service() && call.payload["action"] == "drain"
        })
        .unwrap();
    assert!(release < drain);
    assert!(host.broker.input_lease.is_none());
}

#[test]
fn raw_keyboard_packets_keep_the_keydown_mode_through_release() {
    let mut host = Harness::new();
    enable_input(&mut host);
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");

    host.plane.borrow_mut().controls.extend([
        keyboard_packet(true, 0x0041, 1),
        keyboard_packet(false, 0x8041, 0),
    ]);
    host.idle();

    assert_eq!(
        host.broker.input_events,
        [
            json!({"kind":"key","key":{"platform":"moonlight_vk","code":0x41,"normalized":false},"state":"down"}),
            json!({"kind":"key","key":{"platform":"moonlight_vk","code":0x41,"normalized":false},"state":"up"}),
        ]
    );
}

#[test]
fn raw_vertical_and_horizontal_scroll_units_keep_order_and_fractional_amounts() {
    let mut host = Harness::new();
    enable_input(&mut host);
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");

    host.plane.borrow_mut().controls.extend([
        vertical_scroll(1, 60),
        horizontal_scroll(2, -30),
        vertical_scroll(3, 0),
    ]);
    host.idle();

    assert_eq!(
        host.broker.input_events,
        [
            json!({"kind":"scroll_units","axis":"vertical","amount":60}),
            json!({"kind":"scroll_units","axis":"horizontal","amount":-30}),
        ]
    );
    assert_eq!(host.pipeline.records[&id].input_events_sent, 2);
}

#[test]
fn input_before_activation_is_discarded_without_mutating_translator_state() {
    let mut host = Harness::new();
    enable_input(&mut host);
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    host.broker.enqueue(2, "owner", start(id));
    for _ in 0..100 {
        host.tick();
        if host.pipeline.records[&id].plane.is_some()
            && host.pipeline.records[&id].state == State::Starting
        {
            break;
        }
    }
    assert_eq!(host.pipeline.records[&id].state, State::Starting);
    host.plane.borrow_mut().controls.push(keyboard(true, 0x41));
    host.tick();
    for _ in 0..100 {
        if host.broker.replies.contains_key(&2) {
            break;
        }
        host.tick();
    }
    assert!(host.broker.replies.contains_key(&2));
    assert_eq!(host.pipeline.records[&id].rejected_controls, 1);
    assert!(host.broker.input_events.is_empty());

    host.plane.borrow_mut().controls.push(keyboard(true, 0x41));
    host.idle();
    assert_eq!(host.broker.input_events[0]["state"], "down");
}

#[test]
fn lost_acquire_and_lost_batch_are_settled_without_replaying_effects() {
    let mut acquire = Harness::new();
    enable_input(&mut acquire);
    let id = acquire.response(1, prepare())["session"].as_u64().unwrap();
    acquire.broker.input_cleanup_required = true;
    acquire.broker.input_expires_in_ms = Some(0);
    acquire.broker.fault = Some(("amaterasu.input/1".into(), Fault::AfterMutation));
    let failed = acquire.response(2, start(id));
    assert_eq!(failed["state"], "failed");
    assert!(acquire.broker.input_lease.is_none());
    assert_eq!(
        acquire
            .broker
            .calls
            .iter()
            .filter(
                |call| call.service == "amaterasu.input/1" && call.payload["action"] == "acquire"
            )
            .count(),
        1
    );

    let mut batch = Harness::new();
    enable_input(&mut batch);
    let id = batch.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(batch.response(2, start(id))["state"], "active");
    batch.broker.fault = Some(("amaterasu.input/1".into(), Fault::AfterMutation));
    batch.plane.borrow_mut().controls.push(relative(1));
    batch.idle();
    assert_eq!(batch.pipeline.records[&id].state, State::Failed);
    assert_eq!(batch.broker.input_events.len(), 1);
    assert_eq!(
        batch
            .broker
            .calls
            .iter()
            .filter(|call| call.service == "amaterasu.input/1" && call.payload["action"] == "batch")
            .count(),
        1
    );
    assert!(batch.broker.input_lease.is_none());
}

#[test]
fn delayed_cleanup_never_recreates_an_expired_unknown_input_admission() {
    let mut host = Harness::new();
    enable_input(&mut host);
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    let input_operation = host.pipeline.records[&id].input_operation();
    let mut pending = rpc::Pending::new(
        "amaterasu.input/1",
        json!({
            "action": "acquire",
            "operation_id": input_operation,
            "ttl_ms": 1_000,
        }),
        format!("ss-{id:x}-lost-input-admission"),
        None,
    );
    host.broker.fault = Some(("amaterasu.input/1".into(), Fault::Admission));
    assert!(matches!(
        pending.poll(&mut |command| host.broker.send(command), host.now),
        rpc::Progress::Waiting
    ));
    {
        let record = host.pipeline.records.get_mut(&id).unwrap();
        record.input_attempted = true;
        record.uncertain = Some(pending);
        record.state = State::CleanupFailed;
    }

    host.now += 300_000;
    let stopped = host.response(2, json!({"action":"stop","session":id}));
    assert_eq!(stopped["state"], "stopped");
    assert_eq!(
        host.broker
            .calls
            .iter()
            .filter(
                |call| call.service == "amaterasu.input/1" && call.payload["action"] == "acquire"
            )
            .count(),
        1
    );
    assert!(host.broker.input_lease.is_none());
    assert!(
        host.broker.calls.iter().any(|call| {
            call.service == "amaterasu.input/1" && call.payload["action"] == "lookup"
        })
    );
}

#[test]
fn queue_overflow_stops_session_and_releases_input() {
    let mut host = Harness::new();
    enable_input(&mut host);
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    let active = host.response(2, start(id));
    assert_eq!(active["state"], "active");
    assert_eq!(active["absolute_input"], false);
    host.plane
        .borrow_mut()
        .controls
        .extend((0..129).map(relative));
    host.idle();
    let record = &host.pipeline.records[&id];
    assert_eq!(record.state, State::Failed);
    assert_eq!(record.error, Some("input_queue_overflow"));
    assert!(host.broker.input_lease.is_none());
}

#[test]
fn renewal_uses_its_submission_clock_and_expiry_stops_input() {
    let mut renewed = Harness::new();
    enable_input(&mut renewed);
    let id = renewed.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(renewed.response(2, start(id))["state"], "active");
    renewed.now = renewed.pipeline.records[&id].input_renew_at;
    renewed.idle();
    let record = &renewed.pipeline.records[&id];
    assert_eq!(record.state, State::Active);
    assert_eq!(
        record.input_expires_at,
        record.input_request_started_at + record.input_ttl_ms
    );
    assert!(
        renewed.broker.calls.iter().any(|call| {
            call.service == "amaterasu.input/1" && call.payload["action"] == "renew"
        })
    );

    let mut expired = Harness::new();
    enable_input(&mut expired);
    let id = expired.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(expired.response(2, start(id))["state"], "active");
    expired.now = expired.pipeline.records[&id].input_expires_at;
    expired.idle();
    assert_eq!(expired.pipeline.records[&id].state, State::Failed);
    assert_eq!(
        expired.pipeline.records[&id].error,
        Some("input_lease_expired")
    );
    assert!(expired.broker.input_lease.is_none());
}

#[test]
fn disabled_input_is_reported_unsupported_without_provider_calls() {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");
    host.plane.borrow_mut().controls.push(keyboard(true, 0x41));
    host.idle();
    assert_eq!(host.pipeline.records[&id].unsupported_controls, 1);
    assert!(
        host.broker
            .calls
            .iter()
            .all(|call| call.service != "amaterasu.input/1")
    );
}

#[test]
fn authoritative_capture_geometry_enables_absolute_output_mapping() {
    let mut host = Harness::new();
    enable_input(&mut host);
    host.broker.desktop_geometry = Some(desktop_geometry());
    host.broker.input_absolute_pointer = Some(json!(true));
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    let active = host.response(2, start(id));
    assert_eq!(active["state"], "active");
    assert_eq!(active["absolute_input"], true);
    host.plane.borrow_mut().controls.extend([
        absolute(1, 0, 0, 1000, 500),
        absolute(2, 999, 499, 1000, 500),
    ]);
    host.idle();

    let geometry: amaterasu_media::CaptureGeometry =
        serde_json::from_value(desktop_geometry()).unwrap();
    let expected = geometry.normalized_output().unwrap();
    assert_eq!(host.broker.input_events.len(), 2);
    assert_eq!(
        host.broker.input_events[0],
        json!({"kind":"absolute","x":expected.left,"y":expected.top})
    );
    assert_eq!(
        host.broker.input_events[1],
        json!({"kind":"absolute","x":expected.right,"y":expected.bottom})
    );
}

#[test]
fn absent_geometry_is_unsupported_and_malformed_geometry_fails_startup() {
    let mut absent = Harness::new();
    enable_input(&mut absent);
    let id = absent.response(1, prepare())["session"].as_u64().unwrap();
    let active = absent.response(2, start(id));
    assert_eq!(active["state"], "active");
    assert_eq!(active["absolute_input"], false);
    absent
        .plane
        .borrow_mut()
        .controls
        .push(absolute(1, 999, 499, 1000, 500));
    absent.idle();
    assert_eq!(absent.pipeline.records[&id].unsupported_controls, 1);
    assert!(absent.broker.input_events.is_empty());

    let mut malformed = Harness::new();
    enable_input(&mut malformed);
    let mut geometry = desktop_geometry();
    geometry["output"]["width"] = json!(0);
    malformed.broker.desktop_geometry = Some(geometry);
    let id = malformed.response(1, prepare())["session"]
        .as_u64()
        .unwrap();
    let failed = malformed.response(2, start(id));
    assert_eq!(failed["state"], "failed");
    assert_eq!(failed["error"], "invalid_capture_geometry");
    assert_eq!(malformed.broker.live_components(), 0);
    assert!(malformed.broker.input_lease.is_none());

    let mut malformed_capability = Harness::new();
    enable_input(&mut malformed_capability);
    malformed_capability.broker.desktop_geometry = Some(desktop_geometry());
    malformed_capability.broker.input_absolute_pointer = Some(json!("true"));
    let id = malformed_capability.response(1, prepare())["session"]
        .as_u64()
        .unwrap();
    let failed = malformed_capability.response(2, start(id));
    assert_eq!(failed["state"], "failed");
    assert_eq!(failed["error"], "invalid_input_response");
    assert!(malformed_capability.broker.input_lease.is_none());

    let mut malformed_controller_capability = Harness::new();
    enable_input(&mut malformed_controller_capability);
    malformed_controller_capability
        .broker
        .input_controller_supported = Some(json!(1));
    let id = malformed_controller_capability.response(1, prepare())["session"]
        .as_u64()
        .unwrap();
    let failed = malformed_controller_capability.response(2, start(id));
    assert_eq!(failed["state"], "failed");
    assert_eq!(failed["error"], "invalid_input_response");
    assert!(malformed_controller_capability.broker.input_lease.is_none());
}

#[test]
fn geometry_without_input_backend_capability_remains_unsupported() {
    let mut host = Harness::new();
    enable_input(&mut host);
    host.broker.desktop_geometry = Some(desktop_geometry());
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    let active = host.response(2, start(id));
    assert_eq!(active["state"], "active");
    assert_eq!(active["absolute_input"], false);
    host.plane
        .borrow_mut()
        .controls
        .push(absolute(1, 999, 499, 1000, 500));
    host.idle();
    assert_eq!(host.pipeline.records[&id].unsupported_controls, 1);
    assert!(host.broker.input_events.is_empty());
}

#[test]
fn video_failure_discards_same_tick_absolute_control_and_releases_input() {
    let mut host = Harness::new();
    enable_input(&mut host);
    host.broker.desktop_geometry = Some(desktop_geometry());
    host.broker.input_absolute_pointer = Some(json!(true));
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");
    host.plane.borrow_mut().failure = Some("capture_geometry_changed");
    host.plane
        .borrow_mut()
        .controls
        .push(absolute(1, 999, 499, 1000, 500));
    host.idle();

    assert_eq!(host.pipeline.records[&id].state, State::Failed);
    assert_eq!(
        host.pipeline.records[&id].error,
        Some("capture_geometry_changed")
    );
    assert!(host.broker.input_events.is_empty());
    assert!(host.broker.input_lease.is_none());
}

#[test]
fn second_session_input_busy_never_adopts_or_releases_the_first_lease() {
    let mut host = Harness::new();
    enable_input(&mut host);
    let first = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(first))["state"], "active");
    let first_operation = host.broker.input_operation.clone().unwrap();
    let first_token = host.broker.input_lease.clone().unwrap();

    let mut second_prepare = prepare();
    second_prepare["operation_id"] = json!("launch-two");
    second_prepare["video_port"] = json!(51003);
    second_prepare["audio_port"] = json!(51004);
    let second = host.response(3, second_prepare)["session"]
        .as_u64()
        .unwrap();
    let failed = host.response(4, start(second));
    assert_eq!(failed["state"], "failed");
    assert_eq!(host.pipeline.records[&first].state, State::Active);
    assert_eq!(
        host.broker.input_operation.as_deref(),
        Some(first_operation.as_str())
    );
    assert_eq!(
        host.broker.input_lease.as_deref(),
        Some(first_token.as_str())
    );
    assert!(host.broker.calls.iter().all(|call| {
        call.payload["action"] != "release" || call.payload["lease"] != first_token
    }));

    assert_eq!(
        host.response(5, json!({"action":"stop","session":first}))["state"],
        "stopped"
    );
    assert!(host.broker.input_lease.is_none());
}

#[test]
fn input_acquire_preserves_the_selected_whole_keybindings_list_and_global_inheritance() {
    for selected in [
        None,
        Some(json!(null)),
        Some(json!([])),
        Some(json!([0, 255, 65, 66, 65, 67])),
    ] {
        let mut host = Harness::new();
        enable_input(&mut host);
        let id = host.response(1, prepare())["session"].as_u64().unwrap();
        let mut start_request = start(id);
        if let Some(selected) = &selected {
            start_request["configuration"]["keybindings"] = selected.clone();
        }
        assert_eq!(host.response(2, start_request)["state"], "active");
        let acquire = host
            .broker
            .calls
            .iter()
            .find(|call| call.service == "amaterasu.input/1" && call.payload["action"] == "acquire")
            .expect("input lease acquisition");
        assert_eq!(
            acquire.payload["keybindings"],
            selected.unwrap_or(Value::Null)
        );
    }
}

#[test]
fn malformed_keybindings_start_cannot_acquire_an_input_lease() {
    for invalid in [
        json!([65]),
        json!([0, 256]),
        json!([-1, 0]),
        json!([65.0, 66]),
        json!([true, 66]),
        json!(["65", 66]),
        json!({}),
        json!(vec![0_u8; 514]),
    ] {
        let mut host = Harness::new();
        enable_input(&mut host);
        let id = host.response(1, prepare())["session"].as_u64().unwrap();
        let mut start_request = start(id);
        start_request["configuration"]["keybindings"] = invalid.clone();
        let reply = host.response(2, start_request);
        assert_ne!(reply["state"], "active", "{invalid}: {reply}");
        assert!(host.broker.calls.iter().all(|call| {
            call.service != "amaterasu.input/1" || call.payload["action"] != "acquire"
        }));
    }
}

#[test]
fn feedback_device_incarnations_are_consumed_and_stale_generation_fails_closed() {
    let mut host = Harness::new();
    enable_input(&mut host);
    host.pipeline
        .settings
        .allowed_principals
        .push("amaterasu.rtsp".into());
    let id = host.response_from(1, "amaterasu.rtsp", prepare())["session"]
        .as_u64()
        .unwrap();
    assert_eq!(
        host.response_from(2, "amaterasu.rtsp", start(id))["state"],
        "active"
    );
    let rumble = |sequence: u64, generation: u64| {
        json!({"sequence":sequence,"device_generation":generation,"device_capabilities":0,
        "feedback":{"type":"rumble","controller":2,"low_frequency":0,"high_frequency":0}})
    };
    host.broker.input_feedback_events.push_back(rumble(1, 1));
    host.broker.input_feedback_events.push_back(rumble(2, 2));
    host.idle();
    assert_eq!(
        host.pipeline.records[&id]
            .feedback_device_observations
            .get(&2),
        Some(&(2, 0))
    );
    assert_eq!(host.broker.input_feedback_acks, vec![2]);
    let delivered = host.plane.borrow().feedback.len();
    host.broker.input_feedback_events.push_back(rumble(3, 1));
    host.idle();
    assert_eq!(host.plane.borrow().feedback.len(), delivered);
    assert_eq!(
        host.pipeline.records[&id].error,
        Some("stale_feedback_device")
    );
}

#[test]
fn selected_stream_owner_requests_and_validates_active_source_observation() {
    let mut host = Harness::new();
    enable_input(&mut host);
    host.pipeline.settings.observe_feedback_sources = true;
    host.pipeline
        .settings
        .allowed_principals
        .push("amaterasu.rtsp".into());
    let id = host.response_from(1, "amaterasu.rtsp", prepare())["session"]
        .as_u64()
        .unwrap();
    assert_eq!(
        host.response_from(2, "amaterasu.rtsp", start(id))["state"],
        "active"
    );
    let target = json!({"kind":"stream","host_epoch":"host","session_id":"session","stream_id":"stream","generation":2});
    host.broker.input_source_observation = Some(
        json!({"ok":true,"version":1,"target":target,"source":{"version":1,"id":"b".repeat(32),"generation":2},"input_boot_epoch":"a".repeat(32),"device":{"slot":2,"generation":7,"capabilities":0},"provider_offer":["org.amaterasu.feedback.rumble"],"immutable_controller_policy":true}),
    );
    {
        let record = host.pipeline.records.get_mut(&id).unwrap();
        record.start.as_mut().unwrap().lifecycle_target =
            Some(serde_json::from_value(target.clone()).unwrap());
        record.lifecycle_input_token = Some("binding".into());
        record.lifecycle_delivery_permit = true;
        record.lifecycle_control_permit = true;
        record.feedback_device_observations.insert(2, (7, 0));
    }
    for _ in 0..60 {
        host.tick();
        if host.pipeline.records[&id]
            .feedback_source_observations
            .contains_key(&2)
        {
            break;
        }
    }
    assert_eq!(
        host.pipeline.records[&id].feedback_source_observations[&2]["input_boot_epoch"],
        "a".repeat(32)
    );
    assert!(
        host.broker
            .calls
            .iter()
            .any(|call| call.service == "amaterasu.input/1"
                && call.payload["action"] == "observe_feedback_source"
                && call.payload["controller_slot"] == 2
                && call.payload["input_binding"] == "binding")
    );
    // The observation alone does not install a pairing lease or route.
    assert!(
        !host
            .broker
            .calls
            .iter()
            .any(|call| call.service == "amaterasu.pairing-effects/1"
                && call.payload["action"] == "admit_stream_lease")
    );
}

#[test]
fn stream_owner_rejects_stale_or_mismatched_feedback_source_observations() {
    let mut host = Harness::new();
    enable_input(&mut host);
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");
    let target = json!({"kind":"stream","host_epoch":"host","session_id":"session","stream_id":"stream","generation":2});
    let record = host.pipeline.records.get_mut(&id).unwrap();
    record.start.as_mut().unwrap().lifecycle_target =
        Some(serde_json::from_value(target.clone()).unwrap());
    record.feedback_device_observations.insert(2, (7, 0));
    let valid = json!({"ok":true,"version":1,"target":target,"source":{"version":1,"id":"b".repeat(32),"generation":2},"input_boot_epoch":"a".repeat(32),"device":{"slot":2,"generation":7,"capabilities":0}});
    for field in ["generation", "slot", "capabilities"] {
        record.feedback_source_candidate = Some((2, 7, 0));
        let mut wrong = valid.clone();
        wrong["device"][field] = json!(99);
        assert_eq!(
            super::super::input::accept_feedback_source_observation(record, &wrong),
            Err("invalid_source_observation")
        );
    }
    record.feedback_source_candidate = Some((2, 7, 0));
    let mut wrong = valid.clone();
    wrong["input_boot_epoch"] = json!("bad");
    assert_eq!(
        super::super::input::accept_feedback_source_observation(record, &wrong),
        Err("invalid_source_observation")
    );
    record.feedback_source_candidate = Some((2, 7, 0));
    let mut wrong = valid.clone();
    wrong["source"]["generation"] = json!(3);
    assert_eq!(
        super::super::input::accept_feedback_source_observation(record, &wrong),
        Err("invalid_source_observation")
    );
    assert!(record.feedback_source_observations.is_empty());
    record.feedback_source_candidate = Some((2, 7, 0));
    record.feedback_source_attempts.insert(2, (7, 0));
    super::super::input::retry_feedback_source_observation(record, 10);
    assert_eq!(record.feedback_source_retry_at, 1010);
    assert!(!record.feedback_source_attempts.contains_key(&2));
}

#[test]
fn source_snapshot_intersects_live_offer_client_flags_and_stable_policy() {
    let mut host = Harness::new();
    enable_input(&mut host);
    host.pipeline
        .settings
        .allowed_principals
        .push("amaterasu.rtsp".into());
    let id = host.response_from(1, "amaterasu.rtsp", prepare())["session"]
        .as_u64()
        .unwrap();
    let mut launch = start(id);
    launch["configuration"]["client_feature_flags"] = json!(7);
    assert_eq!(
        host.response_from(2, "amaterasu.rtsp", launch)["state"],
        "active"
    );
    let target = json!({"kind":"stream","host_epoch":"host","session_id":"session","stream_id":"stream","generation":2});
    let record = host.pipeline.records.get_mut(&id).unwrap();
    record.start.as_mut().unwrap().lifecycle_target =
        Some(serde_json::from_value(target.clone()).unwrap());
    record.input_controller_supported = true;
    record.client_haptics_requested = true;
    record.feedback_device_observations.insert(2, (7, 0x8000));
    let payload = json!({"ok":true,"version":1,"target":target,"source":{"version":1,"id":"b".repeat(32),"generation":2},"input_boot_epoch":"a".repeat(32),"device":{"slot":2,"generation":7,"capabilities":0x8000},"provider_offer":["org.amaterasu.feedback.rumble","org.amaterasu.feedback.haptics_pcm"],"immutable_controller_policy":true});
    record.feedback_source_candidate = Some((2, 7, 0x8000));
    super::super::input::accept_feedback_source_observation(record, &payload).unwrap();
    assert_eq!(
        record.feedback_source_observations[&2]["scope_capabilities"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let mut disabled = payload.clone();
    disabled["immutable_controller_policy"] = false.into();
    record.feedback_source_candidate = Some((2, 7, 0x8000));
    super::super::input::accept_feedback_source_observation(record, &disabled).unwrap();
    assert_eq!(
        record.feedback_source_observations[&2]["scope_capabilities"],
        json!([])
    );
    record.start.as_mut().unwrap().client_feature_flags = Some(3);
    record.feedback_source_candidate = Some((2, 7, 0x8000));
    super::super::input::accept_feedback_source_observation(record, &payload).unwrap();
    let result = record.feedback_source_observations[&2]["scope_capabilities"]
        .as_array()
        .unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0]["schema"], "org.amaterasu.feedback.rumble");
    let mut unknown = payload;
    unknown["provider_offer"][0] = "vendor.unknown".into();
    record.feedback_source_candidate = Some((2, 7, 0x8000));
    assert_eq!(
        super::super::input::accept_feedback_source_observation(record, &unknown),
        Err("unsupported_feedback_offer")
    );
}

#[test]
fn retirement_ingress_is_serviced_while_another_workflow_is_active() {
    let mut host = Harness::new();
    host.pipeline.settings.enable_input = true;
    host.pipeline.settings.feedback_retirement_input_principal = Some("input.owner".into());
    host.broker.enqueue(1, "owner", prepare());
    for _ in 0..20 {
        host.tick();
        if host.pipeline.job.is_some() {
            break;
        }
    }
    assert!(host.pipeline.job.is_some());
    let payload = json!({"action":"retire_input_feedback","version":1,"operation_id":"retire-one","token":"binding","target":{"kind":"stream","host_epoch":"host","session_id":"session","stream_id":"stream","generation":1},"input_boot_epoch":"a".repeat(32)});
    host.broker.enqueue(2, "input.owner", payload);
    host.tick();
    assert_eq!(host.pipeline.feedback_retirement_inbox.len(), 1);
    assert!(!host.broker.replies.contains_key(&2));
    host.broker.cancel_parent(2);
    host.tick();
    assert_eq!(host.pipeline.feedback_retirement_inbox.len(), 0);
    assert!(!host.broker.replies.contains_key(&2));
}

#[test]
fn retirement_ingress_defers_regular_requests_without_losing_them() {
    let mut host = Harness::new();
    host.pipeline.settings.enable_input = true;
    host.pipeline.settings.feedback_retirement_input_principal = Some("input.owner".into());
    host.broker.enqueue(1, "owner", prepare());
    for _ in 0..20 {
        host.tick();
        if host.pipeline.job.is_some() {
            break;
        }
    }
    assert!(host.pipeline.job.is_some());
    host.broker.enqueue(2, "owner", json!({"action":"status"}));
    host.tick();
    assert_eq!(host.pipeline.deferred_requests.len(), 1);
    for _ in 0..600 {
        host.tick();
        if host.broker.replies.contains_key(&2) {
            break;
        }
    }
    assert!(host.broker.replies.contains_key(&1));
    assert_eq!(host.broker.replies[&2]["ok"], true);
}

#[test]
fn controller_rearrival_fences_owned_source_before_input_provider_delivery() {
    use amaterasu_module_sdk::effect_authority::{
        FeedbackCapability, FeedbackDirection, Source, StreamScope,
    };
    let mut host = Harness::new();
    enable_input(&mut host);
    host.broker.input_controller_supported = Some(json!(true));
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");
    let target = amaterasu_extension_api::LifecycleTarget::Stream {
        host_epoch: "host".into(),
        session_id: "session".into(),
        stream_id: "stream".into(),
        generation: 2,
    };
    let source = Source {
        version: 1,
        id: "a".repeat(32),
        generation: 2,
        host_epoch: "host".into(),
        session_id: "session".into(),
        stream_id: "stream".into(),
        stream_generation: 2,
        device: "slot-3".into(),
        device_generation: 1,
        effect_owner_epoch: "owner".into(),
    };
    let scope = StreamScope {
        capabilities: [FeedbackCapability {
            schema: "org.amaterasu.feedback.rumble".into(),
            version: 1,
            direction: FeedbackDirection::ToClient,
        }]
        .into(),
        controller_slot: 3,
        transport_service: "amaterasu.control-transport/1".into(),
        transport_stream: 7,
        transport_epoch: "c".repeat(32),
    };
    {
        let record = host.pipeline.records.get_mut(&id).unwrap();
        record.start.as_mut().unwrap().lifecycle_target = Some(target.clone());
        record.lifecycle_input_token = Some("binding".into());
        record.lifecycle_delivery_permit = true;
        record.lifecycle_control_permit = true;
        record.client_haptics_requested = true;
        record.input_controller_supported = true;
        record.feedback_device_observations.insert(3, (1, 0));
        record.feedback_source_observations.insert(
            3,
            json!({"source":{"id":"a".repeat(32)},"input_boot_epoch":"b".repeat(32)}),
        );
    }
    host.pipeline
        .feedback_retirement_inbox
        .install_test_source(target, source, scope);
    host.pipeline.settings.observe_feedback_sources = true;
    host.pipeline.settings.feedback_retirement_input_principal = Some("input.owner".into());
    let mut arrival = [0u8; 8];
    arrival[0] = 3;
    arrival[1] = 2;
    host.plane
        .borrow_mut()
        .controls
        .push(controller_extension_packet(0x55000004, &arrival, 1));
    host.tick();
    assert_eq!(host.pipeline.records[&id].state, State::Stopping);
    assert_eq!(
        host.pipeline.records[&id].error,
        Some("controller_source_retirement")
    );
    assert!(host.broker.input_events.is_empty());
    assert!(host.plane.borrow().stopping);
}
