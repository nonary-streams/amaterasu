mod apply;
mod conversion;
mod query;

use super::Platform;
use super::TopologyShape;
use crate::model::{
    Address, AvailableMode, Configuration, DisplayError, DisplayResult, RouteBinding, Snapshot,
};
use serde_json::{Value, json};
use std::time::Duration;

const CCD_SETTLE_ATTEMPTS: usize = 6;
const CCD_SETTLE_INTERVAL: Duration = Duration::from_millis(50);

fn settled_readback(
    mut read: impl FnMut() -> DisplayResult<Snapshot>,
    mut validate_identity: impl FnMut(&Snapshot) -> DisplayResult<()>,
    mut verify: impl FnMut(&Snapshot) -> DisplayResult<()>,
    mut wait: impl FnMut(),
) -> DisplayResult<Snapshot> {
    for attempt in 0..CCD_SETTLE_ATTEMPTS {
        match read() {
            Ok(snapshot) => {
                validate_identity(&snapshot).map_err(DisplayError::after_mutation)?;
                match verify(&snapshot) {
                    Ok(()) => return Ok(snapshot),
                    Err(error) if attempt + 1 == CCD_SETTLE_ATTEMPTS => {
                        return Err(error.after_mutation());
                    }
                    Err(_) => {}
                }
            }
            // A failed native query can mean hotplug or lost identity. Only a
            // successfully decoded state with the expected physical routes
            // is safe to treat as asynchronously settling.
            Err(error) => return Err(error.after_mutation()),
        }
        wait();
    }
    unreachable!("bounded CCD settling always returns on its last attempt")
}

fn settle(
    validate_identity: impl FnMut(&Snapshot) -> DisplayResult<()>,
    verify: impl FnMut(&Snapshot) -> DisplayResult<()>,
) -> DisplayResult<Snapshot> {
    settled_readback(query::snapshot, validate_identity, verify, || {
        std::thread::sleep(CCD_SETTLE_INTERVAL)
    })
}

fn reject_foreign_apply_state(
    configuration: &Configuration,
    before: &Snapshot,
    route: Option<&RouteBinding>,
    current: &Snapshot,
) -> DisplayResult<()> {
    for path in &current.paths {
        if let Some(saved) = before
            .paths
            .iter()
            .find(|saved| saved.id.eq_ignore_ascii_case(&path.id))
        {
            if path.target_address != saved.target_address || path.source_name != saved.source_name
            {
                return Err(DisplayError::new(
                    "topology_changed",
                    "display physical identity changed after CCD apply",
                ));
            }
        } else if !configuration
            .displays
            .iter()
            .any(|wanted| wanted.id.eq_ignore_ascii_case(&path.id))
            || route.is_none_or(|bound| {
                !bound.id.eq_ignore_ascii_case(&path.id)
                    || bound.target_address != path.target_address
                    || bound.source_address != path.source_address
                    || bound.source_name != path.source_name
            })
        {
            return Err(DisplayError::new(
                "topology_changed",
                "new display used an unexpected physical route",
            ));
        }
    }
    Ok(())
}

fn reject_foreign_restore_state(
    saved: &Snapshot,
    previous: &Snapshot,
    rebound_targets: &[(String, Address)],
    current: &Snapshot,
) -> DisplayResult<()> {
    if current.paths.iter().any(|path| {
        let old = previous
            .paths
            .iter()
            .find(|old| old.id.eq_ignore_ascii_case(&path.id));
        let baseline = saved
            .paths
            .iter()
            .find(|baseline| baseline.id.eq_ignore_ascii_case(&path.id));
        let expected_target = rebound_targets
            .iter()
            .find(|(id, _)| id.eq_ignore_ascii_case(&path.id));
        let known_route = old
            .or(baseline)
            .is_some_and(|known| known.source_name == path.source_name);
        !known_route
            || old.is_some_and(|known| known.target_address != path.target_address)
            || (old.is_none()
                && baseline.is_some()
                && expected_target.is_none_or(|(_, target)| *target != path.target_address))
    }) {
        return Err(DisplayError::new(
            "topology_changed",
            "unexpected display route after CCD restore",
        ));
    }
    Ok(())
}
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
    fn bind_clone(
        &mut self,
        display: &str,
        source: &Address,
        before: &Snapshot,
    ) -> DisplayResult<RouteBinding> {
        apply::bind_clone(display, source, before)
    }
    fn prepare_preserving(
        &mut self,
        configuration: &Configuration,
        selected: &str,
        route: Option<&RouteBinding>,
        shape: TopologyShape,
    ) -> DisplayResult<Snapshot> {
        prepare_configuration(configuration, Some((selected, shape)), route)
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
        shape: TopologyShape,
        guard: &mut dyn FnMut() -> DisplayResult<()>,
    ) -> DisplayResult<()> {
        apply_configuration(configuration, before, Some((selected, shape)), route, guard)
    }
    fn restore(
        &mut self,
        snapshot: &Snapshot,
        guard: &mut dyn FnMut() -> DisplayResult<()>,
    ) -> DisplayResult<()> {
        query::check_legacy_hdr_recovery(snapshot)?;
        let previous = query::snapshot()?;
        let requested = apply::restoration(snapshot)?;
        let rebound_targets: Vec<_> = snapshot
            .paths
            .iter()
            .zip(&requested.paths)
            .map(|(saved, native)| {
                (
                    saved.id.clone(),
                    conversion::address(native.targetInfo.adapterId, native.targetInfo.id),
                )
            })
            .collect();
        apply::set(&requested, false)?;
        guard()?;
        apply::set(&requested, true).map_err(DisplayError::after_mutation)?;
        let rebound = settle(
            |current| reject_foreign_restore_state(snapshot, &previous, &rebound_targets, current),
            |current| verify_restoration_routes(snapshot, current),
        )?;
        for path in &snapshot.paths {
            if let Some(enabled) = path.hdr_enabled {
                let current = rebound
                    .paths
                    .iter()
                    .find(|candidate| candidate.id.eq_ignore_ascii_case(&path.id))
                    .expect("verified restoration contains each saved target");
                query::set_advanced_color(current, enabled)
                    .map_err(DisplayError::after_mutation)?;
            }
        }
        settle(
            |current| reject_foreign_restore_state(snapshot, &previous, &rebound_targets, current),
            |current| {
                if snapshot.equivalent(current) && saved_clone_priorities_match(snapshot, current) {
                    Ok(())
                } else {
                    Err(DisplayError::new(
                        "verification_failed",
                        "restored CCD mode/topology or clone priority differs from the saved snapshot",
                    ))
                }
            },
        )?;
        Ok(())
    }
}

