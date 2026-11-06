//! KDE/KScreen discovery and a capability-gated, conservative transaction path.
//!
//! Native display mutations remain unsupported on Linux until the journaled
//! apply/restore path is centrally tested and its platform behavior reviewed.
use super::{Platform, TopologyShape};
use crate::model::{
    Address, AvailableMode, Configuration, DisplayError, DisplayResult, KScreenModeSnapshot,
    KScreenOutputSnapshot, KScreenPosition, KScreenSnapshot, Rational, RouteBinding, Snapshot,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const MAX_CONFIGURATION_BYTES: usize = 1024 * 1024;
const MAX_OUTPUTS: usize = 64;
const MAX_MODES_PER_OUTPUT: usize = 1024;
const KSCREEN_QUERY_TIMEOUT: Duration = Duration::from_secs(5);
const KSCREEN_MUTATION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq)]
enum KScreenAction {
    Mode { output: String, mode: String },
    Position { output: String, x: i32, y: i32 },
    Scale { output: String, scale: f64 },
    Rotation { output: String, rotation: u16 },
    Priority { output: String, priority: u32 },
    Hdr { output: String, enabled: bool },
}

impl KScreenAction {
    fn argument(&self) -> DisplayResult<String> {
        let output = match self {
            Self::Mode { output, .. }
            | Self::Position { output, .. }
            | Self::Scale { output, .. }
            | Self::Rotation { output, .. }
            | Self::Priority { output, .. }
            | Self::Hdr { output, .. } => output,
        };
        validate_output_id(output)?;
        let prefix = format!("output.{output}.");
        Ok(match self {
            Self::Mode { mode, .. } => {
                safe_mode_id(mode)?;
                format!("{prefix}mode.{mode}")
            }
            Self::Position { x, y, .. } => {
                if x.abs_diff(0) > 65_536 || y.abs_diff(0) > 65_536 {
                    return Err(DisplayError::new(
                        "invalid_snapshot",
                        "KScreen position is out of range",
                    ));
                }
                format!("{prefix}position.{x},{y}")
            }
            Self::Scale { scale, .. } => {
                if !scale.is_finite() || !(0.25..=5.0).contains(scale) {
                    return Err(DisplayError::new(
                        "invalid_snapshot",
                        "KScreen scale is out of range",
                    ));
                }
                format!("{prefix}scale.{scale}")
            }
            Self::Rotation { rotation, .. } => {
                let name = rotation_name(*rotation).ok_or_else(|| {
                    DisplayError::new(
                        "unsupported_topology",
                        "KScreen rotation cannot be represented",
                    )
                })?;
                format!("{prefix}rotation.{name}")
            }
            Self::Priority { priority, .. } => {
                if *priority > 64 {
                    return Err(DisplayError::new(
                        "invalid_snapshot",
                        "KScreen priority is out of range",
                    ));
                }
                format!("{prefix}priority.{priority}")
            }
            Self::Hdr { enabled, .. } => {
                format!(
                    "{prefix}hdr.{}",
                    if *enabled { "enable" } else { "disable" }
                )
            }
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
enum KScreenCommand {
    Query,
    Configure(Vec<KScreenAction>),
}

#[derive(Debug)]
struct KScreenCommandOutput {
    status: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

trait KScreenCommandRunner {
    /// Errors after a configure command may have started must carry the
    /// `may_have_changed` marker so the caller retains its durable journal.
    fn execute(&mut self, command: &KScreenCommand) -> DisplayResult<KScreenCommandOutput>;
}

struct SystemKScreenCommandRunner;

impl KScreenCommandRunner for SystemKScreenCommandRunner {
    fn execute(&mut self, command: &KScreenCommand) -> DisplayResult<KScreenCommandOutput> {
        require_user_session()?;
        let arguments = match command {
            KScreenCommand::Query => vec!["-j".to_owned()],
            KScreenCommand::Configure(actions) => actions
                .iter()
                .map(KScreenAction::argument)
                .collect::<DisplayResult<Vec<_>>>()?,
        };
        if arguments.is_empty() {
            return Ok(KScreenCommandOutput {
                status: Some(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            });
        }
        let timeout = match command {
            KScreenCommand::Query => KSCREEN_QUERY_TIMEOUT,
            KScreenCommand::Configure(_) => KSCREEN_MUTATION_TIMEOUT,
        };
        let mut child = Command::new("kscreen-doctor")
            .args(&arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                DisplayError::new(
                    "unsupported_platform",
                    format!("could not start user-session KScreen command: {error}"),
                )
            })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| {
                DisplayError::new("platform_error", "KScreen stdout pipe is unavailable")
            })
            .map_err(|error| command_error(command, error))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| {
                DisplayError::new("platform_error", "KScreen stderr pipe is unavailable")
            })
            .map_err(|error| command_error(command, error))?;
        let stdout_reader = thread::spawn(move || read_bounded_output(stdout));
        let stderr_reader = thread::spawn(move || read_bounded_output(stderr));
        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {}
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    let failure = DisplayError::new(
                        "platform_error",
                        format!("KScreen command wait failed: {error}"),
                    );
                    return Err(if matches!(command, KScreenCommand::Configure(_)) {
                        failure.after_mutation()
                    } else {
                        failure
                    });
                }
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                let failure = DisplayError::new(
                    "platform_timeout",
                    "KScreen command exceeded its bounded deadline",
                );
                return Err(if matches!(command, KScreenCommand::Configure(_)) {
                    failure.after_mutation()
                } else {
                    failure
                });
            }
            thread::sleep(Duration::from_millis(10));
        };
        let stdout = stdout_reader
            .join()
            .map_err(|_| DisplayError::new("platform_error", "KScreen stdout reader panicked"))
            .and_then(|output| {
                output.map_err(|error| {
                    DisplayError::new(
                        "platform_error",
                        format!("KScreen stdout read failed: {error}"),
                    )
                })
            })
            .map_err(|error| command_error(command, error))?;
        let stderr = stderr_reader
            .join()
            .map_err(|_| DisplayError::new("platform_error", "KScreen stderr reader panicked"))
            .and_then(|output| {
                output.map_err(|error| {
                    DisplayError::new(
                        "platform_error",
                        format!("KScreen stderr read failed: {error}"),
                    )
                })
            })
            .map_err(|error| command_error(command, error))?;
        Ok(KScreenCommandOutput {
            status: status.and_then(|status| status.code()),
            stdout,
            stderr,
        })
    }
}

