use crate::{
    CaptureSelection, GeometryError, SelectedOutput,
    model::{CcdSnapshot, DisplaySnapshot, Topology, unavailable},
};
use amaterasu_media::DesktopRect;
use std::collections::BTreeMap;
use windows::Win32::{
    Devices::Display::*,
    Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, LPARAM, RECT},
    Graphics::{
        Dxgi::{CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, IDXGIFactory1},
        Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW},
    },
    UI::HiDpi::{
        AreDpiAwarenessContextsEqual, DPI_AWARENESS_CONTEXT,
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetThreadDpiAwarenessContext,
        SetThreadDpiAwarenessContext,
    },
};
use windows::core::BOOL;

const MAX_DISPLAYS: usize = 64;

/// Resolve a provider-selected output while every coordinate-producing call is
/// scoped to the same physical per-monitor DPI context.
pub fn resolve_with<T>(
    expected_target: Option<&str>,
    select: impl FnOnce() -> Result<(SelectedOutput, T), GeometryError>,
) -> Result<(CaptureSelection, T), GeometryError> {
    let (topology, (selected, retained)) = with_physical_topology(select)?;
    topology
        .resolve(&selected, expected_target)
        .map(|selection| (selection, retained))
}

/// Runs an output enumeration while Windows reports physical, per-monitor
/// coordinates and confirms that GDI, DXGI, and CCD agree on the active
/// topology. Capture providers must use this for their public enumeration as
/// well as their open path; otherwise a DPI-unaware module thread can publish
/// logical dimensions that disagree with the physical capture session.
pub fn with_verified_physical_topology<T>(
    enumerate: impl FnOnce() -> Result<T, GeometryError>,
) -> Result<T, GeometryError> {
    with_physical_topology(enumerate).map(|(_, value)| value)
}

fn with_physical_topology<T>(
    operation: impl FnOnce() -> Result<T, GeometryError>,
) -> Result<(Topology, T), GeometryError> {
    with_physical_coordinates(|| {
        let ccd_before = enumerate_ccd()?;
        let value = operation()?;
        let gdi = enumerate_gdi()?;
        let dxgi = enumerate_dxgi()?;
        let ccd_after = enumerate_ccd()?;
        if canonical_ccd(&ccd_before) != canonical_ccd(&ccd_after) {
            return Err(unavailable(
                "CCD topology changed while capture geometry was sampled",
            ));
        }
        let topology = Topology::reconcile(gdi, dxgi, ccd_after)?;
        Ok((topology, value))
    })
}

fn canonical_ccd(displays: &[CcdSnapshot]) -> Vec<(String, String, DesktopRect)> {
    let mut canonical = displays
        .iter()
        .map(|display| {
            (
                display.source_name.to_ascii_lowercase(),
                display.target_id.to_ascii_lowercase(),
                display.rectangle,
            )
        })
        .collect::<Vec<_>>();
    canonical.sort_unstable_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.left.cmp(&right.2.left))
            .then_with(|| left.2.top.cmp(&right.2.top))
            .then_with(|| left.2.width.cmp(&right.2.width))
            .then_with(|| left.2.height.cmp(&right.2.height))
    });
    canonical
}

struct GdiEnumeration {
    displays: BTreeMap<usize, DisplaySnapshot>,
    failed: bool,
}

fn enumerate_gdi() -> Result<Vec<DisplaySnapshot>, GeometryError> {
    let mut enumeration = GdiEnumeration {
        displays: BTreeMap::new(),
        failed: false,
    };
    // SAFETY: EnumDisplayMonitors invokes the callback synchronously with the
    // uniquely borrowed enumeration context.
    let succeeded = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(visit_monitor),
            LPARAM((&mut enumeration as *mut GdiEnumeration) as isize),
        )
    };
    if !succeeded.as_bool() || enumeration.failed || enumeration.displays.is_empty() {
        return Err(unavailable("GDI monitor enumeration failed"));
    }
    Ok(enumeration.displays.into_values().collect())
}

unsafe extern "system" fn visit_monitor(
    monitor: HMONITOR,
    _: HDC,
    _: *mut RECT,
    context: LPARAM,
) -> BOOL {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: enumerate_gdi supplies the exclusive context through this call.
        let enumeration = unsafe { &mut *(context.0 as *mut GdiEnumeration) };
        if enumeration.displays.len() >= MAX_DISPLAYS {
            enumeration.failed = true;
            return false;
        }
        match describe_monitor(monitor) {
            Ok(display) => {
                if enumeration
                    .displays
                    .insert(monitor.0 as usize, display)
                    .is_some()
                {
                    enumeration.failed = true;
                    return false;
                }
                true
            }
            Err(_) => {
                enumeration.failed = true;
                false
            }
        }
    }));
    BOOL::from(result.unwrap_or(false))
}