fn prepare_configuration(
    configuration: &Configuration,
    selected: Option<(&str, TopologyShape)>,
    route: Option<&RouteBinding>,
) -> DisplayResult<Snapshot> {
    let before = query::snapshot()?;
    check_inactive_only_recovery(&before, selected, route)?;
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
    if hdr_only_target(configuration, &before, selected, route).is_some() {
        return Ok(before);
    }
    let requested = match selected {
        Some((selected, shape)) => {
            apply::preserving(configuration, &before, selected, route, shape)?
        }
        None => apply::requested(configuration)?,
    };
    if selected.is_some_and(|(_, shape)| shape == TopologyShape::OnlySelected) {
        // This request removes every saved path. Prove that the journal
        // baseline can be rebound and restored before arming the journal.
        let restoration = apply::restoration(&before)?;
        apply::set(&restoration, false)?;
    }
    apply::set(&requested, false)?;
    Ok(before)
}

fn apply_configuration(
    configuration: &Configuration,
    before: &Snapshot,
    selected: Option<(&str, TopologyShape)>,
    route: Option<&RouteBinding>,
    guard: &mut dyn FnMut() -> DisplayResult<()>,
) -> DisplayResult<()> {
    check_inactive_only_recovery(before, selected, route)?;
    if before.revision()? != query::snapshot()?.revision()? {
        return Err(DisplayError::new(
            "topology_changed",
            "display configuration changed before the durable apply boundary",
        ));
    }
    if let Some(target) = hdr_only_target(configuration, before, selected, route) {
        guard()?;
        if before.revision()? != query::snapshot()?.revision()? {
            return Err(DisplayError::new(
                "topology_changed",
                "display configuration changed before the HDR-only apply boundary",
            ));
        }
        let enabled = configuration
            .displays
            .iter()
            .find(|display| display.id.eq_ignore_ascii_case(&target.id))
            .and_then(|display| display.hdr_enabled)
            .expect("HDR-only target has a requested state");
        query::set_advanced_color(target, enabled).map_err(DisplayError::after_mutation)?;
        settle(
            |current| reject_foreign_apply_state(configuration, before, route, current),
            |current| {
                verify_applied_configuration(configuration, Ok(current.clone()))?;
                verify_requested_hdr(configuration, current)?;
                verify_exact_hdr_only_result(before, target, enabled, current)
            },
        )?;
        return Ok(());
    }
    let requested = match selected {
        Some((selected, shape)) => {
            apply::preserving(configuration, before, selected, route, shape)?
        }
        None => apply::requested(configuration)?,
    };
    if selected.is_some_and(|(_, shape)| shape == TopologyShape::OnlySelected) {
        let restoration = apply::restoration(before)?;
        apply::set(&restoration, false)?;
    }
    apply::set(&requested, false)?;
    guard()?;
    if before.revision()? != query::snapshot()?.revision()? {
        return Err(DisplayError::new(
            "topology_changed",
            "display topology changed after native validation",
        ));
    }
    if let Some(route) = route {
        if selected.is_some_and(|(_, shape)| shape == TopologyShape::CloneActive) {
            apply::bound_clone_path(route)?;
        } else {
            apply::bound_inactive_path(route, before)?;
        }
    }
    apply::set(&requested, true).map_err(DisplayError::after_mutation)?;
    settle(
        |current| reject_foreign_apply_state(configuration, before, route, current),
        |current| {
            verify_applied_configuration(configuration, Ok(current.clone()))?;
            if let Some((selected, TopologyShape::PrimaryActive)) = selected {
                verify_primary_path_priority(selected, current)?;
            }
            if let Some(route) = route {
                verify_activated_route(route, current)?;
            }
            Ok(())
        },
    )?;
    for display in &configuration.displays {
        if selected.is_some_and(|(id, _)| !display.id.eq_ignore_ascii_case(id)) {
            continue;
        }
        if let Some(enabled) = display.hdr_enabled {
            if !enabled
                && !before
                    .paths
                    .iter()
                    .any(|path| path.id.eq_ignore_ascii_case(&display.id))
            {
                let route = route.ok_or_else(|| {
                    DisplayError::new("topology_changed", "inactive HDR target has no bound route")
                        .after_mutation()
                })?;
                query::verify_newly_active_hdr_is_off(route)
                    .map_err(DisplayError::after_mutation)?;
            } else {
                let saved = before
                    .paths
                    .iter()
                    .find(|path| path.id.eq_ignore_ascii_case(&display.id))
                    .ok_or_else(|| {
                        DisplayError::new("topology_changed", "HDR target has no saved route")
                            .after_mutation()
                    })?;
                guard().map_err(DisplayError::after_mutation)?;
                query::set_advanced_color(saved, enabled).map_err(DisplayError::after_mutation)?;
            }
        }
    }
    if let Some(route) = route {
        query::verify_newly_active_hdr_is_off(route).map_err(DisplayError::after_mutation)?;
    }
    settle(
        |current| reject_foreign_apply_state(configuration, before, route, current),
        |current| {
            verify_applied_configuration(configuration, Ok(current.clone()))?;
            verify_requested_hdr(configuration, current)?;
            if let Some((selected, TopologyShape::PrimaryActive)) = selected {
                verify_primary_path_priority(selected, current)?;
            }
            if let Some(route) = route {
                verify_activated_route(route, current)?;
            }
            if let Some((
                selected,
                TopologyShape::PreserveActive
                | TopologyShape::PrimaryActive
                | TopologyShape::TranslateActive,
            )) = selected
                && !apply::verifies_untouched(configuration, before, current, selected)
            {
                return Err(DisplayError::new(
                    "verification_failed",
                    "untouched Windows display path changed during apply",
                )
                .after_mutation());
            }
            Ok(())
        },
    )?;
    Ok(())
}

