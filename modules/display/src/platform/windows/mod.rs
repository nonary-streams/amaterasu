mod apply;
mod conversion;
mod query;

use super::Platform;
use crate::model::{AvailableMode, Configuration, DisplayError, DisplayResult, Snapshot};
use serde_json::{Value, json};
#[derive(Default)]
pub struct WindowsPlatform;
impl Platform for WindowsPlatform {
    fn name(&self) -> &'static str {
        "windows_ccd"
    }
    fn supported(&self) -> bool {
        true
    }
    fn enumerate(
        &mut self,
        display: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> DisplayResult<Value> {
        let values = if let Some(id) = display {
            query::modes(id)?
        } else {
            query::monitors()?
        };
        if offset > values.len() {
            return Err(DisplayError::new(
                "invalid_request",
                "enumeration offset exceeds result count",
            ));
        }
        let total = values.len();
        let mut rows = Vec::new();
        let mut bytes = 0;
        for value in values.into_iter().skip(offset).take(limit) {
            let size = serde_json::to_vec(&value)
                .map_err(|error| DisplayError::new("platform_error", error.to_string()))?
                .len();
            if bytes + size > 12_000 {
                break;
            }
            bytes += size;
            rows.push(value);
        }
        if rows.is_empty() && offset < total {
            return Err(DisplayError::new(
                "resource_exhausted",
                "display enumeration row exceeds transport limit",
            ));
        }
        let next = (offset + rows.len() < total).then_some(offset + rows.len());
        Ok(json!({"ok":true,"platform":self.name(),"entries":rows,"next_offset":next}))
    }
    fn prepare(&mut self, configuration: &Configuration) -> DisplayResult<Snapshot> {
        let before = query::snapshot()?;
        for display in &configuration.displays {
            let active = before
                .paths
                .iter()
                .find(|path| path.id.eq_ignore_ascii_case(&display.id));
            if display.hdr_enabled == Some(true) && active.is_none() {
                return Err(DisplayError::new(
                    "unsupported_display",
                    "HDR enablement requires an active target with a recoverable baseline",
                ));
            }
            if display.hdr_enabled == Some(true)
                && active.is_some_and(|path| path.hdr_enabled.is_none())
            {
                return Err(DisplayError::new(
                    "unsupported_display",
                    "HDR enablement requires a target with queryable HDR state",
                ));
            }
        }
        let requested = apply::requested(configuration)?;
        apply::set(&requested, false)?;
        Ok(before)
    }
    fn available_modes(&mut self, display: &str) -> DisplayResult<Vec<AvailableMode>> {
        query::available_modes(display)
    }
    fn apply(
        &mut self,
        configuration: &Configuration,
        before: &Snapshot,
        guard: &mut dyn FnMut() -> DisplayResult<()>,
    ) -> DisplayResult<()> {
        if !before.equivalent(&query::snapshot()?) {
            return Err(DisplayError::new(
                "topology_changed",
                "display configuration changed before the durable apply boundary",
            ));
        }
        let requested = apply::requested(configuration)?;
        apply::set(&requested, false)?;
        guard()?;
        apply::set(&requested, true).map_err(DisplayError::after_mutation)?;
        verify_applied_configuration(configuration, query::snapshot())?;
        for display in &configuration.displays {
            if let Some(enabled) = display.hdr_enabled {
                if !enabled
                    && !before
                        .paths
                        .iter()
                        .any(|path| path.id.eq_ignore_ascii_case(&display.id))
                {
                    query::verify_newly_active_hdr_is_off(&display.id)
                        .map_err(DisplayError::after_mutation)?;
                } else {
                    query::set_advanced_color(&display.id, enabled)
                        .map_err(DisplayError::after_mutation)?;
                }
            }
        }
        // HDR changes can cause a driver to renegotiate its mode or topology.
        // Verify the complete CCD request again after the last HDR operation.
        verify_applied_configuration(configuration, query::snapshot())
    }
    fn restore(
        &mut self,
        snapshot: &Snapshot,
        guard: &mut dyn FnMut() -> DisplayResult<()>,
    ) -> DisplayResult<()> {
        query::check_legacy_hdr_recovery(snapshot)?;
        let requested = apply::restoration(snapshot)?;
        apply::set(&requested, false)?;
        guard()?;
        apply::set(&requested, true).map_err(DisplayError::after_mutation)?;
        for path in &snapshot.paths {
            if let Some(enabled) = path.hdr_enabled {
                query::set_advanced_color(&path.id, enabled)
                    .map_err(DisplayError::after_mutation)?;
            }
        }
        if !snapshot.equivalent(&query::snapshot().map_err(DisplayError::after_mutation)?) {
            return Err(DisplayError::new(
                "verification_failed",
                "restored CCD mode/topology does not exactly match the saved snapshot",
            )
            .after_mutation());
        }
        Ok(())
    }
}

fn verify_applied_configuration(
    configuration: &Configuration,
    readback: DisplayResult<Snapshot>,
) -> DisplayResult<()> {
    if !apply::verifies(
        configuration,
        &readback.map_err(DisplayError::after_mutation)?,
    ) {
        return Err(DisplayError::new(
            "verification_failed",
            "Windows display state differs from the requested configuration",
        )
        .after_mutation());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DisplaySetting, Rational, Rotation};

    fn requested() -> Configuration {
        Configuration {
            displays: vec![DisplaySetting {
                id: "monitor.one".into(),
                width: 1920,
                height: 1080,
                refresh: Rational {
                    numerator: 60,
                    denominator: 1,
                },
                x: 0,
                y: 0,
                rotation: Rotation::Identity,
                hdr_enabled: Some(true),
            }],
        }
    }

    #[test]
    fn final_readback_rejects_late_topology_drift_as_mutated() {
        let empty_topology = Snapshot {
            format_version: 1,
            paths: vec![],
        };
        let error = verify_applied_configuration(&requested(), Ok(empty_topology)).unwrap_err();
        assert_eq!(error.code, "verification_failed");
        assert!(error.may_have_changed);
    }

    #[test]
    fn final_readback_failure_is_also_mutated() {
        let readback_error = DisplayError::new("topology_changed", "late hotplug");
        let error = verify_applied_configuration(&requested(), Err(readback_error)).unwrap_err();
        assert_eq!(error.code, "topology_changed");
        assert!(error.may_have_changed);
    }
}
