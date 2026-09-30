use crate::model::{
    CaptureError, CaptureResult, DesktopGeometry, Frame, MAX_OUTPUTS, NativeAvailability, Output,
    frame_layout,
};
use amaterasu_media::{CaptureGeometry, DesktopRect};
use std::{
    ffi::{CString, c_char, c_void},
    time::Instant,
};

#[repr(C)]
#[derive(Clone, Copy)]
struct NativeOutput {
    output_id: u32,
    left: i32,
    top: i32,
    width: u32,
    height: u32,
    rotation: u32,
    name: [c_char; 128],
}

impl Default for NativeOutput {
    fn default() -> Self {
        Self {
            output_id: 0,
            left: 0,
            top: 0,
            width: 0,
            height: 0,
            rotation: 0,
            name: [0; 128],
        }
    }
}

#[repr(C)]
#[derive(Default)]
struct NativeInfo {
    available: u32,
    root_width: u32,
    root_height: u32,
    error: u32,
}

unsafe extern "C" {
    fn am_x11_probe(display: *const c_char, output: *mut NativeInfo) -> i32;
    fn am_x11_enumerate(
        display: *const c_char,
        outputs: *mut NativeOutput,
        capacity: usize,
        count: *mut usize,
        root_width: *mut u32,
        root_height: *mut u32,
    ) -> i32;
    fn am_x11_open(
        display: *const c_char,
        output_id: u32,
        expected_name: *const c_char,
        error: *mut u32,
    ) -> *mut c_void;
    fn am_x11_read(
        session: *mut c_void,
        destination: *mut u8,
        length: usize,
        width: *mut u32,
        height: *mut u32,
    ) -> i32;
    fn am_x11_close(session: *mut c_void);
    fn am_x11_session_geometry(
        session: *mut c_void,
        output: *mut NativeOutput,
        root_width: *mut u32,
        root_height: *mut u32,
    ) -> i32;
}

#[derive(Clone)]
pub struct NativeBackend {
    display: Option<CString>,
    availability: NativeAvailability,
}

impl NativeBackend {
    pub fn new() -> Self {
        Self::with_display(std::env::var("DISPLAY").ok())
    }

    fn with_display(display: Option<String>) -> Self {
        #[cfg(amaterasu_x11)]
        {
            let display = display
                .as_deref()
                .filter(|display| !display.is_empty())
                .and_then(|display| CString::new(display.as_bytes()).ok());
            let mut info = NativeInfo::default();
            let error = display.as_ref().map_or(1, |display| {
                // SAFETY: the adapter borrows a NUL-terminated display name and
                // writes only the fixed-size initialized information record.
                unsafe { am_x11_probe(display.as_ptr(), &mut info) }
            });
            let reason = if display.is_none() {
                Some("display_not_configured")
            } else if info.available != 0 {
                None
            } else {
                Some(error_reason(error as u32))
            };
            Self {
                display,
                availability: NativeAvailability {
                    native_supported: true,
                    available: reason.is_none(),
                    reason,
                },
            }
        }
        #[cfg(not(amaterasu_x11))]
        {
            let _ = display;
            Self {
                display: None,
                availability: NativeAvailability {
                    native_supported: false,
                    available: false,
                    reason: Some(if cfg!(target_os = "linux") {
                        "native_dependencies_unavailable"
                    } else {
                        "unsupported_platform"
                    }),
                },
            }
        }
    }
}

pub trait Backend: Send + Sync {
    fn availability(&self) -> NativeAvailability;
    fn enumerate(&self) -> CaptureResult<(Vec<Output>, DesktopRect)>;
    fn open(&self, output: &str) -> CaptureResult<Box<dyn CaptureSession>>;
}

impl Backend for NativeBackend {
    fn availability(&self) -> NativeAvailability {
        self.availability
    }

