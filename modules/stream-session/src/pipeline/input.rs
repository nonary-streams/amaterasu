use super::record::{Record, State};
use amaterasu_gamestream::control::{
    AbsoluteViewport, Input, InputEvent, InputTranslator, MouseButton, ScrollAxis,
    TranslatedKeyState, TranslationError,
};
use amaterasu_media::CaptureGeometry;
use serde_json::{Value, json};

const MAX_QUEUED_EVENTS: usize = 128;
pub(super) const MAX_BATCH_EVENTS: usize = 64;

pub(super) fn configure_absolute_input(
    record: &mut Record,
    video_response: &Value,
) -> Result<(), &'static str> {
    let Some(value) = video_response
        .as_object()
        .and_then(|object| object.get("desktop_geometry"))
    else {
        record.capture_geometry = None;
        record.input_absolute_pointer = false;
        record.input_translator = InputTranslator::default();
        return Ok(());
    };
    let geometry: CaptureGeometry =
        serde_json::from_value(value.clone()).map_err(|_| "invalid_capture_geometry")?;
    geometry
        .normalized_output()
        .map_err(|_| "invalid_capture_geometry")?;
    record.capture_geometry = Some(geometry);
    record.input_absolute_pointer = false;
    record.input_translator = InputTranslator::default();
    Ok(())
}

pub(super) fn queue_control_input(
    record: &mut Record,
    input: Input<'_>,
    enabled: bool,
) -> Result<(), &'static str> {
    if !enabled {
        record.unsupported_controls = record.unsupported_controls.saturating_add(1);
        return Ok(());
    }
    if record.state != State::Active || record.input_lease.is_none() {
        record.rejected_controls = record.rejected_controls.saturating_add(1);
        return Ok(());
    }
    let batch = match record.input_translator.translate(input) {
        Ok(batch) => batch,
        Err(TranslationError::InvalidPointer) => {
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
    record.input_lease = Some(lease.to_owned());
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
    record.input_request_started_at = 0;
    record.input_expires_at = 0;
    record.input_renew_at = 0;
    record.input_absolute_pointer = false;
    record.input_events.clear();
    record.input_inflight = 0;
    record.input_translator = Default::default();
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
            state,
        } => {
            let mut key = json!({"platform": "moonlight_vk", "code": virtual_key});
            if !normalized {
                key["normalized"] = json!(false);
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
    }
}
