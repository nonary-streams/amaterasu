use crate::model::{CaptureError, CaptureResult, Output};
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::core::BOOL;

pub struct Monitor {
    pub handle: HMONITOR,
    pub metadata: Output,
}

struct Enumeration {
    monitors: Vec<Monitor>,
    failed: bool,
}

pub fn enumerate() -> CaptureResult<Vec<Monitor>> {
    let mut enumeration = Enumeration {
        monitors: Vec::new(),
        failed: false,
    };
    // SAFETY: EnumDisplayMonitors invokes the callback synchronously. Its
    // context is an exclusive borrow of enumeration lasting through the call.
    let success = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(visit_monitor),
            LPARAM((&mut enumeration as *mut Enumeration) as isize),
        )
    };
    if !success.as_bool() || enumeration.failed {
        return Err(CaptureError::new(
            "backend_error",
            "monitor enumeration failed or exceeded 64 monitors",
        ));
    }
    Ok(enumeration.monitors)
}

unsafe extern "system" fn visit_monitor(
    monitor: HMONITOR,
    _: HDC,
    _: *mut RECT,
    context: LPARAM,
) -> BOOL {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: enumerate supplies a live uniquely borrowed context and GDI
        // supplies a monitor handle valid during this synchronous callback.
        let enumeration = unsafe { &mut *(context.0 as *mut Enumeration) };
        if enumeration.monitors.len() >= 64 {
            enumeration.failed = true;
            return false;
        }
        match describe(monitor) {
            Ok(monitor) => enumeration.monitors.push(monitor),
            Err(_) => {
                enumeration.failed = true;
                return false;
            }
        }
        true
    }));
    BOOL::from(result.unwrap_or(false))
}

fn describe(handle: HMONITOR) -> CaptureResult<Monitor> {
    let mut information = MONITORINFOEXW::default();
    information.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: MONITORINFOEXW begins with MONITORINFO; cbSize advertises the
    // complete writable structure, owned locally through GetMonitorInfoW.
    if !unsafe {
        GetMonitorInfoW(
            handle,
            (&mut information as *mut MONITORINFOEXW).cast::<MONITORINFO>(),
        )
    }
    .as_bool()
    {
        return Err(CaptureError::new("backend_error", "GetMonitorInfoW failed"));
    }
    let name_length = information
        .szDevice
        .iter()
        .position(|word| *word == 0)
        .unwrap_or(information.szDevice.len());
    let device_name = String::from_utf16_lossy(&information.szDevice[..name_length]);
    let rectangle = information.monitorInfo.rcMonitor;
    let width = rectangle_extent(rectangle.left, rectangle.right)?;
    let height = rectangle_extent(rectangle.top, rectangle.bottom)?;
    let metadata = Output {
        id: device_name.clone(),
        adapter: "system-selected".into(),
        device_name,
        attached: true,
        left: rectangle.left,
        top: rectangle.top,
        width,
        height,
        // WGC captures the monitor's presented orientation rather than a
        // DXGI duplication surface needing consumer-side rotation.
        rotation: 1,
    };
    Ok(Monitor { handle, metadata })
}

fn rectangle_extent(start: i32, end: i32) -> CaptureResult<u32> {
    u32::try_from(i64::from(end) - i64::from(start))
        .ok()
        .filter(|extent| *extent != 0)
        .ok_or_else(|| CaptureError::new("backend_error", "GDI returned an invalid monitor span"))
}