    fn enumerate(&self) -> CaptureResult<(Vec<Output>, DesktopRect)> {
        #[cfg(amaterasu_x11)]
        {
            let display = self.display.as_ref().ok_or_else(|| {
                CaptureError::new("display_not_configured", "DISPLAY is not configured")
            })?;
            let mut outputs = [NativeOutput::default(); MAX_OUTPUTS];
            let mut count = 0;
            let mut root_width = 0;
            let mut root_height = 0;
            // SAFETY: output storage has exactly `capacity` initialized records;
            // the adapter writes no more than that capacity and fills scalars.
            let result = unsafe {
                am_x11_enumerate(
                    display.as_ptr(),
                    outputs.as_mut_ptr(),
                    outputs.len(),
                    &mut count,
                    &mut root_width,
                    &mut root_height,
                )
            };
            if result != 0 {
                return Err(native_error(result as u32));
            }
            if count > outputs.len() {
                return Err(CaptureError::new(
                    "resource_exhausted",
                    "too many XRandR outputs",
                ));
            }
            let virtual_desktop = DesktopRect::new(0, 0, root_width, root_height)
                .map_err(|_| CaptureError::new("invalid_geometry", "invalid X11 root geometry"))?;
            let outputs = outputs[..count]
                .iter()
                .map(|output| {
                    let name = output_name(output)?;
                    let id = format!("x11:{name}/{:08x}", output.output_id);
                    Ok(Output {
                        id,
                        adapter: "x11".into(),
                        adapter_vendor_id: None,
                        adapter_device_id: None,
                        adapter_description: None,
                        device_name: name,
                        attached: true,
                        left: output.left,
                        top: output.top,
                        width: output.width,
                        height: output.height,
                        rotation: output.rotation,
                    })
                })
                .collect::<CaptureResult<Vec<_>>>()?;
            Ok((outputs, virtual_desktop))
        }
        #[cfg(not(amaterasu_x11))]
        {
            Err(CaptureError::new(
                self.availability.reason.unwrap_or("unsupported_platform"),
                "X11 capture is unavailable on this build",
            ))
        }
    }

    fn open(&self, output: &str) -> CaptureResult<Box<dyn CaptureSession>> {
        #[cfg(amaterasu_x11)]
        {
            let (name, output_id) = parse_output_id(output)?;
            let name = CString::new(name).map_err(|_| invalid_output_id())?;
            let display = self.display.as_ref().ok_or_else(|| {
                CaptureError::new("display_not_configured", "DISPLAY is not configured")
            })?;
            let mut error = 0;
            // SAFETY: the X11 adapter consumes the display and selected output
            // identity synchronously, then returns one uniquely owned handle.
            let session =
                unsafe { am_x11_open(display.as_ptr(), output_id, name.as_ptr(), &mut error) };
            if session.is_null() {
                return Err(native_error(error));
            }
            let mut session = OwnedX11Handle(session);
            let mut native_output = NativeOutput::default();
            let mut root_width = 0;
            let mut root_height = 0;
            // SAFETY: session remains owned below; outputs are fixed writable slots.
            let result = unsafe {
                am_x11_session_geometry(
                    session.0,
                    &mut native_output,
                    &mut root_width,
                    &mut root_height,
                )
            };
            if result != 0 {
                return Err(native_error(result as u32));
            }
            let output_rect = DesktopRect::new(
                native_output.left,
                native_output.top,
                native_output.width,
                native_output.height,
            )
            .map_err(|_| CaptureError::new("invalid_geometry", "invalid selected X11 output"))?;
            let desktop_rect = DesktopRect::new(0, 0, root_width, root_height)
                .map_err(|_| CaptureError::new("invalid_geometry", "invalid X11 root geometry"))?;
            let geometry = CaptureGeometry::new(output_rect, desktop_rect)
                .map_err(|_| CaptureError::new("invalid_geometry", "output is outside X11 root"))?;
            frame_layout(native_output.width, native_output.height)?;
            Ok(Box::new(X11Session {
                handle: session.take() as usize,
                geometry: DesktopGeometry { capture: geometry },
                width: native_output.width,
                height: native_output.height,
                opened_at: Instant::now(),
                sequence: 0,
            }))
        }
        #[cfg(not(amaterasu_x11))]
        {
            let _ = output;
            Err(CaptureError::new(
                self.availability.reason.unwrap_or("unsupported_platform"),
                "X11 capture is unavailable on this build",
            ))
        }
    }
}

#[cfg(amaterasu_x11)]
struct OwnedX11Handle(*mut c_void);

#[cfg(amaterasu_x11)]
impl OwnedX11Handle {
    fn take(&mut self) -> *mut c_void {
        let pointer = self.0;
        self.0 = std::ptr::null_mut();
        pointer
    }
}

#[cfg(amaterasu_x11)]
impl Drop for OwnedX11Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: this guard uniquely owns the adapter handle and releases it
            // exactly once if later geometry or layout validation fails.
            unsafe { am_x11_close(self.0) };
        }
    }
}

pub trait CaptureSession: Send {
    fn read(&mut self) -> CaptureResult<Frame>;
    fn geometry(&self) -> DesktopGeometry;
}

#[cfg(amaterasu_x11)]
struct X11Session {
    handle: usize,
    geometry: DesktopGeometry,
    width: u32,
    height: u32,
    opened_at: Instant,
    sequence: u64,
}

