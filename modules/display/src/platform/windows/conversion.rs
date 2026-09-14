use super::query::DeviceNames;
use crate::model::{
    Address, DisplayError, DisplayPath, DisplayResult, Rational, Signal, SourceMode,
};
use windows_sys::Win32::Devices::Display::*;
use windows_sys::Win32::Foundation::{LUID, POINTL};

pub fn address(adapter: LUID, id: u32) -> Address {
    Address {
        adapter_low: adapter.LowPart,
        adapter_high: adapter.HighPart,
        id,
    }
}
pub fn rational(value: DISPLAYCONFIG_RATIONAL) -> Rational {
    Rational {
        numerator: value.Numerator,
        denominator: value.Denominator,
    }
}
pub fn native_rational(value: Rational) -> DISPLAYCONFIG_RATIONAL {
    DISPLAYCONFIG_RATIONAL {
        Numerator: value.numerator,
        Denominator: value.denominator,
    }
}
pub fn native_source(value: &SourceMode) -> DISPLAYCONFIG_SOURCE_MODE {
    DISPLAYCONFIG_SOURCE_MODE {
        width: value.width,
        height: value.height,
        pixelFormat: value.pixel_format,
        position: POINTL {
            x: value.x,
            y: value.y,
        },
    }
}
pub fn native_signal(value: &Signal) -> DISPLAYCONFIG_VIDEO_SIGNAL_INFO {
    DISPLAYCONFIG_VIDEO_SIGNAL_INFO {
        pixelRate: value.pixel_rate,
        hSyncFreq: native_rational(value.horizontal_sync),
        vSyncFreq: native_rational(value.vertical_sync),
        activeSize: DISPLAYCONFIG_2DREGION {
            cx: value.active_width,
            cy: value.active_height,
        },
        totalSize: DISPLAYCONFIG_2DREGION {
            cx: value.total_width,
            cy: value.total_height,
        },
        Anonymous: DISPLAYCONFIG_VIDEO_SIGNAL_INFO_0 {
            videoStandard: value.video_standard,
        },
        scanLineOrdering: value.scan_line_ordering,
    }
}
pub fn decode_path(
    path: &DISPLAYCONFIG_PATH_INFO,
    modes: &[DISPLAYCONFIG_MODE_INFO],
    names: DeviceNames,
) -> DisplayResult<DisplayPath> {
    // SAFETY: queries intentionally omit QDC_VIRTUAL_MODE_AWARE, so these unions
    // contain the documented full-width modeInfoIdx compatibility view.
    let (source_index, target_index) = unsafe {
        (
            path.sourceInfo.Anonymous.modeInfoIdx,
            path.targetInfo.Anonymous.modeInfoIdx,
        )
    };
    let source_mode = modes
        .get(source_index as usize)
        .filter(|mode| {
            mode.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE
                && address(mode.adapterId, mode.id)
                    == address(path.sourceInfo.adapterId, path.sourceInfo.id)
        })
        .ok_or_else(|| {
            DisplayError::new(
                "unsupported_display",
                "active path lacks a complete source mode",
            )
        })?;
    let target_mode = modes
        .get(target_index as usize)
        .filter(|mode| {
            mode.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_TARGET
                && address(mode.adapterId, mode.id)
                    == address(path.targetInfo.adapterId, path.targetInfo.id)
        })
        .ok_or_else(|| {
            DisplayError::new(
                "unsupported_display",
                "active path lacks a complete target mode",
            )
        })?;
    // SAFETY: each union tag and adapter/id association was validated above.
    let source = unsafe { source_mode.Anonymous.sourceMode };
    // SAFETY: the validated TARGET mode contains a targetVideoSignalInfo value.
    let signal = unsafe { target_mode.Anonymous.targetMode.targetVideoSignalInfo };
    // SAFETY: videoStandard is the documented raw bitfield union representation.
    let video_standard = unsafe { signal.Anonymous.videoStandard };
    Ok(DisplayPath {
        id: names.id,
        source_name: names.source,
        source_address: address(path.sourceInfo.adapterId, path.sourceInfo.id),
        target_address: address(path.targetInfo.adapterId, path.targetInfo.id),
        source: SourceMode {
            width: source.width,
            height: source.height,
            pixel_format: source.pixelFormat,
            x: source.position.x,
            y: source.position.y,
        },
        signal: Signal {
            pixel_rate: signal.pixelRate,
            horizontal_sync: rational(signal.hSyncFreq),
            vertical_sync: rational(signal.vSyncFreq),
            active_width: signal.activeSize.cx,
            active_height: signal.activeSize.cy,
            total_width: signal.totalSize.cx,
            total_height: signal.totalSize.cy,
            video_standard,
            scan_line_ordering: signal.scanLineOrdering,
        },
        rotation: path.targetInfo.rotation,
        scaling: path.targetInfo.scaling,
        refresh: rational(path.targetInfo.refreshRate),
        scan_line_ordering: path.targetInfo.scanLineOrdering,
        output_technology: path.targetInfo.outputTechnology,
        source_flags: path.sourceInfo.statusFlags,
        target_flags: path.targetInfo.statusFlags,
        path_flags: path.flags,
    })
}
