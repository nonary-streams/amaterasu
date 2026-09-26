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

pub fn advanced_color_enabled(path: &DISPLAYCONFIG_PATH_INFO) -> DisplayResult<bool> {
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
        return Ok(false);
    }
    check(result, "DisplayConfigGetDeviceInfo(advanced color)")?;
    // SAFETY: value is the documented raw bitfield view of this initialized union.
    let flags = unsafe { packet.Anonymous.value };
    Ok(flags & (2 | 4) != 0)
}

pub fn snapshot() -> DisplayResult<Snapshot> {
    let buffers = query(QDC_ONLY_ACTIVE_PATHS)?;
    let mut paths = Vec::new();
    for path in &buffers.paths {
        if advanced_color_enabled(path)? {
            return Err(DisplayError::new(
                "unsupported_display",
                "advanced color/HDR is active; its complete state is outside this CCD recovery format",
            ));
        }
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
        paths.push(decode_path(path, &buffers.modes, names(path)?)?);
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
        monitors.insert(device.id.clone(), json!({"id":device.id,"name":device.friendly,"active":current.is_some(),"source":device.source,"advanced_color_enabled":advanced_color_enabled(path).ok(),"current":mode.map(|path| json!({"width":path.source.width,"height":path.source.height,"x":path.source.x,"y":path.source.y,"rotation":path.rotation,"refresh":path.refresh}))}));
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
            return Ok(modes.into_values().collect());
        }
        if mode.dmPelsWidth == 0 || mode.dmPelsHeight == 0 {
            continue;
        }
        if mode.dmDisplayFrequency == 0 {
            continue;
        }
        let refresh = current
            .as_ref()
            .filter(|current| {
                current.source.width == mode.dmPelsWidth
                    && current.source.height == mode.dmPelsHeight
                    && current.refresh.denominator != 0
                    && (u64::from(current.refresh.numerator)
                        + u64::from(current.refresh.denominator) / 2)
                        / u64::from(current.refresh.denominator)
                        == u64::from(mode.dmDisplayFrequency)
            })
            .map_or(
                Rational {
                    numerator: mode.dmDisplayFrequency,
                    denominator: 1,
                },
                |current| current.refresh,
            );
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
                refresh,
                nominal_refresh_hz: mode.dmDisplayFrequency,
                bits_per_pixel: mode.dmBitsPerPel,
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