fn command_error(command: &KScreenCommand, error: DisplayError) -> DisplayError {
    if matches!(command, KScreenCommand::Configure(_)) {
        error.after_mutation()
    } else {
        error
    }
}

#[derive(Default)]
pub struct LinuxPlatform;

impl Platform for LinuxPlatform {
    fn name(&self) -> &'static str {
        "linux_kscreen"
    }

    // KScreen snapshot journaling is available, but output mutations remain
    // gated until restoration and exact readback are qualified.
    fn supported(&self) -> bool {
        false
    }

    fn supports_scoped_composition(&self) -> bool {
        true
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

    fn capture_kscreen_snapshot(&mut self) -> DisplayResult<Option<KScreenSnapshot>> {
        let configuration = query_configuration()?;
        let outputs = validated_outputs(&configuration)?;
        Ok(Some(typed_snapshot(&outputs)?))
    }

    fn prepare_kscreen_snapshot(
        &mut self,
        requested: &Configuration,
        baseline: &KScreenSnapshot,
    ) -> DisplayResult<()> {
        baseline.validate_requested_topology(requested)?;
        let current = query_configuration()?;
        let outputs = validated_outputs(&current)?;
        if typed_snapshot(&outputs)?.revision()? != baseline.revision()? {
            return Err(DisplayError::new(
                "topology_changed",
                "KScreen topology changed before configuration preparation",
            ));
        }
        for display in &requested.displays {
            let output = outputs
                .iter()
                .copied()
                .find(|output| {
                    output_name(output).is_ok_and(|name| name.eq_ignore_ascii_case(&display.id))
                })
                .ok_or_else(|| {
                    DisplayError::new(
                        "topology_changed",
                        "requested KScreen output is no longer connected",
                    )
                })?;
            let requested_millihz = ((u64::from(display.refresh.numerator) * 1000
                + u64::from(display.refresh.denominator) / 2)
                / u64::from(display.refresh.denominator))
                as u32;
            let mode_available = output_modes(output)?.into_iter().any(|mode| {
                integer_field(mode, "size", "width").ok() == Some(display.width)
                    && integer_field(mode, "size", "height").ok() == Some(display.height)
                    && mode_refresh_millihz(mode).ok() == Some(requested_millihz)
            });
            if !mode_available {
                return Err(DisplayError::new(
                    "unsupported_display",
                    "requested KScreen mode is unavailable",
                ));
            }
            let saved_hdr = baseline
                .outputs
                .iter()
                .find(|saved| saved.id.eq_ignore_ascii_case(&display.id))
                .and_then(|saved| saved.hdr_enabled);
            if display
                .hdr_enabled
                .is_some_and(|requested| saved_hdr != Some(requested))
            {
                return Err(DisplayError::new(
                    "unsupported_topology",
                    "KScreen preparation does not yet support changing HDR state",
                ));
            }
        }
        Ok(())
    }

    fn restore_kscreen_snapshot(
        &mut self,
        baseline: &KScreenSnapshot,
        guard: &mut dyn FnMut() -> DisplayResult<()>,
    ) -> DisplayResult<()> {
        let mut runner = SystemKScreenCommandRunner;
        restore_kscreen_snapshot_with(&mut runner, baseline, guard)
    }

    fn apply_kscreen_snapshot(
        &mut self,
        configuration: &Configuration,
        baseline: &KScreenSnapshot,
        guard: &mut dyn FnMut() -> DisplayResult<()>,
    ) -> DisplayResult<()> {
        let mut runner = SystemKScreenCommandRunner;
        apply_kscreen_snapshot_with(&mut runner, configuration, baseline, guard)
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
        "Linux KScreen mutation capability remains disabled pending central safety qualification",
    )
}

