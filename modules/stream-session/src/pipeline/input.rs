use super::record::{Record, State};
use amaterasu_gamestream::control::{
    AbsoluteViewport, Input, InputEvent, InputTranslator, MAX_CONTROLLER_SLOT, MouseButton,
    PenTouch, PointerDevice, PointerEvent, ScrollAxis, TranslatedKeyState, TranslationError,
};
use amaterasu_media::CaptureGeometry;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug)]
pub(super) struct TouchPort {
    width: f32,
    height: f32,
    client_offset_x: f32,
    client_offset_y: f32,
    logical_width: f32,
    logical_height: f32,
}

impl TouchPort {
    fn from_video_response(response: &Value) -> Option<Self> {
        let value = response.get("touch_port")?.as_object()?;
        if value.len() != 6 {
            return None;
        }
        let dimension = |name: &str| -> Option<f32> {
            value
                .get(name)?
                .as_u64()
                .filter(|value| (1..=1_000_000).contains(value))
                .map(|value| value as f32)
        };
        let offset = |name: &str| -> Option<f32> {
            let number = value.get(name)?.as_f64()?;
            (number.is_finite() && (0.0..=1_000_000.0).contains(&number)).then_some(number as f32)
        };
        let port = Self {
            width: dimension("width")?,
            height: dimension("height")?,
            client_offset_x: offset("client_offset_x")?,
            client_offset_y: offset("client_offset_y")?,
            logical_width: dimension("logical_width")?,
            logical_height: dimension("logical_height")?,
        };
        if port.client_offset_x * 2.0 >= port.width || port.client_offset_y * 2.0 >= port.height {
            return None;
        }
        Some(port)
    }
}

const MAX_QUEUED_EVENTS: usize = 128;
pub(super) const MAX_BATCH_EVENTS: usize = 64;

pub(super) fn next_input_batch_size(record: &Record) -> usize {
    let Some(first) = record.input_events.front() else {
        return 0;
    };
    let controller_batch = first["kind"] == "controller";
    record
        .input_events
        .iter()
        .take(MAX_BATCH_EVENTS)
        .take_while(|event| (event["kind"] == "controller") == controller_batch)
        .count()
}

pub(super) fn configure_absolute_input(
    record: &mut Record,
    video_response: &Value,
) -> Result<(), &'static str> {
    let Some(value) = video_response
        .as_object()
        .and_then(|object| object.get("desktop_geometry"))
    else {
        record.capture_geometry = None;
        record.touch_port = None;
        record.input_absolute_pointer = false;
        record.input_translator = InputTranslator::default();
        return Ok(());
    };
    let geometry: CaptureGeometry =
        serde_json::from_value(value.clone()).map_err(|_| "invalid_capture_geometry")?;
    geometry
        .normalized_output()
        .map_err(|_| "invalid_capture_geometry")?;
    record.touch_port = TouchPort::from_video_response(video_response);
    record.capture_geometry = Some(geometry);
    record.input_absolute_pointer = false;
    record.input_translator = InputTranslator::default();
    Ok(())
}

