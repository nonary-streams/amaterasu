use super::*;

fn enable_input(host: &mut Harness) {
    host.pipeline.settings.enable_input = true;
    host.pipeline.settings.input_lease_ttl_ms = 1_000;
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
