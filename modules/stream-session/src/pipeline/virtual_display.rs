use super::record::Record;
use crate::model::DisplayRequest;
use serde_json::{Value, json};

pub(super) enum ReferenceStatus {
    Ready,
    Pending,
    Missing,
    Failed,
}

pub(super) fn create(record: &Record) -> Value {
    let intent = record
        .config
        .virtual_display_intent
        .as_ref()
        .expect("validated active virtual-display intent");
    json!({
        "action":"create",
        "identity":intent.identity,
        "reference_id":intent.reference_id,
        "name":intent.name,
        "width":intent.width,
        "height":intent.height,
        "refresh_millihz":intent.refresh_millihz,
        "dd_virtual_display_scale":intent.dd_virtual_display_scale,
    })
}

pub(super) fn status(record: &Record) -> Value {
    let intent = record
        .config
        .virtual_display_intent
        .as_ref()
        .expect("virtual-display status requires intent");
    json!({
        "action":"status",
        "identity":intent.identity,
        "reference_id":intent.reference_id,
    })
}

pub(super) fn remove(record: &Record) -> Value {
    let intent = record
        .config
        .virtual_display_intent
        .as_ref()
        .expect("virtual-display removal requires intent");
    json!({
        "action":"remove",
        "identity":intent.identity,
        "reference_id":intent.reference_id,
    })
}

pub(super) fn accept_status(record: &mut Record, response: &Value) -> ReferenceStatus {
    record.virtual_display_status_verified = false;
    if response["ok"] != true {
        record.virtual_display_state = response["code"]
            .as_str()
            .unwrap_or("status_unavailable")
            .into();
        return ReferenceStatus::Failed;
    }
    let Some(intent) = record.config.virtual_display_intent.as_ref() else {
        record.virtual_display_state = "invalid_intent".into();
        return ReferenceStatus::Failed;
    };
    if response["reference_owned"] != true {
        record.virtual_display_reference_owned = false;
        record.virtual_display_status_verified = true;
        record.virtual_display_attempted = false;
        record.virtual_display_remove_attempted = false;
        record.virtual_display_device_path = None;
        record.virtual_display_state = "reference_missing".into();
        return ReferenceStatus::Missing;
    }
    let Some(monitor) = response["monitors"].as_array().and_then(|monitors| {
        monitors.iter().find(|monitor| {
            monitor["specification"]["identity"] == intent.identity
                && monitor["specification"]["width"] == intent.width
                && monitor["specification"]["height"] == intent.height
                && monitor["specification"]["refresh_millihz"] == intent.refresh_millihz
                && monitor["specification"]["name"] == intent.name
        })
    }) else {
        // The provider confirms this reference exists independently of the
        // returned monitor specification. Keep ownership so cleanup can still
        // release the mismatched reference instead of leaking it.
        record.virtual_display_reference_owned = true;
        record.virtual_display_status_verified = true;
        record.virtual_display_device_path = None;
        record.virtual_display_state = "specification_mismatch".into();
        return ReferenceStatus::Failed;
    };
    record.virtual_display_reference_owned = true;
    record.virtual_display_status_verified = true;
    match monitor["state"].as_str() {
        Some("ready") => {
            let Some(device_path) = monitor["ready"]["device_path"].as_str().filter(|path| {
                !path.is_empty() && path.len() <= 4096 && !path.chars().any(char::is_control)
            }) else {
                record.virtual_display_state = "ready_path_unavailable".into();
                return ReferenceStatus::Failed;
            };
            record.virtual_display_device_path = Some(device_path.to_owned());
            record.virtual_display_state = "ready".into();
            ReferenceStatus::Ready
        }
        Some("creating" | "recovered" | "reclaim_pending") => {
            record.virtual_display_device_path = None;
            record.virtual_display_state = "creating".into();
            ReferenceStatus::Pending
        }
        Some(state) => {
            record.virtual_display_device_path = None;
            record.virtual_display_state = state.into();
            record.virtual_display_create_error = monitor["error"].as_str().map(str::to_owned);
            ReferenceStatus::Failed
        }
        None => {
            record.virtual_display_device_path = None;
            record.virtual_display_state = "invalid_status".into();
            ReferenceStatus::Failed
        }
    }
}