pub(super) fn queue_control_input(
    record: &mut Record,
    input: Input<'_>,
    enabled: bool,
    now: u64,
) -> Result<(), &'static str> {
    if !enabled {
        record.unsupported_controls = record.unsupported_controls.saturating_add(1);
        return Ok(());
    }
    if let Input::EnableHaptics { enabled } = input {
        if !matches!(record.state, State::Armed | State::Active) {
            record.rejected_controls = record.rejected_controls.saturating_add(1);
            return Ok(());
        }
        if enabled && (record.input_lease.is_none() || !record.input_controller_supported) {
            record.unsupported_controls = record.unsupported_controls.saturating_add(1);
            return Ok(());
        }
        record.client_haptics_requested = enabled;
        if !enabled {
            queue_rumble_zeros(record, now);
        }
        return Ok(());
    }
    if record.state != State::Active || record.input_lease.is_none() {
        record.rejected_controls = record.rejected_controls.saturating_add(1);
        return Ok(());
    }
    // Require an explicit capability from the active backend lease so a host
    // without virtual gamepad support can keep processing keyboard and mouse.
    if matches!(&input, Input::Controller(_)) && !record.input_controller_supported {
        record.unsupported_controls = record.unsupported_controls.saturating_add(1);
        return Ok(());
    }
    if let Input::PenTouch(packet) = input {
        if !record.input_pen_touch_supported || !record.input_native_pen_touch {
            record.unsupported_controls = record.unsupported_controls.saturating_add(1);
            return Ok(());
        }
        let Some(port) = record.touch_port else {
            record.unsupported_controls = record.unsupported_controls.saturating_add(1);
            return Ok(());
        };
        let Some(geometry) = record.capture_geometry else {
            record.unsupported_controls = record.unsupported_controls.saturating_add(1);
            return Ok(());
        };
        let event = pen_touch_payload(packet, port, geometry)?;
        if record.input_events.len() >= MAX_QUEUED_EVENTS {
            return Err("input_queue_overflow");
        }
        record.input_events.push_back(event);
        return Ok(());
    }
    let batch = match record.input_translator.translate(input) {
        Ok(batch) => batch,
        Err(TranslationError::InvalidPointer) => {
            record.rejected_controls = record.rejected_controls.saturating_add(1);
            return Ok(());
        }
        Err(TranslationError::InvalidController) => {
            record.rejected_controls = record.rejected_controls.saturating_add(1);
            return Ok(());
        }
        Err(
            TranslationError::UnsupportedInput
            | TranslationError::UnsupportedKey
            | TranslationError::UnsupportedKeyboardEncoding
            | TranslationError::UnsupportedModifiers,
        ) => {
            record.unsupported_controls = record.unsupported_controls.saturating_add(1);
            return Ok(());
        }
    };
    if record
        .input_events
        .len()
        .saturating_add(batch.events().len())
        > MAX_QUEUED_EVENTS
    {
        return Err("input_queue_overflow");
    }
    record
        .input_events
        .extend(batch.into_events().into_iter().map(event_payload));
    Ok(())
}

pub(super) fn stop_for_input_failure(record: &mut Record, code: &'static str) {
    record.error.get_or_insert(code);
    record.outcome = State::Failed;
    record.history.stopping(code);
    record.state = State::Stopping;
    if let Some(plane) = record.plane.as_mut() {
        plane.stop();
    }
}

pub(super) fn accept_input_lease(
    record: &mut Record,
    payload: &Value,
    now: u64,
) -> Result<(), &'static str> {
    if payload["ok"] != true
        || payload["state"] != "active"
        || payload["operation_id"] != record.input_operation()
        || payload["ttl_ms"] != record.input_ttl_ms
        || payload["cleanup_required"] != false
    {
        return Err("invalid_input_response");
    }
    let lease = input_lease(payload)?;
    let expires_in_ms = payload["expires_in_ms"]
        .as_u64()
        .filter(|remaining| *remaining != 0 && *remaining <= record.input_ttl_ms)
        .ok_or("invalid_input_response")?;
    let backend = payload["backend"]
        .as_str()
        .filter(|name| valid_label(name, 64))
        .ok_or("invalid_input_response")?;
    let _ = backend;
    let absolute_pointer = match payload
        .as_object()
        .and_then(|object| object.get("absolute_pointer"))
    {
        Some(Value::Bool(supported)) => *supported,
        None => false,
        Some(_) => return Err("invalid_input_response"),
    };
    let controller_input_supported = match payload
        .as_object()
        .and_then(|object| object.get("controller_input"))
    {
        Some(Value::Bool(supported)) => *supported,
        None => false,
        Some(_) => return Err("invalid_input_response"),
    };
    record.input_lease = Some(lease.to_owned());
    record.input_controller_supported = controller_input_supported;
    record.feedback_poll_at = now.saturating_add(40);
    record.input_pen_touch_supported = optional_bool(payload, "pen_touch_supported")?;
    record.input_native_pen_touch = optional_bool(payload, "native_pen_touch")?;
    record.input_expires_at = now.saturating_add(expires_in_ms).min(
        record
            .input_request_started_at
            .saturating_add(record.input_ttl_ms),
    );
    if record.input_expires_at <= now {
        return Err("invalid_input_response");
    }
    let lifetime = record.input_expires_at - now;
    record.input_renew_at = now.saturating_add(lifetime / 2);
    record.input_lookup_required = false;
    configure_translator(record, absolute_pointer)?;
    Ok(())
}

