//! Read-only KDE/KScreen discovery and conservative restoration planning.
//!
//! Native display mutations remain unsupported on Linux until the KScreen
//! topology can be journaled through the display module's existing lease path.
use super::{Platform, TopologyShape};
use crate::model::{
    Address, AvailableMode, Configuration, DisplayError, DisplayResult, KScreenModeSnapshot,
    KScreenOutputSnapshot, KScreenPosition, KScreenSnapshot, Rational, RouteBinding, Snapshot,
};
use serde_json::{Value, json};
#[cfg(test)]
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const MAX_CONFIGURATION_BYTES: usize = 1024 * 1024;
const MAX_OUTPUTS: usize = 64;
const MAX_MODES_PER_OUTPUT: usize = 1024;
const KSCREEN_QUERY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct LinuxPlatform;

impl Platform for LinuxPlatform {
    fn name(&self) -> &'static str {
        "linux_kscreen"
    }

    // KScreen mutation is deliberately not exposed through the existing CCD
    // transaction API. Linux topology needs its own durable journal format.
    fn supported(&self) -> bool {
        false
    }

    fn enumerate(
        &mut self,
        display: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> DisplayResult<Value> {
        let configuration = query_configuration()?;
        let rows = enumeration_rows(&configuration, display)?;

        let total = rows.len();
        if offset > total {
            return Err(DisplayError::new(
                "invalid_request",
                "enumeration offset exceeds result count",
            ));
        }
        let entries: Vec<_> = rows.into_iter().skip(offset).take(limit).collect();
        let encoded_size = serde_json::to_vec(&entries)
            .map_err(|error| DisplayError::new("platform_error", error.to_string()))?
            .len();
        if encoded_size > 12_000 {
            return Err(DisplayError::new(
                "resource_exhausted",
                "display enumeration page exceeds transport limit",
            ));
        }
        let next_offset = (offset + entries.len() < total).then_some(offset + entries.len());
        Ok(json!({
            "ok": true,
            "platform": self.name(),
            "entries": entries,
            "next_offset": next_offset
        }))
    }

    fn read_configuration_snapshot(&mut self) -> DisplayResult<Value> {
        let configuration = query_configuration()?;
        configuration_snapshot(&configuration, self.name())
    }

    fn snapshot(&mut self) -> DisplayResult<Snapshot> {
        Err(unsupported_mutation())
    }

    fn available_modes(&mut self, _: &str) -> DisplayResult<Vec<AvailableMode>> {
        Err(unsupported_mutation())
    }

    fn prepare(&mut self, _: &Configuration) -> DisplayResult<Snapshot> {
        Err(unsupported_mutation())
    }

    fn bind_inactive(&mut self, _: &str, _: &Snapshot) -> DisplayResult<RouteBinding> {
        Err(unsupported_mutation())
    }

    fn bind_clone(&mut self, _: &str, _: &Address, _: &Snapshot) -> DisplayResult<RouteBinding> {
        Err(unsupported_mutation())
    }

    fn prepare_preserving(
        &mut self,
        _: &Configuration,
        _: &str,
        _: Option<&RouteBinding>,
        _: TopologyShape,
    ) -> DisplayResult<Snapshot> {
        Err(unsupported_mutation())
    }

    fn apply(
        &mut self,
        _: &Configuration,
        _: &Snapshot,
        _: &mut dyn FnMut() -> DisplayResult<()>,
    ) -> DisplayResult<()> {
        Err(unsupported_mutation())
    }

    fn apply_preserving(
        &mut self,
        _: &Configuration,
        _: &Snapshot,
        _: &str,
        _: Option<&RouteBinding>,
        _: TopologyShape,
        _: &mut dyn FnMut() -> DisplayResult<()>,
    ) -> DisplayResult<()> {
        Err(unsupported_mutation())
    }

    fn restore(
        &mut self,
        _: &Snapshot,
        _: &mut dyn FnMut() -> DisplayResult<()>,
    ) -> DisplayResult<()> {
        Err(unsupported_mutation())
    }
}

fn unsupported_mutation() -> DisplayError {
    DisplayError::new(
        "unsupported_platform",
        "Linux KScreen enumeration and snapshots are read-only; display mutation requires a journaled KScreen recovery provider",
    )
}

