use super::conversion::{address, decode_path};
use crate::model::{AvailableMode, DisplayError, DisplayResult, Rational, Snapshot};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use windows_sys::Win32::Devices::Display::*;
use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_NOT_SUPPORTED, LUID};
use windows_sys::Win32::Graphics::Gdi::{DEVMODEW, EnumDisplaySettingsExW};

pub struct ConfigurationBuffers {
    pub paths: Vec<DISPLAYCONFIG_PATH_INFO>,
    pub modes: Vec<DISPLAYCONFIG_MODE_INFO>,
}
pub struct DeviceNames {
    pub id: String,
    pub friendly: String,
    pub source: String,
}
pub fn query(flags: QUERY_DISPLAY_CONFIG_FLAGS) -> DisplayResult<ConfigurationBuffers> {
    // Hotplug can invalidate sizes between calls; bounded retries never turn an
    // allocation race into an unbounded buffer or an incomplete snapshot.
    for _ in 0..4 {
        let mut paths = 0;
        let mut modes = 0;
        // SAFETY: both output counters are valid exclusive stack references.
        check(
            unsafe { GetDisplayConfigBufferSizes(flags, &mut paths, &mut modes) } as i32,
            "GetDisplayConfigBufferSizes",
        )?;
        if paths > 256 || modes > 1024 {
            return Err(DisplayError::new(
                "resource_exhausted",
                "display configuration exceeds bounded native buffers",
            ));
        }
        let mut buffers = ConfigurationBuffers {
            paths: vec![DISPLAYCONFIG_PATH_INFO::default(); paths as usize],
            modes: vec![DISPLAYCONFIG_MODE_INFO::default(); modes as usize],
        };
        // SAFETY: initialized vectors own the reported capacities for the entire
        // synchronous call; topology-id must be null for ACTIVE/ALL queries.
        let result = unsafe {
            QueryDisplayConfig(
                flags,
                &mut paths,
                buffers.paths.as_mut_ptr(),
                &mut modes,
                buffers.modes.as_mut_ptr(),
                std::ptr::null_mut(),
            )
        };
        if result == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        check(result as i32, "QueryDisplayConfig")?;
        if paths as usize > buffers.paths.len() || modes as usize > buffers.modes.len() {
            return Err(DisplayError::new(
                "platform_error",
                "display API returned invalid array lengths",
            ));
        }
        buffers.paths.truncate(paths as usize);
        buffers.modes.truncate(modes as usize);
        return Ok(buffers);
    }
    Err(DisplayError::new(
        "topology_changed",
        "display topology changed repeatedly during enumeration",
    ))
}

pub fn names(path: &DISPLAYCONFIG_PATH_INFO) -> DisplayResult<DeviceNames> {
    let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME {
        header: device_header(
            DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
            std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(),
            path.targetInfo.adapterId,
            path.targetInfo.id,
        ),
        ..Default::default()
    };
    let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
        header: device_header(
            DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
            std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(),
            path.sourceInfo.adapterId,
            path.sourceInfo.id,
        ),
        ..Default::default()
    };
    // SAFETY: each header is the first field of its initialized repr(C) device
    // packet; header.size describes the full packet, which lives through the call.
    check(
        unsafe { DisplayConfigGetDeviceInfo(&mut target.header) },
        "DisplayConfigGetDeviceInfo(target)",
    )?;
    // SAFETY: same packet ownership and layout requirements as the target call.
    check(
        unsafe { DisplayConfigGetDeviceInfo(&mut source.header) },
        "DisplayConfigGetDeviceInfo(source)",
    )?;
    let id = wide_string(&target.monitorDevicePath)?.to_lowercase();
    if id.is_empty() {
        return Err(DisplayError::new(
            "unsupported_display",
            "display has no monitor device-interface identity",
        ));
    }
    Ok(DeviceNames {
        id,
        friendly: wide_string(&target.monitorFriendlyDeviceName)?,
        source: wide_string(&source.viewGdiDeviceName)?,
    })
}