fn describe_monitor(monitor: HMONITOR) -> Result<DisplaySnapshot, GeometryError> {
    let mut information = MONITORINFOEXW::default();
    information.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: the extended structure begins with MONITORINFO and owns the
    // complete output buffer advertised by cbSize.
    if !unsafe {
        GetMonitorInfoW(
            monitor,
            (&mut information as *mut MONITORINFOEXW).cast::<MONITORINFO>(),
        )
    }
    .as_bool()
    {
        return Err(unavailable("GetMonitorInfoW failed"));
    }
    Ok(DisplaySnapshot {
        source_name: wide_string(&information.szDevice)?,
        monitor_handle: monitor.0 as usize,
        rectangle: desktop_rect(information.monitorInfo.rcMonitor)?,
    })
}

fn enumerate_dxgi() -> Result<Vec<DisplaySnapshot>, GeometryError> {
    // SAFETY: generated COM wrappers own every returned interface and copy each
    // output description before its interface is released.
    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|_| unavailable("DXGI factory creation failed"))?;
        let mut displays = Vec::new();
        for adapter_index in 0..32 {
            let adapter = match factory.EnumAdapters1(adapter_index) {
                Ok(adapter) => adapter,
                Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => return finish_dxgi(displays),
                Err(_) => return Err(unavailable("DXGI adapter enumeration failed")),
            };
            let mut output_enumeration_complete = false;
            for output_index in 0..=64 {
                let output = match adapter.EnumOutputs(output_index) {
                    Ok(output) => output,
                    Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => {
                        output_enumeration_complete = true;
                        break;
                    }
                    Err(_) => return Err(unavailable("DXGI output enumeration failed")),
                };
                let description = output
                    .GetDesc()
                    .map_err(|_| unavailable("DXGI output description failed"))?;
                if !description.AttachedToDesktop.as_bool() {
                    continue;
                }
                if displays.len() >= MAX_DISPLAYS {
                    return Err(unavailable("attached DXGI output count exceeds 64"));
                }
                displays.push(DisplaySnapshot {
                    source_name: wide_string(&description.DeviceName)?,
                    monitor_handle: description.Monitor.0 as usize,
                    rectangle: desktop_rect(description.DesktopCoordinates)?,
                });
            }
            if !output_enumeration_complete {
                return Err(unavailable(
                    "DXGI output enumeration exceeds 64 per adapter",
                ));
            }
        }
        Err(unavailable("DXGI adapter enumeration exceeds 32 adapters"))
    }
}

fn finish_dxgi(displays: Vec<DisplaySnapshot>) -> Result<Vec<DisplaySnapshot>, GeometryError> {
    if displays.is_empty() {
        Err(unavailable("DXGI reports no attached outputs"))
    } else {
        Ok(displays)
    }
}

fn enumerate_ccd() -> Result<Vec<CcdSnapshot>, GeometryError> {
    for _ in 0..4 {
        let mut path_count = 0;
        let mut mode_count = 0;
        // SAFETY: both counters are valid exclusive outputs.
        let sizes = unsafe {
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
        };
        if sizes != ERROR_SUCCESS {
            return Err(unavailable("CCD buffer sizing failed"));
        }
        if path_count as usize > MAX_DISPLAYS || mode_count > 1024 {
            return Err(unavailable("CCD active topology exceeds bounded buffers"));
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        // SAFETY: vectors own initialized writable buffers matching the counts.
        let result = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                None,
            )
        };
        if result == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        if result != ERROR_SUCCESS
            || path_count as usize > paths.len()
            || mode_count as usize > modes.len()
        {
            return Err(unavailable("CCD active topology query failed"));
        }
        paths.truncate(path_count as usize);
        modes.truncate(mode_count as usize);
        if paths.is_empty() {
            return Err(unavailable("CCD reports no active paths"));
        }
        return paths
            .iter()
            .map(|path| ccd_snapshot(path, &modes))
            .collect();
    }
    Err(unavailable(
        "CCD topology changed repeatedly during enumeration",
    ))
}