fn query_configuration() -> DisplayResult<Value> {
    require_user_session()?;
    let mut child = Command::new("kscreen-doctor")
        .arg("-j")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            DisplayError::new(
                "unsupported_platform",
                format!("could not start the user-session KScreen query: {error}"),
            )
        })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        DisplayError::new("platform_error", "KScreen query stdout pipe is unavailable")
    })?;
    let reader = thread::spawn(move || read_bounded_output(stdout));
    let deadline = Instant::now() + KSCREEN_QUERY_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(DisplayError::new(
                    "platform_error",
                    format!("KScreen query wait failed: {error}"),
                ));
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err(DisplayError::new(
                "platform_timeout",
                "user-session KScreen query exceeded its five-second deadline",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = reader
        .join()
        .map_err(|_| DisplayError::new("platform_error", "KScreen query reader panicked"))?
        .map_err(|error| {
            DisplayError::new(
                "platform_error",
                format!("KScreen query read failed: {error}"),
            )
        })?;
    if !status.success() {
        return Err(DisplayError::new(
            "platform_error",
            format!(
                "user-session KScreen query failed with exit status {}",
                status.code().unwrap_or(-1)
            ),
        ));
    }
    if stdout.len() > MAX_CONFIGURATION_BYTES {
        return Err(DisplayError::new(
            "resource_exhausted",
            "KScreen configuration exceeds the snapshot size limit",
        ));
    }
    let configuration: Value = serde_json::from_slice(&stdout).map_err(|error| {
        DisplayError::new("platform_error", format!("invalid KScreen JSON: {error}"))
    })?;
    validated_outputs(&configuration)?;
    Ok(configuration)
}

fn read_bounded_output(mut reader: impl Read) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(MAX_CONFIGURATION_BYTES + 1);
    let mut buffer = [0_u8; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let remaining = (MAX_CONFIGURATION_BYTES + 1).saturating_sub(bytes.len());
        let retained = count.min(remaining);
        bytes.extend_from_slice(&buffer[..retained]);
    }
    Ok(bytes)
}

fn enumeration_rows(configuration: &Value, display: Option<&str>) -> DisplayResult<Vec<Value>> {
    let outputs = validated_outputs(configuration)?;
    if let Some(name) = display {
        let output = outputs
            .iter()
            .copied()
            .find(|output| output_name(output).ok() == Some(name))
            .ok_or_else(|| {
                DisplayError::new("unsupported_display", "KScreen output was not found")
            })?;
        if !bool_field(output, "connected")? {
            return Err(DisplayError::new(
                "unsupported_display",
                "display modes are unavailable for a disconnected output",
            ));
        }
        return output_modes(output)?
            .into_iter()
            .map(|mode| {
                Ok(json!({
                    "id": string_field(mode, "id")?,
                    "width": integer_field(mode, "size", "width")?,
                    "height": integer_field(mode, "size", "height")?,
                    "refresh": refresh_rational(mode)?
                }))
            })
            .collect();
    }
    outputs
        .iter()
        .copied()
        .filter(|output| output.get("connected").and_then(Value::as_bool) == Some(true))
        .map(output_enumeration_row)
        .collect()
}

fn configuration_snapshot(configuration: &Value, platform: &str) -> DisplayResult<Value> {
    let outputs = validated_outputs(configuration)?;
    let kscreen = typed_snapshot(&outputs)?;
    let revision = kscreen.revision()?;
    let mut displays = outputs
        .iter()
        .copied()
        .filter(|output| {
            output.get("connected").and_then(Value::as_bool) == Some(true)
                && output.get("enabled").and_then(Value::as_bool) == Some(true)
        })
        .map(output_configuration_row)
        .collect::<DisplayResult<Vec<_>>>()?;
    displays.sort_by(|left, right| {
        left.get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .cmp(right.get("id").and_then(Value::as_str).unwrap_or_default())
    });

    let snapshot = json!({
        "platform": platform,
        "configuration": { "displays": displays },
        "topology_revision": revision,
        "session_state": kscreen
    });
    let encoded_size = serde_json::to_vec(&snapshot)
        .map_err(|error| DisplayError::new("platform_error", error.to_string()))?
        .len();
    if encoded_size > 12_000 {
        return Err(DisplayError::new(
            "resource_exhausted",
            "KScreen configuration snapshot exceeds the transport limit",
        ));
    }
    Ok(snapshot)
}