/// Returns None when the target has no advanced-color control. The bitfield's
/// support and enabled bits are distinct; support alone must not imply HDR.
pub fn advanced_color_enabled(path: &DISPLAYCONFIG_PATH_INFO) -> DisplayResult<Option<bool>> {
    let mut packet = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
        header: device_header(
            DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
            std::mem::size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>(),
            path.targetInfo.adapterId,
            path.targetInfo.id,
        ),
        ..Default::default()
    };
    // SAFETY: the initialized packet owns the output storage described by header.
    let result = unsafe { DisplayConfigGetDeviceInfo(&mut packet.header) };
    if result == ERROR_NOT_SUPPORTED as i32 {
        return Ok(None);
    }
    check(result, "DisplayConfigGetDeviceInfo(advanced color)")?;
    // SAFETY: value is the documented raw bitfield view of this initialized union.
    decode_advanced_color_flags(unsafe { packet.Anonymous.value })
}

fn decode_advanced_color_flags(flags: u32) -> DisplayResult<Option<bool>> {
    if flags & 4 != 0 {
        return Err(DisplayError::new(
            "unsupported_display",
            "wide-color-gamut state cannot be restored by the HDR recovery journal",
        ));
    }
    if flags & 2 != 0 && flags & 1 == 0 {
        return Err(DisplayError::new(
            "platform_error",
            "advanced color is enabled without reported target support",
        ));
    }
    Ok((flags & 1 != 0).then_some(flags & 2 != 0))
}

pub fn check_legacy_hdr_recovery(snapshot: &Snapshot) -> DisplayResult<()> {
    if !snapshot.paths.iter().any(|path| path.hdr_enabled.is_none()) {
        return Ok(());
    }
    let active = query(QDC_ONLY_ACTIVE_PATHS)?;
    let mut observed_legacy_targets = Vec::new();
    for path in &active.paths {
        let id = names(path)?.id;
        if !snapshot
            .paths
            .iter()
            .any(|saved| saved.hdr_enabled.is_none() && saved.id.eq_ignore_ascii_case(&id))
        {
            continue;
        }
        observed_legacy_targets.push(id.clone());
        if snapshot.legacy_hdr_target_conflicts(&id, advanced_color_enabled(path)?) {
            return Err(DisplayError::new(
                "recovery_blocked",
                "HDR was enabled outside the old SDR recovery journal",
            ));
        }
    }
    if snapshot.paths.iter().any(|saved| {
        saved.hdr_enabled.is_none()
            && !observed_legacy_targets
                .iter()
                .any(|id| saved.id.eq_ignore_ascii_case(id))
    }) {
        return Err(DisplayError::new(
            "recovery_blocked",
            "old SDR journal target is inactive; its HDR state cannot be checked before reactivation",
        ));
    }
    Ok(())
}

/// An inactive target has no saved HDR baseline. Inspect the state after CCD
/// activation without changing HDR; rollback restores the prior topology if
/// Windows activated the target with HDR already enabled.
pub fn verify_newly_active_hdr_is_off(id: &str) -> DisplayResult<()> {
    let active = query(QDC_ONLY_ACTIVE_PATHS)?;
    let path = active
        .paths
        .iter()
        .find(|path| names(path).is_ok_and(|device| device.id.eq_ignore_ascii_case(id)))
        .ok_or_else(|| DisplayError::new("topology_changed", "requested target is not active"))?;
    if advanced_color_enabled(path)? == Some(true) {
        return Err(DisplayError::new(
            "verification_failed",
            "newly active target enabled HDR without a saved recovery baseline",
        ));
    }
    Ok(())
}