pub(super) fn accept_input_renewal(
    record: &mut Record,
    payload: &Value,
    now: u64,
) -> Result<(), &'static str> {
    if payload["ok"] != true
        || payload["lease"] != json!(record.input_lease)
        || payload["ttl_ms"] != record.input_ttl_ms
    {
        return Err("invalid_input_response");
    }
    record.input_expires_at = record
        .input_request_started_at
        .saturating_add(record.input_ttl_ms);
    if record.input_expires_at <= now {
        return Err("input_lease_expired");
    }
    let lifetime = record.input_expires_at - now;
    record.input_renew_at = now.saturating_add(lifetime / 2);
    Ok(())
}

pub(super) fn adopt_input_for_cleanup(
    record: &mut Record,
    payload: &Value,
) -> Result<(), &'static str> {
    if payload["ok"] != true
        || payload["state"] != "active"
        || payload["operation_id"] != record.input_operation()
    {
        return Err("invalid_input_response");
    }
    let lease = input_lease(payload)?;
    if record
        .input_lease
        .as_deref()
        .is_some_and(|known| known != lease)
    {
        return Err("invalid_input_response");
    }
    record.input_lease = Some(lease.to_owned());
    record.input_attempted = true;
    record.input_lookup_required = false;
    Ok(())
}

pub(super) fn accept_input_batch(record: &mut Record, payload: &Value) -> Result<(), &'static str> {
    if record.input_inflight == 0
        || payload["ok"] != true
        || payload["lease"] != json!(record.input_lease)
        || payload["accepted"] != record.input_inflight
    {
        return Err("input_batch_failed");
    }
    for _ in 0..record.input_inflight {
        record.input_events.pop_front();
    }
    record.input_events_sent = record
        .input_events_sent
        .saturating_add(record.input_inflight as u64);
    record.input_batches = record.input_batches.saturating_add(1);
    record.input_inflight = 0;
    Ok(())
}

pub(super) fn accept_controller_batch_failure(
    record: &mut Record,
    payload: &Value,
) -> Result<(), &'static str> {
    let accepted = payload["accepted"]
        .as_u64()
        .and_then(|accepted| usize::try_from(accepted).ok())
        .filter(|accepted| *accepted < record.input_inflight)
        .ok_or("input_batch_failed")?;
    if record.input_inflight == 0
        || !record.input_controller_supported
        || payload["lease"] != json!(record.input_lease)
        || payload["partial_event_possible"] != true
        || payload["error"]["code"] != "controller_unavailable"
        || record
            .input_events
            .iter()
            .take(record.input_inflight)
            .any(|event| event["kind"] != "controller")
    {
        return Err("input_batch_failed");
    }

    for _ in 0..accepted {
        record.input_events.pop_front();
    }
    record.input_events.pop_front();
    record.input_events_sent = record.input_events_sent.saturating_add(accepted as u64);
    if accepted != 0 {
        record.input_batches = record.input_batches.saturating_add(1);
    }
    record.unsupported_controls = record.unsupported_controls.saturating_add(1);
    record.input_inflight = 0;
    Ok(())
}

pub(super) fn accept_input_release(
    record: &mut Record,
    payload: &Value,
) -> Result<(), &'static str> {
    if payload["ok"] != true || payload["released"] != true {
        return Err("input_release_failed");
    }
    clear_input(record);
    Ok(())
}

