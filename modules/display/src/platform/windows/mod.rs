mod apply;
mod conversion;
mod query;

use super::Platform;
use crate::model::{
    AvailableMode, Configuration, DisplayError, DisplayResult, RouteBinding, Snapshot,
};
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
        prepare_configuration(configuration, None, None)
    }
    fn bind_inactive(&mut self, display: &str, before: &Snapshot) -> DisplayResult<RouteBinding> {
        apply::bind_inactive(display, before)
    }
    fn prepare_preserving(
        &mut self,
        configuration: &Configuration,
        selected: &str,
        route: Option<&RouteBinding>,
    ) -> DisplayResult<Snapshot> {
        prepare_configuration(configuration, Some(selected), route)
    }
    fn snapshot(&mut self) -> DisplayResult<Snapshot> {
        let first = query::snapshot()?;
        let second = query::snapshot()?;
        if first.revision()? != second.revision()? {
            return Err(DisplayError::new(
                "topology_changed",
                "active display topology changed during snapshot read",
            ));
        }
        Ok(second)
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
        apply_configuration(configuration, before, None, None, guard)
    }
    fn apply_preserving(
        &mut self,
        configuration: &Configuration,
        before: &Snapshot,
        selected: &str,
        route: Option<&RouteBinding>,
        guard: &mut dyn FnMut() -> DisplayResult<()>,
    ) -> DisplayResult<()> {
        apply_configuration(configuration, before, Some(selected), route, guard)
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

fn prepare_configuration(
    configuration: &Configuration,
    selected: Option<&str>,
    route: Option<&RouteBinding>,
) -> DisplayResult<Snapshot> {
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
    let requested = match selected {
        Some(selected) => apply::preserving(configuration, &before, selected, route)?,
        None => apply::requested(configuration)?,
    };
    apply::set(&requested, false)?;
    Ok(before)
}

fn apply_configuration(
    configuration: &Configuration,
    before: &Snapshot,
    selected: Option<&str>,
    route: Option<&RouteBinding>,
    guard: &mut dyn FnMut() -> DisplayResult<()>,
) -> DisplayResult<()> {
    if before.revision()? != query::snapshot()?.revision()? {
        return Err(DisplayError::new(
            "topology_changed",
            "display configuration changed before the durable apply boundary",
        ));
    }
    let requested = match selected {
        Some(selected) => apply::preserving(configuration, before, selected, route)?,
        None => apply::requested(configuration)?,
    };
    apply::set(&requested, false)?;
    guard()?;
    if before.revision()? != query::snapshot()?.revision()? {
        return Err(DisplayError::new(
            "topology_changed",
            "display topology changed after native validation",
        ));
    }
    if let Some(route) = route {
        apply::bound_inactive_path(route, before)?;
    }
    apply::set(&requested, true).map_err(DisplayError::after_mutation)?;
    let initial_state = verify_applied_configuration(configuration, query::snapshot())?;
    if let Some(route) = route {
        verify_activated_route(route, &initial_state)?;
    }
    for display in &configuration.displays {
        if selected.is_some_and(|id| !display.id.eq_ignore_ascii_case(id)) {
            continue;
        }
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
    if let Some(route) = route {
        query::verify_newly_active_hdr_is_off(&route.id).map_err(DisplayError::after_mutation)?;
    }
    let final_state = verify_applied_configuration(configuration, query::snapshot())?;
    verify_requested_hdr(configuration, &final_state)?;
    if let Some(route) = route {
        verify_activated_route(route, &final_state)?;
    }
    if let Some(selected) = selected
        && !apply::verifies_untouched(configuration, before, &final_state, selected)
    {
        return Err(DisplayError::new(
            "verification_failed",
            "untouched Windows display path changed during apply",
        )
        .after_mutation());
    }
    Ok(())
}

fn verify_activated_route(route: &RouteBinding, readback: &Snapshot) -> DisplayResult<()> {
    let actual = readback
        .paths
        .iter()
        .find(|path| path.id.eq_ignore_ascii_case(&route.id))
        .ok_or_else(|| {
            DisplayError::new("verification_failed", "newly active display disappeared")
                .after_mutation()
        })?;
    if actual.source_name != route.source_name
        || actual.source_address != route.source_address
        || actual.target_address != route.target_address
    {
        return Err(DisplayError::new(
            "verification_failed",
            "activated display used a different physical route",
        )
        .after_mutation());
    }
    Ok(())
}

fn verify_applied_configuration(
    configuration: &Configuration,
    readback: DisplayResult<Snapshot>,
) -> DisplayResult<Snapshot> {
    let readback = readback.map_err(DisplayError::after_mutation)?;
    if !apply::verifies(configuration, &readback) {
        return Err(DisplayError::new(
            "verification_failed",
            "Windows display state differs from the requested configuration",
        )
        .after_mutation());
    }
    Ok(readback)
}

fn verify_requested_hdr(configuration: &Configuration, readback: &Snapshot) -> DisplayResult<()> {
    for display in &configuration.displays {
        let Some(requested) = display.hdr_enabled else {
            continue;
        };
        let actual = readback
            .paths
            .iter()
            .find(|path| path.id.eq_ignore_ascii_case(&display.id))
            .ok_or_else(|| {
                DisplayError::new("verification_failed", "requested HDR target disappeared")
                    .after_mutation()
            })?
            .hdr_enabled;
        if (requested && actual != Some(true)) || (!requested && actual == Some(true)) {
            return Err(DisplayError::new(
                "verification_failed",
                "Windows HDR state differs from the requested state after all display changes",
            )
            .after_mutation());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::conversion::{native_signal, native_source};
    use super::*;
    use crate::model::{
        Address, DisplayPath, DisplaySetting, Rational, Rotation, Signal, SourceMode,
    };
    use windows_sys::Win32::Devices::Display::*;
    use windows_sys::Win32::Foundation::LUID;
    use windows_sys::Win32::Graphics::Gdi::DISPLAYCONFIG_PATH_MODE_IDX_INVALID;

    fn synthetic_bound(snapshot: &Snapshot) -> query::ConfigurationBuffers {
        let mut paths = Vec::new();
        let mut modes = Vec::new();
        for saved in &snapshot.paths {
            let mut path = DISPLAYCONFIG_PATH_INFO::default();
            path.sourceInfo.adapterId = LUID {
                LowPart: saved.source_address.adapter_low,
                HighPart: saved.source_address.adapter_high,
            };
            path.sourceInfo.id = saved.source_address.id;
            path.targetInfo.adapterId = LUID {
                LowPart: saved.target_address.adapter_low,
                HighPart: saved.target_address.adapter_high,
            };
            path.targetInfo.id = saved.target_address.id;
            path.sourceInfo.Anonymous.modeInfoIdx = modes.len() as u32;
            modes.push(DISPLAYCONFIG_MODE_INFO {
                infoType: DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE,
                id: path.sourceInfo.id,
                adapterId: path.sourceInfo.adapterId,
                Anonymous: DISPLAYCONFIG_MODE_INFO_0 {
                    sourceMode: native_source(&saved.source),
                },
            });
            path.targetInfo.Anonymous.modeInfoIdx = modes.len() as u32;
            modes.push(DISPLAYCONFIG_MODE_INFO {
                infoType: DISPLAYCONFIG_MODE_INFO_TYPE_TARGET,
                id: path.targetInfo.id,
                adapterId: path.targetInfo.adapterId,
                Anonymous: DISPLAYCONFIG_MODE_INFO_0 {
                    targetMode: DISPLAYCONFIG_TARGET_MODE {
                        targetVideoSignalInfo: native_signal(&saved.signal),
                    },
                },
            });
            path.targetInfo.scaling = saved.scaling;
            path.targetInfo.rotation = saved.rotation;
            path.flags = saved.path_flags;
            paths.push(path);
        }
        query::ConfigurationBuffers { paths, modes }
    }

    fn readback_path(id: &str, x: i32, hdr_enabled: Option<bool>) -> DisplayPath {
        let refresh = Rational {
            numerator: 60,
            denominator: 1,
        };
        DisplayPath {
            id: id.into(),
            source_name: id.into(),
            source_address: Address {
                adapter_low: 1,
                adapter_high: 0,
                id: x as u32,
            },
            target_address: Address {
                adapter_low: 1,
                adapter_high: 0,
                id: x as u32 + 1,
            },
            source: SourceMode {
                width: 1920,
                height: 1080,
                pixel_format: 4,
                x,
                y: 0,
            },
            signal: Signal {
                pixel_rate: 148_500_000,
                horizontal_sync: refresh,
                vertical_sync: refresh,
                active_width: 1920,
                active_height: 1080,
                total_width: 2200,
                total_height: 1125,
                video_standard: 0,
                scan_line_ordering: 1,
            },
            rotation: 1,
            scaling: 1,
            refresh,
            scan_line_ordering: 1,
            output_technology: 5,
            source_flags: 0,
            target_flags: 0,
            path_flags: 1,
            hdr_enabled,
        }
    }

    fn two_target_configuration(second_hdr: Option<bool>) -> Configuration {
        let mut configuration = requested();
        let mut second = configuration.displays[0].clone();
        second.id = "monitor.two".into();
        second.x = 1920;
        second.hdr_enabled = second_hdr;
        configuration.displays.push(second);
        configuration
    }

    #[test]
    fn preserving_builder_keeps_untouched_native_modes_and_reindexes_selected() {
        let mut unchanged = readback_path("monitor.one", 0, Some(false));
        unchanged.source.pixel_format = 7;
        unchanged.scaling = 3;
        unchanged.signal.pixel_rate = 123_456_789;
        unchanged.path_flags = 5;
        let selected = readback_path("monitor.two", 1920, Some(false));
        let before = Snapshot {
            format_version: 1,
            paths: vec![unchanged.clone(), selected],
        };
        let mut configuration = before.configuration().unwrap();
        configuration.displays[0].x = -1920;
        configuration.displays[1].x = 0;
        configuration.displays[1].width = 1280;
        let bound = apply::preserve_bound(
            synthetic_bound(&before),
            &configuration,
            &before,
            "monitor.two",
        )
        .unwrap();
        assert_eq!(bound.modes.len(), 3);
        assert_eq!(bound.paths[0].targetInfo.scaling, 3);
        assert_eq!(bound.paths[0].flags, 5);
        // SAFETY: preserve_bound remapped these indexes to their validated
        // SOURCE and TARGET variants in the returned mode array.
        let source_index = unsafe { bound.paths[0].sourceInfo.Anonymous.modeInfoIdx } as usize;
        let target_index = unsafe { bound.paths[0].targetInfo.Anonymous.modeInfoIdx } as usize;
        let source = unsafe { bound.modes[source_index].Anonymous.sourceMode };
        let target = unsafe { bound.modes[target_index].Anonymous.targetMode };
        assert_eq!(
            bound.modes[source_index].infoType,
            DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE
        );
        assert_eq!(
            bound.modes[target_index].infoType,
            DISPLAYCONFIG_MODE_INFO_TYPE_TARGET
        );
        assert_eq!(
            bound.modes[source_index].adapterId.LowPart,
            unchanged.source_address.adapter_low
        );
        assert_eq!(
            bound.modes[target_index].adapterId.LowPart,
            unchanged.target_address.adapter_low
        );
        assert_eq!(bound.modes[source_index].id, unchanged.source_address.id);
        assert_eq!(bound.modes[target_index].id, unchanged.target_address.id);
        assert_eq!(source.pixelFormat, 7);
        assert_eq!(source.position.x, -1920);
        assert_eq!(target.targetVideoSignalInfo.pixelRate, 123_456_789);
        assert_eq!(
            unsafe { bound.paths[1].targetInfo.Anonymous.modeInfoIdx },
            DISPLAYCONFIG_PATH_MODE_IDX_INVALID
        );
        let selected_source_index =
            unsafe { bound.paths[1].sourceInfo.Anonymous.modeInfoIdx } as usize;
        let selected_source = unsafe { bound.modes[selected_source_index].Anonymous.sourceMode };
        assert_eq!(selected_source.width, 1280);
        assert_eq!(selected_source.position.x, 0);
        let mut changed = configuration.clone();
        changed.displays[0].width = 1600;
        assert_eq!(
            apply::preserve_bound(synthetic_bound(&before), &changed, &before, "monitor.two")
                .err()
                .unwrap()
                .code,
            "unsupported_topology"
        );
        let mut wrong_type = synthetic_bound(&before);
        wrong_type.paths[0].targetInfo.Anonymous.modeInfoIdx = 0;
        assert_eq!(
            apply::preserve_bound(wrong_type, &configuration, &before, "monitor.two")
                .err()
                .unwrap()
                .code,
            "unsupported_topology"
        );
        let mut after = before.clone();
        after.paths[0].source.x = -1920;
        after.paths[1].source.x = 0;
        after.paths[1].source.width = 1280;
        assert!(apply::verifies_untouched(
            &configuration,
            &before,
            &after,
            "monitor.two"
        ));
        after.paths[0].scaling = 1;
        assert!(!apply::verifies_untouched(
            &configuration,
            &before,
            &after,
            "monitor.two"
        ));
    }

    #[test]
    fn inactive_builder_appends_one_bound_route_and_rejects_ambiguous_candidates() {
        let mut saved = readback_path("monitor.one", 0, Some(false));
        saved.source.pixel_format = 7;
        saved.scaling = 3;
        saved.signal.pixel_rate = 123_456_789;
        let before = Snapshot {
            format_version: 1,
            paths: vec![saved.clone()],
        };
        let mut configuration = before.configuration().unwrap();
        let mut selected = configuration.displays[0].clone();
        selected.id = "monitor.two".into();
        selected.width = 1280;
        selected.x = 1920;
        selected.hdr_enabled = None;
        configuration.displays.push(selected);
        let route = RouteBinding {
            id: "monitor.two".into(),
            source_name: "DISPLAY2".into(),
            source_address: Address {
                adapter_low: 1,
                adapter_high: 0,
                id: 42,
            },
            target_address: Address {
                adapter_low: 1,
                adapter_high: 0,
                id: 43,
            },
            output_technology: 0,
        };
        let mut path = DISPLAYCONFIG_PATH_INFO::default();
        path.sourceInfo.adapterId = LUID {
            LowPart: 1,
            HighPart: 0,
        };
        path.sourceInfo.id = 42;
        path.targetInfo.adapterId = LUID {
            LowPart: 1,
            HighPart: 0,
        };
        path.targetInfo.id = 43;
        path.targetInfo.targetAvailable = 1;
        let bound = apply::append_bound_inactive(
            synthetic_bound(&before),
            &configuration,
            &before,
            "monitor.two",
            &route,
            path,
        )
        .unwrap();
        assert_eq!(bound.paths.len(), 2);
        assert_eq!(bound.modes.len(), 3);
        assert_eq!(bound.paths[0].targetInfo.scaling, 3);
        assert_eq!(bound.paths[0].flags, saved.path_flags);
        // SAFETY: append_bound_inactive compacts and type-checks every mode
        // index before returning these initialized SOURCE/TARGET records.
        let source_index = unsafe { bound.paths[0].sourceInfo.Anonymous.modeInfoIdx } as usize;
        let target_index = unsafe { bound.paths[0].targetInfo.Anonymous.modeInfoIdx } as usize;
        assert_eq!(
            unsafe { bound.modes[source_index].Anonymous.sourceMode }.pixelFormat,
            7
        );
        assert_eq!(
            unsafe { bound.modes[target_index].Anonymous.targetMode }
                .targetVideoSignalInfo
                .pixelRate,
            123_456_789
        );
        assert_eq!(
            bound.paths[1].targetInfo.scanLineOrdering,
            DISPLAYCONFIG_SCANLINE_ORDERING_UNSPECIFIED
        );
        assert_eq!(
            unsafe { bound.paths[1].targetInfo.Anonymous.modeInfoIdx },
            DISPLAYCONFIG_PATH_MODE_IDX_INVALID
        );
        let selected_index = unsafe { bound.paths[1].sourceInfo.Anonymous.modeInfoIdx } as usize;
        assert_eq!(
            unsafe { bound.modes[selected_index].Anonymous.sourceMode }.width,
            1280
        );
        let device = || query::DeviceNames {
            id: "monitor.two".into(),
            friendly: String::new(),
            source: "DISPLAY2".into(),
        };
        let mut occupied = path;
        occupied.sourceInfo.id = saved.source_address.id;
        let accepted = apply::select_candidate_route(
            vec![(occupied, device()), (path, device())],
            &before,
            "unsupported_topology",
        )
        .unwrap();
        assert_eq!(accepted.0.sourceInfo.id, 42);
        let mut other_target = path;
        other_target.targetInfo.id = 44;
        assert_eq!(
            apply::select_candidate_route(
                vec![(path, device()), (other_target, device())],
                &before,
                "unsupported_topology"
            )
            .err()
            .unwrap()
            .code,
            "unsupported_topology"
        );
        let mut other_source = path;
        other_source.sourceInfo.id = 45;
        assert_eq!(
            apply::select_candidate_route(
                vec![(path, device()), (other_source, device())],
                &before,
                "unsupported_topology"
            )
            .err()
            .unwrap()
            .code,
            "unsupported_topology"
        );
        let mut wireless = path;
        wireless.targetInfo.outputTechnology = DISPLAYCONFIG_OUTPUT_TECHNOLOGY_MIRACAST;
        assert_eq!(
            apply::select_candidate_route(
                vec![(wireless, device())],
                &before,
                "unsupported_topology"
            )
            .err()
            .unwrap()
            .code,
            "unsupported_topology"
        );
        let conflicting_name = query::DeviceNames {
            source: "DISPLAY-OTHER".into(),
            ..device()
        };
        assert_eq!(
            apply::select_candidate_route(
                vec![(path, device()), (path, conflicting_name)],
                &before,
                "unsupported_topology"
            )
            .err()
            .unwrap()
            .code,
            "unsupported_topology"
        );
    }

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
    fn newly_activated_route_readback_rejects_wrong_physical_path_as_mutated() {
        let selected = readback_path("monitor.two", 1920, Some(false));
        let route = RouteBinding {
            id: selected.id.clone(),
            source_name: selected.source_name.clone(),
            source_address: selected.source_address.clone(),
            target_address: selected.target_address.clone(),
            output_technology: selected.output_technology,
        };
        let mut readback = Snapshot {
            format_version: 1,
            paths: vec![readback_path("monitor.one", 0, Some(false)), selected],
        };
        assert!(verify_activated_route(&route, &readback).is_ok());

        readback.paths[1].target_address.id += 1;
        let error = verify_activated_route(&route, &readback).unwrap_err();
        assert_eq!(error.code, "verification_failed");
        assert!(error.may_have_changed);

        readback.paths[1].target_address = route.target_address.clone();
        readback.paths[1].source_address.id += 1;
        let error = verify_activated_route(&route, &readback).unwrap_err();
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

    #[test]
    fn final_hdr_readback_rejects_earlier_target_drift() {
        let configuration = two_target_configuration(Some(true));
        let readback = Snapshot {
            format_version: 1,
            paths: vec![
                readback_path("monitor.two", 1920, Some(true)),
                readback_path("MONITOR.ONE", 0, Some(false)),
            ],
        };
        assert!(apply::verifies(&configuration, &readback));
        let error = verify_requested_hdr(&configuration, &readback).unwrap_err();
        assert_eq!(error.code, "verification_failed");
        assert!(error.may_have_changed);
    }

    #[test]
    fn final_hdr_readback_accepts_both_requested_targets() {
        let configuration = two_target_configuration(Some(true));
        let readback = Snapshot {
            format_version: 1,
            paths: vec![
                readback_path("monitor.one", 0, Some(true)),
                readback_path("monitor.two", 1920, Some(true)),
            ],
        };
        assert!(apply::verifies(&configuration, &readback));
        verify_requested_hdr(&configuration, &readback).unwrap();
    }

    #[test]
    fn final_hdr_readback_accepts_unsupported_sdr_and_ignores_unspecified_hdr() {
        let mut configuration = two_target_configuration(Some(false));
        let readback = Snapshot {
            format_version: 1,
            paths: vec![
                readback_path("monitor.one", 0, Some(true)),
                readback_path("monitor.two", 1920, None),
            ],
        };
        assert!(apply::verifies(&configuration, &readback));
        verify_requested_hdr(&configuration, &readback).unwrap();
        configuration.displays[0].hdr_enabled = None;
        verify_requested_hdr(&configuration, &readback).unwrap();
    }
}