fn typed_snapshot(outputs: &[&Value]) -> DisplayResult<KScreenSnapshot> {
    let mut saved = Vec::with_capacity(outputs.len());
    for output in outputs {
        let position = output.get("pos").ok_or_else(|| {
            DisplayError::new("platform_error", "KScreen output position is missing")
        })?;
        let priority = output
            .get("priority")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(0);
        let mode = match output.get("currentModeId").and_then(Value::as_str) {
            Some(id) if !id.is_empty() => {
                let candidate = output_modes(output)?
                    .into_iter()
                    .find(|mode| mode.get("id").and_then(Value::as_str) == Some(id))
                    .ok_or_else(|| {
                        DisplayError::new(
                            "unsupported_topology",
                            "KScreen current mode identity has no matching mode record",
                        )
                    })?;
                let refresh_millihz = mode_refresh_millihz(candidate)?;
                Some(KScreenModeSnapshot {
                    id: id.to_owned(),
                    width: integer_field(candidate, "size", "width")?,
                    height: integer_field(candidate, "size", "height")?,
                    refresh_millihz,
                })
            }
            _ => None,
        };
        saved.push(KScreenOutputSnapshot {
            id: output_name(output)?.to_owned(),
            connected: bool_field(output, "connected")?,
            enabled: bool_field(output, "enabled")?,
            mode,
            position: KScreenPosition {
                x: bounded_integer(position, "x", -65_536, 65_536)?,
                y: bounded_integer(position, "y", -65_536, 65_536)?,
            },
            scale: output.get("scale").and_then(Value::as_f64).unwrap_or(1.0),
            rotation: output
                .get("rotation")
                .and_then(Value::as_u64)
                .and_then(|value| u16::try_from(value).ok())
                .unwrap_or(1),
            priority,
            primary: priority == 1,
            hdr_enabled: output.get("hdr").and_then(Value::as_bool),
        });
    }
    let snapshot = KScreenSnapshot {
        version: 1,
        outputs: saved,
    };
    snapshot.validate()?;
    Ok(snapshot)
}

fn require_user_session() -> DisplayResult<()> {
    let runtime_directory = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    let bus_address = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap_or_default();
    if !std::path::Path::new(&runtime_directory).is_absolute() || bus_address.is_empty() {
        return Err(DisplayError::new(
            "unsupported_platform",
            "KScreen access requires the current user's active D-Bus session and XDG_RUNTIME_DIR",
        ));
    }
    Ok(())
}