fn hdr_only_target<'a>(
    configuration: &Configuration,
    before: &'a Snapshot,
    selected: Option<(&str, TopologyShape)>,
    route: Option<&RouteBinding>,
) -> Option<&'a crate::model::DisplayPath> {
    if selected.is_some()
        || route.is_some()
        || !apply::verifies(configuration, before)
        || before.paths.iter().any(|path| {
            path.source.pixel_format
                != windows_sys::Win32::Devices::Display::DISPLAYCONFIG_PIXELFORMAT_32BPP
                || path.path_flags != windows_sys::Win32::Graphics::Gdi::DISPLAYCONFIG_PATH_ACTIVE
        })
    {
        return None;
    }
    let mut changed = configuration.displays.iter().filter_map(|display| {
        let saved = before
            .paths
            .iter()
            .find(|path| path.id.eq_ignore_ascii_case(&display.id))?;
        (saved.hdr_enabled.is_some()
            && display.hdr_enabled.is_some()
            && display.hdr_enabled != saved.hdr_enabled)
            .then_some(saved)
    });
    let target = changed.next()?;
    changed.next().is_none().then_some(target)
}

fn verify_exact_hdr_only_result(
    before: &Snapshot,
    target: &crate::model::DisplayPath,
    enabled: bool,
    current: &Snapshot,
) -> DisplayResult<()> {
    let mut exact_expected = before.clone();
    exact_expected
        .paths
        .iter_mut()
        .find(|path| path.id.eq_ignore_ascii_case(&target.id))
        .expect("HDR-only target belongs to the baseline")
        .hdr_enabled = Some(enabled);
    if exact_expected.revision()? != current.revision()? {
        return Err(DisplayError::new(
            "verification_failed",
            "HDR-only apply changed another CCD or display state",
        )
        .after_mutation());
    }
    Ok(())
}

fn verify_restoration_routes(saved: &Snapshot, current: &Snapshot) -> DisplayResult<()> {
    let mut expected = saved.clone();
    for path in &mut expected.paths {
        path.hdr_enabled = current
            .paths
            .iter()
            .find(|candidate| candidate.id.eq_ignore_ascii_case(&path.id))
            .map(|candidate| candidate.hdr_enabled)
            .ok_or_else(|| {
                DisplayError::new(
                    "topology_changed",
                    "saved HDR target disappeared during restore",
                )
                .after_mutation()
            })?;
    }
    if !expected.equivalent(current) {
        return Err(DisplayError::new(
            "topology_changed",
            "restored display routes differ from the recovery baseline",
        )
        .after_mutation());
    }
    if !saved_clone_priorities_match(saved, current) {
        return Err(DisplayError::new(
            "verification_failed",
            "restored clone path priority differs from the saved snapshot",
        )
        .after_mutation());
    }
    Ok(())
}

fn saved_clone_priorities_match(saved: &Snapshot, current: &Snapshot) -> bool {
    let has_clone = saved.paths.iter().enumerate().any(|(index, path)| {
        saved
            .paths
            .iter()
            .skip(index + 1)
            .any(|peer| peer.source_address == path.source_address)
    });
    !has_clone
        || (saved.paths.len() == current.paths.len()
            && saved
                .paths
                .iter()
                .zip(&current.paths)
                .all(|(saved, observed)| saved.id.eq_ignore_ascii_case(&observed.id)))
}

fn verify_primary_path_priority(selected: &str, current: &Snapshot) -> DisplayResult<()> {
    if current
        .paths
        .first()
        .is_some_and(|path| path.id.eq_ignore_ascii_case(selected))
    {
        Ok(())
    } else {
        Err(DisplayError::new(
            "verification_failed",
            "Windows did not give the selected primary target first CCD path priority",
        )
        .after_mutation())
    }
}

