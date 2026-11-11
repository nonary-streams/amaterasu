//! Configure the newly owned connector in the current KDE session, either
//! beside the existing desktop or as its only enabled display.
use super::{
    layout::Layout,
    restore::{Desktop, READBACK_INTERVAL, restore_layout},
};
use crate::{model::Specification, scale::ScaleSetting};
use serde_json::Value;
use std::{
    io::{self, Read},
    os::fd::AsRawFd,
    os::unix::process::CommandExt,
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

/// Where the owned output goes relative to the rest of the desktop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Placement {
    /// Right of the existing desktop, which keeps its current layout.
    Extended,
    /// Keep the desktop extended while making the owned output primary.
    ExtendedPrimary,
    /// Place the owned output near the compositor's far desktop boundary.
    ExtendedIsolated,
    /// Combine primary ownership and isolated placement.
    ExtendedPrimaryIsolated,
    /// The only enabled display apart from `keep`, the outputs of other
    /// Amaterasu streams, which stay on and are never displaced.
    Exclusive { keep: Vec<String> },
}
impl Placement {
    fn primary(&self) -> bool {
        matches!(self, Self::ExtendedPrimary | Self::ExtendedPrimaryIsolated)
    }
    fn isolated(&self) -> bool {
        matches!(self, Self::ExtendedIsolated | Self::ExtendedPrimaryIsolated)
    }
}

pub(super) trait Compositor: Send {
    fn verify_retained_mode(
        &mut self,
        _output: &str,
        _desired: &Specification,
    ) -> Result<(), String> {
        Err("retained mode observation unavailable".into())
    }
    fn verify_mode_only(
        &mut self,
        _output: &str,
        _desired: &Specification,
        _baseline: &Value,
    ) -> Result<Value, String> {
        Err("mode-only verification is unavailable".into())
    }
    fn mode_only_baseline(
        &mut self,
        _output: &str,
        _old: &Specification,
        _desired: &Specification,
    ) -> Result<Value, String> {
        Err("mode-only preservation is unavailable".into())
    }
    fn apply_mode_only(
        &mut self,
        _output: &str,
        _desired: &Specification,
        _baseline: &Value,
    ) -> Result<(), String> {
        Err("mode-only preservation is unavailable".into())
    }
    fn snapshot(&mut self, _changing: &str) -> Result<Option<Layout>, String> {
        Ok(None)
    }
    fn restore(&mut self, _layout: Option<&Layout>) -> Result<(), String> {
        Ok(())
    }
    /// Returns false only when the owned output is still observable but no
    /// distinct active output can guard its removal.
    fn verify_removal_guard(&mut self, output: &str) -> Result<bool, String>;
    fn verify_parked(&mut self, output: &str) -> Result<(), String>;
    fn configure(
        &mut self,
        output: &str,
        specification: &Specification,
        layout: Option<&Layout>,
        placement: &Placement,
        scale: ScaleSetting,
        newly_connected: bool,
    ) -> Result<(), String>;
}
pub(super) struct KScreen;
impl Compositor for KScreen {
    fn verify_retained_mode(
        &mut self,
        output: &str,
        desired: &Specification,
    ) -> Result<(), String> {
        verify_retained_readback(&query()?, output, desired)
    }
    fn verify_mode_only(
        &mut self,
        output: &str,
        desired: &Specification,
        baseline: &Value,
    ) -> Result<Value, String> {
        verify_mode_only_state(&query()?, output, desired, baseline)
    }
    fn mode_only_baseline(
        &mut self,
        output: &str,
        old: &Specification,
        desired: &Specification,
    ) -> Result<Value, String> {
        let configuration = query()?;
        let old_mode = mode(&configuration, output, old).ok_or("current mode unavailable")?;
        if find_output(&configuration, output).and_then(|value| value["currentModeId"].as_str())
            != Some(old_mode.as_str())
        {
            return Err("current mode does not match owned record".into());
        }
        let desired_mode =
            mode(&configuration, output, desired).ok_or("requested mode unavailable")?;
        Ok(
            serde_json::json!({"state": preservation_state(&configuration, output)?, "old_mode":old_mode, "desired_mode":desired_mode}),
        )
    }
    fn apply_mode_only(
        &mut self,
        output: &str,
        desired: &Specification,
        baseline: &Value,
    ) -> Result<(), String> {
        let current = query()?;
        let desired_mode = baseline["desired_mode"]
            .as_str()
            .ok_or("missing requested mode")?;
        if preservation_state(&current, output)? != baseline["state"]
            || find_output(&current, output).and_then(|value| value["currentModeId"].as_str())
                != baseline["old_mode"].as_str()
            || mode(&current, output, desired).as_deref() != Some(desired_mode)
        {
            return Err("display state changed before mode-only operation".into());
        }
        run(&mode_only_arguments(output, desired_mode)?)?;
        let after = query()?;
        if preservation_state(&after, output)? != baseline["state"]
            || find_output(&after, output).and_then(|value| value["currentModeId"].as_str())
                != Some(desired_mode)
        {
            return Err("mode-only preservation readback differs".into());
        }
        Ok(())
    }
    fn snapshot(&mut self, changing: &str) -> Result<Option<Layout>, String> {
        Layout::capture(&query()?, changing).map(Some)
    }
    fn restore(&mut self, layout: Option<&Layout>) -> Result<(), String> {
        let Some(layout) = layout else {
            return Ok(());
        };
        let mut desktop = KScreenDesktop {
            started: Instant::now(),
        };
        let settled = restore_layout(&mut desktop, layout)?;
        if let Some(difference) = settled.last_difference {
            // Names the output and property that needed time, so a recurring
            // slow restoration can be traced to its cause from the journal.
            eprintln!(
                "amaterasu virtual display desktop restoration settled late: applications={} differing_readbacks={} waited_ms={} last_difference={:?}",
                settled.applications,
                settled.differing_readbacks,
                settled.waited.as_millis(),
                bounded_detail(&difference)
            );
        }
        Ok(())
    }
    fn verify_removal_guard(&mut self, output: &str) -> Result<bool, String> {
        let started = Instant::now();
        loop {
            let observation = observe_removal_guard(output);
            let settling = match &observation {
                Ok((RemovalGuard::Survivor, _)) => return Ok(true),
                Ok((_, awaiting_survivor)) => *awaiting_survivor,
                // The compositor answers neither protocol during a modeset.
                Err(_) => true,
            };
            if !settling || started.elapsed() >= GUARD_SETTLE_DEADLINE {
                return match observation?.0 {
                    RemovalGuard::Survivor => Ok(true),
                    RemovalGuard::OwnedOnly => Ok(false),
                    RemovalGuard::Unobserved => Err(
                        "neither a surviving output nor the owned output can be observed".into(),
                    ),
                };
            }
            thread::sleep(READBACK_INTERVAL);
        }
    }
    fn verify_parked(&mut self, output: &str) -> Result<(), String> {
        let configuration = query()?;
        let active = configuration["outputs"].as_array().is_some_and(|outputs| {
            outputs.iter().any(|candidate| {
                candidate["name"] == output
                    && candidate["connected"] == true
                    && candidate["enabled"] == true
            })
        });
        let observed = super::observer::Observer::open()
            .and_then(|mut observer| observer.read(output))
            .is_some_and(|seen| seen.width > 0 && seen.height > 0 && seen.refresh > 0);
        if active && observed {
            Ok(())
        } else {
            Err("owned output is not observable for parking".into())
        }
    }
    fn configure(
        &mut self,
        output: &str,
        specification: &Specification,
        layout: Option<&Layout>,
        placement: &Placement,
        scale: ScaleSetting,
        newly_connected: bool,
    ) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let configuration = loop {
            let configuration = query()?;
            if mode(&configuration, output, specification).is_some() {
                break configuration;
            }
            if Instant::now() >= deadline {
                return Err(if find_output(&configuration, output).is_some() {
                    "requested owned display mode is unavailable"
                } else {
                    "owned connector did not appear in KScreen"
                }
                .into());
            }
            thread::sleep(Duration::from_millis(25));
        };
        let selected = mode(&configuration, output, specification)
            .ok_or("requested owned display mode is unavailable")?;
        let target = OwnedTarget {
            mode: selected,
            hdr: specification.hdr_enabled,
            scale: scale.factor(specification.width, specification.height),
        };
        if let Placement::Exclusive { keep } = placement {
            return configure_exclusive(&configuration, output, &target, keep, newly_connected);
        }
        let (plan, arguments) = extended_configuration(
            &configuration,
            output,
            specification,
            &target,
            layout,
            placement,
        )?;
        configure_target_at(
            &mut KScreenDesktop {
                started: Instant::now(),
            },
            output,
            &target,
            plan,
            arguments,
            newly_connected,
            |confirmed| {
                Ok(layout.is_none_or(|layout| {
                    if placement.primary() {
                        layout.matches_with_primary(confirmed, output)
                    } else {
                        layout.matches(confirmed)
                    }
                }) && (!placement.primary()
                    || find_output(confirmed, output)
                        .is_some_and(|actual| actual["priority"] == 1)))
            },
        )
    }
}