pub fn set_advanced_color(id: &str, enabled: bool) -> DisplayResult<()> {
    let active = query(QDC_ONLY_ACTIVE_PATHS)?;
    let path = active
        .paths
        .iter()
        .find(|path| names(path).is_ok_and(|device| device.id.eq_ignore_ascii_case(id)))
        .ok_or_else(|| DisplayError::new("unsupported_display", "HDR target is not active"))?;
    match advanced_color_enabled(path)? {
        Some(current) if current == enabled => return Ok(()),
        Some(_) => {}
        None if !enabled => return Ok(()),
        None => {
            return Err(DisplayError::new(
                "unsupported_display",
                "HDR control is unavailable for this target",
            ));
        }
    }
    let packet = DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE {
        header: device_header(
            DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE,
            std::mem::size_of::<DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE>(),
            path.targetInfo.adapterId,
            path.targetInfo.id,
        ),
        Anonymous: DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE_0 {
            value: u32::from(enabled),
        },
    };
    // SAFETY: the initialized repr(C) packet owns the input bytes described by
    // its header; Windows consumes them synchronously and retains no pointer.
    check(
        unsafe { DisplayConfigSetDeviceInfo(&packet.header) },
        "DisplayConfigSetDeviceInfo(advanced color)",
    )?;
    let current = query(QDC_ONLY_ACTIVE_PATHS)?;
    let current = current
        .paths
        .iter()
        .find(|path| names(path).is_ok_and(|device| device.id.eq_ignore_ascii_case(id)))
        .ok_or_else(|| DisplayError::new("topology_changed", "HDR target disappeared"))?;
    if advanced_color_enabled(current)? != Some(enabled) {
        return Err(DisplayError::new(
            "verification_failed",
            "Windows HDR state differs from the requested state",
        ));
    }
    Ok(())
}

pub fn snapshot() -> DisplayResult<Snapshot> {
    let buffers = query(QDC_ONLY_ACTIVE_PATHS)?;
    let mut paths = Vec::new();
    for path in &buffers.paths {
        if matches!(
            path.targetInfo.outputTechnology,
            DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INDIRECT_VIRTUAL
                | DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INDIRECT_WIRED
                | DISPLAYCONFIG_OUTPUT_TECHNOLOGY_MIRACAST
        ) {
            return Err(DisplayError::new(
                "unsupported_display",
                "indirect and wireless displays require a dedicated adapter",
            ));
        }
        let mut decoded = decode_path(path, &buffers.modes, names(path)?)?;
        decoded.hdr_enabled = advanced_color_enabled(path)?;
        paths.push(decoded);
    }
    let snapshot = Snapshot {
        format_version: 1,
        paths,
    };
    snapshot.validate()?;
    Ok(snapshot)
}

pub fn monitors() -> DisplayResult<Vec<Value>> {
    let active = query(QDC_ONLY_ACTIVE_PATHS)?;
    let available = query(QDC_ALL_PATHS)?;
    let mut monitors = BTreeMap::new();
    for path in available
        .paths
        .iter()
        .filter(|path| path.targetInfo.targetAvailable != 0)
    {
        let device = names(path)?;
        if monitors.contains_key(&device.id) {
            continue;
        }
        let current = active.paths.iter().find(|active| {
            address(active.targetInfo.adapterId, active.targetInfo.id)
                == address(path.targetInfo.adapterId, path.targetInfo.id)
        });
        let device = names(current.unwrap_or(path))?;
        let mode = current
            .map(|path| decode_path(path, &active.modes, names(path)?))
            .transpose()?;
        monitors.insert(device.id.clone(), json!({"id":device.id,"name":device.friendly,"active":current.is_some(),"source":device.source,"advanced_color_enabled":advanced_color_enabled(path).ok().flatten(),"current":mode.map(|path| json!({"width":path.source.width,"height":path.source.height,"x":path.source.x,"y":path.source.y,"rotation":path.rotation,"refresh":path.refresh}))}));
    }
    Ok(monitors.into_values().collect())
}

pub fn modes(id: &str) -> DisplayResult<Vec<Value>> {
    Ok(available_modes(id)?
        .into_iter()
        .map(|mode| json!({"width":mode.width,"height":mode.height,"refresh_hz":mode.nominal_refresh_hz,"bits_per_pixel":mode.bits_per_pixel,"refresh":mode.refresh}))
        .collect())
}

