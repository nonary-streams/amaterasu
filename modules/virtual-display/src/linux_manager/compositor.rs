//! Configure only the newly owned connector in the current KDE session.
use super::layout::Layout;
use crate::model::Specification;
use serde_json::Value;
use std::{
    io::Read,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub(super) trait Compositor: Send {
    fn snapshot(&mut self, _changing: &str) -> Result<Option<Layout>, String> {
        Ok(None)
    }
    fn restore(&mut self, _layout: Option<&Layout>) -> Result<(), String> {
        Ok(())
    }
    fn configure(
        &mut self,
        output: &str,
        specification: &Specification,
        layout: Option<&Layout>,
    ) -> Result<(), String>;
}
pub(super) struct KScreen;
impl Compositor for KScreen {
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
    fn configure(
        &mut self,
        output: &str,
        specification: &Specification,
        layout: Option<&Layout>,
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
        let right = match layout {
            Some(layout) => layout.right_edge(),
            None => right_edge(&configuration, output)?,
        };
        let mut arguments = match layout {
            Some(layout) => layout.arguments(&configuration)?,
            None => Vec::new(),
        };
        arguments.extend([
            format!("output.{output}.enable"),
            format!("output.{output}.mode.{selected}"),
            format!("output.{output}.scale.1"),
            format!("output.{output}.rotation.none"),
            format!("output.{output}.position.{right},0"),
            format!("output.{output}.hdr.disable"),
            format!("output.{output}.wcg.disable"),
            format!(
                "output.{output}.priority.{}",
                layout.map_or(1, Layout::next_priority)
            ),
        ]);
        run(&arguments)?;
        let confirmed = query()?;
        if layout.is_some_and(|layout| !layout.matches(&confirmed)) {
            return Err("existing desktop changed during hotplug".into());
        }
        let actual = find_output(&confirmed, output)
            .ok_or("owned display disappeared after configuration")?;
        if actual["enabled"] != true
            || actual["scale"].as_f64() != Some(1.0)
            || actual["pos"]["x"].as_i64() != Some(right)
            || actual["pos"]["y"].as_i64() != Some(0)
            || actual["currentModeId"].as_str() != Some(&selected)
            || actual["rotation"] != 1
            || actual["hdr"] != false
            || actual["wcg"] != false
        {
            return Err("owned display configuration readback differs".into());
        }
        Ok(())
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
fn right_edge(configuration: &Value, owned: &str) -> Result<i64, String> {
    let outputs = configuration["outputs"]
        .as_array()
        .filter(|outputs| outputs.len() <= 64)
        .ok_or("invalid KScreen output list")?;
    let mut right = 0i64;
    for output in outputs.iter().filter(|output| {
        output["name"] != owned && output["connected"] == true && output["enabled"] == true
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
    fn new_display_is_placed_after_scaled_existing_outputs_without_moving_them() {
        let configuration = json!({"outputs":[{"name":"Virtual-1","connected":true,"enabled":true,"pos":{"x":0},"size":{"width":3840,"height":2160},"scale":2.5,"rotation":1},
            {"name":"Virtual-2","connected":true,"enabled":true,"pos":{"x":0},"size":{"width":1920,"height":1080},"scale":1,"rotation":1}]});
        assert_eq!(right_edge(&configuration, "Virtual-2").unwrap(), 1536);
        assert_eq!(configuration["outputs"][0]["pos"]["x"], 0);
    }
}