fn ccd_snapshot(
    path: &DISPLAYCONFIG_PATH_INFO,
    modes: &[DISPLAYCONFIG_MODE_INFO],
) -> Result<CcdSnapshot, GeometryError> {
    let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
            size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
            adapterId: path.sourceInfo.adapterId,
            id: path.sourceInfo.id,
        },
        ..Default::default()
    };
    let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
            size: size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
            adapterId: path.targetInfo.adapterId,
            id: path.targetInfo.id,
        },
        ..Default::default()
    };
    // SAFETY: each initialized packet begins with a header describing the full
    // writable concrete packet retained through the synchronous call.
    if unsafe { DisplayConfigGetDeviceInfo(&mut source.header) } != 0
        || unsafe { DisplayConfigGetDeviceInfo(&mut target.header) } != 0
    {
        return Err(unavailable("CCD device identity lookup failed"));
    }
    // SAFETY: without virtual-mode-aware flags, modeInfoIdx is the documented
    // active union view for this path.
    let mode_index = unsafe { path.sourceInfo.Anonymous.modeInfoIdx } as usize;
    let mode = modes
        .get(mode_index)
        .filter(|mode| {
            mode.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE
                && mode.id == path.sourceInfo.id
                && mode.adapterId == path.sourceInfo.adapterId
        })
        .ok_or_else(|| unavailable("CCD active path lacks its source mode"))?;
    // SAFETY: the validated SOURCE tag selects the sourceMode union member.
    let mode = unsafe { mode.Anonymous.sourceMode };
    let rectangle = DesktopRect::new(mode.position.x, mode.position.y, mode.width, mode.height)
        .map_err(|_| unavailable("CCD returned an invalid source rectangle"))?;
    let target_id = wide_string(&target.monitorDevicePath)?.to_ascii_lowercase();
    if target_id.is_empty() {
        return Err(unavailable("CCD target has no device-interface identity"));
    }
    Ok(CcdSnapshot {
        source_name: wide_string(&source.viewGdiDeviceName)?,
        target_id,
        rectangle,
    })
}

fn desktop_rect(rectangle: RECT) -> Result<DesktopRect, GeometryError> {
    let width = u32::try_from(i64::from(rectangle.right) - i64::from(rectangle.left))
        .ok()
        .filter(|width| *width != 0)
        .ok_or_else(|| unavailable("Windows returned an invalid desktop width"))?;
    let height = u32::try_from(i64::from(rectangle.bottom) - i64::from(rectangle.top))
        .ok()
        .filter(|height| *height != 0)
        .ok_or_else(|| unavailable("Windows returned an invalid desktop height"))?;
    DesktopRect::new(rectangle.left, rectangle.top, width, height)
        .map_err(|_| unavailable("Windows returned an invalid desktop rectangle"))
}

fn wide_string(value: &[u16]) -> Result<String, GeometryError> {
    let end = value
        .iter()
        .position(|word| *word == 0)
        .unwrap_or(value.len());
    String::from_utf16(&value[..end]).map_err(|_| unavailable("Windows returned invalid UTF-16"))
}

fn with_physical_coordinates<T>(
    operation: impl FnOnce() -> Result<T, GeometryError>,
) -> Result<T, GeometryError> {
    let mut scope = PhysicalCoordinateScope::enter()?;
    let result = operation();
    scope.restore()?;
    result
}

struct PhysicalCoordinateScope {
    previous: Option<DPI_AWARENESS_CONTEXT>,
}

impl PhysicalCoordinateScope {
    fn enter() -> Result<Self, GeometryError> {
        // SAFETY: this changes only the current thread and returns its previous
        // context, retained for restoration before returning to the caller.
        let previous =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        if previous.0.is_null() {
            return Err(unavailable(
                "per-monitor physical coordinates are unavailable",
            ));
        }
        let mut scope = Self {
            previous: Some(previous),
        };
        if !same_context(
            // SAFETY: reads the current thread's DPI awareness context.
            unsafe { GetThreadDpiAwarenessContext() },
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        ) {
            scope.restore()?;
            return Err(unavailable(
                "per-monitor physical coordinates are unavailable",
            ));
        }
        Ok(scope)
    }

    fn restore(&mut self) -> Result<(), GeometryError> {
        let Some(previous) = self.previous else {
            return Ok(());
        };
        // SAFETY: previous belongs to this thread and is restored exactly once.
        let replaced = unsafe { SetThreadDpiAwarenessContext(previous) };
        if replaced.0.is_null()
            || !same_context(
                // SAFETY: reads the current thread's restored context.
                unsafe { GetThreadDpiAwarenessContext() },
                previous,
            )
        {
            return Err(unavailable("failed to restore the thread DPI context"));
        }
        self.previous = None;
        Ok(())
    }
}

impl Drop for PhysicalCoordinateScope {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            // SAFETY: best-effort restoration during unwind on the same thread.
            unsafe { SetThreadDpiAwarenessContext(previous) };
        }
    }
}

fn same_context(left: DPI_AWARENESS_CONTEXT, right: DPI_AWARENESS_CONTEXT) -> bool {
    // SAFETY: both values are predefined or returned DPI awareness contexts.
    unsafe { AreDpiAwarenessContextsEqual(left, right) }.as_bool()
}
