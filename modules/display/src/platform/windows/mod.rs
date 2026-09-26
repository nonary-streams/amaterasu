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
            if display.hdr_enabled.is_some() && active.is_none() {
                return Err(DisplayError::new(
                    "unsupported_display",
                    "HDR changes require an active target",
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
        if !apply::verifies(
            configuration,
            &query::snapshot().map_err(DisplayError::after_mutation)?,
        ) {
            return Err(DisplayError::new(
                "verification_failed",
                "Windows display state differs from the requested configuration",
            )
            .after_mutation());
        }
        for display in &configuration.displays {
            if let Some(enabled) = display.hdr_enabled {
                query::set_advanced_color(&display.id, enabled)
                    .map_err(DisplayError::after_mutation)?;
            }
        }
        Ok(())
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