/// A just-enabled output is published to Wayland clients moments after KScreen
/// lists it; wait this long for it before deciding that nothing survives.
const GUARD_SETTLE_DEADLINE: Duration = Duration::from_secs(2);

/// The live KDE session, timed from the start of one restoration.
struct KScreenDesktop {
    started: Instant,
}
impl Desktop for KScreenDesktop {
    fn query(&mut self) -> Result<Value, String> {
        query()
    }
    fn apply(&mut self, arguments: &[String]) -> Result<(), String> {
        run(arguments).map(drop)
    }
    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
    fn pause(&mut self, duration: Duration) {
        thread::sleep(duration);
    }
}

/// What can keep the desktop alive once the owned output is unplugged.
#[derive(Debug, PartialEq, Eq)]
enum RemovalGuard {
    /// A distinct active output is independently observed.
    Survivor,
    /// Only the owned output is observed; unplugging it leaves no desktop.
    OwnedOnly,
    /// Neither a distinct output nor the owned output is observed.
    Unobserved,
}

fn observe_removal_guard(output: &str) -> Result<(RemovalGuard, bool), String> {
    let configuration = query()?;
    let mut observer =
        super::observer::Observer::open().ok_or("cannot observe surviving Wayland output")?;
    classify_removal_guard(&configuration, output, |name| {
        observer
            .read(name)
            .is_some_and(|seen| seen.width > 0 && seen.height > 0 && seen.refresh > 0)
    })
}

/// Classifies the desktop from KScreen's output list and an independent
/// per-output observation. The flag is true while KScreen lists a distinct
/// enabled output that the observation has not confirmed yet.
fn classify_removal_guard(
    configuration: &Value,
    output: &str,
    mut observed: impl FnMut(&str) -> bool,
) -> Result<(RemovalGuard, bool), String> {
    let outputs = configuration["outputs"]
        .as_array()
        .ok_or("missing KScreen outputs")?;
    let active = |candidate: &Value| candidate["connected"] == true && candidate["enabled"] == true;
    let mut awaiting_survivor = false;
    for candidate in outputs.iter().filter(|candidate| active(candidate)) {
        let Some(name) = candidate["name"].as_str().filter(|name| *name != output) else {
            continue;
        };
        if observed(name) {
            return Ok((RemovalGuard::Survivor, false));
        }
        awaiting_survivor = true;
    }
    let owned_active = outputs
        .iter()
        .any(|candidate| candidate["name"] == output && active(candidate));
    let guard = if owned_active && observed(output) {
        RemovalGuard::OwnedOnly
    } else {
        RemovalGuard::Unobserved
    };
    Ok((guard, awaiting_survivor))
}

/// Printable, length-bounded text for a journal line.
fn bounded_detail(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .take(480)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn mode_only_arguments(output: &str, mode: &str) -> Result<Vec<String>, String> {
    if !valid_output_name(output)
        || mode.is_empty()
        || !mode.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("invalid mode-only target".into());
    }
    Ok(vec![format!("output.{output}.mode.{mode}")])
}

fn verify_mode_only_state(
    configuration: &Value,
    output: &str,
    desired: &Specification,
    baseline: &Value,
) -> Result<Value, String> {
    let expected_mode = baseline["desired_mode"]
        .as_str()
        .ok_or("missing requested mode")?;
    let state = preservation_state(configuration, output)?;
    if state != baseline["state"]
        || find_output(configuration, output).and_then(|value| value["currentModeId"].as_str())
            != Some(expected_mode)
        || mode(configuration, output, desired).as_deref() != Some(expected_mode)
    {
        return Err("mode-only preservation readback differs".into());
    }
    let readback = state
        .get(output)
        .cloned()
        .ok_or("owned output unavailable")?;
    let before_digest = state_digest(&baseline["state"])?;
    let after_digest = state_digest(&state)?;
    if before_digest != after_digest {
        return Err("preservation digest mismatch".into());
    }
    Ok(
        serde_json::json!({"readback": readback, "before_digest": before_digest, "after_digest": after_digest}),
    )
}

fn state_digest(state: &Value) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    fn canonical(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut sorted: Vec<_> = map.iter().collect();
                sorted.sort_by_key(|(key, _)| *key);
                Value::Object(
                    sorted
                        .into_iter()
                        .map(|(key, value)| (key.clone(), canonical(value)))
                        .collect(),
                )
            }
            Value::Array(values) => Value::Array(values.iter().map(canonical).collect()),
            _ => value.clone(),
        }
    }
    let encoded =
        serde_json::to_vec(&canonical(state)).map_err(|_| "invalid preservation state")?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

