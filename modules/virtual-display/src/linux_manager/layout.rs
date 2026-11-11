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
                let width = if matches!(o.rotation, 2 | 8 | 32 | 128) {
                    o.height
                } else {
                    o.width
                };
                o.x + (f64::from(width) / o.scale).ceil() as i64
            })
            .max()
            .unwrap_or(0)
            .max(0)
    }
    pub fn bottom_edge(&self) -> i64 {
        self.outputs
            .iter()
            .filter(|output| output.enabled)
            .map(|output| {
                let height = if matches!(output.rotation, 2 | 8 | 32 | 128) {
                    output.width
                } else {
                    output.height
                };
                output.y + (f64::from(height) / output.scale).ceil() as i64
            })
            .max()
            .unwrap_or(0)
            .max(0)
    }
    pub fn next_priority(&self) -> u32 {
        self.outputs.iter().filter(|output| output.enabled).count() as u32 + 1
    }
    fn normalized_outputs(&self, current: &Value) -> Result<Vec<Output>, String> {
        let mut outputs: Vec<_> = self
            .outputs
            .iter()
            .filter(|expected| {
                current["outputs"].as_array().is_some_and(|outputs| {
                    outputs.iter().any(|actual| {
                        actual["connected"] == true && actual["name"] == expected.name
                    })
                })
            })
            .cloned()
            .collect();
        // Another display owner may have unplugged every originally active
        // output. Keep the currently active fallback desktop in that case;
        // disabling the last physical output cannot restore the missing one.
        if !outputs.iter().any(|output| output.enabled) {
            for actual in current["outputs"]
                .as_array()
                .ok_or("missing current outputs")?
                .iter()
                .filter(|output| output["connected"] == true && output["enabled"] == true)
            {
                let fallback = Output::capture(actual)?;
                if let Some(saved) = outputs.iter_mut().find(|saved| saved.name == fallback.name) {
                    *saved = fallback;
                } else {
                    outputs.push(fallback);
                }
            }
        }
        outputs.sort_by(|left, right| {
            (!left.enabled, left.priority, &left.name).cmp(&(
                !right.enabled,
                right.priority,
                &right.name,
            ))
        });
        // KDE ranks enabled outputs densely. Removing priority two necessarily
        // makes the old priority three become two; preserve order, not the gap.
        for (index, output) in outputs
            .iter_mut()
            .filter(|output| output.enabled)
            .enumerate()
        {
            output.priority = index as u32 + 1;
        }
        Ok(outputs)
    }
    fn expected_outputs(
        &self,
        current: &Value,
        owned: Option<&str>,
    ) -> Result<Vec<Output>, String> {
        let mut outputs = self.normalized_outputs(current)?;
        if let Some(owned) = owned {
            outputs.retain(|output| output.name != owned);
            // The owned output occupies priority one. Peer priorities are
            // checked after this shift, so readback cannot hide a lost primary.
            for (index, output) in outputs
                .iter_mut()
                .filter(|output| output.enabled)
                .enumerate()
            {
                output.priority = index as u32 + 2;
            }
        }
        Ok(outputs)
    }
    pub fn arguments_with_primary(
        &self,
        current: &Value,
        owned: &str,
    ) -> Result<Vec<String>, String> {
        self.arguments_for(current, Some(owned))
    }
    pub fn arguments(&self, current: &Value) -> Result<Vec<String>, String> {
        self.arguments_for(current, None)
    }
    fn arguments_for(&self, current: &Value, owned: Option<&str>) -> Result<Vec<String>, String> {
        let outputs = current["outputs"]
            .as_array()
            .ok_or("missing current outputs")?;
        let mut arguments = Vec::new();
        for expected in self.expected_outputs(current, owned)? {
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
    pub fn matches_with_primary(&self, current: &Value, owned: &str) -> bool {
        self.difference_for(current, Some(owned)).is_none()
    }
    pub fn matches(&self, current: &Value) -> bool {
        self.difference(current).is_none()
    }
    /// The first saved output the current desktop does not reproduce, with
    /// each differing property, for the journal. `None` when it matches.
    pub fn difference(&self, current: &Value) -> Option<String> {
        self.difference_for(current, None)
    }
    fn difference_for(&self, current: &Value, owned: Option<&str>) -> Option<String> {
        let outputs = match self.expected_outputs(current, owned) {
            Ok(outputs) => outputs,
            Err(error) => return Some(error),
        };
        outputs.iter().find_map(|expected| {
            // An independently unplugged monitor is never reconnected by us.
            current["outputs"]
                .as_array()
                .and_then(|outputs| {
                    outputs
                        .iter()
                        .find(|o| o["connected"] == true && o["name"] == expected.name)
                })
                .and_then(|actual| expected.difference(actual))
        })
    }
}
impl Output {
    fn capture(output: &Value) -> Result<Self, String> {
        let name = output["name"]
            .as_str()
            .ok_or("missing output name")?
            .to_owned();
        let enabled = output["enabled"].as_bool().ok_or("invalid output state")?;
        // A disabled connector need not have an active mode. Its restoration
        // obligation is to remain disabled, without inventing a pixel mode.
        let (mode, width, height) = if enabled {
            let mode_id = output["currentModeId"]
                .as_str()
                .ok_or("missing current mode")?;
            let mode = output["modes"]
                .as_array()
                .and_then(|modes| modes.iter().find(|mode| mode["id"] == mode_id))
                .ok_or("missing current mode description")?;
            let width = mode["size"]["width"]
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .ok_or("invalid output width")?;
            let height = mode["size"]["height"]
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .ok_or("invalid output height")?;
            let rate = mode["refreshRate"]
                .as_f64()
                .filter(|rate| rate.is_finite() && *rate > 0.0 && *rate <= 1000.0)
                .ok_or("invalid output rate")?;
            (format!("{width}x{height}@{rate}"), width, height)
        } else {
            (String::new(), 0, 0)
        };
        Ok(Self {
            name,
            enabled,
            mode,
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
            && ((!self.enabled && self.mode.is_empty() && self.width == 0 && self.height == 0)
                || ((1..=32768).contains(&self.width)
                    && (1..=32768).contains(&self.height)
                    && self.refresh().is_some()))
            && self.x.unsigned_abs() <= 65536
            && self.y.unsigned_abs() <= 65536
            && self.scale.is_finite()
            && (0.25..=5.0).contains(&self.scale)
            && matches!(self.rotation, 1 | 2 | 4 | 8 | 16 | 32 | 64 | 128)
            && self.priority <= 99
    }
    fn refresh(&self) -> Option<f64> {
        self.mode
            .strip_prefix(&format!("{}x{}@", self.width, self.height))?
            .parse::<f64>()
            .ok()
            .filter(|rate| rate.is_finite() && *rate > 0.0 && *rate <= 1000.0)
    }
    fn mode_matches(&self, width: u64, height: u64, rate: f64) -> bool {
        width == u64::from(self.width)
            && height == u64::from(self.height)
            && rate.is_finite()
            && self
                .refresh()
                .is_some_and(|saved| (saved - rate).abs() < 0.2)
    }
    fn matches(&self, actual: &Value) -> bool {
        self.difference(actual).is_none()
    }
    /// Names every property of `actual` that differs from this saved output.
    fn difference(&self, actual: &Value) -> Option<String> {
        if !self.enabled {
            return match actual["enabled"].as_bool() {
                Some(false) => None,
                Some(true) => Some(format!(
                    "{}: enabled expected false observed true",
                    self.name
                )),
                None => Some(format!("{}: invalid output state", self.name)),
            };
        }
        let other = match Self::capture(actual) {
            Ok(other) => other,
            Err(error) => return Some(format!("{}: {error}", self.name)),
        };
        let mut properties = Vec::new();
        let mut compare = |property: &str, expected: String, observed: String| {
            if expected != observed {
                properties.push(format!(
                    "{property} expected {expected} observed {observed}"
                ));
            }
        };
        compare(
            "enabled",
            self.enabled.to_string(),
            other.enabled.to_string(),
        );
        if !other.refresh().is_some_and(|rate| {
            self.mode_matches(u64::from(other.width), u64::from(other.height), rate)
        }) {
            compare("mode", self.mode.clone(), other.mode.clone());
        }
        compare(
            "position",
            format!("{},{}", self.x, self.y),
            format!("{},{}", other.x, other.y),
        );
        if (self.scale - other.scale).abs() >= 0.01 {
            compare("scale", self.scale.to_string(), other.scale.to_string());
        }
        compare(
            "rotation",
            self.rotation.to_string(),
            other.rotation.to_string(),
        );
        compare("hdr", self.hdr.to_string(), other.hdr.to_string());
        compare("wcg", self.wcg.to_string(), other.wcg.to_string());
        if self.enabled {
            compare(
                "priority",
                self.priority.to_string(),
                other.priority.to_string(),
            );
        }
        (!properties.is_empty()).then(|| format!("{}: {}", self.name, properties.join(", ")))
    }
    fn arguments(&self, actual: &Value) -> Result<Vec<String>, String> {
        if !self.enabled {
            return Ok(vec![format!("output.{}.disable", self.name)]);
        }
        // Resolve the semantic mode to the current numeric ID. Decimal refresh
        // rates contain dots, which kscreen-doctor uses as argument separators.
        let mode_id = actual["modes"]
            .as_array()
            .and_then(|modes| {
                modes.iter().find(|mode| {
                    mode["size"]["width"].as_u64().is_some_and(|width| {
                        mode["size"]["height"].as_u64().is_some_and(|height| {
                            mode["refreshRate"]
                                .as_f64()
                                .is_some_and(|rate| self.mode_matches(width, height, rate))
                        })
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
            16 => "flipped",
            32 => "flipped90",
            64 => "flipped180",
            128 => "flipped270",
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
    fn primary_reservation_changes_only_peer_priorities_and_checks_the_shift() {
        let mut before = desktop();
        let mut peer = before["outputs"][0].clone();
        peer["name"] = json!("HDMI-A-1");
        peer["priority"] = json!(3);
        peer["pos"] = json!({"x":1024,"y":15});
        before["outputs"].as_array_mut().unwrap().push(peer);
        let layout = Layout::capture(&before, "Virtual-2").unwrap();
        let mut after = before.clone();
        after["outputs"][0]["priority"] = json!(2);
        after["outputs"][1]["priority"] = json!(3);
        assert!(layout.matches_with_primary(&after, "Virtual-2"));
        assert!(
            layout
                .arguments_with_primary(&after, "Virtual-2")
                .unwrap()
                .is_empty()
        );
        assert!(!layout.matches(&after));
        let commands = layout.arguments_with_primary(&before, "Virtual-2").unwrap();
        assert!(commands.contains(&"output.Virtual-1.priority.2".into()));
        assert!(!commands.contains(&"output.HDMI-A-1.priority.4".into()));
        after["outputs"][0]["priority"] = json!(1);
        assert!(!layout.matches_with_primary(&after, "Virtual-2"));
        after["outputs"][0]["priority"] = json!(2);
        after["outputs"][1]["pos"]["x"] = json!(0);
        assert!(!layout.matches_with_primary(&after, "Virtual-2"));
        assert!(
            layout
                .arguments_with_primary(&after, "Virtual-2")
                .unwrap()
                .contains(&"output.HDMI-A-1.position.1024,15".into())
        );
    }

    #[test]
    fn primary_reservation_excludes_owned_fallback_but_retains_physical_fallback() {
        let empty = Layout::capture(&json!({"outputs":[]}), "Virtual-1").unwrap();
        let mut current = desktop();
        assert!(empty.matches_with_primary(&current, "Virtual-1"));
        assert!(
            empty
                .arguments_with_primary(&current, "Virtual-1")
                .unwrap()
                .is_empty()
        );

        let mut physical = current["outputs"][0].clone();
        physical["name"] = json!("HDMI-A-1");
        physical["enabled"] = json!(false);
        physical["priority"] = json!(0);
        let layout = Layout::capture(&json!({"outputs":[physical.clone()]}), "Virtual-1").unwrap();
        physical["enabled"] = json!(true);
        physical["priority"] = json!(2);
        current["outputs"].as_array_mut().unwrap().push(physical);
        assert!(layout.matches_with_primary(&current, "Virtual-1"));
        assert!(
            layout
                .arguments_with_primary(&current, "Virtual-1")
                .unwrap()
                .is_empty()
        );
        current["outputs"][1]["priority"] = json!(1);
        assert!(!layout.matches_with_primary(&current, "Virtual-1"));
        let commands = layout
            .arguments_with_primary(&current, "Virtual-1")
            .unwrap();
        assert!(commands.contains(&"output.HDMI-A-1.priority.2".into()));
        assert!(
            commands
                .iter()
                .all(|command| !command.starts_with("output.Virtual-1."))
        );
    }

    #[test]
    fn bottom_edge_uses_rotated_scaled_height_and_rounds_up() {
        for (rotation, bottom) in [
            (1, 490),
            (2, 864),
            (8, 864),
            (16, 490),
            (32, 864),
            (64, 490),
            (128, 864),
        ] {
            let mut before = desktop();
            before["outputs"][0]["rotation"] = json!(rotation);
            before["outputs"][0]["scale"] = json!(3.0);
            before["outputs"][0]["pos"]["y"] = json!(10);
            let layout = Layout::capture(&before, "Virtual-2").unwrap();
            assert_eq!(layout.bottom_edge(), bottom);
        }
        let mut before = desktop();
        before["outputs"][0]["pos"]["y"] = json!(-2000);
        assert_eq!(
            Layout::capture(&before, "Virtual-2").unwrap().bottom_edge(),
            0
        );
        before["outputs"][0]["enabled"] = json!(false);
        before["outputs"][0]["pos"]["y"] = json!(5000);
        assert_eq!(
            Layout::capture(&before, "Virtual-2").unwrap().bottom_edge(),
            0
        );
    }

    #[test]
    fn disabled_connector_without_a_current_mode_is_preserved_and_only_disabled() {
        let mut before = desktop();
        let mut disabled = before["outputs"][0].clone();
        disabled["name"] = json!("HDMI-A-1");
        disabled["enabled"] = json!(false);
        disabled["currentModeId"] = Value::Null;
        disabled["modes"] = json!([]);
        disabled["priority"] = json!(0);
        before["outputs"].as_array_mut().unwrap().push(disabled);
        let layout = Layout::capture(&before, "Virtual-2").unwrap();
        let durable = serde_json::to_vec(&layout).unwrap();
        let layout: Layout = serde_json::from_slice(&durable).unwrap();
        assert!(layout.valid());
        assert!(layout.matches(&before));
        let mut after = before.clone();
        after["outputs"][1]["enabled"] = json!(true);
        assert_eq!(
            layout.arguments(&after).unwrap(),
            vec!["output.HDMI-A-1.disable"]
        );
        after["outputs"][1]["enabled"] = json!(false);
        after["outputs"][1]["scale"] = json!(1.0);
        assert!(layout.matches(&after));
    }

    #[test]
    fn reflected_rotations_round_trip_and_use_the_rotated_logical_width() {
        for (rotation, token, right_edge) in [
            (16, "flipped", 1024),
            (32, "flipped90", 576),
            (64, "flipped180", 1024),
            (128, "flipped270", 576),
        ] {
            let mut before = desktop();
            before["outputs"][0]["rotation"] = json!(rotation);
            let layout = Layout::capture(&before, "Virtual-2").unwrap();
            assert_eq!(layout.right_edge(), right_edge);
            let mut after = before.clone();
            after["outputs"][0]["rotation"] = json!(1);
            assert!(
                layout
                    .arguments(&after)
                    .unwrap()
                    .contains(&format!("output.Virtual-1.rotation.{token}"))
            );
            assert!(layout.matches(&before));
        }
        let mut fractional = desktop();
        fractional["outputs"][0]["scale"] = json!(3.0);
        assert_eq!(
            Layout::capture(&fractional, "Virtual-2")
                .unwrap()
                .right_edge(),
            854
        );
    }

    #[test]
    fn equivalent_refresh_and_scale_readbacks_do_not_block_restoration() {
        let mut before = desktop();
        before["outputs"][0]["modes"][0]["refreshRate"] = json!(59.94);
        let layout = Layout::capture(&before, "Virtual-2").unwrap();
        let mut after = before.clone();
        after["outputs"][0]["currentModeId"] = json!("9");
        after["outputs"][0]["modes"][0]["id"] = json!("9");
        after["outputs"][0]["modes"][0]["refreshRate"] = json!(60.0);
        after["outputs"][0]["scale"] = json!(2.499999);
        assert!(layout.matches(&after));
        assert!(layout.arguments(&after).unwrap().is_empty());
        after["outputs"][0]["pos"]["x"] = json!(10);
        assert!(
            layout
                .arguments(&after)
                .unwrap()
                .contains(&"output.Virtual-1.mode.9".into())
        );
        after["outputs"][0]["modes"][0]["refreshRate"] = json!(60.3);
        assert!(!layout.matches(&after));
        assert!(layout.arguments(&after).is_err());
        after["outputs"][0]["modes"][0]["refreshRate"] = json!(59.94);
        after["outputs"][0]["modes"][0]["size"]["width"] = json!(1920);
        assert!(layout.arguments(&after).is_err());
    }

    #[test]
    fn disappearing_original_display_preserves_the_active_physical_fallback() {
        let mut before = desktop();
        let mut physical = before["outputs"][0].clone();
        physical["name"] = json!("HDMI-A-1");
        physical["enabled"] = json!(false);
        physical["priority"] = json!(0);
        before["outputs"]
            .as_array_mut()
            .unwrap()
            .push(physical.clone());
        let layout = Layout::capture(&before, "Virtual-2").unwrap();
        physical["enabled"] = json!(true);
        physical["priority"] = json!(1);
        physical["scale"] = json!(2.0);
        let after = json!({"outputs":[physical]});
        assert!(layout.arguments(&after).unwrap().is_empty());
        assert!(layout.matches(&after));
    }

    #[test]
    fn removing_a_middle_output_preserves_relative_priority_without_an_unrestorable_gap() {
        let mut before = desktop();
        let mut peer = before["outputs"][0].clone();
        peer["name"] = json!("Virtual-3");
        peer["priority"] = json!(3);
        before["outputs"].as_array_mut().unwrap().push(peer);
        let layout = Layout::capture(&before, "Virtual-2").unwrap();
        let mut after = before.clone();
        after["outputs"][1]["priority"] = json!(2);
        assert!(layout.matches(&after));
        assert!(layout.arguments(&after).unwrap().is_empty());
        // A pending durable snapshot can also outlive another peer's removal.
        after["outputs"].as_array_mut().unwrap().remove(1);
        assert!(layout.matches(&after));
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