pub(super) fn input_missing(payload: &Value) -> bool {
    payload["ok"] == false
        && matches!(
            payload["error"]["code"].as_str(),
            Some("not_found" | "input_not_found")
        )
}

pub(super) fn input_released(record: &Record, payload: &Value) -> bool {
    payload["ok"] == true
        && payload["state"] == "released"
        && payload["operation_id"] == record.input_operation()
        && payload["lease"].is_null()
        && payload["reason"]
            .as_str()
            .is_some_and(|reason| valid_label(reason, 64))
}

pub(super) fn clear_input(record: &mut Record) {
    record.input_attempted = false;
    record.input_lookup_required = false;
    record.input_lease = None;
    record.rumble_active_until.clear();
    record.rumble_pending.clear();
    record.input_request_started_at = 0;
    record.input_expires_at = 0;
    record.input_renew_at = 0;
    record.feedback_poll_at = 0;
    record.input_absolute_pointer = false;
    record.input_controller_supported = false;
    record.client_haptics_requested = false;
    record.input_pen_touch_supported = false;
    record.input_native_pen_touch = false;
    record.touch_port = None;
    record.input_events.clear();
    record.input_inflight = 0;
    record.input_translator = Default::default();
}

pub(super) fn queue_rumble_zeros(record: &mut Record, now: u64) {
    let mut slots: std::collections::BTreeSet<u16> =
        record.rumble_active_until.keys().copied().collect();
    slots.extend(record.rumble_pending.keys().copied());
    for slot in slots {
        record.rumble_pending.insert(
            slot,
            (
                crate::plane::RumbleCommand {
                    slot,
                    low_frequency: 0,
                    high_frequency: 0,
                },
                None,
            ),
        );
    }
    if record.state == State::Stopping && !record.rumble_pending.is_empty() {
        record
            .rumble_shutdown_deadline
            .get_or_insert(now.saturating_add(200));
    }
}

pub(super) fn accept_gamepad_feedback(
    record: &mut Record,
    payload: &Value,
    now: u64,
) -> Result<(), &'static str> {
    if !record.input_controller_supported
        || record.input_lease.is_none()
        || payload["ok"] != true
        || payload["lease"] != json!(record.input_lease)
    {
        return Err("invalid_gamepad_feedback_response");
    }
    if payload.as_object().is_none_or(|object| object.len() != 3) {
        return Err("invalid_gamepad_feedback_response");
    }
    record.feedback_poll_at = now.saturating_add(40);
    let Some(feedback) = payload.get("feedback") else {
        return Err("invalid_gamepad_feedback_response");
    };
    if feedback.is_null() {
        return Ok(());
    }
    let object = feedback
        .as_object()
        .ok_or("invalid_gamepad_feedback_response")?;
    if object.len() != 4 {
        return Err("invalid_gamepad_feedback_response");
    }
    let slot = feedback["slot"]
        .as_u64()
        .filter(|slot| *slot < u64::from(MAX_CONTROLLER_SLOT))
        .ok_or("invalid_gamepad_feedback_response")? as u16;
    let strong = feedback["strong_motor"]
        .as_u64()
        .filter(|value| *value <= u16::MAX as u64)
        .ok_or("invalid_gamepad_feedback_response")? as u16;
    let weak = feedback["weak_motor"]
        .as_u64()
        .filter(|value| *value <= u16::MAX as u64)
        .ok_or("invalid_gamepad_feedback_response")? as u16;
    let duration = feedback["duration_ms"]
        .as_u64()
        .filter(|value| *value <= 10_000)
        .ok_or("invalid_gamepad_feedback_response")?;
    let zero = strong == 0 && weak == 0;
    if (duration == 0) != zero {
        return Err("invalid_gamepad_feedback_response");
    }
    if !record.client_haptics_requested && !zero {
        return Ok(());
    }
    if zero {
        // A terminal zero is accepted only for a slot this session already owns.
        if !record.rumble_active_until.contains_key(&slot)
            && !record.rumble_pending.contains_key(&slot)
        {
            return Ok(());
        }
        record.rumble_active_until.remove(&slot);
        record.rumble_pending.insert(
            slot,
            (
                crate::plane::RumbleCommand {
                    slot,
                    low_frequency: 0,
                    high_frequency: 0,
                },
                None,
            ),
        );
    } else {
        let expires = now.saturating_add(duration);
        record.rumble_active_until.insert(slot, expires);
        record.rumble_pending.insert(
            slot,
            (
                crate::plane::RumbleCommand {
                    slot,
                    low_frequency: strong,
                    high_frequency: weak,
                },
                Some(expires),
            ),
        );
    }
    Ok(())
}