/// Strictly read every connected output. Missing facts cannot prove that a
/// mode operation preserved them; never substitute a default.
fn preservation_state(configuration: &Value, owned: &str) -> Result<Value, String> {
    let outputs = configuration["outputs"]
        .as_array()
        .filter(|outputs| outputs.len() <= 64)
        .ok_or("invalid output list")?;
    let mut states = std::collections::BTreeMap::new();
    for output in outputs {
        let name = output["name"]
            .as_str()
            .filter(|name| valid_output_name(name))
            .ok_or("invalid output name")?;
        let connected = output["connected"]
            .as_bool()
            .ok_or("missing connection state")?;
        if !connected {
            continue;
        }
        let native_id = output
            .get("id")
            .filter(|value| value.is_string() || value.is_u64())
            .ok_or("missing output identity")?;
        let enabled = output["enabled"].as_bool().ok_or("missing enabled state")?;
        let x = output["pos"]["x"].as_i64().ok_or("missing position")?;
        let y = output["pos"]["y"].as_i64().ok_or("missing position")?;
        let scale = output["scale"]
            .as_f64()
            .filter(|v| v.is_finite() && (0.25..=5.0).contains(v))
            .ok_or("missing scale")?;
        let rotation = output["rotation"]
            .as_u64()
            .filter(|v| matches!(v, 1 | 2 | 4 | 8 | 16 | 32 | 64 | 128))
            .ok_or("missing rotation")?;
        let priority = output["priority"]
            .as_u64()
            .filter(|v| *v <= 64)
            .ok_or("missing priority")?;
        let hdr = output["hdr"].as_bool().ok_or("missing HDR state")?;
        let wcg = output["wcg"].as_bool().ok_or("missing WCG state")?;
        let vrr = output
            .get("vrrPolicy")
            .filter(|value| value.is_string() || value.is_u64())
            .ok_or("missing VRR policy")?;
        let mode = output["currentModeId"]
            .as_str()
            .filter(|value| !value.is_empty())
            .ok_or("missing current mode")?;
        let state = serde_json::json!({"id":native_id,"enabled":enabled,"x":x,"y":y,"scale":scale,"rotation":rotation,"priority":priority,"hdr":hdr,"wcg":wcg,"vrr":vrr,"mode":if name == owned {None} else {Some(mode)}});
        if states.insert(name.to_owned(), state).is_some() {
            return Err("duplicate output name".into());
        }
    }
    if !states.contains_key(owned) {
        return Err("owned output unavailable".into());
    }
    serde_json::to_value(states).map_err(|_| "invalid preservation state".into())
}
/// Turns off every other enabled display except `keep`, makes the owned output
/// the primary display at the origin (or right of the kept outputs), and
/// confirms the result. The caller journaled the prior layout for restore.
fn configure_exclusive(
    configuration: &Value,
    output: &str,
    target: &OwnedTarget,
    keep: &[String],
    newly_connected: bool,
) -> Result<(), String> {
    let right = right_edge_of(configuration, |name| keep.iter().any(|kept| kept == name))?;
    // Kept peer outputs retain their order; this stream's output follows them.
    let priority = keep.len() + 1;
    let mut arguments = exclusive_disable_arguments(configuration, output, keep)?;
    arguments.extend(target.arguments(output, right, priority));
    configure_target(
        &mut KScreenDesktop {
            started: Instant::now(),
        },
        output,
        target,
        right,
        arguments,
        newly_connected,
        |confirmed| Ok(exclusive_disable_arguments(confirmed, output, keep)?.is_empty()),
    )
}

/// Commit the mode before enabling HDR. KWin can publish a stale HDR bit during
/// hotplug, or lose a color change submitted in the same transaction as a mode.
fn configure_target(
    desktop: &mut impl Desktop,
    output: &str,
    target: &OwnedTarget,
    x: i64,
    arguments: Vec<String>,
    newly_connected: bool,
    preserved: impl FnMut(&Value) -> Result<bool, String>,
) -> Result<(), String> {
    configure_target_at(
        desktop,
        output,
        target,
        PlacementPlan {
            position: (x, 0),
            fallback: None,
        },
        arguments,
        newly_connected,
        preserved,
    )
}

struct PlacementPlan {
    position: (i64, i64),
    fallback: Option<(i64, i64)>,
}

fn extended_configuration(
    configuration: &Value,
    output: &str,
    specification: &Specification,
    target: &OwnedTarget,
    layout: Option<&Layout>,
    placement: &Placement,
) -> Result<(PlacementPlan, Vec<String>), String> {
    let right = match layout {
        Some(layout) => layout.right_edge(),
        None => right_edge(configuration, output)?,
    };
    let mut arguments = match layout {
        Some(layout) if placement.primary() => {
            layout.arguments_with_primary(configuration, output)?
        }
        Some(layout) => layout.arguments(configuration)?,
        None if placement.primary() => primary_arguments(configuration, output)?,
        None => Vec::new(),
    };
    let priority = if placement.primary() {
        1
    } else {
        match layout {
            Some(layout) => layout.next_priority() as usize,
            None => live_next_priority(configuration, output)?,
        }
    };
    let mut plan = PlacementPlan {
        position: (right, 0),
        fallback: None,
    };
    if placement.isolated() {
        let bottom = match layout {
            Some(layout) => layout.bottom_edge(),
            None => bottom_edge(configuration, output)?,
        };
        let scale = target
            .scale
            .or_else(|| find_output(configuration, output)?["scale"].as_f64())
            .filter(|scale| scale.is_finite() && (0.25..=5.0).contains(scale))
            .ok_or("invalid owned display scale")?;
        let maximum = |axis| {
            configuration["screen"]["maxSize"][axis]
                .as_i64()
                .filter(|value| (1..=65536).contains(value))
                .unwrap_or(64000)
        };
        let max_width = maximum("width");
        let max_height = maximum("height");
        let x = right.max(max_width - (f64::from(specification.width) / scale) as i64);
        let y = bottom.max(max_height - (f64::from(specification.height) / scale) as i64);
        if (0..=65536).contains(&x) && (0..=65536).contains(&y) {
            plan.position = (x, y);
            plan.fallback = Some((right, 0));
        }
    }
    arguments.extend(target.arguments_at(output, plan.position, priority));
    Ok((plan, arguments))
}

fn primary_arguments(configuration: &Value, owned: &str) -> Result<Vec<String>, String> {
    let outputs = configuration["outputs"]
        .as_array()
        .filter(|outputs| outputs.len() <= 64)
        .ok_or("invalid KScreen output list")?;
    let mut peers = outputs
        .iter()
        .filter(|output| {
            output["name"] != owned && output["connected"] == true && output["enabled"] == true
        })
        .collect::<Vec<_>>();
    peers.sort_by_key(|output| output["priority"].as_u64().unwrap_or(0));
    peers
        .iter()
        .enumerate()
        .map(|(index, output)| {
            let name = output["name"]
                .as_str()
                .filter(|name| valid_output_name(name))
                .ok_or("invalid KScreen output name")?;
            Ok(format!("output.{name}.priority.{}", index + 2))
        })
        .collect()
}

fn configure_target_at(
    desktop: &mut impl Desktop,
    output: &str,
    target: &OwnedTarget,
    mut plan: PlacementPlan,
    mut arguments: Vec<String>,
    newly_connected: bool,
    mut preserved: impl FnMut(&Value) -> Result<bool, String>,
) -> Result<(), String> {
    let rearm_hdr = target.hdr && newly_connected;
    if target.hdr {
        let prefix = format!("output.{output}.");
        arguments.retain(|argument| {
            !argument.starts_with(&format!("{prefix}hdr."))
                && !argument.starts_with(&format!("{prefix}wcg."))
        });
        if rearm_hdr {
            arguments.push(format!("{prefix}hdr.disable"));
            arguments.push(format!("{prefix}wcg.disable"));
        }
    }
    if let Err(error) = desktop.apply(&arguments) {
        let Some(fallback) = plan.fallback else {
            return Err(error);
        };
        // Retry the rejected isolated placement while preserving every other
        // setting and the durable pre-effect desktop baseline.
        let prefix = format!("output.{output}.position.");
        arguments.retain(|argument| !argument.starts_with(&prefix));
        arguments.push(format!("{prefix}{},{}", fallback.0, fallback.1));
        desktop.apply(&arguments)?;
        plan.position = fallback;
    }
    if target.hdr {
        wait_for_configuration(desktop, 1, |configuration| {
            Ok(find_output(configuration, output).is_some_and(|actual| {
                target.base_matches_at(actual, plan.position)
                    && (!rearm_hdr || actual["hdr"] == false)
            }) && preserved(configuration)?)
        })?;
        desktop.apply(&[
            format!("output.{output}.hdr.enable"),
            format!("output.{output}.wcg.enable"),
        ])?;
    }
    wait_for_configuration(desktop, if rearm_hdr { 3 } else { 1 }, |configuration| {
        Ok(find_output(configuration, output)
            .is_some_and(|actual| target.matches_at(actual, plan.position))
            && preserved(configuration)?)
    })
}