pub(super) fn accept_remove(record: &mut Record, response: &Value) -> bool {
    if response["ok"] == true
        && (response["reference_owned"] == false
            || response["reference_released"] == true
            || response["removed"] == true)
    {
        record.virtual_display_reference_owned = false;
        record.virtual_display_status_verified = true;
        record.virtual_display_attempted = false;
        record.virtual_display_remove_attempted = false;
        record.virtual_display_device_path = None;
        record.virtual_display_state = "removed".into();
        return true;
    }
    record.virtual_display_state = response["code"]
        .as_str()
        .unwrap_or("remove_unverified")
        .into();
    false
}

pub(super) fn same_shared_specification(left: &Record, right: &Record) -> bool {
    let (Some(left), Some(right)) = (
        left.config.virtual_display_intent.as_ref(),
        right.config.virtual_display_intent.as_ref(),
    ) else {
        return false;
    };
    left.virtual_display_mode == amaterasu_contracts::virtual_display::VirtualDisplayMode::Shared
        && right.virtual_display_mode
            == amaterasu_contracts::virtual_display::VirtualDisplayMode::Shared
        && left.identity == right.identity
        && left.name == right.name
        && left.width == right.width
        && left.height == right.height
        && left.refresh_millihz == right.refresh_millihz
        && left.dd_virtual_display_scale == right.dd_virtual_display_scale
        && left.dd_activate_virtual_display == right.dd_activate_virtual_display
        && left.dd_use_sunshine_virtual_display_driver
            == right.dd_use_sunshine_virtual_display_driver
        && left.virtual_display_mode == right.virtual_display_mode
        && left.virtual_display_layout == right.virtual_display_layout
        && left.display_policy == right.display_policy
}

pub(super) fn is_read_only_policy(record: &Record) -> bool {
    record
        .config
        .virtual_display_intent
        .as_ref()
        .and_then(|intent| intent.display_policy.as_ref())
        .and_then(|policy| policy.configuration_option)
        .is_some_and(|option| {
            matches!(
                option,
                amaterasu_contracts::virtual_display::VirtualDisplayConfigurationOption::Disabled
                    | amaterasu_contracts::virtual_display::VirtualDisplayConfigurationOption::VerifyOnly
            )
        })
}

pub(super) fn read_only_policy_is_satisfied(record: &Record, response: &Value) -> bool {
    if !is_read_only_policy(record) || response["ok"] != true || response["applied"] != false {
        return false;
    }
    match record
        .config
        .virtual_display_intent
        .as_ref()
        .and_then(|intent| intent.display_policy.as_ref())
        .and_then(|policy| policy.configuration_option)
    {
        Some(amaterasu_contracts::virtual_display::VirtualDisplayConfigurationOption::Disabled) => {
            response["configuration_option"] == "disabled"
        }
        Some(
            amaterasu_contracts::virtual_display::VirtualDisplayConfigurationOption::VerifyOnly,
        ) => response["verified"] == true,
        _ => false,
    }
}

