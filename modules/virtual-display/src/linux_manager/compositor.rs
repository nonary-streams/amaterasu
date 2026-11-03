//! Configure the newly owned connector in the current KDE session, either
//! beside the existing desktop or as its only enabled display.
use super::layout::Layout;
use crate::{model::Specification, scale::ScaleSetting};
use serde_json::Value;
use std::{
    io::Read,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

/// Where the owned output goes relative to the rest of the desktop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Placement {
    /// Right of the existing desktop, which keeps its current layout.
    Extended,
    /// The only enabled display apart from `keep`, the outputs of other
    /// Amaterasu streams, which stay on and are never displaced.
    Exclusive { keep: Vec<String> },
}

pub(super) trait Compositor: Send {
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
    ) -> Result<(), String>;
}
pub(super) struct KScreen;
impl Compositor for KScreen {
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
        if let Some(layout) = layout {
            let arguments = layout.arguments(&query()?)?;
            if !arguments.is_empty() {
                run(&arguments)?;
            }
            if !layout.matches(&query()?) {
                return Err("existing desktop restoration readback differs".into());
            }
        }
        Ok(())
    }
    fn verify_removal_guard(&mut self, output: &str) -> Result<bool, String> {
        let configuration = query()?;
        let outputs = configuration["outputs"]
            .as_array()
            .ok_or("missing KScreen outputs")?;
        let mut observer =
            super::observer::Observer::open().ok_or("cannot observe surviving Wayland output")?;
        let guarded = outputs.iter().any(|candidate| {
            candidate["connected"] == true
                && candidate["enabled"] == true
                && candidate["name"].as_str().is_some_and(|name| {
                    name != output
                        && observer.read(name).is_some_and(|seen| {
                            seen.width > 0 && seen.height > 0 && seen.refresh > 0
                        })
                })
        });
        if guarded {
            return Ok(true);
        }
        let owned_active = outputs.iter().any(|candidate| {
            candidate["name"] == output
                && candidate["connected"] == true
                && candidate["enabled"] == true
        });
        if owned_active
            && observer
                .read(output)
                .is_some_and(|seen| seen.width > 0 && seen.height > 0 && seen.refresh > 0)
        {
            Ok(false)
        } else {
            Err("neither a surviving output nor the owned output can be observed".into())
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
    ) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut configuration = loop {
            let configuration = query()?;
            if find_output(&configuration, output).is_some() {
                break configuration;
            }
            if Instant::now() >= deadline {
                return Err("owned connector did not appear in KScreen".into());
            }
            thread::sleep(Duration::from_millis(25));
        };
        if mode(&configuration, output, specification).is_none() {
            run(&[format!(
                "output.{output}.addCustomMode.{}.{}.{}.reduced",
                specification.width, specification.height, specification.refresh_millihz
            )])?;
            configuration = query()?;
        }
        let selected = mode(&configuration, output, specification)
            .ok_or("requested owned display mode is unavailable")?;
        let target = OwnedTarget {
            mode: selected,
            hdr: specification.hdr_enabled,
            scale: scale.factor(specification.width, specification.height),
        };
        if let Placement::Exclusive { keep } = placement {
            return configure_exclusive(&configuration, output, &target, keep);
        }
        let right = match layout {
            Some(layout) => layout.right_edge(),
            None => right_edge(&configuration, output)?,
        };
        let mut arguments = match layout {
            Some(layout) => layout.arguments(&configuration)?,
            None => Vec::new(),
        };
        let priority = match layout {
            Some(layout) => layout.next_priority() as usize,
            None => live_next_priority(&configuration, output)?,
        };
        arguments.extend(target.arguments(output, right, priority));
        run(&arguments)?;
        let confirmed = query()?;
        if layout.is_some_and(|layout| !layout.matches(&confirmed)) {
            return Err("existing desktop changed during hotplug".into());
        }
        let actual = find_output(&confirmed, output)
            .ok_or("owned display disappeared after configuration")?;
        if !target.matches(actual, right) {
            return Err("owned display configuration readback differs".into());
        }
        Ok(())
    }
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
            .filter(|v| matches!(v, 1 | 2 | 4 | 8))
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
) -> Result<(), String> {
    let right = right_edge_of(configuration, |name| keep.iter().any(|kept| kept == name))?;
    // Kept peer outputs retain their order; this stream's output follows them.
    let priority = keep.len() + 1;
    let mut arguments = exclusive_disable_arguments(configuration, output, keep)?;
    arguments.extend(target.arguments(output, right, priority));
    run(&arguments)?;
    let confirmed = query()?;
    let actual =
        find_output(&confirmed, output).ok_or("owned display disappeared after configuration")?;
    if !target.matches(actual, right)
        || !exclusive_disable_arguments(&confirmed, output, keep)?.is_empty()
    {
        return Err("exclusive owned display configuration readback differs".into());
    }
    Ok(())
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
        let mut arguments = vec![
            format!("output.{output}.enable"),
            format!("output.{output}.mode.{}", self.mode),
            format!("output.{output}.rotation.none"),
            format!("output.{output}.position.{x},0"),
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
    fn matches(&self, actual: &Value, x: i64) -> bool {
        let scale_matches = match self.scale {
            Some(scale) => actual["scale"]
                .as_f64()
                .is_some_and(|actual| (actual - scale).abs() < 0.001),
            None => actual["scale"].as_f64().is_some(),
        };
        actual["enabled"] == true
            && scale_matches
            && actual["pos"]["x"].as_i64() == Some(x)
            && actual["pos"]["y"].as_i64() == Some(0)
            && actual["currentModeId"].as_str() == Some(&self.mode)
            && actual["rotation"] == 1
            && actual["hdr"] == self.hdr
            && actual["wcg"] == self.hdr
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
fn mode(configuration: &Value, output: &str, spec: &Specification) -> Option<String> {
    find_output(configuration, output)?["modes"]
        .as_array()?
        .iter()
        .find(|mode| {
            mode["size"]["width"].as_u64() == Some(u64::from(spec.width))
                && mode["size"]["height"].as_u64() == Some(u64::from(spec.height))
                && mode["refreshRate"].as_f64().is_some_and(|rate| {
                    (rate * 1000.0 - f64::from(spec.refresh_millihz)).abs() <= 100.0
                })
        })?["id"]
        .as_str()
        .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
        .map(str::to_owned)
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
        let x = output["pos"]["x"]
            .as_i64()
            .ok_or("invalid KScreen position")?;
        let scale = output["scale"]
            .as_f64()
            .filter(|v| v.is_finite() && (0.25..=5.0).contains(v))
            .ok_or("invalid KScreen scale")?;
        let size = if matches!(output["rotation"].as_u64(), Some(2 | 8)) {
            &output["size"]["height"]
        } else {
            &output["size"]["width"]
        };
        let width = size
            .as_u64()
            .filter(|v| *v <= 8192)
            .ok_or("invalid KScreen width")?;
        right = right.max(x + (width as f64 / scale).round() as i64);
    }
    if !(0..=65536).contains(&right) {
        return Err("desktop bounds exceed supported coordinates".into());
    }
    Ok(right)
}
fn run(arguments: &[String]) -> Result<Vec<u8>, String> {
    let mut child = Command::new("kscreen-doctor")
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let stdout = child.stdout.take().ok_or("KScreen stdout unavailable")?;
    let stderr = child.stderr.take().ok_or("KScreen stderr unavailable")?;
    let output = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let errors = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.take(65537).read_to_end(&mut bytes).map(|_| bytes)
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        thread::sleep(Duration::from_millis(10));
    };
    let bytes = output
        .join()
        .map_err(|_| "KScreen output reader panicked")?
        .map_err(|e| e.to_string())?;
    let _ = errors.join();
    if !status.is_some_and(|status| status.success()) || bytes.len() > 1024 * 1024 {
        return Err("KScreen command failed or exceeded its bound".into());
    }
    Ok(bytes)
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
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
