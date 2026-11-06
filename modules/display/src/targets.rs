//! Private native target observations and the public, opaque selection boundary.
use crate::model::{DisplayError, DisplayResult, DisplaySetting};
use amaterasu_module_sdk::capture::exact_target_selector;
use amaterasu_module_sdk::display::targets::{Binding, Descriptor, MAX_TARGETS, Mode};
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Clone, Debug)]
pub struct NativeTarget {
    pub descriptor: Descriptor,
    pub binding: Option<Binding>,
    pub native_id: String,
    pub setting: DisplaySetting,
}

impl NativeTarget {
    #[cfg(any(windows, test))]
    pub fn unavailable(mut self, error: &str) -> Self {
        self.descriptor.ready = false;
        self.descriptor.mode = None;
        self.descriptor.error = Some(error.into());
        self.binding = None;
        self
    }
    /// Inputs have already been matched to one exact native output by the
    /// platform provider. No native identity is serialized by these helpers.
    pub fn observed(
        platform: &str,
        setting: DisplaySetting,
        label: &str,
        output: String,
    ) -> DisplayResult<Self> {
        let native_id = setting.id.clone();
        let selector = selector_for(platform, &native_id)?;
        let label = if label.is_empty() { "Display" } else { label };
        let mut bounded_label = String::new();
        for character in label.chars().filter(|character| !character.is_control()) {
            if bounded_label.len() + character.len_utf8() > 160 {
                break;
            }
            bounded_label.push(character);
        }
        let refresh = u64::from(setting.refresh.numerator)
            .checked_mul(1000)
            .and_then(|numerator| numerator.checked_add(u64::from(setting.refresh.denominator) / 2))
            .and_then(|numerator| numerator.checked_div(u64::from(setting.refresh.denominator)))
            .and_then(|refresh| u32::try_from(refresh).ok());
        let mode = match (refresh, setting.hdr_enabled) {
            (Some(refresh_millihz), Some(hdr_enabled)) => {
                let mode = Mode {
                    width: setting.width,
                    height: setting.height,
                    refresh_millihz,
                    hdr_enabled,
                };
                mode.validate().ok().map(|()| mode)
            }
            _ => None,
        };
        let binding = mode.clone().map(|mode| Binding {
            output,
            mode,
            route_key: "desktop_output".into(),
        });
        if let Some(binding) = &binding {
            binding
                .validate()
                .map_err(|message| DisplayError::new("invalid_capture_target", message))?;
        }
        let descriptor = Descriptor {
            selector,
            label: bounded_label,
            ready: mode.is_some(),
            mode,
            error: binding
                .is_none()
                .then(|| "source_mode_or_color_unavailable".into()),
        };
        descriptor
            .validate()
            .map_err(|message| DisplayError::new("invalid_capture_target", message))?;
        Ok(Self {
            descriptor,
            binding,
            native_id,
            setting,
        })
    }

    pub fn resolve_reply(&self) -> DisplayResult<Value> {
        let binding = self.binding.as_ref().ok_or_else(|| {
            DisplayError::new(
                "capture_target_unavailable",
                "exact source mode and color are unavailable",
            )
        })?;
        Ok(
            json!({"ok":true,"target":self.descriptor,"capture_output":binding.output,"capture_route_key":binding.route_key}),
        )
    }
}

pub fn selector_for(platform: &str, native_id: &str) -> DisplayResult<String> {
    let identity = format!(
        "amaterasu.display.target/1:{platform}:{}",
        if platform == "windows_ccd" {
            native_id.to_ascii_lowercase()
        } else {
            native_id.to_owned()
        }
    );
    exact_target_selector(&identity)
        .map(|selector| selector.replacen("exact-target:", "display-target:", 1))
        .map_err(|message| DisplayError::new("invalid_capture_target", message))
}

pub fn validate_inventory(targets: &[NativeTarget]) -> DisplayResult<()> {
    if targets.len() > MAX_TARGETS {
        return Err(DisplayError::new(
            "resource_exhausted",
            "too many capture targets",
        ));
    }
    let mut selectors = BTreeSet::new();
    let mut native_ids = BTreeSet::new();
    for target in targets {
        if target.native_id != target.setting.id {
            return Err(DisplayError::new(
                "capture_target_changed",
                "exact target observation identity changed",
            ));
        }
        target
            .descriptor
            .validate()
            .map_err(|message| DisplayError::new("invalid_capture_target", message))?;
        if !selectors.insert(&target.descriptor.selector)
            || !native_ids.insert(target.native_id.to_lowercase())
        {
            return Err(DisplayError::new(
                "ambiguous_capture_target",
                "capture target inventory is ambiguous",
            ));
        }
    }
    Ok(())
}

pub fn select(targets: Vec<NativeTarget>, selector: &str) -> DisplayResult<NativeTarget> {
    validate_inventory(&targets)?;
    let mut matching = targets
        .into_iter()
        .filter(|target| target.descriptor.selector == selector);
    let target = matching.next().ok_or_else(|| {
        DisplayError::new(
            "capture_target_unavailable",
            "selected source is no longer available",
        )
    })?;
    if matching.next().is_some() {
        return Err(DisplayError::new(
            "ambiguous_capture_target",
            "selected source is ambiguous",
        ));
    }
    if !target.descriptor.ready || target.binding.is_none() {
        return Err(DisplayError::new(
            "capture_target_unavailable",
            "selected source is not ready",
        ));
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Rational, Rotation};
    fn target() -> NativeTarget {
        NativeTarget::observed(
            "test",
            DisplaySetting {
                id: "private-device-path".into(),
                width: 1920,
                height: 1080,
                refresh: Rational {
                    numerator: 60,
                    denominator: 1,
                },
                x: 0,
                y: 0,
                rotation: Rotation::Identity,
                hdr_enabled: Some(false),
            },
            "Living room",
            "opaque-capture-output".into(),
        )
        .unwrap()
    }
    #[test]
    fn public_inventory_hides_native_identifiers_and_resolution_is_exact() {
        let target = target();
        assert!(
            !target
                .clone()
                .unavailable("capture_unavailable")
                .descriptor
                .ready
        );
        let public = serde_json::to_string(&target.descriptor).unwrap();
        assert!(!public.contains("private-device-path"));
        assert!(target.descriptor.selector.starts_with("display-target:"));
        assert_eq!(
            select(vec![target.clone()], &target.descriptor.selector)
                .unwrap()
                .binding
                .unwrap()
                .output,
            "opaque-capture-output"
        );
        assert!(select(vec![target], "missing").is_err());
    }
    #[test]
    fn ambiguous_or_unknown_color_targets_never_become_ready() {
        let target = target();
        assert!(validate_inventory(&[target.clone(), target.clone()]).is_err());
        let mut setting = target.setting;
        setting.hdr_enabled = None;
        let target =
            NativeTarget::observed("test", setting, "Display", "exact-source".into()).unwrap();
        assert!(!target.descriptor.ready);
        assert!(target.resolve_reply().is_err());
    }
}