fn validated_outputs(configuration: &Value) -> DisplayResult<Vec<&Value>> {
    let outputs = configuration
        .get("outputs")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            DisplayError::new("platform_error", "KScreen response has no outputs array")
        })?;
    if outputs.len() > MAX_OUTPUTS {
        return Err(DisplayError::new(
            "resource_exhausted",
            "KScreen output count is outside the supported limit",
        ));
    }
    let mut names = BTreeSet::new();
    for output in outputs {
        let name = output_name(output)?;
        if !names.insert(name.to_ascii_lowercase()) {
            return Err(DisplayError::new(
                "platform_error",
                "KScreen response contains duplicate output identities",
            ));
        }
        bool_field(output, "connected")?;
        bool_field(output, "enabled")?;
        let modes = output_modes(output)?;
        if modes.len() > MAX_MODES_PER_OUTPUT {
            return Err(DisplayError::new(
                "resource_exhausted",
                "KScreen mode count exceeds the supported limit",
            ));
        }
        for mode in &modes {
            safe_mode_id(string_field(mode, "id")?)?;
            let width = integer_field(mode, "size", "width")?;
            let height = integer_field(mode, "size", "height")?;
            let refresh = refresh_rational(mode)?;
            if !(1..=16_384).contains(&width)
                || !(1..=16_384).contains(&height)
                || refresh.denominator == 0
                || refresh.numerator == 0
            {
                return Err(DisplayError::new(
                    "platform_error",
                    "KScreen response contains invalid mode dimensions or refresh rate",
                ));
            }
        }
        let position = output
            .get("pos")
            .filter(|value| value.is_object())
            .ok_or_else(|| DisplayError::new("platform_error", "KScreen output has no position"))?;
        bounded_integer(position, "x", -65_536, 65_536)?;
        bounded_integer(position, "y", -65_536, 65_536)?;
        let scale = match output.get("scale") {
            Some(value) => value.as_f64().ok_or_else(|| {
                DisplayError::new("platform_error", "KScreen output scale is not numeric")
            })?,
            None => 1.0,
        };
        if !scale.is_finite() || !(0.25..=5.0).contains(&scale) {
            return Err(DisplayError::new(
                "platform_error",
                "KScreen output has an invalid scale",
            ));
        }
        if let Some(rotation) = output.get("rotation") {
            let rotation = rotation.as_i64().ok_or_else(|| {
                DisplayError::new("platform_error", "KScreen rotation is not an integer")
            })?;
            if ![1, 2, 4, 8, 16, 32, 64, 128].contains(&rotation) {
                return Err(DisplayError::new(
                    "unsupported_topology",
                    "KScreen rotation has an unknown compositor value",
                ));
            }
        }
        if let Some(priority) = output.get("priority") {
            if priority.as_u64().is_none_or(|value| value > 64) {
                return Err(DisplayError::new(
                    "platform_error",
                    "KScreen output priority is outside the supported range",
                ));
            }
        }
        if let Some(hdr) = output.get("hdr")
            && !hdr.is_boolean()
        {
            return Err(DisplayError::new(
                "platform_error",
                "KScreen HDR state is not boolean",
            ));
        }
        if bool_field(output, "enabled")? && bool_field(output, "connected")? {
            let current_mode = string_field(output, "currentModeId")?;
            safe_mode_id(current_mode)?;
            if !modes
                .iter()
                .any(|mode| mode.get("id").and_then(Value::as_str) == Some(current_mode))
            {
                return Err(DisplayError::new(
                    "unsupported_topology",
                    "active KScreen output has no matching current mode record",
                ));
            }
        }
    }
    Ok(outputs.iter().collect())
}

fn output_name(output: &Value) -> DisplayResult<&str> {
    let name = string_field(output, "name")?;
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(DisplayError::new(
            "platform_error",
            "KScreen output identity is not a bounded connector name",
        ));
    }
    Ok(name)
}

fn safe_mode_id(id: &str) -> DisplayResult<()> {
    if id.is_empty()
        || id.len() > 128
        || !id.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'@' | b'x')
        })
    {
        return Err(DisplayError::new(
            "platform_error",
            "KScreen mode identity is invalid",
        ));
    }
    Ok(())
}

fn output_modes(output: &Value) -> DisplayResult<Vec<&Value>> {
    let modes = match output.get("modes") {
        Some(modes) => modes
            .as_array()
            .ok_or_else(|| DisplayError::new("platform_error", "KScreen mode list is invalid"))?,
        None => return Ok(Vec::new()),
    };
    Ok(modes.iter().collect())
}

fn string_field<'a>(value: &'a Value, field: &str) -> DisplayResult<&'a str> {
    value.get(field).and_then(Value::as_str).ok_or_else(|| {
        DisplayError::new(
            "platform_error",
            format!("KScreen field {field} is missing or invalid"),
        )
    })
}

fn bool_field(value: &Value, field: &str) -> DisplayResult<bool> {
    value.get(field).and_then(Value::as_bool).ok_or_else(|| {
        DisplayError::new(
            "platform_error",
            format!("KScreen field {field} is missing or invalid"),
        )
    })
}

fn integer_field(value: &Value, object: &str, field: &str) -> DisplayResult<u32> {
    value
        .get(object)
        .and_then(|value| value.get(field))
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            DisplayError::new(
                "platform_error",
                format!("KScreen {object}.{field} is invalid"),
            )
        })
}

fn bounded_integer(value: &Value, field: &str, minimum: i64, maximum: i64) -> DisplayResult<i32> {
    let integer = value.get(field).and_then(Value::as_i64).ok_or_else(|| {
        DisplayError::new(
            "platform_error",
            format!("KScreen position {field} is invalid"),
        )
    })?;
    if !(minimum..=maximum).contains(&integer) {
        return Err(DisplayError::new(
            "platform_error",
            "KScreen position is outside the supported range",
        ));
    }
    Ok(integer as i32)
}