pub fn available_modes(id: &str) -> DisplayResult<Vec<AvailableMode>> {
    // GDI enumerates modes for an active source, not an arbitrary inactive CCD
    // route. Inactive targets can still be enabled through validated CCD apply.
    let buffers = query(QDC_ONLY_ACTIVE_PATHS)?;
    let mut source = None;
    let mut current = None;
    for path in &buffers.paths {
        let device = names(path)?;
        if device.id.eq_ignore_ascii_case(id) {
            source = Some(device.source);
            current = Some(decode_path(path, &buffers.modes, names(path)?)?);
            break;
        }
    }
    let source = source.ok_or_else(|| {
        DisplayError::new(
            "unsupported_display",
            "mode enumeration requires an active display source",
        )
    })?;
    let name: Vec<u16> = source.encode_utf16().chain(Some(0)).collect();
    let mut modes = BTreeMap::new();
    for index in 0..4096 {
        let mut mode = DEVMODEW {
            dmSize: std::mem::size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        // SAFETY: name is NUL-terminated; the initialized DEVMODEW owns dmSize
        // bytes, and no driver-private trailing storage is requested.
        if unsafe { EnumDisplaySettingsExW(name.as_ptr(), index, &mut mode, 0) } == 0 {
            let mut available: Vec<AvailableMode> = modes.into_values().collect();
            if let Some(current) = current {
                let refresh = current.refresh;
                if refresh.denominator != 0
                    && current.source.pixel_format == DISPLAYCONFIG_PIXELFORMAT_32BPP
                {
                    let nominal = (u64::from(refresh.numerator)
                        + u64::from(refresh.denominator) / 2)
                        / u64::from(refresh.denominator);
                    if let Ok(nominal) = u32::try_from(nominal)
                        && available.iter().any(|mode| {
                            mode.width == current.source.width
                                && mode.height == current.source.height
                                && mode.bits_per_pixel == 32
                                && mode.nominal_refresh_hz == nominal
                        })
                    {
                        // CCD provides the exact rate of this active setting.
                        // GDI's other rows expose only integer nominal rates.
                        available.push(AvailableMode {
                            width: current.source.width,
                            height: current.source.height,
                            refresh,
                            nominal_refresh_hz: nominal,
                            bits_per_pixel: 32,
                            exact_current: true,
                        });
                    }
                }
            }
            return Ok(available);
        }
        if mode.dmPelsWidth == 0 || mode.dmPelsHeight == 0 {
            continue;
        }
        if mode.dmDisplayFrequency == 0 {
            continue;
        }
        modes.insert(
            (
                mode.dmPelsWidth,
                mode.dmPelsHeight,
                mode.dmDisplayFrequency,
                mode.dmBitsPerPel,
            ),
            AvailableMode {
                width: mode.dmPelsWidth,
                height: mode.dmPelsHeight,
                refresh: Rational {
                    numerator: mode.dmDisplayFrequency,
                    denominator: 1,
                },
                nominal_refresh_hz: mode.dmDisplayFrequency,
                bits_per_pixel: mode.dmBitsPerPel,
                exact_current: false,
            },
        );
    }
    Err(DisplayError::new(
        "resource_exhausted",
        "driver mode enumeration exceeds 4096 records",
    ))
}
fn device_header(
    kind: DISPLAYCONFIG_DEVICE_INFO_TYPE,
    size: usize,
    adapter: LUID,
    id: u32,
) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
    DISPLAYCONFIG_DEVICE_INFO_HEADER {
        r#type: kind,
        size: size as u32,
        adapterId: adapter,
        id,
    }
}
fn wide_string(value: &[u16]) -> DisplayResult<String> {
    let end = value
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(value.len());
    String::from_utf16(&value[..end])
        .map_err(|_| DisplayError::new("platform_error", "display API returned invalid UTF-16"))
}
pub fn check(code: i32, operation: &str) -> DisplayResult<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(DisplayError::new(
            "platform_error",
            format!("{operation} failed with Windows error {code}"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::decode_advanced_color_flags;

    #[test]
    fn advanced_color_support_and_enablement_bits_are_distinct() {
        assert_eq!(decode_advanced_color_flags(0).unwrap(), None);
        assert_eq!(decode_advanced_color_flags(1).unwrap(), Some(false));
        assert_eq!(decode_advanced_color_flags(3).unwrap(), Some(true));
        assert!(decode_advanced_color_flags(2).is_err());
        assert!(decode_advanced_color_flags(5).is_err());
    }
}
