use super::conversion::{address, native_rational, native_signal, native_source};
use super::query::{ConfigurationBuffers, check, names, query};
use crate::model::{Configuration, DisplayError, DisplayResult, Rotation, Snapshot, SourceMode};
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
        let mut matched = None;
        for candidate in &available.paths {
            if candidate.targetInfo.targetAvailable == 0 {
                continue;
            }
            let device = names(candidate)?;
            // Monitor device-interface paths are durable identities. Adapter
            // LUIDs are session-local and must be rebound after an OS restart.
            if device.id == saved.id && device.source == saved.source_name {
                matched = Some(*candidate);
                break;
            }
        }
        let mut path = matched.ok_or_else(|| {
            DisplayError::new(
                "recovery_blocked",
                format!("saved monitor/source route is unavailable: {}", saved.id),
            )
        })?;
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