fn refresh_rational(mode: &Value) -> DisplayResult<Rational> {
    let refresh = mode
        .get("refreshRate")
        .and_then(Value::as_f64)
        .ok_or_else(|| {
            DisplayError::new("platform_error", "KScreen mode refresh rate is invalid")
        })?;
    if !refresh.is_finite() || !(1.0..=10_000.0).contains(&refresh) {
        return Err(DisplayError::new(
            "platform_error",
            "KScreen mode refresh rate is outside the supported range",
        ));
    }
    let millihertz = (refresh * 1000.0).round() as u32;
    let divisor = greatest_common_divisor(millihertz, 1000);
    Ok(Rational {
        numerator: millihertz / divisor,
        denominator: 1000 / divisor,
    })
}

fn mode_refresh_millihz(mode: &Value) -> DisplayResult<u32> {
    let refresh = mode
        .get("refreshRate")
        .and_then(Value::as_f64)
        .ok_or_else(|| {
            DisplayError::new("platform_error", "KScreen mode refresh rate is invalid")
        })?;
    if !refresh.is_finite() || !(1.0..=10_000.0).contains(&refresh) {
        return Err(DisplayError::new(
            "platform_error",
            "KScreen mode refresh rate is outside the supported range",
        ));
    }
    Ok((refresh * 1000.0).round() as u32)
}

fn greatest_common_divisor(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left.max(1)
}

fn current_mode(output: &Value) -> DisplayResult<&Value> {
    let id = string_field(output, "currentModeId")?;
    output_modes(output)?
        .into_iter()
        .find(|mode| mode.get("id").and_then(Value::as_str) == Some(id))
        .ok_or_else(|| {
            DisplayError::new("unsupported_topology", "active KScreen mode is unavailable")
        })
}

fn output_enumeration_row(output: &Value) -> DisplayResult<Value> {
    let name = output_name(output)?;
    let current = if bool_field(output, "enabled")? {
        let mode = current_mode(output)?;
        Some(json!({
            "width": integer_field(mode, "size", "width")?,
            "height": integer_field(mode, "size", "height")?,
            "refresh": refresh_rational(mode)?,
            "x": bounded_integer(output.get("pos").unwrap_or(&Value::Null), "x", -65_536, 65_536)?,
            "y": bounded_integer(output.get("pos").unwrap_or(&Value::Null), "y", -65_536, 65_536)?
        }))
    } else {
        None
    };
    Ok(json!({
        "id": name,
        "name": output.get("description").and_then(Value::as_str).unwrap_or(name),
        "connected": bool_field(output, "connected")?,
        "active": bool_field(output, "enabled")?,
        "current": current,
        "scale": output.get("scale").and_then(Value::as_f64).unwrap_or(1.0),
        "rotation": output.get("rotation").and_then(Value::as_i64).unwrap_or(1),
        "hdr_enabled": output.get("hdr").cloned().unwrap_or(Value::Null)
    }))
}

fn output_configuration_row(output: &Value) -> DisplayResult<Value> {
    let mode = current_mode(output)?;
    let position = output
        .get("pos")
        .ok_or_else(|| DisplayError::new("platform_error", "KScreen output position is missing"))?;
    let rotation = match output.get("rotation").and_then(Value::as_i64).unwrap_or(1) {
        1 => "identity",
        2 => "rotate90",
        4 => "rotate180",
        8 => "rotate270",
        _ => {
            return Err(DisplayError::new(
                "unsupported_topology",
                "KScreen rotation cannot be represented",
            ));
        }
    };
    Ok(json!({
        "id": output_name(output)?,
        "width": integer_field(mode, "size", "width")?,
        "height": integer_field(mode, "size", "height")?,
        "refresh": refresh_rational(mode)?,
        "x": bounded_integer(position, "x", -65_536, 65_536)?,
        "y": bounded_integer(position, "y", -65_536, 65_536)?,
        "rotation": rotation,
        "hdr_enabled": output.get("hdr").cloned().unwrap_or(Value::Null)
    }))
}