fn wait_for_configuration(
    desktop: &mut impl Desktop,
    required_observations: u32,
    mut matches: impl FnMut(&Value) -> Result<bool, String>,
) -> Result<(), String> {
    let deadline = desktop.elapsed() + Duration::from_secs(5);
    let mut observations = 0;
    loop {
        let matching = desktop
            .query()
            .is_ok_and(|configuration| matches(&configuration).unwrap_or(false));
        observations = if matching { observations + 1 } else { 0 };
        if observations >= required_observations {
            return Ok(());
        }
        if desktop.elapsed() >= deadline {
            return Err("owned display configuration did not settle before its deadline".into());
        }
        desktop.pause(READBACK_INTERVAL);
    }
}
/// `disable` for every connected, enabled display that is neither the owned
/// output nor kept. Empty once the desktop is exclusive.
fn exclusive_disable_arguments(
    configuration: &Value,
    owned: &str,
    keep: &[String],
) -> Result<Vec<String>, String> {
    let outputs = configuration["outputs"]
        .as_array()
        .filter(|outputs| outputs.len() <= 64)
        .ok_or("invalid KScreen output list")?;
    let mut arguments = Vec::new();
    for other in outputs {
        let name = other["name"].as_str().ok_or("missing output name")?;
        let kept = name == owned || keep.iter().any(|kept| kept == name);
        if !kept && other["connected"] == true && other["enabled"] == true {
            if !valid_output_name(name) {
                return Err("unsupported KScreen output name".into());
            }
            arguments.push(format!("output.{name}.disable"));
        }
    }
    Ok(arguments)
}
fn valid_output_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}
/// The requested mode and scale of the owned output. `scale` is `None` when
/// the compositor's own or remembered choice is kept.
struct OwnedTarget {
    mode: String,
    scale: Option<f64>,
    hdr: bool,
}
impl OwnedTarget {
    /// Like Vibeshine, ask for adaptive sync so the compositor presents at the
    /// content's rate. It is not read back: a compositor without VRR support
    /// for this output keeps streaming at the fixed mode rate.
    fn arguments(&self, output: &str, x: i64, priority: usize) -> Vec<String> {
        self.arguments_at(output, (x, 0), priority)
    }
    fn arguments_at(&self, output: &str, position: (i64, i64), priority: usize) -> Vec<String> {
        let mut arguments = vec![
            format!("output.{output}.enable"),
            format!("output.{output}.mode.{}", self.mode),
            format!("output.{output}.rotation.none"),
            format!("output.{output}.position.{},{}", position.0, position.1),
            format!(
                "output.{output}.hdr.{}",
                if self.hdr { "enable" } else { "disable" }
            ),
            format!(
                "output.{output}.wcg.{}",
                if self.hdr { "enable" } else { "disable" }
            ),
            format!("output.{output}.vrrpolicy.always"),
            format!("output.{output}.priority.{priority}"),
        ];
        if let Some(scale) = self.scale {
            arguments.insert(2, format!("output.{output}.scale.{scale}"));
        }
        arguments
    }
    #[cfg(test)]
    fn matches(&self, actual: &Value, x: i64) -> bool {
        self.matches_at(actual, (x, 0))
    }
    fn matches_at(&self, actual: &Value, position: (i64, i64)) -> bool {
        self.base_matches_at(actual, position)
            && actual["hdr"] == self.hdr
            && actual["wcg"] == self.hdr
    }
    fn base_matches_at(&self, actual: &Value, position: (i64, i64)) -> bool {
        let scale_matches = match self.scale {
            Some(scale) => actual["scale"]
                .as_f64()
                .is_some_and(|actual| (actual - scale).abs() < 0.001),
            None => actual["scale"].as_f64().is_some(),
        };
        actual["enabled"] == true
            && scale_matches
            && actual["pos"]["x"].as_i64() == Some(position.0)
            && actual["pos"]["y"].as_i64() == Some(position.1)
            && actual["currentModeId"].as_str() == Some(&self.mode)
            && actual["rotation"] == 1
    }
}
fn query() -> Result<Value, String> {
    serde_json::from_slice(&run(&["-j".into()])?).map_err(|_| "invalid KScreen JSON".into())
}
fn find_output<'a>(configuration: &'a Value, name: &str) -> Option<&'a Value> {
    configuration["outputs"]
        .as_array()?
        .iter()
        .find(|output| output["name"] == name && output["connected"] == true)
}

fn verify_retained_readback(
    configuration: &Value,
    name: &str,
    desired: &Specification,
) -> Result<(), String> {
    let outputs = configuration["outputs"]
        .as_array()
        .filter(|outputs| outputs.len() <= 64)
        .ok_or("invalid output inventory")?;
    let mut matches = outputs
        .iter()
        .filter(|output| output["name"] == name && output["connected"] == true);
    let output = matches.next().ok_or("owned output unavailable")?;
    if matches.next().is_some()
        || output["enabled"] != true
        || output["hdr"].as_bool() != Some(desired.hdr_enabled)
    {
        return Err("owned output activation or color differs".into());
    }
    let mode_id = output["currentModeId"]
        .as_str()
        .ok_or("current mode unavailable")?;
    let modes = output["modes"]
        .as_array()
        .ok_or("mode inventory unavailable")?;
    let mut modes = modes.iter().filter(|mode| mode["id"] == mode_id);
    let mode = modes.next().ok_or("current mode missing")?;
    if modes.next().is_some()
        || mode["size"]["width"].as_u64() != Some(u64::from(desired.width))
        || mode["size"]["height"].as_u64() != Some(u64::from(desired.height))
        || !mode["refreshRate"]
            .as_f64()
            .is_some_and(|rate| refresh_matches(rate, desired.refresh_millihz))
    {
        return Err("owned output mode differs".into());
    }
    Ok(())
}
fn mode(configuration: &Value, output: &str, spec: &Specification) -> Option<String> {
    find_output(configuration, output)?["modes"]
        .as_array()?
        .iter()
        .filter_map(|mode| {
            let id = mode["id"]
                .as_str()
                .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))?;
            let rate = mode["refreshRate"].as_f64()?;
            (mode["size"]["width"].as_u64() == Some(u64::from(spec.width))
                && mode["size"]["height"].as_u64() == Some(u64::from(spec.height))
                && refresh_matches(rate, spec.refresh_millihz))
            .then_some((id, rate))
        })
        .min_by(|left, right| {
            let requested = f64::from(spec.refresh_millihz) / 1000.0;
            (left.1 - requested)
                .abs()
                .total_cmp(&(right.1 - requested).abs())
        })
        .map(|(id, _)| id.to_owned())
}
fn refresh_matches(rate: f64, requested_millihz: u32) -> bool {
    rate.is_finite() && (rate - f64::from(requested_millihz) / 1000.0).abs() < 0.2
}
fn live_next_priority(configuration: &Value, owned: &str) -> Result<usize, String> {
    let outputs = configuration["outputs"]
        .as_array()
        .ok_or("missing current outputs")?;
    if outputs.len() > 64 {
        return Err("too many outputs".into());
    }
    Ok(outputs
        .iter()
        .filter(|output| {
            output["name"] != owned && output["connected"] == true && output["enabled"] == true
        })
        .count()
        + 1)
}