fn optional_bool(payload: &Value, name: &str) -> Result<bool, &'static str> {
    match payload.get(name) {
        Some(Value::Bool(value)) => Ok(*value),
        None => Ok(false),
        Some(_) => Err("invalid_input_response"),
    }
}

fn pen_touch_payload(
    packet: PenTouch,
    port: TouchPort,
    geometry: CaptureGeometry,
) -> Result<Value, &'static str> {
    let normalized = geometry
        .normalized_output()
        .map_err(|_| "invalid_capture_geometry")?;
    let x = f32::from_bits(packet.x).clamp(0.0, 1.0);
    let y = f32::from_bits(packet.y).clamp(0.0, 1.0);
    let local_x = (x * port.width).clamp(port.client_offset_x, port.width - port.client_offset_x)
        - port.client_offset_x;
    let local_y = (y * port.height).clamp(port.client_offset_y, port.height - port.client_offset_y)
        - port.client_offset_y;
    let map = |value: f32, extent: f32, first: u16, last: u16| -> u16 {
        (f32::from(first) + value / extent * f32::from(last - first))
            .round()
            .clamp(0.0, 65535.0) as u16
    };
    let rotation = (packet.rotation != 0xffff).then_some(packet.rotation % 360);
    let angle = f32::from(rotation.unwrap_or(45)).to_radians();
    let major = f32::from_bits(packet.contact_major).clamp(0.0, 1.0);
    let minor = f32::from_bits(packet.contact_minor).clamp(0.0, 1.0);
    let minor = if minor == 0.0 { major } else { minor };
    let scale = |radius: f32, radians: f32| -> f32 {
        let horizontal = radius * radians.cos() * port.logical_width / 65535.0;
        let vertical = radius * radians.sin() * port.logical_height / 65535.0;
        horizontal.hypot(vertical).clamp(0.0, 1.0)
    };
    let mut value = json!({
        "kind":"pen_touch", "pointer_id":packet.pointer_id,
        "device":match packet.device { PointerDevice::Pen => "pen", PointerDevice::Touch => "touch" },
        "phase":match packet.event { PointerEvent::Down => "down", PointerEvent::Move => "move", PointerEvent::Up => "up", PointerEvent::Cancel => "cancel" },
        "x":map(local_x, port.width - 2.0 * port.client_offset_x, normalized.left, normalized.right),
        "y":map(local_y, port.height - 2.0 * port.client_offset_y, normalized.top, normalized.bottom),
        "pressure":f32::from_bits(packet.pressure).clamp(0.0, 1.0),
        "contact_area_major":scale(major, angle),
        "contact_area_minor":scale(minor, angle + std::f32::consts::FRAC_PI_2),
    });
    if let Some(rotation) = rotation {
        value["rotation_degrees"] = json!(rotation);
    }
    if packet.device == PointerDevice::Pen {
        value["pen_tool"] = json!(match packet.tool {
            Some(1) => "pen",
            Some(2) => "eraser",
            _ => "unknown",
        });
        value["pen_buttons"] = json!(packet.buttons);
        if let Some(tilt) = packet.tilt.filter(|tilt| *tilt != 0xff) {
            value["tilt"] = json!(tilt);
        }
    }
    Ok(value)
}