/// Produce a restoration request only when every enabled baseline output is
/// still connected and no unknown connected output appeared. It is fixture-only
/// until the existing lease pipeline accepts KScreen journal records.
#[cfg(test)]
fn restoration_arguments(saved: &KScreenSnapshot, current: &Value) -> DisplayResult<Vec<String>> {
    saved.validate()?;
    let current_outputs = validated_outputs(current)?;
    let current_by_name: BTreeMap<_, _> = current_outputs
        .iter()
        .map(|output| (output_name(output).expect("validated output name"), *output))
        .collect();
    let saved_by_name: BTreeMap<_, _> = saved
        .outputs
        .iter()
        .map(|output| (output.id.as_str(), output))
        .collect();

    for saved_output in &saved.outputs {
        match (
            saved_output.connected,
            current_by_name.get(saved_output.id.as_str()),
        ) {
            (true, Some(present)) if bool_field(present, "connected")? => {}
            (false, Some(present)) if bool_field(present, "connected")? => {
                return Err(DisplayError::new(
                    "topology_changed",
                    "a previously disconnected output appeared; restoration is held for recovery",
                ));
            }
            (true, _) => {
                return Err(DisplayError::new(
                    "topology_changed",
                    "a saved connected output is unavailable; restoration is held for recovery",
                ));
            }
            _ => {}
        }
    }
    for present in &current_outputs {
        if bool_field(present, "connected")? && !saved_by_name.contains_key(output_name(present)?) {
            return Err(DisplayError::new(
                "topology_changed",
                "an output appeared after the snapshot; restoration will not guess its state",
            ));
        }
    }

    let mut arguments = Vec::new();
    let mut sorted_names: Vec<_> = saved_by_name.keys().copied().collect();
    sorted_names.sort_unstable();
    for name in sorted_names {
        let saved_output = saved_by_name[name];
        let present = current_by_name.get(name).copied();
        if !saved_output.connected {
            continue;
        }
        let present = present.ok_or_else(|| {
            DisplayError::new(
                "topology_changed",
                "saved output is missing during restore planning",
            )
        })?;
        let prefix = format!("output.{name}.");
        if !saved_output.enabled {
            arguments.push(format!("{prefix}disable"));
            continue;
        }
        let saved_mode = saved_output.mode.as_ref().ok_or_else(|| {
            DisplayError::new(
                "invalid_snapshot",
                "enabled KScreen output has no saved mode",
            )
        })?;
        let present_modes = output_modes(present)?;
        let matches: Vec<_> = present_modes
            .into_iter()
            .filter(|mode| {
                integer_field(mode, "size", "width").ok() == Some(saved_mode.width)
                    && integer_field(mode, "size", "height").ok() == Some(saved_mode.height)
                    && mode_refresh_millihz(mode).ok() == Some(saved_mode.refresh_millihz)
            })
            .collect();
        let mode = matches
            .iter()
            .copied()
            .find(|mode| mode.get("id").and_then(Value::as_str) == Some(saved_mode.id.as_str()))
            .or_else(|| (matches.len() == 1).then(|| matches[0]))
            .ok_or_else(|| {
                DisplayError::new(
                    "topology_changed",
                    "the saved mode is no longer uniquely available; restoration is held for recovery",
                )
            })?;
        let mode_id = string_field(mode, "id")?;
        safe_mode_id(mode_id)?;
        arguments.push(format!("{prefix}enable"));
        arguments.push(format!("{prefix}mode.{mode_id}"));
        let rotation = i64::from(saved_output.rotation);
        let rotation_name = match rotation {
            1 => "none",
            2 => "left",
            4 => "inverted",
            8 => "right",
            16 => "flipped",
            32 => "flipped90",
            64 => "flipped180",
            128 => "flipped270",
            _ => {
                return Err(DisplayError::new(
                    "unsupported_topology",
                    "saved rotation cannot be restored",
                ));
            }
        };
        arguments.push(format!("{prefix}rotation.{rotation_name}"));
        arguments.push(format!("{prefix}scale.{}", saved_output.scale));
        arguments.push(format!(
            "{prefix}position.{},{}",
            saved_output.position.x, saved_output.position.y
        ));
        if saved_output.priority > 0 {
            arguments.push(format!("{prefix}priority.{}", saved_output.priority));
        }
        if let Some(hdr) = saved_output.hdr_enabled {
            arguments.push(format!(
                "{prefix}hdr.{}",
                if hdr { "enable" } else { "disable" }
            ));
        }
    }
    Ok(arguments)
}

#[cfg(test)]
#[path = "linux/tests.rs"]
mod tests;