fn right_edge(configuration: &Value, owned: &str) -> Result<i64, String> {
    right_edge_of(configuration, |name| name != owned)
}
/// Right edge, in logical pixels, of the connected and enabled outputs that
/// `included` selects; zero when it selects none.
fn right_edge_of(configuration: &Value, included: impl Fn(&str) -> bool) -> Result<i64, String> {
    desktop_edge_of(configuration, included, true)
}
fn bottom_edge(configuration: &Value, owned: &str) -> Result<i64, String> {
    desktop_edge_of(configuration, |name| name != owned, false)
}
fn desktop_edge_of(
    configuration: &Value,
    included: impl Fn(&str) -> bool,
    horizontal: bool,
) -> Result<i64, String> {
    let outputs = configuration["outputs"]
        .as_array()
        .filter(|outputs| outputs.len() <= 64)
        .ok_or("invalid KScreen output list")?;
    let mut right = 0i64;
    for output in outputs.iter().filter(|output| {
        output["name"].as_str().is_some_and(&included)
            && output["connected"] == true
            && output["enabled"] == true
    }) {
        let position = output["pos"][if horizontal { "x" } else { "y" }]
            .as_i64()
            .filter(|position| position.unsigned_abs() <= 65536)
            .ok_or("invalid KScreen position")?;
        let scale = output["scale"]
            .as_f64()
            .filter(|v| v.is_finite() && (0.25..=5.0).contains(v))
            .ok_or("invalid KScreen scale")?;
        let rotated = matches!(output["rotation"].as_u64(), Some(2 | 8 | 32 | 128));
        let size = if horizontal == rotated {
            &output["size"]["height"]
        } else {
            &output["size"]["width"]
        };
        let width = size
            .as_u64()
            .filter(|v| *v <= 8192)
            .ok_or("invalid KScreen width")?;
        right = right.max(position + (width as f64 / scale).ceil() as i64);
    }
    if !(0..=65536).contains(&right) {
        return Err("desktop bounds exceed supported coordinates".into());
    }
    Ok(right)
}
fn run(arguments: &[String]) -> Result<Vec<u8>, String> {
    let child = Command::new("kscreen-doctor")
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| e.to_string())?;
    collect_output(child, Duration::from_secs(5))
}

/// A failed pipe setup, read, wait or deadline must not orphan the command.
struct CommandOwner {
    child: Child,
    process_group: libc::pid_t,
    status: Option<ExitStatus>,
    output_complete: bool,
}