fn configure_translator(record: &mut Record, absolute_pointer: bool) -> Result<(), &'static str> {
    record.input_absolute_pointer = absolute_pointer;
    let Some(geometry) = record.capture_geometry.filter(|_| absolute_pointer) else {
        record.input_translator = InputTranslator::default();
        return Ok(());
    };
    let normalized = geometry
        .normalized_output()
        .map_err(|_| "invalid_capture_geometry")?;
    let viewport = AbsoluteViewport::new(
        normalized.left,
        normalized.top,
        normalized.right,
        normalized.bottom,
    )
    .map_err(|_| "invalid_capture_geometry")?;
    record.input_translator = InputTranslator::new(Some(viewport));
    Ok(())
}

fn input_lease(payload: &Value) -> Result<&str, &'static str> {
    payload["lease"]
        .as_str()
        .filter(|token| {
            token.len() == 32
                && token
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or("invalid_input_response")
}

fn valid_label(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b'"' && byte != b'\\')
}

fn event_payload(event: InputEvent) -> Value {
    match event {
        InputEvent::Key {
            virtual_key,
            normalized,
            repeat_modifiers,
            temporary_modifier,
            state,
        } => {
            let mut key = json!({"platform": "moonlight_vk", "code": virtual_key});
            if !normalized {
                key["normalized"] = json!(false);
            }
            if repeat_modifiers != 0 {
                key["repeat_modifiers"] = json!(repeat_modifiers);
            }
            if temporary_modifier {
                key["temporary_modifier"] = json!(true);
            }
            json!({
                "kind": "key",
                "key": key,
                "state": match state {
                    TranslatedKeyState::Down => "down",
                    TranslatedKeyState::Up => "up",
                    TranslatedKeyState::Repeat => "repeat",
                },
            })
        }
        InputEvent::Button { button, pressed } => json!({
            "kind": "button",
            "button": match button {
                MouseButton::Left => "left",
                MouseButton::Middle => "middle",
                MouseButton::Right => "right",
                MouseButton::Back => "back",
                MouseButton::Forward => "forward",
            },
            "pressed": pressed,
        }),
        InputEvent::Relative { dx, dy } => {
            json!({"kind": "relative", "dx": dx, "dy": dy})
        }
        InputEvent::Absolute { x, y } => {
            json!({"kind": "absolute", "x": x, "y": y})
        }
        InputEvent::Scroll { axis, steps } => json!({
            "kind": "scroll",
            "axis": match axis {
                ScrollAxis::Horizontal => "horizontal",
                ScrollAxis::Vertical => "vertical",
            },
            "steps": steps,
        }),
        InputEvent::ScrollUnits { axis, amount } => json!({
            "kind": "scroll_units",
            "axis": match axis {
                ScrollAxis::Horizontal => "horizontal",
                ScrollAxis::Vertical => "vertical",
            },
            "amount": amount,
        }),
        InputEvent::Controller(state) => json!({
            "kind": "controller",
            "number": state.number,
            "active_mask": state.active_mask,
            "buttons": state.buttons,
            "left_trigger": state.left_trigger,
            "right_trigger": state.right_trigger,
            "left_x": state.left_x,
            "left_y": state.left_y,
            "right_x": state.right_x,
            "right_y": state.right_y,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_gamestream::control::ControllerState;

    #[test]
    fn controller_event_payload_preserves_each_decoded_field() {
        let state = ControllerState {
            number: 15,
            active_mask: 0xa55a,
            buttons: 0x89abcdef,
            left_trigger: 0,
            right_trigger: 255,
            left_x: i16::MIN,
            left_y: i16::MAX,
            right_x: -1,
            right_y: 1,
        };

        assert_eq!(
            event_payload(InputEvent::Controller(state)),
            json!({
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
            })
        );
    }
}