pub(super) fn build_display_request(
    record: &mut Record,
    response: &Value,
) -> Result<(), &'static str> {
    if response["ok"] != true {
        return Err("display_snapshot_unavailable");
    }
    let Some(intent) = record.config.virtual_display_intent.as_ref() else {
        return Err("invalid_virtual_display_intent");
    };
    let Some(device_path) = record.virtual_display_device_path.as_deref() else {
        return Err("virtual_display_ready_unverified");
    };
    let Some(revision) = response["topology_revision"].as_str().filter(|revision| {
        revision.len() == 32
            && revision
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    }) else {
        return Err("invalid_display_snapshot");
    };
    let Some(mut displays) = response["configuration"]["displays"].as_array().cloned() else {
        return Err("invalid_display_snapshot");
    };
    displays.retain(|display| {
        !display["id"]
            .as_str()
            .is_some_and(|id| id.eq_ignore_ascii_case(device_path))
    });
    let target = json!({
        "id":device_path,
        "width":intent.width,
        "height":intent.height,
        "refresh":{"numerator":intent.refresh_millihz,"denominator":1000},
        "x":0,
        "y":0,
        "rotation":"identity",
    });
    let (configuration_option, x, y) = match intent.virtual_display_layout {
        amaterasu_contracts::virtual_display::VirtualDisplayLayout::Exclusive => {
            displays.clear();
            ("ensure_only_display", 0_i32, 0_i32)
        }
        amaterasu_contracts::virtual_display::VirtualDisplayLayout::Extended => {
            ("ensure_active", right_edge(&displays)?, 0)
        }
        amaterasu_contracts::virtual_display::VirtualDisplayLayout::ExtendedPrimary => {
            let shift = primary_shift(&displays, intent.width)?;
            shift_horizontal(&mut displays, shift)?;
            ("ensure_primary", 0, 0)
        }
        amaterasu_contracts::virtual_display::VirtualDisplayLayout::ExtendedIsolated => {
            let (x, y) = isolated_position(&displays, intent.width, intent.height)?;
            ("ensure_active", x, y)
        }
        amaterasu_contracts::virtual_display::VirtualDisplayLayout::ExtendedPrimaryIsolated => {
            let shift = primary_isolated_shift(&displays)?;
            shift_horizontal(&mut displays, shift)?;
            ("ensure_primary", 0, 0)
        }
    };
    let mut target = target;
    target["x"] = json!(x);
    target["y"] = json!(y);
    displays.push(target);
    let mut mode_policy = intent
        .display_policy
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .map_err(|_| "invalid_virtual_display_policy")?
        .unwrap_or_else(|| {
            json!({
                "remapping":{"mixed":[],"resolution_only":[],"refresh_rate_only":[]},
                "context":{
                    "resolution_is_automatic":false,
                    "refresh_rate_is_automatic":false,
                    "client_display_mode_override":false,
                    "optimize_game_settings":false
                }
            })
        });
    mode_policy["display_id"] = json!(device_path);
    if mode_policy.get("configuration_option").is_none() {
        mode_policy["configuration_option"] = json!(configuration_option);
    }
    let request = json!({
        "configuration":{"displays":displays},
        "expected_topology_revision":revision,
        "mode_policy":mode_policy
    });
    let display_request: DisplayRequest =
        serde_json::from_value(request).map_err(|_| "invalid_display_snapshot")?;
    record.config.display_request = Some(Box::new(display_request));
    Ok(())
}

pub(super) fn creation_failure(record: &Record) -> &'static str {
    match record.virtual_display_create_error.as_deref() {
        Some("unsupported_driver") => "unsupported_virtual_display_driver",
        Some("unsupported_scale") => "unsupported_virtual_display_scale",
        Some("unsupported_platform") => "virtual_display_platform_unsupported",
        Some("disabled") => "virtual_display_disabled",
        Some("identity_conflict" | "identity_collision" | "reference_conflict") => {
            "virtual_display_identity_conflict"
        }
        Some("readiness_timeout") => "virtual_display_readiness_timeout",
        _ => "virtual_display_unavailable",
    }
}

fn right_edge(displays: &[Value]) -> Result<i32, &'static str> {
    let right = displays.iter().try_fold(0_i64, |right, display| {
        let x = display["x"].as_i64()?;
        let width = display["width"].as_u64()?;
        Some(right.max(x.checked_add(i64::try_from(width).ok()?)?))
    });
    i32::try_from(right.ok_or("invalid_display_snapshot")?)
        .map_err(|_| "unsupported_virtual_display_layout")
}

fn primary_shift(displays: &[Value], target_width: u32) -> Result<i32, &'static str> {
    let minimum_x = displays
        .iter()
        .map(|display| display["x"].as_i64().ok_or("invalid_display_snapshot"))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .min()
        .unwrap_or(0);
    let shift = i64::from(target_width)
        .checked_sub(minimum_x)
        .ok_or("unsupported_virtual_display_layout")?;
    i32::try_from(shift).map_err(|_| "unsupported_virtual_display_layout")
}

