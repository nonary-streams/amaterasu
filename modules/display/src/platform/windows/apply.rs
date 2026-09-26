use super::conversion::{address, native_rational, native_signal, native_source};
use super::query::{ConfigurationBuffers, check, names, query};
use crate::model::{
    Configuration, DisplayError, DisplayResult, Rotation, RouteBinding, Snapshot, SourceMode,
};
use std::collections::BTreeMap;
use windows_sys::Win32::Devices::Display::*;
use windows_sys::Win32::Graphics::Gdi::{
    DISPLAYCONFIG_PATH_ACTIVE, DISPLAYCONFIG_PATH_MODE_IDX_INVALID,
};

pub fn requested(configuration: &Configuration) -> DisplayResult<ConfigurationBuffers> {
    configuration.validate()?;
    let available = query(QDC_ALL_PATHS)?;
    let mut paths = Vec::new();
    let mut modes = Vec::new();
    let mut sources = Vec::new();
    for display in &configuration.displays {
        let mut candidates = Vec::new();
        for candidate in &available.paths {
            if candidate.targetInfo.targetAvailable == 0 {
                continue;
            }
            if names(candidate)?.id.eq_ignore_ascii_case(&display.id) {
                candidates.push(*candidate);
            }
        }
        let mut targets = std::collections::BTreeSet::new();
        for candidate in &candidates {
            let target = address(candidate.targetInfo.adapterId, candidate.targetInfo.id);
            targets.insert((target.adapter_low, target.adapter_high, target.id));
        }
        if targets.len() > 1 {
            return Err(DisplayError::new(
                "unsupported_topology",
                "display ID resolves to multiple physical targets",
            ));
        }
        candidates.sort_by_key(|path| path.flags & DISPLAYCONFIG_PATH_ACTIVE == 0);
        let mut path = candidates
            .into_iter()
            .find(|path| !sources.contains(&address(path.sourceInfo.adapterId, path.sourceInfo.id)))
            .ok_or_else(|| {
                DisplayError::new(
                    "unsupported_topology",
                    "display is missing or has no independent source route",
                )
            })?;
        if matches!(
            path.targetInfo.outputTechnology,
            DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INDIRECT_VIRTUAL
                | DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INDIRECT_WIRED
                | DISPLAYCONFIG_OUTPUT_TECHNOLOGY_MIRACAST
        ) {
            return Err(DisplayError::new(
                "unsupported_display",
                "indirect and wireless display mutation is unsupported",
            ));
        }
        sources.push(address(path.sourceInfo.adapterId, path.sourceInfo.id));
        path.sourceInfo.Anonymous.modeInfoIdx = modes.len() as u32;
        modes.push(DISPLAYCONFIG_MODE_INFO {
            infoType: DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE,
            id: path.sourceInfo.id,
            adapterId: path.sourceInfo.adapterId,
            Anonymous: DISPLAYCONFIG_MODE_INFO_0 {
                sourceMode: native_source(&SourceMode {
                    width: display.width,
                    height: display.height,
                    pixel_format: DISPLAYCONFIG_PIXELFORMAT_32BPP,
                    x: display.x,
                    y: display.y,
                }),
            },
        });
        path.targetInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
        path.targetInfo.refreshRate = native_rational(display.refresh);
        path.targetInfo.rotation = rotation(display.rotation);
        path.targetInfo.scaling = DISPLAYCONFIG_SCALING_PREFERRED;
        path.flags = DISPLAYCONFIG_PATH_ACTIVE;
        paths.push(path);
    }
    Ok(ConfigurationBuffers { paths, modes })
}

