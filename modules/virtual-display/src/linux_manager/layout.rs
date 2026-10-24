//! Preserve the current desktop across KDE's cached hotplug configuration.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Layout {
    outputs: Vec<Output>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Output {
    name: String,
    enabled: bool,
    mode: String,
    width: u32,
    height: u32,
    x: i64,
    y: i64,
    scale: f64,
    rotation: u32,
    hdr: bool,
    wcg: bool,
    priority: u32,
}
impl Layout {
    pub fn capture(configuration: &Value, changing: &str) -> Result<Self, String> {
        let outputs = configuration["outputs"]
            .as_array()
            .ok_or("missing KScreen outputs")?;
        let mut preserved = Vec::new();
        for output in outputs
            .iter()
            .filter(|o| o["connected"] == true && o["name"] != changing)
        {
            preserved.push(Output::capture(output)?);
        }
        let layout = Self { outputs: preserved };
        if !layout.valid() {
            return Err("unsupported existing desktop layout".into());
        }
        Ok(layout)
    }
    pub fn valid(&self) -> bool {
        self.outputs.len() <= 64
            && self.outputs.iter().all(Output::valid)
            && self
                .outputs
                .iter()
                .map(|output| &output.name)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == self.outputs.len()
            && self.right_edge() <= 65536
    }
    pub fn right_edge(&self) -> i64 {
        self.outputs
            .iter()
            .filter(|o| o.enabled)
            .map(|o| {
                let width = if matches!(o.rotation, 2 | 8) {
                    o.height
                } else {
                    o.width
                };
                o.x + (f64::from(width) / o.scale).round() as i64
            })
            .max()
            .unwrap_or(0)
            .max(0)
    }
    pub fn next_priority(&self) -> u32 {
        self.outputs
            .iter()
            .filter(|o| o.enabled)
            .map(|o| o.priority)
            .max()
            .unwrap_or(0)
            + 1
    }
    pub fn arguments(&self, current: &Value) -> Result<Vec<String>, String> {
        let outputs = current["outputs"]
            .as_array()
            .ok_or("missing current outputs")?;
        let mut arguments = Vec::new();
        for expected in &self.outputs {
            if let Some(actual) = outputs
                .iter()
                .find(|o| o["connected"] == true && o["name"] == expected.name)
                && !expected.matches(actual)
            {
                arguments.extend(expected.arguments(actual)?);
            }
        }
        Ok(arguments)
    }
    pub fn matches(&self, current: &Value) -> bool {
        self.outputs.iter().all(|expected| {
            // An independently unplugged monitor is never reconnected by us.
            current["outputs"]
                .as_array()
                .and_then(|outputs| {
                    outputs
                        .iter()
                        .find(|o| o["connected"] == true && o["name"] == expected.name)
                })
                .is_none_or(|actual| expected.matches(actual))
        })
    }
}
impl Output {
    fn capture(output: &Value) -> Result<Self, String> {
        let name = output["name"]
            .as_str()
            .ok_or("missing output name")?
            .to_owned();
        let mode_id = output["currentModeId"]
            .as_str()
            .ok_or("missing current mode")?;
        let mode = output["modes"]
            .as_array()
            .and_then(|modes| modes.iter().find(|m| m["id"] == mode_id))
            .ok_or("missing current mode description")?;
        let width = mode["size"]["width"]
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .ok_or("invalid output width")?;
        let height = mode["size"]["height"]
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .ok_or("invalid output height")?;
        let rate = mode["refreshRate"]
            .as_f64()
            .filter(|v| v.is_finite() && *v > 0.0 && *v <= 1000.0)
            .ok_or("invalid output rate")?;
        Ok(Self {
            name,
            enabled: output["enabled"].as_bool().ok_or("invalid output state")?,
            mode: format!("{width}x{height}@{rate}"),
            width,
            height,
            x: output["pos"]["x"]
                .as_i64()
                .ok_or("invalid output position")?,
            y: output["pos"]["y"]
                .as_i64()
                .ok_or("invalid output position")?,
            scale: output["scale"].as_f64().ok_or("invalid output scale")?,
            rotation: output["rotation"]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .ok_or("invalid output rotation")?,
            hdr: output["hdr"].as_bool().unwrap_or(false),
            wcg: output["wcg"].as_bool().unwrap_or(false),
            priority: output["priority"]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(0),
        })
    }
    fn valid(&self) -> bool {
        !self.name.is_empty()
            && self.name.len() <= 128
            && self
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            && self.mode.len() <= 64
            && self
                .mode
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'x' | b'@' | b'.'))
            && (1..=32768).contains(&self.width)
            && (1..=32768).contains(&self.height)
            && self.x.unsigned_abs() <= 65536
            && self.y.unsigned_abs() <= 65536
            && self.scale.is_finite()
            && (0.25..=5.0).contains(&self.scale)
            && matches!(self.rotation, 1 | 2 | 4 | 8)
            && self.priority <= 99
    }
    fn matches(&self, actual: &Value) -> bool {
        Self::capture(actual).is_ok_and(|other| {
            self.enabled == other.enabled
                && self.mode == other.mode
                && self.x == other.x
                && self.y == other.y
                && self.scale == other.scale
                && self.rotation == other.rotation
                && self.hdr == other.hdr
                && self.wcg == other.wcg
                && (!self.enabled || self.priority == other.priority)
        })
    }
    fn arguments(&self, actual: &Value) -> Result<Vec<String>, String> {
        // Resolve the semantic mode to the current numeric ID. Decimal refresh
        // rates contain dots, which kscreen-doctor uses as argument separators.
        let mode_id = actual["modes"]
            .as_array()
            .and_then(|modes| {
                modes.iter().find(|mode| {
                    mode["size"]["width"].as_u64() == Some(u64::from(self.width))
                        && mode["size"]["height"].as_u64() == Some(u64::from(self.height))
                        && mode["refreshRate"].as_f64().is_some_and(|rate| {
                            format!("{}x{}@{rate}", self.width, self.height) == self.mode
                        })
                })
            })
            .and_then(|mode| mode["id"].as_str())
            .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
            .ok_or("preserved desktop mode is unavailable")?;
        let prefix = format!("output.{}", self.name);
        let rotation = match self.rotation {
            2 => "left",
            4 => "inverted",
            8 => "right",
            _ => "none",
        };
        Ok(vec![
            format!(
                "{prefix}.{}",
                if self.enabled { "enable" } else { "disable" }
            ),
            format!("{prefix}.mode.{mode_id}"),
            format!("{prefix}.scale.{}", self.scale),
            format!("{prefix}.position.{},{}", self.x, self.y),
            format!("{prefix}.rotation.{rotation}"),
            format!(
                "{prefix}.hdr.{}",
                if self.hdr { "enable" } else { "disable" }
            ),
            format!(
                "{prefix}.wcg.{}",
                if self.wcg { "enable" } else { "disable" }
            ),
            format!("{prefix}.priority.{}", self.priority),
        ])
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn desktop() -> Value {
        json!({"outputs":[{"name":"Virtual-1","connected":true,"enabled":true,"currentModeId":"1","modes":[{"id":"1","size":{"width":2560,"height":1440},"refreshRate":120}],"pos":{"x":0,"y":0},"scale":2.5,"rotation":1,"hdr":false,"wcg":true,"priority":1}]})
    }
    #[test]
    fn fractional_refresh_uses_current_mode_id_and_corrupt_coordinates_are_rejected() {
        let mut before = desktop();
        before["outputs"][0]["modes"][0]["refreshRate"] = json!(59.94);
        let layout = Layout::capture(&before, "Virtual-2").unwrap();
        before["outputs"][0]["enabled"] = json!(false);
        assert!(
            layout
                .arguments(&before)
                .unwrap()
                .contains(&"output.Virtual-1.mode.1".into())
        );
        before["outputs"][0]["pos"]["x"] = json!(i64::MIN);
        assert!(Layout::capture(&before, "Virtual-2").is_err());
    }
    #[test]
    fn cached_hotplug_cannot_replace_an_existing_active_desktop() {
        let before = desktop();
        let layout = Layout::capture(&before, "Virtual-2").unwrap();
        let mut after = before.clone();
        after["outputs"][0]["enabled"] = json!(false);
        assert!(!layout.matches(&after));
        let commands = layout.arguments(&after).unwrap();
        assert!(commands.contains(&"output.Virtual-1.enable".into()));
        assert!(commands.contains(&"output.Virtual-1.mode.1".into()));
        assert_eq!(layout.right_edge(), 1024);
        assert!(layout.arguments(&before).unwrap().is_empty());
        assert!(layout.matches(&json!({"outputs":[]})));
    }
}