fn shift_horizontal(displays: &mut [Value], shift: i32) -> Result<(), &'static str> {
    for display in displays {
        let x = display["x"].as_i64().ok_or("invalid_display_snapshot")?;
        display["x"] = json!(
            i32::try_from(x + i64::from(shift))
                .map_err(|_| "unsupported_virtual_display_layout")?
        );
    }
    Ok(())
}

fn isolated_position(
    displays: &[Value],
    width: u32,
    height: u32,
) -> Result<(i32, i32), &'static str> {
    let right = right_edge(displays)?;
    let x = right
        .checked_add(16_384)
        .ok_or("unsupported_virtual_display_layout")?;
    if i64::from(x) + i64::from(width) <= 65_536 {
        return Ok((x, 0));
    }
    let bottom = displays.iter().try_fold(0_i64, |bottom, display| {
        let y = display["y"].as_i64()?;
        let height = display["height"].as_u64()?;
        Some(bottom.max(y.checked_add(i64::try_from(height).ok()?)?))
    });
    let y = i32::try_from(
        bottom
            .ok_or("invalid_display_snapshot")?
            .checked_add(16_384)
            .ok_or("unsupported_virtual_display_layout")?,
    )
    .map_err(|_| "unsupported_virtual_display_layout")?;
    if i64::from(y) + i64::from(height) > 65_536 {
        return Err("unsupported_virtual_display_layout");
    }
    Ok((0, y))
}

fn primary_isolated_shift(displays: &[Value]) -> Result<i32, &'static str> {
    let minimum_x = displays
        .iter()
        .map(|display| display["x"].as_i64().ok_or("invalid_display_snapshot"))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .min()
        .unwrap_or(0);
    let shift = 32_768_i64
        .checked_sub(minimum_x)
        .ok_or("unsupported_virtual_display_layout")?;
    let shift = i32::try_from(shift).map_err(|_| "unsupported_virtual_display_layout")?;
    for display in displays {
        let x = display["x"].as_i64().ok_or("invalid_display_snapshot")?;
        let width = display["width"]
            .as_u64()
            .ok_or("invalid_display_snapshot")?;
        if x + i64::from(shift)
            + i64::try_from(width).map_err(|_| "unsupported_virtual_display_layout")?
            > 65_536
        {
            return Err("unsupported_virtual_display_layout");
        }
    }
    Ok(shift)
}

/// Module unload cannot await cleanup ticks; reconcile the stable reference
/// once and remove only the caller-owned reference.
pub(super) fn shutdown(record: &mut Record, send: &mut super::rpc::Send<'_>) -> bool {
    if !record.virtual_display_attempted && !record.virtual_display_reference_owned {
        return true;
    }
    if record.owns_display_restore && record.display_applied {
        return false;
    }
    let Some(status) = exchange(record, status(record), "status", send) else {
        return false;
    };
    let _ = accept_status(record, &status);
    if !record.virtual_display_reference_owned {
        return record.virtual_display_status_verified;
    }
    let Some(removed) = exchange(record, remove(record), "remove", send) else {
        return false;
    };
    accept_remove(record, &removed)
}

fn exchange(
    record: &Record,
    payload: Value,
    action: &str,
    send: &mut super::rpc::Send<'_>,
) -> Option<Value> {
    let mut pending = super::rpc::Pending::new(
        "amaterasu.virtual-display/1",
        payload,
        format!("ss-{:x}-virtual-display-shutdown-{action}", record.id),
        None,
    );
    for turn in 0..8 {
        match pending.poll(send, turn) {
            super::rpc::Progress::Result(response) => {
                let _ = pending.release_result(send);
                return Some(response);
            }
            super::rpc::Progress::Waiting => {}
            super::rpc::Progress::Failed | super::rpc::Progress::Unresolved => break,
        }
    }
    let _ = pending.shutdown(send);
    None
}