fn check_inactive_only_recovery(
    before: &Snapshot,
    selected: Option<(&str, TopologyShape)>,
    route: Option<&RouteBinding>,
) -> DisplayResult<()> {
    if selected.is_some_and(|(_, shape)| shape == TopologyShape::OnlySelected)
        && route.is_some()
        && before.paths.iter().any(|path| path.hdr_enabled.is_none())
    {
        return Err(DisplayError::new(
            "unsupported_topology",
            "saved display HDR state is not queryable for selected-only recovery",
        ));
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
        let mut source_indexes = std::collections::BTreeMap::new();
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
            let source_key = (
                saved.source_address.adapter_low,
                saved.source_address.adapter_high,
                saved.source_address.id,
            );
            let source_index = if let Some(index) = source_indexes.get(&source_key) {
                *index
            } else {
                let index = modes.len() as u32;
                source_indexes.insert(source_key, index);
                modes.push(DISPLAYCONFIG_MODE_INFO {
                    infoType: DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE,
                    id: path.sourceInfo.id,
                    adapterId: path.sourceInfo.adapterId,
                    Anonymous: DISPLAYCONFIG_MODE_INFO_0 {
                        sourceMode: native_source(&saved.source),
                    },
                });
                index
            };
            path.sourceInfo.Anonymous.modeInfoIdx = source_index;
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
    fn preserving_clone_changes_one_shared_source_and_keeps_peer_target_state() {
        for selected in ["monitor.one", "monitor.two"] {
            let first = readback_path("monitor.one", 0, Some(false));
            let mut second = readback_path("monitor.two", 0, Some(true));
            second.source_name = first.source_name.clone();
            second.source_address = first.source_address.clone();
            second.target_address.id = 2;
            let before = Snapshot {
                format_version: 1,
                paths: vec![first, second],
            };
            let mut configuration = before.configuration().unwrap();
            for display in &mut configuration.displays {
                display.width = 1280;
                display.height = 720;
            }
            configuration
                .displays
                .iter_mut()
                .find(|display| display.id == selected)
                .unwrap()
                .refresh
                .numerator = 75;
            let bound =
                apply::preserve_bound(synthetic_bound(&before), &configuration, &before, selected)
                    .unwrap();
            assert_eq!(
                unsafe { bound.paths[0].sourceInfo.Anonymous.modeInfoIdx },
                unsafe { bound.paths[1].sourceInfo.Anonymous.modeInfoIdx }
            );
            let index = unsafe { bound.paths[0].sourceInfo.Anonymous.modeInfoIdx } as usize;
            let source = unsafe { bound.modes[index].Anonymous.sourceMode };
            assert_eq!((source.width, source.height), (1280, 720));
            assert_eq!(bound.modes.len(), 2);
            let mut after = before.clone();
            for path in &mut after.paths {
                path.source.width = 1280;
                path.source.height = 720;
            }
            after
                .paths
                .iter_mut()
                .find(|path| path.id == selected)
                .unwrap()
                .refresh
                .numerator = 75;
            assert!(apply::verifies(&configuration, &after));
            assert!(apply::verifies_untouched(
                &configuration,
                &before,
                &after,
                selected
            ));
            let mut false_clone = after.clone();
            false_clone.paths[1].source_address.id += 1;
            assert!(!apply::verifies(&configuration, &false_clone));
            let peer = after
                .paths
                .iter_mut()
                .find(|path| path.id != selected)
                .unwrap();
            peer.scaling += 1;
            assert!(!apply::verifies_untouched(
                &configuration,
                &before,
                &after,
                selected
            ));
        }
    }

    #[test]
    fn active_clone_primary_promotes_selected_path_without_splitting_source() {
        let first = readback_path("monitor.one", 1920, Some(false));
        let mut second = readback_path("monitor.two", 1920, Some(false));
        second.source_name = first.source_name.clone();
        second.source_address = first.source_address.clone();
        second.target_address.id = 99;
        second.scaling = 3;
        second.refresh.numerator = 75;
        let mut third = readback_path("monitor.three", 0, Some(false));
        third.target_address.id = 100;
        let before = Snapshot {
            format_version: 1,
            paths: vec![first, second, third],
        };
        let mut configuration = before.configuration().unwrap();
        for display in &mut configuration.displays {
            display.x -= 1920;
        }
        let bound = apply::preserve_bound(
            synthetic_bound(&before),
            &configuration,
            &before,
            "monitor.two",
        )
        .unwrap();
        let bound = apply::promote_primary_path(bound, &before, "monitor.two").unwrap();
        assert_eq!(bound.paths[0].targetInfo.id, 99);
        assert_eq!(
            bound.paths[1].targetInfo.id,
            before.paths[0].target_address.id
        );
        assert_eq!(bound.paths[2].targetInfo.id, 100);
        assert_eq!(
            bound.paths[0].targetInfo.scaling,
            DISPLAYCONFIG_SCALING_PREFERRED
        );
        assert_eq!(bound.paths[1].targetInfo.scaling, before.paths[0].scaling);
        assert_eq!(bound.paths[0].targetInfo.refreshRate.Numerator, 75);
        assert_eq!(
            unsafe { bound.paths[0].sourceInfo.Anonymous.modeInfoIdx },
            unsafe { bound.paths[1].sourceInfo.Anonymous.modeInfoIdx }
        );
        let index = unsafe { bound.paths[0].sourceInfo.Anonymous.modeInfoIdx } as usize;
        let source = unsafe { bound.modes[index].Anonymous.sourceMode };
        assert_eq!(source.position.x, 0);
        assert_eq!(
            bound.paths[2].sourceInfo.id,
            before.paths[2].source_address.id
        );
    }

    #[test]
    fn primary_clone_readback_retries_normalized_priority_and_fails_closed() {
        let first = readback_path("monitor.one", 0, Some(false));
        let mut second = readback_path("monitor.two", 0, Some(false));
        second.source_name = first.source_name.clone();
        second.source_address = first.source_address.clone();
        second.target_address.id = 99;
        let normalized = Snapshot {
            format_version: 1,
            paths: vec![first, second],
        };
        let mut wanted = normalized.clone();
        wanted.paths.swap(0, 1);
        let configuration = wanted.configuration().unwrap();
        assert!(apply::verifies(&configuration, &normalized));
        assert_eq!(
            verify_primary_path_priority("monitor.two", &normalized)
                .unwrap_err()
                .code,
            "verification_failed"
        );
        let mut reads = 0;
        let converged = settled_readback(
            || {
                reads += 1;
                Ok(if reads == 1 {
                    normalized.clone()
                } else {
                    wanted.clone()
                })
            },
            |_| Ok(()),
            |current| {
                verify_applied_configuration(&configuration, Ok(current.clone()))?;
                verify_primary_path_priority("monitor.two", current)
            },
            || {},
        );
        assert!(converged.is_ok());
        assert_eq!(reads, 2);
        let mut reads = 0;
        let denied = settled_readback(
            || {
                reads += 1;
                Ok(normalized.clone())
            },
            |_| Ok(()),
            |current| verify_primary_path_priority("monitor.two", current),
            || {},
        )
        .unwrap_err();
        assert_eq!(reads, CCD_SETTLE_ATTEMPTS);
        assert_eq!(denied.code, "verification_failed");
        assert!(denied.may_have_changed);
    }

    #[test]
    fn clone_restore_requires_saved_path_priority_before_lease_cleanup() {
        let first = readback_path("monitor.one", 0, Some(false));
        let mut second = readback_path("monitor.two", 0, Some(false));
        second.source_name = first.source_name.clone();
        second.source_address = first.source_address.clone();
        second.target_address.id = 99;
        let saved = Snapshot {
            format_version: 1,
            paths: vec![first, second],
        };
        let mut reordered = saved.clone();
        reordered.paths.swap(0, 1);
        assert!(saved.equivalent(&reordered));
        assert_eq!(
            verify_restoration_routes(&saved, &reordered)
                .unwrap_err()
                .code,
            "verification_failed"
        );
        let mut reads = 0;
        let recovered = settled_readback(
            || {
                reads += 1;
                Ok(if reads == 1 {
                    reordered.clone()
                } else {
                    saved.clone()
                })
            },
            |_| Ok(()),
            |current| verify_restoration_routes(&saved, current),
            || {},
        );
        assert!(recovered.is_ok());
        assert_eq!(reads, 2);
        let mut saved_with_peer = saved;
        saved_with_peer
            .paths
            .push(readback_path("monitor.three", 1920, Some(false)));
        let mut reordered_peer = saved_with_peer.clone();
        reordered_peer.paths.swap(1, 2);
        assert!(saved_with_peer.equivalent(&reordered_peer));
        assert!(!saved_clone_priorities_match(
            &saved_with_peer,
            &reordered_peer
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
            false,
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

    #[test]
    fn inactive_only_builder_excludes_saved_paths_and_uses_bound_route() {
        let before = Snapshot {
            format_version: 1,
            paths: vec![
                readback_path("monitor.one", 0, Some(false)),
                readback_path("monitor.two", 1920, Some(false)),
            ],
        };
        let mut wanted = requested().displays.remove(0);
        wanted.id = "monitor.three".into();
        wanted.hdr_enabled = Some(false);
        let configuration = Configuration {
            displays: vec![wanted],
        };
        let route = RouteBinding {
            id: "monitor.three".into(),
            source_name: "DISPLAY3".into(),
            source_address: Address {
                adapter_low: 1,
                adapter_high: 0,
                id: 52,
            },
            target_address: Address {
                adapter_low: 1,
                adapter_high: 0,
                id: 53,
            },
            output_technology: 0,
        };
        let mut path = DISPLAYCONFIG_PATH_INFO::default();
        path.sourceInfo.adapterId = LUID {
            LowPart: 1,
            HighPart: 0,
        };
        path.sourceInfo.id = 52;
        path.targetInfo.adapterId = LUID {
            LowPart: 1,
            HighPart: 0,
        };
        path.targetInfo.id = 53;
        path.targetInfo.targetAvailable = 1;
        let buffers =
            apply::only_from_bound_inactive(&configuration, &before, "monitor.three", &route, path)
                .unwrap();
        assert_eq!(buffers.paths.len(), 1);
        assert_eq!(buffers.modes.len(), 1);
        assert_eq!(buffers.paths[0].sourceInfo.id, 52);
        assert_eq!(buffers.paths[0].targetInfo.id, 53);
        assert_eq!(
            buffers.paths[0].targetInfo.scanLineOrdering,
            DISPLAYCONFIG_SCANLINE_ORDERING_UNSPECIFIED
        );
        // SAFETY: the builder created one initialized SOURCE mode and its index.
        let source_index = unsafe { buffers.paths[0].sourceInfo.Anonymous.modeInfoIdx } as usize;
        assert_eq!(source_index, 0);
        let source = unsafe { buffers.modes[source_index].Anonymous.sourceMode };
        assert_eq!((source.position.x, source.position.y), (0, 0));
        assert_eq!(source.width, 1920);
    }

    #[test]
    fn inactive_primary_builder_translates_two_saved_sources_without_changing_modes() {
        let mut left = readback_path("monitor.one", -1920, Some(false));
        let mut primary = readback_path("monitor.two", 0, Some(false));
        left.source.y = 800;
        left.source.pixel_format = 7;
        left.scaling = 3;
        left.signal.pixel_rate = 123_456_789;
        left.path_flags = 5;
        primary.source.pixel_format = 6;
        primary.scaling = 4;
        primary.signal.pixel_rate = 234_567_890;
        primary.path_flags = 9;
        let before = Snapshot {
            format_version: 1,
            paths: vec![left.clone(), primary.clone()],
        };
        let mut configuration = before.configuration().unwrap();
        configuration.displays[0].x = 1280;
        configuration.displays[1].x = 3200;
        configuration.displays[0].y = 0;
        configuration.displays[1].y = -800;
        let mut selected = requested().displays.remove(0);
        selected.id = "monitor.three".into();
        selected.width = 1280;
        selected.hdr_enabled = None;
        configuration.displays.push(selected);
        let route = RouteBinding {
            id: "monitor.three".into(),
            source_name: "DISPLAY3".into(),
            source_address: Address {
                adapter_low: 1,
                adapter_high: 0,
                id: 52,
            },
            target_address: Address {
                adapter_low: 1,
                adapter_high: 0,
                id: 53,
            },
            output_technology: 0,
        };
        let mut path = DISPLAYCONFIG_PATH_INFO::default();
        path.sourceInfo.adapterId = LUID {
            LowPart: 1,
            HighPart: 0,
        };
        path.sourceInfo.id = 52;
        path.targetInfo.adapterId = LUID {
            LowPart: 1,
            HighPart: 0,
        };
        path.targetInfo.id = 53;
        path.targetInfo.targetAvailable = 1;
        let buffers = apply::append_bound_inactive(
            synthetic_bound(&before),
            &configuration,
            &before,
            "monitor.three",
            &route,
            path,
            true,
        )
        .unwrap();
        assert_eq!(buffers.paths.len(), 3);
        assert_eq!(buffers.modes.len(), 5);
        assert_eq!(buffers.paths[0].targetInfo.id, 53);
        assert_eq!(
            (
                buffers.paths[1].targetInfo.id,
                buffers.paths[2].targetInfo.id
            ),
            (left.target_address.id, primary.target_address.id)
        );
        for (index, (saved, translated_x, translated_y)) in
            [(left.clone(), 1280, 0), (primary.clone(), 3200, -800)]
                .into_iter()
                .enumerate()
        {
            let old_path = &buffers.paths[index + 1];
            // SAFETY: the builder checked and compacted both mode indexes.
            let source_index = unsafe { old_path.sourceInfo.Anonymous.modeInfoIdx } as usize;
            let target_index = unsafe { old_path.targetInfo.Anonymous.modeInfoIdx } as usize;
            let source = unsafe { buffers.modes[source_index].Anonymous.sourceMode };
            let target = unsafe { buffers.modes[target_index].Anonymous.targetMode };
            assert_eq!(source.position.x, translated_x);
            assert_eq!(source.position.y, translated_y);
            assert_eq!(source.pixelFormat, saved.source.pixel_format);
            assert_eq!(
                (source.width, source.height),
                (saved.source.width, saved.source.height)
            );
            assert_eq!(
                target.targetVideoSignalInfo.pixelRate,
                saved.signal.pixel_rate
            );
            assert_eq!(old_path.targetInfo.scaling, saved.scaling);
            assert_eq!(old_path.flags, saved.path_flags);
        }
        let mut readback = before.clone();
        readback.paths[0].source.x = 1280;
        readback.paths[0].source.y = 0;
        readback.paths[1].source.x = 3200;
        readback.paths[1].source.y = -800;
        readback
            .paths
            .push(readback_path("monitor.three", 0, Some(false)));
        assert!(apply::verifies_untouched(
            &configuration,
            &before,
            &readback,
            "monitor.three"
        ));
        readback.paths[1].source.y += 1;
        assert!(!apply::verifies_untouched(
            &configuration,
            &before,
            &readback,
            "monitor.three"
        ));
        let mut wrong = configuration.clone();
        wrong.displays[1].x += 1;
        assert_eq!(
            apply::append_bound_inactive(
                synthetic_bound(&before),
                &wrong,
                &before,
                "monitor.three",
                &route,
                path,
                true,
            )
            .err()
            .unwrap()
            .code,
            "unsupported_topology"
        );
    }

    #[test]
    fn inactive_only_rejects_unknown_saved_hdr_before_journal() {
        let mut before = Snapshot {
            format_version: 1,
            paths: vec![readback_path("monitor.one", 0, None)],
        };
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
        let intent = Some(("monitor.two", TopologyShape::OnlySelected));
        assert_eq!(
            check_inactive_only_recovery(&before, intent, Some(&route))
                .unwrap_err()
                .code,
            "unsupported_topology"
        );
        before.paths[0].hdr_enabled = Some(false);
        assert!(check_inactive_only_recovery(&before, intent, Some(&route)).is_ok());
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
    fn ccd_settling_waits_for_exact_applied_mode() {
        let requested = requested();
        let mut before = Snapshot {
            format_version: 1,
            paths: vec![readback_path("monitor.one", 0, Some(true))],
        };
        before.paths[0].source.width = 1280;
        let mut after = before.clone();
        after.paths[0].source.width = 1920;
        let mut reads = 0;
        let mut waits = 0;
        let result = settled_readback(
            || {
                reads += 1;
                Ok(if reads < 3 {
                    before.clone()
                } else {
                    after.clone()
                })
            },
            |current| reject_foreign_apply_state(&requested, &before, None, current),
            |current| {
                verify_applied_configuration(&requested, Ok(current.clone()))?;
                verify_requested_hdr(&requested, current)
            },
            || waits += 1,
        );
        assert!(result.is_ok());
        assert_eq!((reads, waits), (3, 2));
    }

    #[test]
    fn ccd_settling_keeps_failed_verification_as_mutated() {
        let requested = requested();
        let before = Snapshot {
            format_version: 1,
            paths: vec![readback_path("monitor.one", 0, Some(false))],
        };
        let mut reads = 0;
        let error = settled_readback(
            || {
                reads += 1;
                Ok(before.clone())
            },
            |current| reject_foreign_apply_state(&requested, &before, None, current),
            |current| verify_requested_hdr(&requested, current),
            || {},
        )
        .unwrap_err();
        assert_eq!(reads, CCD_SETTLE_ATTEMPTS);
        assert_eq!(error.code, "verification_failed");
        assert!(error.may_have_changed);
    }

    #[test]
    fn ccd_settling_rejects_foreign_route_before_retry() {
        let requested = requested();
        let before = Snapshot {
            format_version: 1,
            paths: vec![readback_path("monitor.one", 0, Some(false))],
        };
        let mut foreign = before.clone();
        foreign.paths[0].target_address.id += 1;
        let mut reads = 0;
        let error = settled_readback(
            || {
                reads += 1;
                Ok(foreign.clone())
            },
            |current| reject_foreign_apply_state(&requested, &before, None, current),
            |_| Ok(()),
            || panic!("foreign route must not be retried"),
        )
        .unwrap_err();
        assert_eq!(reads, 1);
        assert_eq!(error.code, "topology_changed");
        assert!(error.may_have_changed);
    }

    #[test]
    fn ccd_settling_does_not_retry_native_topology_errors() {
        let mut reads = 0;
        let error = settled_readback(
            || {
                reads += 1;
                Err(DisplayError::new(
                    "topology_changed",
                    "hotplug during query",
                ))
            },
            |_| Ok(()),
            |_| Ok(()),
            || panic!("native topology errors must not be retried"),
        )
        .unwrap_err();
        assert_eq!(reads, 1);
        assert_eq!(error.code, "topology_changed");
        assert!(error.may_have_changed);
    }

    #[test]
    fn ccd_restore_settles_from_previous_topology_then_requires_exact_baseline() {
        let saved = Snapshot {
            format_version: 1,
            paths: vec![readback_path("monitor.one", 0, Some(false))],
        };
        let mut previous = saved.clone();
        previous
            .paths
            .push(readback_path("monitor.two", 1920, Some(false)));
        let rebound_targets = vec![(
            saved.paths[0].id.clone(),
            saved.paths[0].target_address.clone(),
        )];
        let mut reads = 0;
        let result = settled_readback(
            || {
                reads += 1;
                Ok(if reads == 1 {
                    previous.clone()
                } else {
                    saved.clone()
                })
            },
            |current| reject_foreign_restore_state(&saved, &previous, &rebound_targets, current),
            |current| {
                if saved.equivalent(current) {
                    Ok(())
                } else {
                    Err(DisplayError::new(
                        "verification_failed",
                        "restore is still settling",
                    ))
                }
            },
            || {},
        );
        assert!(result.is_ok());
        assert_eq!(reads, 2);
    }

    #[test]
    fn ccd_restore_rejects_foreign_route_before_retry() {
        let saved = Snapshot {
            format_version: 1,
            paths: vec![readback_path("monitor.one", 0, Some(false))],
        };
        let previous = saved.clone();
        let rebound_targets = vec![(
            saved.paths[0].id.clone(),
            saved.paths[0].target_address.clone(),
        )];
        let foreign = Snapshot {
            format_version: 1,
            paths: vec![readback_path("monitor.foreign", 0, Some(false))],
        };
        let error = settled_readback(
            || Ok(foreign.clone()),
            |current| reject_foreign_restore_state(&saved, &previous, &rebound_targets, current),
            |_| Ok(()),
            || panic!("foreign restore route must not be retried"),
        )
        .unwrap_err();
        assert_eq!(error.code, "topology_changed");
        assert!(error.may_have_changed);
    }

    #[test]
    fn ccd_restore_rejects_changed_native_target_even_when_saved_mode_matches() {
        let saved = Snapshot {
            format_version: 1,
            paths: vec![readback_path("monitor.one", 0, Some(false))],
        };
        let previous = saved.clone();
        let rebound_targets = vec![(
            saved.paths[0].id.clone(),
            saved.paths[0].target_address.clone(),
        )];
        let mut wrong = saved.clone();
        wrong.paths[0].target_address.id += 1;
        assert!(
            saved.equivalent(&wrong),
            "mode equivalence alone cannot prove the native route"
        );
        let mut reads = 0;
        let error = settled_readback(
            || {
                reads += 1;
                Ok(wrong.clone())
            },
            |current| reject_foreign_restore_state(&saved, &previous, &rebound_targets, current),
            |_| Ok(()),
            || panic!("wrong native target must fail immediately"),
        )
        .unwrap_err();
        assert_eq!(reads, 1);
        assert_eq!(error.code, "topology_changed");
        assert!(error.may_have_changed);
    }

    #[test]
    fn ccd_restore_accepts_rebound_target_after_restart() {
        let saved = Snapshot {
            format_version: 1,
            paths: vec![readback_path("monitor.one", 0, Some(false))],
        };
        let previous = Snapshot {
            format_version: 1,
            paths: vec![],
        };
        let mut rebound = saved.clone();
        rebound.paths[0].target_address.id += 1;
        let rebound_targets = vec![(
            saved.paths[0].id.clone(),
            rebound.paths[0].target_address.clone(),
        )];
        assert!(
            reject_foreign_restore_state(&saved, &previous, &rebound_targets, &rebound).is_ok()
        );
        rebound.paths[0].target_address.id += 1;
        assert_eq!(
            reject_foreign_restore_state(&saved, &previous, &rebound_targets, &rebound)
                .unwrap_err()
                .code,
            "topology_changed"
        );
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

    #[test]
    fn hdr_restore_rebinds_after_target_luid_changes() {
        let saved = Snapshot {
            format_version: 1,
            paths: vec![readback_path("monitor.one", 0, Some(true))],
        };
        let mut rebound = saved.clone();
        rebound.paths[0].target_address.adapter_low += 1;
        rebound.paths[0].source_address.adapter_low += 1;
        rebound.paths[0].hdr_enabled = Some(false);
        verify_restoration_routes(&saved, &rebound).unwrap();

        rebound.paths[0].source_name = "foreign source".into();
        let error = verify_restoration_routes(&saved, &rebound).unwrap_err();
        assert_eq!(error.code, "topology_changed");
        assert!(error.may_have_changed);
    }

    #[test]
    fn hdr_only_path_skips_ccd_only_for_one_changed_active_target() {
        let before = Snapshot {
            format_version: 1,
            paths: vec![
                readback_path("monitor.one", 0, Some(false)),
                readback_path("monitor.two", 1920, Some(false)),
            ],
        };
        let mut wanted = before.configuration().unwrap();
        wanted.displays[0].hdr_enabled = Some(true);
        assert_eq!(
            hdr_only_target(&wanted, &before, None, None).map(|path| path.id.as_str()),
            Some("monitor.one")
        );
        assert!(
            hdr_only_target(
                &wanted,
                &before,
                Some(("monitor.one", TopologyShape::PreserveActive)),
                None
            )
            .is_none()
        );
        wanted.displays[0].width += 1;
        assert!(hdr_only_target(&wanted, &before, None, None).is_none());
        wanted.displays[0].width -= 1;
        wanted.displays[0].x += 1;
        assert!(hdr_only_target(&wanted, &before, None, None).is_none());
        wanted.displays[0].x -= 1;
        wanted.displays[1].hdr_enabled = Some(true);
        assert!(hdr_only_target(&wanted, &before, None, None).is_none());
        wanted.displays[1].hdr_enabled = Some(false);
        let mut noncanonical = before.clone();
        noncanonical.paths[0].source.pixel_format = 3;
        assert!(hdr_only_target(&wanted, &noncanonical, None, None).is_none());
        noncanonical.paths[0].source.pixel_format =
            windows_sys::Win32::Devices::Display::DISPLAYCONFIG_PIXELFORMAT_32BPP;
        noncanonical.paths[0].path_flags = 0;
        assert!(hdr_only_target(&wanted, &noncanonical, None, None).is_none());
    }

    #[test]
    fn hdr_only_path_requires_queryable_recovery_baseline() {
        let mut before = Snapshot {
            format_version: 1,
            paths: vec![readback_path("monitor.one", 0, None)],
        };
        let mut wanted = before.configuration().unwrap();
        wanted.displays[0].hdr_enabled = Some(true);
        assert!(hdr_only_target(&wanted, &before, None, None).is_none());
        before.paths[0].hdr_enabled = Some(false);
        assert!(hdr_only_target(&wanted, &before, None, None).is_some());
    }

    #[test]
    fn hdr_only_result_preserves_nonpreferred_scaling_and_signal_timing() {
        let before = Snapshot {
            format_version: 1,
            paths: vec![readback_path("monitor.one", 0, Some(false))],
        };
        assert_ne!(
            before.paths[0].scaling,
            windows_sys::Win32::Devices::Display::DISPLAYCONFIG_SCALING_PREFERRED
        );
        let mut after = before.clone();
        after.paths[0].hdr_enabled = Some(true);
        verify_exact_hdr_only_result(&before, &before.paths[0], true, &after).unwrap();

        after.paths[0].signal.pixel_rate += 1;
        let error =
            verify_exact_hdr_only_result(&before, &before.paths[0], true, &after).unwrap_err();
        assert_eq!(error.code, "verification_failed");
        assert!(error.may_have_changed);
    }
}