#[cfg(amaterasu_x11)]
impl CaptureSession for X11Session {
    fn read(&mut self) -> CaptureResult<Frame> {
        let (stride, length) = frame_layout(self.width, self.height)?;
        let mut bytes = vec![0; length];
        let mut width = 0;
        let mut height = 0;
        // SAFETY: the handle remains live until Drop and `bytes` is a writable
        // allocation of exactly the checked BGRA frame size for this call.
        let result = unsafe {
            am_x11_read(
                self.handle as *mut c_void,
                bytes.as_mut_ptr(),
                bytes.len(),
                &mut width,
                &mut height,
            )
        };
        if result != 0 {
            return Err(native_error(result as u32));
        }
        if (width, height) != (self.width, self.height) {
            return Err(CaptureError::new("access_lost", "X11 output mode changed"));
        }
        self.sequence = self.sequence.checked_add(1).ok_or_else(|| {
            CaptureError::new("sequence_exhausted", "X11 capture sequence exhausted")
        })?;
        let timestamp_us = self.opened_at.elapsed().as_micros().min(u64::MAX as u128) as u64;
        Ok(Frame {
            width,
            height,
            stride,
            timestamp_us,
            sequence: self.sequence,
            bytes,
        })
    }

    fn geometry(&self) -> DesktopGeometry {
        self.geometry
    }
}

#[cfg(amaterasu_x11)]
impl Drop for X11Session {
    fn drop(&mut self) {
        // SAFETY: the session owns this nonzero handle uniquely and Drop runs once.
        unsafe { am_x11_close(self.handle as *mut c_void) };
        self.handle = 0;
    }
}

#[cfg(amaterasu_x11)]
fn output_name(output: &NativeOutput) -> CaptureResult<String> {
    let bytes = output
        .name
        .iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| *byte as u8)
        .collect::<Vec<_>>();
    let name = String::from_utf8(bytes)
        .map_err(|_| CaptureError::new("invalid_output", "XRandR returned invalid output name"))?;
    if name.is_empty() || name.len() > 127 || name.chars().any(char::is_control) {
        return Err(CaptureError::new(
            "invalid_output",
            "XRandR output name is invalid",
        ));
    }
    Ok(name)
}

#[cfg(amaterasu_x11)]
fn parse_output_id(output: &str) -> CaptureResult<(&str, u32)> {
    let rest = output.strip_prefix("x11:").ok_or_else(invalid_output_id)?;
    let (name, raw_id) = rest.rsplit_once('/').ok_or_else(invalid_output_id)?;
    if name.is_empty()
        || name.len() > 127
        || raw_id.len() != 8
        || !raw_id.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(invalid_output_id());
    }
    let id = u32::from_str_radix(raw_id, 16).map_err(|_| invalid_output_id())?;
    Ok((name, id))
}

#[cfg(amaterasu_x11)]
fn invalid_output_id() -> CaptureError {
    CaptureError::new("invalid_output", "selected X11 output ID is invalid")
}

fn error_reason(error: u32) -> &'static str {
    match error {
        1 => "display_not_configured",
        2 => "display_connection_failed",
        3 => "xrandr_unavailable",
        4 => "output_unavailable",
        5 => "unsupported_pixel_format",
        6 => "capture_read_failed",
        7 => "too_many_outputs",
        _ => "capture_unavailable",
    }
}

fn native_error(error: u32) -> CaptureError {
    let code = match error {
        1 => "display_not_configured",
        2 => "display_connection_failed",
        3 => "xrandr_unavailable",
        4 => "output_unavailable",
        5 => "unsupported_pixel_format",
        6 => "capture_read_failed",
        7 => "too_many_outputs",
        _ => "capture_unavailable",
    };
    CaptureError::new(code, format!("X11 capture operation failed: {code}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_display_is_an_explicit_unavailable_state() {
        let backend = NativeBackend::with_display(None);
        assert!(!backend.availability().available);
        assert_eq!(
            backend.availability().reason,
            Some("display_not_configured")
        );
    }

    #[test]
    fn selected_output_id_requires_name_and_fixed_hex_server_id() {
        #[cfg(amaterasu_x11)]
        {
            assert_eq!(
                parse_output_id("x11:DP-1/000000a4").unwrap(),
                ("DP-1", 0xa4)
            );
            for invalid in [
                "DP-1/000000a4",
                "x11:/000000a4",
                "x11:DP-1/a4",
                "x11:DP-1/000000ag",
            ] {
                assert!(parse_output_id(invalid).is_err(), "{invalid}");
            }
        }
    }

    #[test]
    fn frame_sizes_are_bounded_and_checked() {
        assert_eq!(frame_layout(1920, 1080).unwrap(), (7680, 8_294_400));
        assert!(frame_layout(0, 1).is_err());
        assert!(frame_layout(8192, 8192).is_err());
        assert!(frame_layout(u32::MAX, 2).is_err());
    }
}