fn query_configuration() -> DisplayResult<Value> {
    let mut runner = SystemKScreenCommandRunner;
    query_configuration_with(&mut runner)
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

fn query_configuration_with(runner: &mut impl KScreenCommandRunner) -> DisplayResult<Value> {
    let result = runner.execute(&KScreenCommand::Query)?;
    if result.status != Some(0) {
        return Err(DisplayError::new(
            "platform_error",
            format!(
                "user-session KScreen query failed with exit status {:?}",
                result.status
            ),
        ));
    }
    if result.stdout.len() > MAX_CONFIGURATION_BYTES {
        return Err(DisplayError::new(
            "resource_exhausted",
            "KScreen configuration exceeds the snapshot size limit",
        ));
    }
    let configuration: Value = serde_json::from_slice(&result.stdout).map_err(|error| {
        DisplayError::new("platform_error", format!("invalid KScreen JSON: {error}"))
    })?;
    validated_outputs(&configuration)?;
    Ok(configuration)
}

fn run_configuration_with(
    runner: &mut impl KScreenCommandRunner,
    actions: Vec<KScreenAction>,
) -> DisplayResult<()> {
    if actions.is_empty() {
        return Ok(());
    }
    let result = runner.execute(&KScreenCommand::Configure(actions))?;
    let output_bytes = result.stdout.len().saturating_add(result.stderr.len());
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    let rejected = stdout
        .to_ascii_lowercase()
        .contains("applying config failed")
        || stderr
            .to_ascii_lowercase()
            .contains("applying config failed");
    if output_bytes > MAX_CONFIGURATION_BYTES || result.status != Some(0) || rejected {
        return Err(DisplayError::new(
            if output_bytes > MAX_CONFIGURATION_BYTES {
                "resource_exhausted"
            } else {
                "platform_error"
            },
            "KScreen configuration completion was rejected or could not be confirmed",
        )
        .after_mutation());
    }
    Ok(())
}

#[cfg(test)]
fn action_arguments(actions: &[KScreenAction]) -> DisplayResult<Vec<String>> {
    actions.iter().map(KScreenAction::argument).collect()
}

fn apply_kscreen_snapshot_with(
    runner: &mut impl KScreenCommandRunner,
    configuration: &Configuration,
    baseline: &KScreenSnapshot,
    guard: &mut dyn FnMut() -> DisplayResult<()>,
) -> DisplayResult<()> {
    baseline.validate_requested_topology(configuration)?;
    guard()?;
    let before_raw = query_configuration_with(runner)?;
    let before_outputs = validated_outputs(&before_raw)?;
    let before = typed_snapshot(&before_outputs)?;
    if before.revision()? != baseline.revision()? {
        return Err(DisplayError::new(
            "topology_changed",
            "KScreen changed after the recovery lease was armed",
        ));
    }
    if !baseline.matches_restoration(&before) {
        return Err(DisplayError::new(
            "topology_changed",
            "KScreen output state changed after the recovery lease was armed",
        ));
    }
    apply_actions(configuration, &before_raw)?;
    guard()?;
    let final_raw = query_configuration_with(runner)?;
    let final_outputs = validated_outputs(&final_raw)?;
    let final_snapshot = typed_snapshot(&final_outputs)?;
    if final_snapshot.revision()? != baseline.revision()? {
        return Err(DisplayError::new(
            "topology_changed",
            "KScreen changed immediately before configuration",
        ));
    }
    if !baseline.matches_restoration(&final_snapshot) {
        return Err(DisplayError::new(
            "topology_changed",
            "KScreen output state changed immediately before configuration",
        ));
    }
    let actions = apply_actions(configuration, &final_raw)?;
    run_configuration_with(runner, actions)?;
    let after = query_configuration_with(runner).map_err(DisplayError::after_mutation)?;
    let after_outputs = validated_outputs(&after).map_err(DisplayError::after_mutation)?;
    let after = typed_snapshot(&after_outputs).map_err(DisplayError::after_mutation)?;
    if !after.matches_configuration(baseline, configuration) {
        return Err(DisplayError::new(
            "verification_failed",
            "KScreen did not read back the requested display configuration",
        )
        .after_mutation());
    }
    Ok(())
}

fn restore_kscreen_snapshot_with(
    runner: &mut impl KScreenCommandRunner,
    baseline: &KScreenSnapshot,
    guard: &mut dyn FnMut() -> DisplayResult<()>,
) -> DisplayResult<()> {
    baseline.validate()?;
    guard()?;
    let before_raw = query_configuration_with(runner)?;
    let before_outputs = validated_outputs(&before_raw)?;
    let before = typed_snapshot(&before_outputs)?;
    if baseline.matches_restoration(&before) {
        return Ok(());
    }
    restoration_actions(baseline, &before_raw)?;
    guard()?;
    let final_raw = query_configuration_with(runner)?;
    let final_outputs = validated_outputs(&final_raw)?;
    let final_snapshot = typed_snapshot(&final_outputs)?;
    if final_snapshot.revision()? != before.revision()? {
        return Err(DisplayError::new(
            "topology_changed",
            "KScreen changed immediately before restoration",
        ));
    }
    if baseline.matches_restoration(&final_snapshot) {
        return Ok(());
    }
    let actions = restoration_actions(baseline, &final_raw)?;
    run_configuration_with(runner, actions)?;
    let after = query_configuration_with(runner).map_err(DisplayError::after_mutation)?;
    let after_outputs = validated_outputs(&after).map_err(DisplayError::after_mutation)?;
    let after = typed_snapshot(&after_outputs).map_err(DisplayError::after_mutation)?;
    if !baseline.matches_restoration(&after) {
        return Err(DisplayError::new(
            "verification_failed",
            "KScreen restoration did not match its durable recovery baseline",
        )
        .after_mutation());
    }
    Ok(())
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
        if let Some(priority) = output.get("priority")
            && priority.as_u64().is_none_or(|value| value > 64)
        {
            return Err(DisplayError::new(
                "platform_error",
                "KScreen output priority is outside the supported range",
            ));
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

/// Produce restoration actions only when the journaled connector set and
/// enabled-output set still match. This path never enables/disables outputs.
fn restoration_actions(
    saved: &KScreenSnapshot,
    current: &Value,
) -> DisplayResult<Vec<KScreenAction>> {
    saved.validate()?;
    if !saved
        .outputs
        .iter()
        .any(|output| output.connected && output.enabled)
    {
        return Err(DisplayError::new(
            "unsupported_topology",
            "restoration refuses a baseline with no active output",
        ));
    }
    let current_outputs = validated_outputs(current)?;
    let current_snapshot = typed_snapshot(&current_outputs)?;
    let current_by_name: BTreeMap<_, _> = current_outputs
        .iter()
        .map(|output| {
            (
                output_name(output)
                    .expect("validated output name")
                    .to_ascii_lowercase(),
                *output,
            )
        })
        .collect();
    let current_state_by_name: BTreeMap<_, _> = current_snapshot
        .outputs
        .iter()
        .map(|output| (output.id.to_ascii_lowercase(), output))
        .collect();
    let saved_by_name: BTreeMap<_, _> = saved
        .outputs
        .iter()
        .map(|output| (output.id.to_ascii_lowercase(), output))
        .collect();

    for saved_output in &saved.outputs {
        let current = current_by_name.get(&saved_output.id.to_ascii_lowercase());
        match (saved_output.connected, current) {
            (true, Some(output)) if bool_field(output, "connected")? => {}
            (false, Some(output)) if bool_field(output, "connected")? => {
                return Err(DisplayError::new(
                    "topology_changed",
                    "a previously disconnected output appeared; restoration is held",
                ));
            }
            (true, _) => {
                return Err(DisplayError::new(
                    "topology_changed",
                    "a saved connected output is unavailable; restoration is held",
                ));
            }
            _ => {}
        }
    }
    for present in &current_outputs {
        if bool_field(present, "connected")?
            && !saved_by_name.contains_key(&output_name(present)?.to_ascii_lowercase())
        {
            return Err(DisplayError::new(
                "topology_changed",
                "an output appeared after the snapshot; restoration will not guess its state",
            ));
        }
    }

    let mut names: Vec<_> = saved_by_name.keys().cloned().collect();
    names.sort_unstable();
    let mut actions = Vec::new();
    for identity in names {
        let saved_output = saved_by_name[&identity];
        let (Some(present), Some(present_state)) = (
            current_by_name.get(&identity).copied(),
            current_state_by_name.get(&identity).copied(),
        ) else {
            if saved_output.connected {
                return Err(DisplayError::new(
                    "topology_changed",
                    "saved connector disappeared",
                ));
            }
            continue;
        };
        if !saved_output.connected || !saved_output.enabled {
            if !output_state_matches(saved_output, present_state) {
                return Err(DisplayError::new(
                    "unsupported_topology",
                    "restoration will not change a disconnected or disabled output",
                ));
            }
            continue;
        }
        if !bool_field(present, "enabled")? {
            return Err(DisplayError::new(
                "topology_changed",
                "restoration will not enable or disable outputs",
            ));
        }
        append_output_restoration(&mut actions, saved_output, present)?;
    }
    Ok(actions)
}

fn apply_actions(
    configuration: &Configuration,
    current: &Value,
) -> DisplayResult<Vec<KScreenAction>> {
    configuration.validate()?;
    let outputs = validated_outputs(current)?;
    let by_name: BTreeMap<_, _> = outputs
        .iter()
        .map(|output| {
            (
                output_name(output)
                    .expect("validated output name")
                    .to_ascii_lowercase(),
                *output,
            )
        })
        .collect();
    let mut actions = Vec::new();
    for display in &configuration.displays {
        validate_output_id(&display.id)?;
        let output = by_name
            .get(&display.id.to_ascii_lowercase())
            .copied()
            .ok_or_else(|| {
                DisplayError::new(
                    "topology_changed",
                    "requested KScreen connector disappeared",
                )
            })?;
        if !bool_field(output, "connected")? || !bool_field(output, "enabled")? {
            return Err(DisplayError::new(
                "topology_changed",
                "KScreen apply will not change output enable state",
            ));
        }
        let requested = KScreenModeSnapshot {
            id: String::new(),
            width: display.width,
            height: display.height,
            refresh_millihz: rational_millihz(display.refresh)?,
        };
        let current_mode = current_mode(output)?;
        let selected_mode = resolve_mode(output, &requested, None)?;
        if !mode_timing_matches(current_mode, &requested)? {
            actions.push(KScreenAction::Mode {
                output: output_name(output)?.to_owned(),
                mode: string_field(selected_mode, "id")?.to_owned(),
            });
        }
        let position = output.get("pos").ok_or_else(|| {
            DisplayError::new("platform_error", "KScreen output position is missing")
        })?;
        if bounded_integer(position, "x", -65_536, 65_536)? != display.x
            || bounded_integer(position, "y", -65_536, 65_536)? != display.y
        {
            actions.push(KScreenAction::Position {
                output: output_name(output)?.to_owned(),
                x: display.x,
                y: display.y,
            });
        }
        let rotation = match display.rotation {
            crate::model::Rotation::Identity => 1,
            crate::model::Rotation::Rotate90 => 2,
            crate::model::Rotation::Rotate180 => 4,
            crate::model::Rotation::Rotate270 => 8,
        };
        let current_rotation = output
            .get("rotation")
            .and_then(Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(1);
        if rotation != current_rotation {
            actions.push(KScreenAction::Rotation {
                output: output_name(output)?.to_owned(),
                rotation,
            });
        }
        if let Some(requested_hdr) = display.hdr_enabled {
            let current_hdr = output.get("hdr").and_then(Value::as_bool).ok_or_else(|| {
                DisplayError::new(
                    "unsupported_topology",
                    "KScreen HDR state must be readable before it can be changed",
                )
            })?;
            if current_hdr != requested_hdr {
                actions.push(KScreenAction::Hdr {
                    output: output_name(output)?.to_owned(),
                    enabled: requested_hdr,
                });
            }
        }
    }
    Ok(actions)
}

fn append_output_restoration(
    actions: &mut Vec<KScreenAction>,
    saved: &KScreenOutputSnapshot,
    present: &Value,
) -> DisplayResult<()> {
    let output = output_name(present)?.to_owned();
    let saved_mode = saved.mode.as_ref().ok_or_else(|| {
        DisplayError::new(
            "invalid_snapshot",
            "enabled KScreen output has no saved mode",
        )
    })?;
    let selected_mode = resolve_mode(present, saved_mode, Some(&saved_mode.id))?;
    let current_mode = current_mode(present)?;
    if !mode_timing_matches(current_mode, saved_mode)? {
        actions.push(KScreenAction::Mode {
            output: output.clone(),
            mode: string_field(selected_mode, "id")?.to_owned(),
        });
    }
    let position = present
        .get("pos")
        .ok_or_else(|| DisplayError::new("platform_error", "KScreen output position is missing"))?;
    if bounded_integer(position, "x", -65_536, 65_536)? != saved.position.x
        || bounded_integer(position, "y", -65_536, 65_536)? != saved.position.y
    {
        actions.push(KScreenAction::Position {
            output: output.clone(),
            x: saved.position.x,
            y: saved.position.y,
        });
    }
    if present.get("scale").and_then(Value::as_f64).unwrap_or(1.0) != saved.scale {
        actions.push(KScreenAction::Scale {
            output: output.clone(),
            scale: saved.scale,
        });
    }
    let rotation = present
        .get("rotation")
        .and_then(Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())
        .unwrap_or(1);
    if rotation != saved.rotation {
        actions.push(KScreenAction::Rotation {
            output: output.clone(),
            rotation: saved.rotation,
        });
    }
    let priority = present
        .get("priority")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(0);
    if priority != saved.priority {
        actions.push(KScreenAction::Priority {
            output: output.clone(),
            priority: saved.priority,
        });
    }
    match (
        saved.hdr_enabled,
        present.get("hdr").and_then(Value::as_bool),
    ) {
        (Some(expected), Some(actual)) if expected != actual => actions.push(KScreenAction::Hdr {
            output,
            enabled: expected,
        }),
        (Some(_), Some(_)) | (None, None) => {}
        _ => {
            return Err(DisplayError::new(
                "unsupported_topology",
                "KScreen HDR state is not readable enough for safe restoration",
            ));
        }
    }
    Ok(())
}

fn output_state_matches(saved: &KScreenOutputSnapshot, present: &KScreenOutputSnapshot) -> bool {
    saved.connected == present.connected
        && saved.enabled == present.enabled
        && saved
            .mode
            .as_ref()
            .map(|mode| (mode.width, mode.height, mode.refresh_millihz))
            == present
                .mode
                .as_ref()
                .map(|mode| (mode.width, mode.height, mode.refresh_millihz))
        && saved.position == present.position
        && saved.scale == present.scale
        && saved.rotation == present.rotation
        && saved.priority == present.priority
        && saved.primary == present.primary
        && saved.hdr_enabled == present.hdr_enabled
}

fn resolve_mode<'a>(
    output: &'a Value,
    requested: &KScreenModeSnapshot,
    preferred_id: Option<&str>,
) -> DisplayResult<&'a Value> {
    let mut matches = Vec::new();
    for mode in output_modes(output)? {
        if mode_timing_matches(mode, requested)? {
            matches.push(mode);
        }
    }
    let selected = preferred_id
        .and_then(|id| {
            matches
                .iter()
                .copied()
                .find(|mode| mode.get("id").and_then(Value::as_str) == Some(id))
        })
        .or_else(|| (matches.len() == 1).then(|| matches[0]));
    selected.ok_or_else(|| {
        DisplayError::new(
            "unsupported_topology",
            "matching KScreen mode is unavailable or ambiguous",
        )
    })
}

fn mode_timing_matches(mode: &Value, requested: &KScreenModeSnapshot) -> DisplayResult<bool> {
    Ok(integer_field(mode, "size", "width")? == requested.width
        && integer_field(mode, "size", "height")? == requested.height
        && mode_refresh_millihz(mode)? == requested.refresh_millihz)
}

fn rational_millihz(refresh: Rational) -> DisplayResult<u32> {
    if refresh.denominator == 0 {
        return Err(DisplayError::new(
            "invalid_request",
            "refresh denominator cannot be zero",
        ));
    }
    u32::try_from(
        (u64::from(refresh.numerator) * 1000 + u64::from(refresh.denominator) / 2)
            / u64::from(refresh.denominator),
    )
    .map_err(|_| DisplayError::new("invalid_request", "refresh rate is out of range"))
}

fn validate_output_id(id: &str) -> DisplayResult<()> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(DisplayError::new(
            "invalid_request",
            "KScreen connector identity is invalid",
        ));
    }
    Ok(())
}

fn rotation_name(rotation: u16) -> Option<&'static str> {
    match rotation {
        1 => Some("none"),
        2 => Some("left"),
        4 => Some("inverted"),
        8 => Some("right"),
        16 => Some("flipped"),
        32 => Some("flipped90"),
        64 => Some("flipped180"),
        128 => Some("flipped270"),
        _ => None,
    }
}

#[cfg(test)]
#[path = "linux/tests.rs"]
mod tests;