pub fn restoration(snapshot: &Snapshot) -> DisplayResult<ConfigurationBuffers> {
    snapshot.validate()?;
    let available = query(QDC_ALL_PATHS)?;
    let mut paths = Vec::new();
    let mut modes = Vec::new();
    let mut source_modes = BTreeMap::new();
    for saved in &snapshot.paths {
        let mut matched: Option<DISPLAYCONFIG_PATH_INFO> = None;
        for candidate in &available.paths {
            if candidate.targetInfo.targetAvailable == 0 {
                continue;
            }
            let device = names(candidate)?;
            // Monitor device-interface paths are durable identities. Adapter
            // LUIDs are session-local and must be rebound after an OS restart.
            if device.id == saved.id && device.source == saved.source_name {
                if let Some(previous) = matched {
                    let old_target = address(previous.targetInfo.adapterId, previous.targetInfo.id);
                    let new_target =
                        address(candidate.targetInfo.adapterId, candidate.targetInfo.id);
                    let old_source = address(previous.sourceInfo.adapterId, previous.sourceInfo.id);
                    let new_source =
                        address(candidate.sourceInfo.adapterId, candidate.sourceInfo.id);
                    if old_target != new_target || old_source != new_source {
                        return Err(DisplayError::new(
                            "unsupported_topology",
                            "saved monitor/source route is ambiguous",
                        ));
                    }
                } else {
                    matched = Some(*candidate);
                }
            }
        }
        let mut path = matched.ok_or_else(|| {
            DisplayError::new(
                "recovery_blocked",
                format!("saved monitor/source route is unavailable: {}", saved.id),
            )
        })?;
        if path.targetInfo.outputTechnology != saved.output_technology {
            return Err(DisplayError::new(
                "recovery_blocked",
                "saved display output technology changed",
            ));
        }
        let source_key = (
            path.sourceInfo.adapterId.LowPart,
            path.sourceInfo.adapterId.HighPart,
            path.sourceInfo.id,
        );
        let source_index = if let Some((index, original)) = source_modes.get(&source_key) {
            if original != &saved.source {
                return Err(DisplayError::new(
                    "invalid_snapshot",
                    "clone-group sources contain conflicting modes",
                ));
            }
            *index
        } else {
            let index = modes.len() as u32;
            source_modes.insert(source_key, (index, saved.source.clone()));
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
        path.sourceInfo.statusFlags = saved.source_flags;
        path.targetInfo.statusFlags = saved.target_flags;
        path.targetInfo.rotation = saved.rotation;
        path.targetInfo.scaling = saved.scaling;
        path.targetInfo.refreshRate = native_rational(saved.refresh);
        path.targetInfo.scanLineOrdering = saved.scan_line_ordering;
        path.flags = saved.path_flags;
        paths.push(path);
    }
    Ok(ConfigurationBuffers { paths, modes })
}

pub fn preserving(
    configuration: &Configuration,
    before: &Snapshot,
    selected: &str,
    route: Option<&RouteBinding>,
    shape: super::super::TopologyShape,
) -> DisplayResult<ConfigurationBuffers> {
    match (route, shape) {
        (Some(route), super::super::TopologyShape::OnlySelected) => {
            only_bound_inactive(configuration, before, selected, route)
        }
        (Some(route), super::super::TopologyShape::PreserveActive) => {
            append_inactive(restoration(before)?, configuration, before, selected, route)
        }
        (None, super::super::TopologyShape::PreserveActive) => {
            preserve_bound(restoration(before)?, configuration, before, selected)
        }
        (None, super::super::TopologyShape::OnlySelected) => Err(DisplayError::new(
            "unsupported_topology",
            "selected-only inactive topology requires a bound route",
        )),
    }
}

pub fn bind_inactive(display: &str, before: &Snapshot) -> DisplayResult<RouteBinding> {
    before.configuration()?;
    if before
        .paths
        .iter()
        .any(|path| path.id.eq_ignore_ascii_case(display))
    {
        return Err(DisplayError::new(
            "invalid_request",
            "selected display is already active",
        ));
    }
    let (path, device) = select_inactive_route(display, before, "unsupported_topology")?;
    Ok(RouteBinding {
        id: device.id,
        source_name: device.source,
        source_address: address(path.sourceInfo.adapterId, path.sourceInfo.id),
        target_address: address(path.targetInfo.adapterId, path.targetInfo.id),
        output_technology: path.targetInfo.outputTechnology,
    })
}

pub fn bound_inactive_path(
    route: &RouteBinding,
    before: &Snapshot,
) -> DisplayResult<DISPLAYCONFIG_PATH_INFO> {
    let (path, device) = select_inactive_route(&route.id, before, "topology_changed")?;
    if device.source != route.source_name
        || address(path.sourceInfo.adapterId, path.sourceInfo.id) != route.source_address
        || address(path.targetInfo.adapterId, path.targetInfo.id) != route.target_address
        || path.targetInfo.outputTechnology != route.output_technology
    {
        return Err(DisplayError::new(
            "topology_changed",
            "inactive target route changed",
        ));
    }
    Ok(path)
}

fn select_inactive_route(
    display: &str,
    before: &Snapshot,
    ambiguous_code: &'static str,
) -> DisplayResult<(DISPLAYCONFIG_PATH_INFO, super::query::DeviceNames)> {
    let available = query(QDC_ALL_PATHS)?;
    let mut candidates = Vec::new();
    for path in &available.paths {
        if path.targetInfo.targetAvailable == 0 {
            continue;
        }
        let device = names(path)?;
        if device.id.eq_ignore_ascii_case(display) {
            candidates.push((*path, device));
        }
    }
    select_candidate_route(candidates, before, ambiguous_code)
}

pub(super) fn select_candidate_route(
    candidates: Vec<(DISPLAYCONFIG_PATH_INFO, super::query::DeviceNames)>,
    before: &Snapshot,
    ambiguous_code: &'static str,
) -> DisplayResult<(DISPLAYCONFIG_PATH_INFO, super::query::DeviceNames)> {
    let mut physical_targets = std::collections::BTreeSet::new();
    let mut eligible: Vec<(
        (u32, i32, u32),
        DISPLAYCONFIG_PATH_INFO,
        super::query::DeviceNames,
    )> = Vec::new();
    for (path, device) in candidates {
        let target = address(path.targetInfo.adapterId, path.targetInfo.id);
        physical_targets.insert((target.adapter_low, target.adapter_high, target.id));
        if matches!(
            path.targetInfo.outputTechnology,
            DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INDIRECT_VIRTUAL
                | DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INDIRECT_WIRED
                | DISPLAYCONFIG_OUTPUT_TECHNOLOGY_MIRACAST
        ) {
            return Err(DisplayError::new(
                ambiguous_code,
                "inactive indirect or wireless route is unsupported",
            ));
        }
        let source = address(path.sourceInfo.adapterId, path.sourceInfo.id);
        if before
            .paths
            .iter()
            .any(|active| active.source_address == source)
        {
            continue;
        }
        let key = (source.adapter_low, source.adapter_high, source.id);
        if let Some((_, old_path, old_device)) =
            eligible.iter().find(|(old_key, _, _)| *old_key == key)
        {
            if old_device.source != device.source
                || old_path.targetInfo.outputTechnology != path.targetInfo.outputTechnology
            {
                return Err(DisplayError::new(
                    ambiguous_code,
                    "inactive route has conflicting source identity",
                ));
            }
        } else {
            eligible.push((key, path, device));
        }
    }
    if physical_targets.len() != 1 || eligible.len() != 1 {
        return Err(DisplayError::new(
            ambiguous_code,
            "inactive display lacks one unambiguous free physical route",
        ));
    }
    let (_, path, device) = eligible.pop().expect("one eligible route");
    Ok((path, device))
}

fn append_inactive(
    bound: ConfigurationBuffers,
    configuration: &Configuration,
    before: &Snapshot,
    selected: &str,
    route: &RouteBinding,
) -> DisplayResult<ConfigurationBuffers> {
    let path = bound_inactive_path(route, before)?;
    append_bound_inactive(bound, configuration, before, selected, route, path)
}

fn only_bound_inactive(
    configuration: &Configuration,
    before: &Snapshot,
    selected: &str,
    route: &RouteBinding,
) -> DisplayResult<ConfigurationBuffers> {
    let path = bound_inactive_path(route, before)?;
    only_from_bound_inactive(configuration, before, selected, route, path)
}

pub(super) fn only_from_bound_inactive(
    configuration: &Configuration,
    before: &Snapshot,
    selected: &str,
    route: &RouteBinding,
    path: DISPLAYCONFIG_PATH_INFO,
) -> DisplayResult<ConfigurationBuffers> {
    configuration.validate()?;
    before.configuration()?;
    let wanted = configuration.displays.first().ok_or_else(|| {
        DisplayError::new("unsupported_topology", "selected-only topology is empty")
    })?;
    if configuration.displays.len() != 1
        || !wanted.id.eq_ignore_ascii_case(selected)
        || !route.id.eq_ignore_ascii_case(selected)
        || wanted.x != 0
        || wanted.y != 0
    {
        return Err(DisplayError::new(
            "unsupported_topology",
            "selected-only topology must contain the bound target at origin",
        ));
    }
    append_selected_inactive(
        ConfigurationBuffers {
            paths: Vec::new(),
            modes: Vec::new(),
        },
        wanted,
        route,
        path,
    )
}

pub(super) fn append_bound_inactive(
    bound: ConfigurationBuffers,
    configuration: &Configuration,
    before: &Snapshot,
    selected: &str,
    route: &RouteBinding,
    path: DISPLAYCONFIG_PATH_INFO,
) -> DisplayResult<ConfigurationBuffers> {
    configuration.validate()?;
    before.configuration()?;
    if !route.id.eq_ignore_ascii_case(selected)
        || bound.paths.len() != before.paths.len()
        || configuration.displays.len() != before.paths.len() + 1
    {
        return Err(DisplayError::new(
            "unsupported_topology",
            "inactive selected route does not match complete desired topology",
        ));
    }
    for saved in &before.paths {
        let wanted = configuration
            .displays
            .iter()
            .find(|display| display.id.eq_ignore_ascii_case(&saved.id))
            .ok_or_else(|| {
                DisplayError::new("unsupported_topology", "active display was omitted")
            })?;
        if saved.source.width != wanted.width
            || saved.source.height != wanted.height
            || saved.source.x != wanted.x
            || saved.source.y != wanted.y
            || !saved.refresh.equivalent(wanted.refresh)
            || saved.rotation != rotation(wanted.rotation)
        {
            return Err(DisplayError::new(
                "unsupported_topology",
                "inactive activation must preserve every active path",
            ));
        }
    }
    let wanted = configuration
        .displays
        .iter()
        .find(|display| display.id.eq_ignore_ascii_case(selected))
        .ok_or_else(|| {
            DisplayError::new(
                "unsupported_display",
                "inactive target is missing from request",
            )
        })?;
    append_selected_inactive(bound, wanted, route, path)
}

fn append_selected_inactive(
    mut bound: ConfigurationBuffers,
    wanted: &crate::model::DisplaySetting,
    route: &RouteBinding,
    mut path: DISPLAYCONFIG_PATH_INFO,
) -> DisplayResult<ConfigurationBuffers> {
    if wanted.hdr_enabled == Some(true) {
        return Err(DisplayError::new(
            "unsupported_display",
            "inactive HDR target has no recoverable baseline",
        ));
    }
    if address(path.sourceInfo.adapterId, path.sourceInfo.id) != route.source_address
        || address(path.targetInfo.adapterId, path.targetInfo.id) != route.target_address
    {
        return Err(DisplayError::new(
            "topology_changed",
            "bound inactive path differs from journal route",
        ));
    }
    path.sourceInfo.Anonymous.modeInfoIdx = bound.modes.len() as u32;
    bound.modes.push(DISPLAYCONFIG_MODE_INFO {
        infoType: DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE,
        id: path.sourceInfo.id,
        adapterId: path.sourceInfo.adapterId,
        Anonymous: DISPLAYCONFIG_MODE_INFO_0 {
            sourceMode: native_source(&SourceMode {
                width: wanted.width,
                height: wanted.height,
                pixel_format: DISPLAYCONFIG_PIXELFORMAT_32BPP,
                x: wanted.x,
                y: wanted.y,
            }),
        },
    });
    path.targetInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
    path.targetInfo.refreshRate = native_rational(wanted.refresh);
    path.targetInfo.rotation = rotation(wanted.rotation);
    path.targetInfo.scaling = DISPLAYCONFIG_SCALING_PREFERRED;
    path.targetInfo.scanLineOrdering = DISPLAYCONFIG_SCANLINE_ORDERING_UNSPECIFIED;
    path.flags = DISPLAYCONFIG_PATH_ACTIVE;
    bound.paths.push(path);
    compact_modes(&mut bound)?;
    Ok(bound)
}

pub(super) fn preserve_bound(
    mut bound: ConfigurationBuffers,
    configuration: &Configuration,
    before: &Snapshot,
    selected: &str,
) -> DisplayResult<ConfigurationBuffers> {
    configuration.validate()?;
    before.configuration()?;
    if bound.paths.len() != before.paths.len() || configuration.displays.len() != before.paths.len()
    {
        return Err(DisplayError::new(
            "unsupported_topology",
            "preserved request must contain every active path",
        ));
    }
    let mut selected_found = false;
    for (path, saved) in bound.paths.iter_mut().zip(&before.paths) {
        let wanted = configuration
            .displays
            .iter()
            .find(|display| display.id.eq_ignore_ascii_case(&saved.id))
            .ok_or_else(|| {
                DisplayError::new("unsupported_topology", "active display was omitted")
            })?;
        let mode = bound
            .modes
            .iter_mut()
            .find(|mode| {
                mode.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE
                    && address(mode.adapterId, mode.id)
                        == address(path.sourceInfo.adapterId, path.sourceInfo.id)
            })
            .ok_or_else(|| {
                DisplayError::new("unsupported_topology", "bound source mode is missing")
            })?;
        let mut source = saved.source.clone();
        source.x = wanted.x;
        source.y = wanted.y;
        if saved.id.eq_ignore_ascii_case(selected) {
            selected_found = true;
            source.width = wanted.width;
            source.height = wanted.height;
            source.pixel_format = DISPLAYCONFIG_PIXELFORMAT_32BPP;
            path.targetInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
            path.targetInfo.refreshRate = native_rational(wanted.refresh);
            path.targetInfo.rotation = rotation(wanted.rotation);
            path.targetInfo.scaling = DISPLAYCONFIG_SCALING_PREFERRED;
            path.flags = DISPLAYCONFIG_PATH_ACTIVE;
        } else if saved.source.width != wanted.width
            || saved.source.height != wanted.height
            || !saved.refresh.equivalent(wanted.refresh)
            || saved.rotation != rotation(wanted.rotation)
        {
            return Err(DisplayError::new(
                "unsupported_topology",
                "untouched monitor mode changed in preserving request",
            ));
        }
        mode.Anonymous = DISPLAYCONFIG_MODE_INFO_0 {
            sourceMode: native_source(&source),
        };
    }
    if !selected_found {
        return Err(DisplayError::new(
            "unsupported_display",
            "selected monitor is not active",
        ));
    }
    compact_modes(&mut bound)?;
    Ok(bound)
}

fn compact_modes(bound: &mut ConfigurationBuffers) -> DisplayResult<()> {
    let mut compact = Vec::new();
    let mut indexes = BTreeMap::new();
    for path in &mut bound.paths {
        // SAFETY: restoration and preserving wrote the modeInfoIdx union variant
        // for every supplied path; target INVALID is the documented sentinel.
        let source_index = unsafe { path.sourceInfo.Anonymous.modeInfoIdx };
        let source = remap_mode(
            &bound.modes,
            &mut compact,
            &mut indexes,
            source_index,
            DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE,
        )?;
        path.sourceInfo.Anonymous.modeInfoIdx = source;
        let target_index = unsafe { path.targetInfo.Anonymous.modeInfoIdx };
        if target_index != DISPLAYCONFIG_PATH_MODE_IDX_INVALID {
            let target = remap_mode(
                &bound.modes,
                &mut compact,
                &mut indexes,
                target_index,
                DISPLAYCONFIG_MODE_INFO_TYPE_TARGET,
            )?;
            path.targetInfo.Anonymous.modeInfoIdx = target;
        }
    }
    bound.modes = compact;
    Ok(())
}

fn remap_mode(
    old: &[DISPLAYCONFIG_MODE_INFO],
    compact: &mut Vec<DISPLAYCONFIG_MODE_INFO>,
    indexes: &mut BTreeMap<u32, u32>,
    index: u32,
    expected_type: DISPLAYCONFIG_MODE_INFO_TYPE,
) -> DisplayResult<u32> {
    let mode = old.get(index as usize).ok_or_else(|| {
        DisplayError::new(
            "unsupported_topology",
            "CCD mode index exceeds bound snapshot",
        )
    })?;
    if mode.infoType != expected_type {
        return Err(DisplayError::new(
            "unsupported_topology",
            "CCD mode index has wrong type",
        ));
    }
    if let Some(mapped) = indexes.get(&index) {
        return Ok(*mapped);
    }
    let mapped = compact.len() as u32;
    compact.push(*mode);
    indexes.insert(index, mapped);
    Ok(mapped)
}

pub fn verifies_untouched(
    configuration: &Configuration,
    before: &Snapshot,
    current: &Snapshot,
    selected: &str,
) -> bool {
    let mut expected = before.clone();
    expected
        .paths
        .retain(|path| !path.id.eq_ignore_ascii_case(selected));
    if expected.paths.is_empty() {
        return true;
    }
    for path in &mut expected.paths {
        let Some(wanted) = configuration
            .displays
            .iter()
            .find(|display| display.id.eq_ignore_ascii_case(&path.id))
        else {
            return false;
        };
        path.source.x = wanted.x;
        path.source.y = wanted.y;
    }
    let mut observed = current.clone();
    observed
        .paths
        .retain(|path| !path.id.eq_ignore_ascii_case(selected));
    expected.equivalent(&observed)
}

pub fn set(buffers: &ConfigurationBuffers, apply: bool) -> DisplayResult<()> {
    let mode = if apply {
        SDC_APPLY | SDC_NO_OPTIMIZATION
    } else {
        SDC_VALIDATE
    };
    // Do not save temporary session modes to the Windows persistence database,
    // and do not permit SDC_ALLOW_CHANGES to silently change supplied settings.
    let flags = SDC_USE_SUPPLIED_DISPLAY_CONFIG | mode;
    // SAFETY: vectors own fully initialized CCD records, all mode indexes were
    // constructed within these buffers, and SetDisplayConfig does not retain them.
    check(
        unsafe {
            SetDisplayConfig(
                buffers.paths.len() as u32,
                buffers.paths.as_ptr(),
                buffers.modes.len() as u32,
                buffers.modes.as_ptr(),
                flags,
            )
        },
        if apply {
            "SetDisplayConfig(apply)"
        } else {
            "SetDisplayConfig(validate)"
        },
    )
}

pub fn verifies(configuration: &Configuration, current: &Snapshot) -> bool {
    configuration.displays.len() == current.paths.len()
        && configuration.displays.iter().all(|wanted| {
            current.paths.iter().any(|actual| {
                actual.id.eq_ignore_ascii_case(&wanted.id)
                    && actual.source.width == wanted.width
                    && actual.source.height == wanted.height
                    && actual.source.x == wanted.x
                    && actual.source.y == wanted.y
                    && actual.rotation == rotation(wanted.rotation)
                    && actual.refresh.equivalent(wanted.refresh)
            })
        })
}
fn rotation(value: Rotation) -> DISPLAYCONFIG_ROTATION {
    match value {
        Rotation::Identity => DISPLAYCONFIG_ROTATION_IDENTITY,
        Rotation::Rotate90 => DISPLAYCONFIG_ROTATION_ROTATE90,
        Rotation::Rotate180 => DISPLAYCONFIG_ROTATION_ROTATE180,
        Rotation::Rotate270 => DISPLAYCONFIG_ROTATION_ROTATE270,
    }
}