impl Drop for CommandOwner {
    fn drop(&mut self) {
        if !self.output_complete {
            // The direct child can exit while a helper still owns one of its
            // pipes. Retire that fresh process group on every incomplete path.
            // SAFETY: process_group was created specifically for this child.
            unsafe { libc::kill(-self.process_group, libc::SIGKILL) };
        }
        if self.status.is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn nonblocking(pipe: &(impl AsRawFd + ?Sized)) -> io::Result<()> {
    // SAFETY: this live owned pipe descriptor is valid for both fcntl calls.
    let flags = unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: F_SETFL accepts the existing status flags plus O_NONBLOCK.
    if unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[derive(PartialEq, Eq)]
enum PipeProgress {
    Closed,
    Pending,
    More,
}

fn read_batch(
    reader: &mut impl Read,
    saved: &mut Vec<u8>,
    limit: usize,
) -> io::Result<PipeProgress> {
    let mut buffer = [0; 8192];
    // Bound each pass to preserve deadline and stderr fairness even if a
    // descendant writes continuously.
    for _ in 0..8 {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(PipeProgress::Closed),
            Ok(count) => {
                let retained = count.min((limit + 1).saturating_sub(saved.len()));
                saved.extend_from_slice(&buffer[..retained]);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return Ok(PipeProgress::Pending);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(PipeProgress::More)
}

fn collect_output(child: Child, timeout: Duration) -> Result<Vec<u8>, String> {
    const MAX_STDOUT: usize = 1024 * 1024;
    const MAX_STDERR: usize = 65536;
    let process_group = child.id() as libc::pid_t;
    let mut owner = CommandOwner {
        child,
        process_group,
        status: None,
        output_complete: false,
    };
    let mut stdout = owner
        .child
        .stdout
        .take()
        .ok_or("KScreen stdout unavailable")?;
    let mut stderr = owner
        .child
        .stderr
        .take()
        .ok_or("KScreen stderr unavailable")?;
    nonblocking(&stdout).map_err(|error| error.to_string())?;
    nonblocking(&stderr).map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    let mut errors = Vec::new();
    let deadline = Instant::now() + timeout;
    loop {
        // Observe exit before reading so EOF proves all command output was read.
        if owner.status.is_none() {
            owner.status = owner.child.try_wait().map_err(|error| error.to_string())?;
        }
        let output_state =
            read_batch(&mut stdout, &mut bytes, MAX_STDOUT).map_err(|error| error.to_string())?;
        let error_state =
            read_batch(&mut stderr, &mut errors, MAX_STDERR).map_err(|error| error.to_string())?;
        if let Some(status) = owner.status {
            if output_state == PipeProgress::Closed && error_state == PipeProgress::Closed {
                owner.output_complete = true;
                let reported_failure =
                    doctor_reported_failure(&bytes) || doctor_reported_failure(&errors);
                if status.success()
                    && bytes.len() <= MAX_STDOUT
                    && errors.len() <= MAX_STDERR
                    && !reported_failure
                {
                    return Ok(bytes);
                }
                // The command's own explanation otherwise never reaches the journal.
                let detail = bounded_detail(&format!(
                    "{} {}",
                    String::from_utf8_lossy(&errors),
                    String::from_utf8_lossy(&bytes)
                ));
                return Err(if detail.is_empty() {
                    "KScreen command failed or exceeded its bound".into()
                } else {
                    format!("KScreen command failed or exceeded its bound: {detail}")
                });
            }
            if output_state == PipeProgress::Pending || error_state == PipeProgress::Pending {
                return Err("KScreen output pipe remained open after command exit".into());
            }
        }
        if Instant::now() >= deadline {
            return Err("KScreen command exceeded its deadline".into());
        }
        if output_state != PipeProgress::More && error_state != PipeProgress::More {
            thread::sleep(Duration::from_millis(10));
        }
    }
}
fn doctor_reported_failure(output: &[u8]) -> bool {
    let output = String::from_utf8_lossy(output);
    [
        "Unable to parse arguments:",
        "applying config failed!",
        "Invalid config.",
    ]
    .iter()
    .any(|marker| output.contains(marker))
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_command(script: &str) -> Child {
        Command::new("/bin/sh")
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap()
    }

    #[test]
    fn command_does_not_accept_unfinished_descendant_output() {
        let child = test_command("(sleep 2; printf 'failed' >&2) & printf good");
        let start = Instant::now();
        let result = collect_output(child, Duration::from_secs(1));
        assert!(result.unwrap_err().contains("pipe remained open"));
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn command_kills_descendants_that_keep_output_pipes_open() {
        let marker = std::env::temp_dir().join(format!(
            "amaterasu-virtual-display-descendant-{}-{}.tmp",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let quoted_marker = format!("'{}'", marker.to_string_lossy().replace('\'', "'\\''"));
        let script = format!("(sleep 0.15; printf leaked > {quoted_marker}) & printf good");
        let result = collect_output(test_command(&script), Duration::from_secs(1));
        assert!(result.unwrap_err().contains("pipe remained open"));
        thread::sleep(Duration::from_millis(300));
        let descendant_ran = marker.exists();
        let _ = std::fs::remove_file(marker);
        assert!(!descendant_ran, "a descendant outlived command collection");
    }

    #[test]
    fn command_deadline_does_not_wait_for_inherited_pipes() {
        let child = test_command("sleep 2 & exec sleep 5");
        let start = Instant::now();
        let result = collect_output(child, Duration::from_millis(50));
        assert!(result.unwrap_err().contains("deadline"));
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn command_drains_large_stderr_without_blocking_the_child() {
        let child = test_command("head -c 70000 /dev/zero >&2; printf done");
        let result = collect_output(child, Duration::from_secs(2));
        assert!(result.unwrap_err().contains("exceeded its bound"));
    }

    #[test]
    fn failed_command_reports_its_own_explanation() {
        let child = test_command("printf 'Applying config failed\\n' >&2; exit 1");
        let error = collect_output(child, Duration::from_secs(2)).unwrap_err();
        assert!(error.ends_with(": Applying config failed"), "{error}");
    }

    #[test]
    fn command_rejects_kscreen_failure_messages_even_with_successful_exit() {
        for script in [
            "printf 'Unable to parse arguments: output.Virtual-1.mode.bad'",
            "printf 'applying config failed!' >&2",
            "printf 'Invalid config.'",
        ] {
            assert!(collect_output(test_command(script), Duration::from_secs(2)).is_err());
        }
        assert_eq!(
            collect_output(test_command("printf applied"), Duration::from_secs(2)).unwrap(),
            b"applied"
        );
    }

    struct SequenceDesktop {
        observations: std::collections::VecDeque<Value>,
        fallback: Value,
        applications: Vec<Vec<String>>,
        queries: usize,
        elapsed: Duration,
        reject_applications: usize,
    }
    impl SequenceDesktop {
        fn new(observations: Vec<Value>) -> Self {
            Self {
                fallback: observations.last().unwrap().clone(),
                observations: observations.into(),
                applications: Vec::new(),
                queries: 0,
                elapsed: Duration::ZERO,
                reject_applications: 0,
            }
        }
    }
    impl Desktop for SequenceDesktop {
        fn query(&mut self) -> Result<Value, String> {
            self.queries += 1;
            Ok(self
                .observations
                .pop_front()
                .unwrap_or_else(|| self.fallback.clone()))
        }
        fn apply(&mut self, arguments: &[String]) -> Result<(), String> {
            self.applications.push(arguments.to_vec());
            if self.reject_applications > 0 {
                self.reject_applications -= 1;
                Err("compositor rejected placement".into())
            } else {
                Ok(())
            }
        }
        fn elapsed(&self) -> Duration {
            self.elapsed
        }
        fn pause(&mut self, duration: Duration) {
            self.elapsed += duration;
        }
    }
    fn owned_state(mode: &str, hdr: bool) -> Value {
        json!({"outputs":[{"name":"Virtual-2","connected":true,"enabled":true,"scale":1.5,"pos":{"x":0,"y":0},"currentModeId":mode,"rotation":1,"hdr":hdr,"wcg":hdr}]})
    }
    #[test]
    fn extended_layouts_resolve_primary_and_isolated_positions_without_disabling_peers() {
        let configuration = json!({"screen":{"maxSize":{"width":64000,"height":64000}},"outputs":[
            {"name":"HDMI-A-1","connected":true,"enabled":true,"size":{"width":1920,"height":1080},"pos":{"x":0,"y":0},"rotation":1,"scale":1.0,"priority":1,"currentModeId":"1","modes":[{"id":"1","size":{"width":1920,"height":1080},"refreshRate":60.0}]},
            {"name":"Virtual-2","connected":true,"enabled":true,"scale":1.5,"pos":{"x":0,"y":0},"currentModeId":"7","rotation":1,"hdr":false,"wcg":false,"priority":2}
        ]});
        let layout = Layout::capture(&configuration, "Virtual-2").unwrap();
        let specification = Specification {
            identity: "11111111-1111-4111-8111-111111111111".into(),
            name: "Display".into(),
            width: 1280,
            height: 720,
            refresh_millihz: 60000,
            hdr_enabled: false,
        };
        let target = OwnedTarget {
            mode: "7".into(),
            scale: Some(1.5),
            hdr: false,
        };
        for (placement, expected_position, expected_priority) in [
            (Placement::Extended, (1920, 0), 2),
            (Placement::ExtendedPrimary, (1920, 0), 1),
            (Placement::ExtendedIsolated, (63147, 63520), 2),
            (Placement::ExtendedPrimaryIsolated, (63147, 63520), 1),
        ] {
            let (plan, arguments) = extended_configuration(
                &configuration,
                "Virtual-2",
                &specification,
                &target,
                Some(&layout),
                &placement,
            )
            .unwrap();
            assert_eq!(plan.position, expected_position);
            assert_eq!(plan.fallback, placement.isolated().then_some((1920, 0)));
            assert!(arguments.contains(&format!("output.Virtual-2.priority.{expected_priority}")));
            assert!(arguments.contains(&format!(
                "output.Virtual-2.position.{},{}",
                expected_position.0, expected_position.1
            )));
            assert!(!arguments.contains(&"output.HDMI-A-1.disable".into()));
            if placement.primary() {
                assert!(arguments.contains(&"output.HDMI-A-1.priority.2".into()));
            }
        }
    }
    #[test]
    fn rejected_isolated_placement_retries_adjacent_and_verifies_that_position() {
        let target = OwnedTarget {
            mode: "7".into(),
            scale: Some(1.5),
            hdr: false,
        };
        let mut desktop = SequenceDesktop::new(vec![owned_state("7", false)]);
        desktop.reject_applications = 1;
        configure_target_at(
            &mut desktop,
            "Virtual-2",
            &target,
            PlacementPlan {
                position: (63000, 63000),
                fallback: Some((0, 0)),
            },
            target.arguments_at("Virtual-2", (63000, 63000), 1),
            true,
            |_| Ok(true),
        )
        .unwrap();
        assert_eq!(desktop.applications.len(), 2);
        assert!(desktop.applications[0].contains(&"output.Virtual-2.position.63000,63000".into()));
        assert!(desktop.applications[1].contains(&"output.Virtual-2.position.0,0".into()));
        assert!(!desktop.applications[1].contains(&"output.Virtual-2.position.63000,63000".into()));
        assert_eq!(desktop.queries, 1);
        let mut desktop = SequenceDesktop::new(vec![owned_state("7", false)]);
        desktop.reject_applications = 1;
        assert!(
            configure_target(
                &mut desktop,
                "Virtual-2",
                &target,
                0,
                target.arguments("Virtual-2", 0, 1),
                true,
                |_| Ok(true)
            )
            .is_err()
        );
        assert_eq!(desktop.applications.len(), 1);
        assert_eq!(desktop.queries, 0);
    }
    #[test]
    fn hdr_activation_follows_settled_mode_and_rearm_then_stable_color_readbacks() {
        let target = OwnedTarget {
            mode: "7".into(),
            scale: Some(1.5),
            hdr: true,
        };
        let mut desktop = SequenceDesktop::new(vec![
            owned_state("1", true),
            owned_state("7", false),
            owned_state("7", true),
            owned_state("7", false),
            owned_state("7", true),
            owned_state("7", true),
            owned_state("7", true),
        ]);
        configure_target(
            &mut desktop,
            "Virtual-2",
            &target,
            0,
            target.arguments("Virtual-2", 0, 1),
            true,
            |_| Ok(true),
        )
        .unwrap();
        assert_eq!(desktop.applications.len(), 2);
        assert!(desktop.applications[0].contains(&"output.Virtual-2.hdr.disable".into()));
        assert!(!desktop.applications[0].contains(&"output.Virtual-2.hdr.enable".into()));
        assert_eq!(
            desktop.applications[1],
            ["output.Virtual-2.hdr.enable", "output.Virtual-2.wcg.enable"]
        );
        assert_eq!(desktop.queries, 7);
        assert_eq!(desktop.elapsed, READBACK_INTERVAL * 5);
    }
    #[test]
    fn hdr_is_not_enabled_before_the_mode_and_desktop_settle() {
        let target = OwnedTarget {
            mode: "7".into(),
            scale: Some(1.5),
            hdr: true,
        };
        let mut desktop = SequenceDesktop::new(vec![owned_state("7", false)]);
        let result = configure_target(
            &mut desktop,
            "Virtual-2",
            &target,
            0,
            target.arguments("Virtual-2", 0, 1),
            true,
            |_| Ok(false),
        );
        assert!(result.unwrap_err().contains("deadline"));
        assert_eq!(desktop.applications.len(), 1);
        assert_eq!(desktop.elapsed, Duration::from_secs(5));
    }
    #[test]
    fn retained_hdr_does_not_rearm_and_sdr_waits_for_fresh_readback() {
        let target = OwnedTarget {
            mode: "7".into(),
            scale: Some(1.5),
            hdr: true,
        };
        let mut desktop = SequenceDesktop::new(vec![owned_state("7", true)]);
        configure_target(
            &mut desktop,
            "Virtual-2",
            &target,
            0,
            target.arguments("Virtual-2", 0, 1),
            false,
            |_| Ok(true),
        )
        .unwrap();
        assert!(
            desktop.applications[0]
                .iter()
                .all(|argument| !argument.contains(".hdr.") && !argument.contains(".wcg."))
        );
        assert_eq!(desktop.queries, 2);
        let target = OwnedTarget {
            hdr: false,
            ..target
        };
        let mut desktop =
            SequenceDesktop::new(vec![owned_state("1", true), owned_state("7", false)]);
        configure_target(
            &mut desktop,
            "Virtual-2",
            &target,
            0,
            target.arguments("Virtual-2", 0, 1),
            true,
            |_| Ok(true),
        )
        .unwrap();
        assert_eq!(desktop.applications.len(), 1);
        assert_eq!(desktop.queries, 2);
        assert_eq!(desktop.elapsed, READBACK_INTERVAL);
    }

    #[test]
    fn removal_guard_needs_an_independently_observed_distinct_output() {
        let desktop = json!({"outputs":[
            {"name":"HDMI-A-1","connected":true,"enabled":true},
            {"name":"DP-1","connected":true,"enabled":false},
            {"name":"Virtual-9","connected":true,"enabled":true}]});
        // The restored monitor is published: the stream output can go.
        assert_eq!(
            classify_removal_guard(&desktop, "Virtual-9", |_| true).unwrap(),
            (RemovalGuard::Survivor, false)
        );
        // KScreen lists the monitor before Wayland clients can see it. That is
        // a reason to look again, not proof that nothing survives.
        assert_eq!(
            classify_removal_guard(&desktop, "Virtual-9", |name| name == "Virtual-9").unwrap(),
            (RemovalGuard::OwnedOnly, true)
        );
        assert_eq!(
            classify_removal_guard(&desktop, "Virtual-9", |_| false).unwrap(),
            (RemovalGuard::Unobserved, true)
        );
        // A monitor that is off cannot guard and is not worth waiting for.
        let alone = json!({"outputs":[
            {"name":"HDMI-A-1","connected":true,"enabled":false},
            {"name":"Virtual-9","connected":true,"enabled":true}]});
        assert_eq!(
            classify_removal_guard(&alone, "Virtual-9", |_| true).unwrap(),
            (RemovalGuard::OwnedOnly, false)
        );
        assert!(classify_removal_guard(&json!({}), "Virtual-9", |_| true).is_err());
    }

    #[test]
    fn preservation_digest_is_stable_and_covers_nested_changes() {
        let before = json!({"z":{"b":2,"a":1},"a":[true,false]});
        let reordered = json!({"a":[true,false],"z":{"a":1,"b":2}});
        assert_eq!(
            state_digest(&before).unwrap(),
            state_digest(&reordered).unwrap()
        );
        let changed = json!({"a":[true,false],"z":{"a":1,"b":3}});
        assert_ne!(
            state_digest(&before).unwrap(),
            state_digest(&changed).unwrap()
        );
    }
    #[test]
    fn mode_only_status_rejects_fresh_desktop_or_mode_drift() {
        let mut configuration = json!({"outputs":[{"id":1,"name":"Virtual-1","connected":true,"enabled":true,"pos":{"x":0,"y":0},"scale":1.5,"rotation":1,"priority":1,"hdr":false,"wcg":false,"vrrPolicy":0,"currentModeId":"2","modes":[{"id":"2","size":{"width":1280,"height":720},"refreshRate":60.0}]}]});
        let desired = Specification {
            identity: "11111111-1111-4111-8111-111111111111".into(),
            name: "Amaterasu".into(),
            width: 1280,
            height: 720,
            refresh_millihz: 60000,
            hdr_enabled: false,
        };
        let baseline = json!({"state":preservation_state(&configuration, "Virtual-1").unwrap(),"desired_mode":"2"});
        let current =
            verify_mode_only_state(&configuration, "Virtual-1", &desired, &baseline).unwrap();
        assert_eq!(current["readback"]["scale"], json!(1.5));
        assert_eq!(current["before_digest"], current["after_digest"]);
        assert_eq!(current["before_digest"].as_str().unwrap().len(), 64);
        configuration["outputs"][0]["scale"] = json!(2.0);
        assert!(verify_mode_only_state(&configuration, "Virtual-1", &desired, &baseline).is_err());
        configuration["outputs"][0]["scale"] = json!(1.5);
        configuration["outputs"][0]["currentModeId"] = json!("3");
        assert!(verify_mode_only_state(&configuration, "Virtual-1", &desired, &baseline).is_err());
    }
    #[test]
    fn mode_only_verifies_all_preserved_axes_and_emits_only_mode_command() {
        let output = json!({"id":1,"name":"Virtual-1","connected":true,"enabled":true,"pos":{"x":0,"y":0},"scale":1.5,"rotation":1,"priority":1,"hdr":false,"wcg":false,"vrrPolicy":0,"currentModeId":"1"});
        let mut peer = output.clone();
        peer["name"] = json!("HDMI-A-1");
        peer["id"] = json!(2);
        peer["priority"] = json!(2);
        let before = json!({"outputs":[output,peer]});
        let saved = preservation_state(&before, "Virtual-1").unwrap();
        let mut after = before.clone();
        after["outputs"][0]["currentModeId"] = json!("2");
        assert_eq!(preservation_state(&after, "Virtual-1").unwrap(), saved);
        for field in [
            "scale",
            "rotation",
            "priority",
            "hdr",
            "wcg",
            "vrrPolicy",
            "currentModeId",
        ] {
            let mut changed = after.clone();
            changed["outputs"][1][field] = json!(null);
            assert!(preservation_state(&changed, "Virtual-1").is_err());
        }
        after["outputs"][1]["pos"]["x"] = json!(1);
        assert_ne!(preservation_state(&after, "Virtual-1").unwrap(), saved);
        assert_eq!(
            mode_only_arguments("Virtual-1", "2").unwrap(),
            ["output.Virtual-1.mode.2"]
        );
        assert!(mode_only_arguments("Virtual-1", "2.inject").is_err());
    }
    #[test]
    fn new_display_is_placed_after_scaled_existing_outputs_without_moving_them() {
        let configuration = json!({"outputs":[{"name":"Virtual-1","connected":true,"enabled":true,"pos":{"x":0},"size":{"width":3840,"height":2160},"scale":2.5,"rotation":1},
            {"name":"Virtual-2","connected":true,"enabled":true,"pos":{"x":0},"size":{"width":1920,"height":1080},"scale":1,"rotation":1}]});
        assert_eq!(right_edge(&configuration, "Virtual-2").unwrap(), 1536);
        assert_eq!(configuration["outputs"][0]["pos"]["x"], 0);
        assert_eq!(live_next_priority(&configuration, "Virtual-2").unwrap(), 2);
        let no_survivor = json!({"outputs":[{"name":"Virtual-2","connected":true,"enabled":true}]});
        assert_eq!(live_next_priority(&no_survivor, "Virtual-2").unwrap(), 1);
    }
    #[test]
    fn placement_uses_the_full_logical_width_of_flipped_rotated_outputs() {
        let configuration = json!({"outputs":[{"name":"HDMI-A-1","connected":true,"enabled":true,"pos":{"x":0},"size":{"width":1920,"height":1081},"scale":1.5,"rotation":32}]});
        assert_eq!(right_edge(&configuration, "Virtual-2").unwrap(), 721);
        let mut reversed = configuration;
        reversed["outputs"][0]["rotation"] = json!(128);
        assert_eq!(right_edge(&reversed, "Virtual-2").unwrap(), 721);
    }
    #[test]
    fn exclusive_turns_off_only_other_enabled_displays_amaterasu_does_not_own() {
        let configuration = json!({"outputs":[
            {"name":"HDMI-A-1","connected":true,"enabled":true},
            {"name":"DP-1","connected":true,"enabled":false},
            {"name":"DP-2","connected":false,"enabled":true},
            {"name":"Virtual-2","connected":true,"enabled":true},
            {"name":"Virtual-3","connected":true,"enabled":true}]});
        assert_eq!(
            exclusive_disable_arguments(&configuration, "Virtual-3", &["Virtual-2".into()])
                .unwrap(),
            ["output.HDMI-A-1.disable"]
        );
        assert_eq!(
            exclusive_disable_arguments(&configuration, "Virtual-3", &[]).unwrap(),
            ["output.HDMI-A-1.disable", "output.Virtual-2.disable"]
        );
        let exclusive = json!({"outputs":[
            {"name":"HDMI-A-1","connected":true,"enabled":false},
            {"name":"Virtual-3","connected":true,"enabled":true}]});
        assert!(
            exclusive_disable_arguments(&exclusive, "Virtual-3", &[])
                .unwrap()
                .is_empty()
        );
        let hostile = json!({"outputs":[{"name":"HDMI.mode.1","connected":true,"enabled":true}]});
        assert!(exclusive_disable_arguments(&hostile, "Virtual-3", &[]).is_err());
    }
    #[test]
    fn exclusive_output_sits_right_of_kept_peers_only() {
        let configuration = json!({"outputs":[
            {"name":"HDMI-A-1","connected":true,"enabled":true,"pos":{"x":0},"size":{"width":1920,"height":1080},"scale":1,"rotation":1},
            {"name":"Virtual-2","connected":true,"enabled":true,"pos":{"x":1920},"size":{"width":2560,"height":1440},"scale":1,"rotation":1}]});
        let kept = |name: &str| name == "Virtual-2";
        assert_eq!(right_edge_of(&configuration, kept).unwrap(), 4480);
        assert_eq!(right_edge_of(&configuration, |_| false).unwrap(), 0);
        let automatic = OwnedTarget {
            hdr: false,
            mode: "7".into(),
            scale: Some(2.5),
        };
        let arguments = automatic.arguments("Virtual-3", 0, 1);
        assert!(arguments.contains(&"output.Virtual-3.position.0,0".into()));
        assert!(arguments.contains(&"output.Virtual-3.priority.1".into()));
        assert!(arguments.contains(&"output.Virtual-3.scale.2.5".into()));
        assert!(arguments.contains(&"output.Virtual-3.vrrpolicy.always".into()));
        let retained = OwnedTarget {
            hdr: false,
            mode: "7".into(),
            scale: None,
        };
        assert!(
            !retained
                .arguments("Virtual-3", 0, 1)
                .iter()
                .any(|argument| argument.contains(".scale."))
        );
        let actual = json!({"enabled":true,"scale":1.75,"pos":{"x":0,"y":0},"currentModeId":"7","rotation":1,"hdr":false,"wcg":false});
        assert!(retained.matches(&actual, 0));
        assert!(!automatic.matches(&actual, 0));
    }
}

#[cfg(test)]
mod retained_mode_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn retained_readback_checks_connected_active_mode_and_color() {
        let configuration = json!({"outputs":[{"name":"Virtual-2","connected":true,"enabled":true,"hdr":true,"currentModeId":"7","modes":[{"id":"7","size":{"width":1280,"height":720},"refreshRate":119.88}]}]});
        let desired = Specification {
            identity: "11111111-1111-4111-8111-111111111111".into(),
            name: "Monitor".into(),
            width: 1280,
            height: 720,
            refresh_millihz: 119880,
            hdr_enabled: true,
        };
        assert!(verify_retained_readback(&configuration, "Virtual-2", &desired).is_ok());
        assert!(verify_retained_readback(&configuration, "Virtual-3", &desired).is_err());
        for (field, value) in [
            ("enabled", json!(false)),
            ("connected", json!(false)),
            ("hdr", json!(false)),
            ("hdr", json!(null)),
            ("currentModeId", json!("99")),
        ] {
            let mut invalid = configuration.clone();
            invalid["outputs"][0][field] = value;
            assert!(verify_retained_readback(&invalid, "Virtual-2", &desired).is_err());
        }
        let mut wrong_mode = configuration.clone();
        wrong_mode["outputs"][0]["modes"][0]["refreshRate"] = json!(119.0);
        assert!(verify_retained_readback(&wrong_mode, "Virtual-2", &desired).is_err());
        let mut duplicate = configuration.clone();
        duplicate["outputs"]
            .as_array_mut()
            .unwrap()
            .push(configuration["outputs"][0].clone());
        assert!(verify_retained_readback(&duplicate, "Virtual-2", &desired).is_err());
    }

    #[test]
    fn mode_selection_prefers_the_closest_valid_nominal_refresh() {
        let configuration = json!({"outputs":[{"name":"Virtual-2","connected":true,"modes":[
            {"id":"invalid","size":{"width":2560,"height":1440},"refreshRate":120.0},
            {"id":"1","size":{"width":2560,"height":1440},"refreshRate":119.88},
            {"id":"2","size":{"width":2560,"height":1440},"refreshRate":119.999},
            {"id":"3","size":{"width":2560,"height":1440},"refreshRate":144.0}
        ]}]});
        let desired = Specification {
            identity: "11111111-1111-4111-8111-111111111111".into(),
            name: "Monitor".into(),
            width: 2560,
            height: 1440,
            refresh_millihz: 120000,
            hdr_enabled: false,
        };
        assert_eq!(
            mode(&configuration, "Virtual-2", &desired).as_deref(),
            Some("2")
        );
        assert!(refresh_matches(119.88, 120000));
        assert!(!refresh_matches(119.0, 120000));
        assert!(!refresh_matches(f64::NAN, 120000));
    }
}
