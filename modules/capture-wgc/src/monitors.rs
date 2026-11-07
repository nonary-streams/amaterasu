use crate::model::{CaptureError, CaptureResult, Output};
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, IDXGIFactory1, IDXGIOutput6,
};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::core::{BOOL, Interface};

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
    let adapter = dxgi_adapter_for(&device_name, rectangle);
    let metadata = Output {
        id: device_name.clone(),
        adapter: adapter
            .as_ref()
            .map(|adapter| adapter.id.clone())
            .unwrap_or_else(|| "unmapped".into()),
        adapter_vendor_id: adapter.as_ref().map(|adapter| adapter.vendor_id),
        adapter_device_id: adapter.as_ref().map(|adapter| adapter.device_id),
        adapter_description: adapter.map(|adapter| adapter.description),
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

struct Adapter {
    id: String,
    vendor_id: u32,
    device_id: u32,
    description: String,
}

/// Correlate the GDI monitor chosen for WGC with the same physical DXGI output
/// identity used by desktop duplication. Failure deliberately leaves the
/// adapter unknown; callers must then reject implicit encoder selection.
fn dxgi_adapter_for(device_name: &str, rectangle: RECT) -> Option<Adapter> {
    // SAFETY: all returned interfaces are local COM wrappers and no pointer
    // escapes this bounded, read-only topology walk.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
        for adapter_index in 0..32 {
            let adapter = match factory.EnumAdapters1(adapter_index) {
                Ok(adapter) => adapter,
                Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => return None,
                Err(_) => return None,
            };
            let description = adapter.GetDesc1().ok()?;
            for output_index in 0..64 {
                let output = match adapter.EnumOutputs(output_index) {
                    Ok(output) => output,
                    Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                    Err(_) => return None,
                };
                let output_description = output.GetDesc().ok()?;
                if wide_string(&output_description.DeviceName) == device_name
                    && output_description.DesktopCoordinates.left == rectangle.left
                    && output_description.DesktopCoordinates.top == rectangle.top
                    && output_description.DesktopCoordinates.right == rectangle.right
                    && output_description.DesktopCoordinates.bottom == rectangle.bottom
                {
                    return Some(Adapter {
                        id: format!(
                            "{:08x}:{:08x}",
                            description.AdapterLuid.HighPart as u32,
                            description.AdapterLuid.LowPart
                        ),
                        vendor_id: description.VendorId,
                        device_id: description.DeviceId,
                        description: wide_string(&description.Description),
                    });
                }
            }
        }
        None
    }
}

fn wide_string(value: &[u16]) -> String {
    let length = value
        .iter()
        .position(|word| *word == 0)
        .unwrap_or(value.len());
    String::from_utf16_lossy(&value[..length])
}

fn rectangle_extent(start: i32, end: i32) -> CaptureResult<u32> {
    u32::try_from(i64::from(end) - i64::from(start))
        .ok()
        .filter(|extent| *extent != 0)
        .ok_or_else(|| CaptureError::new("backend_error", "GDI returned an invalid monitor span"))
}

/// Match the exact selected GDI monitor to an attached DXGI Output6 before
/// trusting its live scan-out transfer function. Desktop HDR flags alone do
/// not establish the captured texture's pixel encoding.
pub fn desktop_color(monitor: &Monitor) -> CaptureResult<crate::format::DesktopColor> {
    let unavailable = || {
        CaptureError::new(
            "source_color_unavailable",
            "exact monitor has no unambiguous DXGI Output6 color observation",
        )
    };
    // SAFETY: bounded read-only COM enumeration; returned values are copied,
    // and no borrowed native output or OS handle escapes the call.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|_| unavailable())?;
        let mut observed = None;
        for adapter_index in 0..32 {
            let adapter = match factory.EnumAdapters1(adapter_index) {
                Ok(adapter) => adapter,
                Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(_) => return Err(unavailable()),
            };
            for output_index in 0..64 {
                let output = match adapter.EnumOutputs(output_index) {
                    Ok(output) => output,
                    Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                    Err(_) => return Err(unavailable()),
                };
                let description = output.GetDesc().map_err(|_| unavailable())?;
                if description.Monitor != monitor.handle
                    || !description.AttachedToDesktop.as_bool()
                    || !wide_string(&description.DeviceName)
                        .eq_ignore_ascii_case(&monitor.metadata.device_name)
                    || description.DesktopCoordinates.left != monitor.metadata.left
                    || description.DesktopCoordinates.top != monitor.metadata.top
                    || i64::from(description.DesktopCoordinates.right)
                        - i64::from(description.DesktopCoordinates.left)
                        != i64::from(monitor.metadata.width)
                    || i64::from(description.DesktopCoordinates.bottom)
                        - i64::from(description.DesktopCoordinates.top)
                        != i64::from(monitor.metadata.height)
                {
                    continue;
                }
                if observed.is_some() {
                    return Err(unavailable());
                }
                let output: IDXGIOutput6 = output.cast().map_err(|_| unavailable())?;
                let color = output.GetDesc1().map_err(|_| unavailable())?;
                observed = Some(crate::format::DesktopColor {
                    color_space: color.ColorSpace.0,
                    bits_per_color: color.BitsPerColor,
                });
            }
        }
        observed.ok_or_else(unavailable)
    }
}
