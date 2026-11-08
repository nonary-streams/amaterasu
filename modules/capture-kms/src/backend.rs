use crate::model::{
    CaptureError, CaptureResult, DesktopGeometry, Frame, MAX_OUTPUTS, NativeAvailability, Output,
    frame_layout, validate_output_id,
};
use amaterasu_media::{CaptureGeometry, DesktopRect};
use std::{ffi::CString, time::Instant};

#[cfg(amaterasu_kms)]
use std::ffi::{c_char, c_void};

#[cfg(amaterasu_kms)]
use crate::native::{
    NativeInfo, NativeOutput, am_kms_close, am_kms_enumerate, am_kms_open, am_kms_probe,
    am_kms_read, am_kms_session_geometry,
};

#[derive(Clone, Default)]
pub struct NativeBackend;

impl NativeBackend {
    pub fn new() -> Self {
        Self
    }
}

pub trait Backend: Send + Sync {
    fn availability(&self) -> NativeAvailability;
    fn enumerate(&self) -> CaptureResult<(Vec<Output>, DesktopRect)>;
    fn open(&self, output: &str) -> CaptureResult<Box<dyn CaptureSession>>;
}

impl Backend for NativeBackend {
    fn availability(&self) -> NativeAvailability {
        probe_availability()
    }

    fn enumerate(&self) -> CaptureResult<(Vec<Output>, DesktopRect)> {
        #[cfg(amaterasu_kms)]
        {
            let mut native_outputs = [NativeOutput::default(); MAX_OUTPUTS];
            let mut count = 0;
            // SAFETY: the array has exactly `capacity` initialized ABI records;
            // the adapter writes no more than that capacity and initializes count.
            let result = unsafe {
                am_kms_enumerate(
                    native_outputs.as_mut_ptr(),
                    native_outputs.len(),
                    &mut count,
                )
            };
            if result != 0 {
                return Err(native_error(result as u32));
            }
            if count > native_outputs.len() {
                return Err(CaptureError::new(
                    "resource_exhausted",
                    "too many DRM outputs",
                ));
            }
            let outputs = native_outputs[..count]
                .iter()
                .map(convert_native_output)
                .collect::<CaptureResult<Vec<_>>>()?;
            let display_rectangles = outputs
                .iter()
                .map(|output| {
                    DesktopRect::new(output.left, output.top, output.width, output.height).map_err(
                        |_| {
                            CaptureError::new(
                                "invalid_geometry",
                                "DRM returned an invalid output rectangle",
                            )
                        },
                    )
                })
                .collect::<CaptureResult<Vec<_>>>()?;
            let desktop = bounding_rectangle(&display_rectangles)?;
            Ok((outputs, desktop))
        }
        #[cfg(not(amaterasu_kms))]
        {
            Err(CaptureError::new(
                probe_availability()
                    .reason
                    .unwrap_or("unsupported_platform"),
                "DRM capture is unavailable on this build",
            ))
        }
    }

    fn open(&self, output: &str) -> CaptureResult<Box<dyn CaptureSession>> {
        validate_output_id(output)?;
        #[cfg(amaterasu_kms)]
        {
            let output_id = CString::new(output)
                .map_err(|_| CaptureError::new("invalid_output", "invalid DRM output ID"))?;
            let mut error = 0;
            // SAFETY: the NUL-terminated identity is borrowed synchronously; a
            // non-null return is one uniquely owned native session handle.
            let handle = unsafe { am_kms_open(output_id.as_ptr(), &mut error) };
            if handle.is_null() {
                return Err(native_error(error));
            }
            let mut session = OwnedKmsHandle(handle);
            let mut native_output = NativeOutput::default();
            // SAFETY: the guard keeps this handle live and the output record is
            // a fixed initialized writable ABI slot.
            let result = unsafe { am_kms_session_geometry(session.0, &mut native_output) };
            if result != 0 {
                return Err(native_error(result as u32));
            }
            let output = convert_native_output(&native_output)?;
            if output.id != output_id.to_string_lossy().as_ref()
                || !output.framebuffer_export_supported
            {
                return Err(CaptureError::new(
                    "output_unavailable",
                    "selected DRM output changed or cannot export its framebuffer",
                ));
            }
            let (outputs, desktop) = self.enumerate()?;
            let selected = outputs
                .iter()
                .find(|candidate| candidate.id == output.id)
                .ok_or_else(|| {
                    CaptureError::new("output_unavailable", "selected DRM output disappeared")
                })?;
            if !selected.framebuffer_export_supported
                || (
                    selected.left,
                    selected.top,
                    selected.width,
                    selected.height,
                    selected.rotation,
                ) != (
                    output.left,
                    output.top,
                    output.width,
                    output.height,
                    output.rotation,
                )
            {
                return Err(CaptureError::new(
                    "access_lost",
                    "DRM output geometry changed while opening the capture stream",
                ));
            }
            let output_rect =
                DesktopRect::new(selected.left, selected.top, selected.width, selected.height)
                    .map_err(|_| {
                        CaptureError::new("invalid_geometry", "invalid DRM output bounds")
                    })?;
            let geometry = CaptureGeometry::new(output_rect, desktop).map_err(|_| {
                CaptureError::new("invalid_geometry", "DRM output is outside desktop")
            })?;
            frame_layout(selected.width, selected.height)?;
            Ok(Box::new(KmsSession {
                handle: session.take() as usize,
                output: selected.clone(),
                geometry: DesktopGeometry { capture: geometry },
                opened_at: Instant::now(),
                sequence: 0,
                last_timestamp_us: 0,
            }))
        }
        #[cfg(not(amaterasu_kms))]
        {
            let _ = self;
            let _ = output;
            Err(CaptureError::new(
                probe_availability()
                    .reason
                    .unwrap_or("unsupported_platform"),
                "DRM capture is unavailable on this build",
            ))
        }
    }
}

pub trait CaptureSession: Send {
    fn read(&mut self) -> CaptureResult<Frame>;
    fn geometry(&self) -> DesktopGeometry;
}

#[cfg(amaterasu_kms)]
struct OwnedKmsHandle(*mut c_void);

#[cfg(amaterasu_kms)]
impl OwnedKmsHandle {
    fn take(&mut self) -> *mut c_void {
        let pointer = self.0;
        self.0 = std::ptr::null_mut();
        pointer
    }
}

#[cfg(amaterasu_kms)]
impl Drop for OwnedKmsHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: this guard uniquely owns the adapter handle and releases it
            // exactly once if later geometry or validation fails.
            unsafe { am_kms_close(self.0) };
        }
    }
}

#[cfg(amaterasu_kms)]
struct KmsSession {
    handle: usize,
    output: Output,
    geometry: DesktopGeometry,
    opened_at: Instant,
    sequence: u64,
    last_timestamp_us: u64,
}

#[cfg(amaterasu_kms)]
impl CaptureSession for KmsSession {
    fn read(&mut self) -> CaptureResult<Frame> {
        let (stride, length) = frame_layout(self.output.width, self.output.height)?;
        let mut bytes = vec![0; length];
        let mut width = 0;
        let mut height = 0;
        // SAFETY: the native handle remains owned until Drop; `bytes` is a
        // writable allocation of the checked frame size for this synchronous call.
        let result = unsafe {
            am_kms_read(
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
        if (width, height) != (self.output.width, self.output.height) {
            return Err(CaptureError::new("access_lost", "DRM output mode changed"));
        }
        self.sequence = self.sequence.checked_add(1).ok_or_else(|| {
            CaptureError::new("sequence_exhausted", "DRM capture sequence exhausted")
        })?;
        let elapsed_us = self.opened_at.elapsed().as_micros().min(u64::MAX as u128) as u64;
        self.last_timestamp_us =
            elapsed_us.max(self.last_timestamp_us.checked_add(1).ok_or_else(|| {
                CaptureError::new("timestamp_exhausted", "DRM capture timestamp exhausted")
            })?);
        Ok(Frame {
            width,
            height,
            stride,
            timestamp_us: self.last_timestamp_us,
            sequence: self.sequence,
            bytes,
        })
    }

    fn geometry(&self) -> DesktopGeometry {
        self.geometry
    }
}

#[cfg(amaterasu_kms)]
impl Drop for KmsSession {
    fn drop(&mut self) {
        if self.handle != 0 {
            // SAFETY: the session owns this native handle uniquely and Drop runs once.
            unsafe { am_kms_close(self.handle as *mut c_void) };
            self.handle = 0;
        }
    }
}

fn bounding_rectangle(rectangles: &[DesktopRect]) -> CaptureResult<DesktopRect> {
    let first = rectangles
        .first()
        .copied()
        .ok_or_else(|| CaptureError::new("output_unavailable", "no active DRM outputs"))?;
    let mut left = i64::from(first.left);
    let mut top = i64::from(first.top);
    let mut right = left + i64::from(first.width);
    let mut bottom = top + i64::from(first.height);
    for rectangle in rectangles.iter().skip(1) {
        left = left.min(i64::from(rectangle.left));
        top = top.min(i64::from(rectangle.top));
        right = right.max(i64::from(rectangle.left) + i64::from(rectangle.width));
        bottom = bottom.max(i64::from(rectangle.top) + i64::from(rectangle.height));
    }
    DesktopRect::new(
        i32::try_from(left)
            .map_err(|_| CaptureError::new("invalid_geometry", "DRM desktop origin overflows"))?,
        i32::try_from(top)
            .map_err(|_| CaptureError::new("invalid_geometry", "DRM desktop origin overflows"))?,
        u32::try_from(right - left)
            .map_err(|_| CaptureError::new("invalid_geometry", "DRM desktop width overflows"))?,
        u32::try_from(bottom - top)
            .map_err(|_| CaptureError::new("invalid_geometry", "DRM desktop height overflows"))?,
    )
    .map_err(|_| CaptureError::new("invalid_geometry", "DRM desktop bounds are invalid"))
}

fn probe_availability() -> NativeAvailability {
    #[cfg(amaterasu_kms)]
    {
        let mut info = NativeInfo::default();
        // SAFETY: native probe receives one initialized fixed-size record and
        // writes scalar capability fields only; it reads metadata but no pixels.
        let result = unsafe { am_kms_probe(&mut info) };
        NativeAvailability {
            native_supported: true,
            available: result == 0 && info.available != 0,
            framebuffer_export_supported: result == 0 && info.framebuffer_export_supported != 0,
            presentation_events: false,
            managed_virtual_output: false,
            reason: (result != 0).then_some(error_reason(info.error.max(result as u32))),
        }
    }
    #[cfg(not(amaterasu_kms))]
    {
        NativeAvailability {
            native_supported: false,
            available: false,
            framebuffer_export_supported: false,
            presentation_events: false,
            managed_virtual_output: false,
            reason: Some(if cfg!(target_os = "linux") {
                "native_dependencies_unavailable"
            } else {
                "unsupported_platform"
            }),
        }
    }
}

#[cfg(amaterasu_kms)]
fn convert_native_output(output: &NativeOutput) -> CaptureResult<Output> {
    let id = fixed_string(&output.id)?;
    validate_output_id(&id)?;
    let name = fixed_string(&output.name)?;
    let card = fixed_string(&output.card)?;
    if name.is_empty()
        || card.is_empty()
        || output.attached > 1
        || output.framebuffer_export_supported > 1
    {
        return Err(CaptureError::new(
            "invalid_output",
            "DRM returned malformed output metadata",
        ));
    }
    Ok(Output {
        id,
        adapter: card.clone(),
        adapter_vendor_id: None,
        adapter_device_id: None,
        adapter_description: Some("DRM/KMS primary plane".into()),
        device_name: format!("{name} ({card})"),
        attached: output.attached != 0,
        framebuffer_export_supported: output.framebuffer_export_supported != 0,
        readback_reason: (output.framebuffer_export_supported == 0)
            .then_some(error_reason(output.error)),
        left: output.left,
        top: output.top,
        width: output.width,
        height: output.height,
        rotation: output.rotation,
    })
}

#[cfg(amaterasu_kms)]
fn fixed_string<const N: usize>(bytes: &[c_char; N]) -> CaptureResult<String> {
    let bytes = bytes
        .iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| *byte as u8)
        .collect::<Vec<_>>();
    let value = String::from_utf8(bytes)
        .map_err(|_| CaptureError::new("invalid_output", "DRM returned invalid UTF-8 text"))?;
    if value.len() >= N || value.chars().any(char::is_control) {
        return Err(CaptureError::new(
            "invalid_output",
            "DRM returned invalid output text",
        ));
    }
    Ok(value)
}

fn error_reason(error: u32) -> &'static str {
    match error {
        1 => "drm_device_unavailable",
        2 => "active_output_unavailable",
        3 => "drm_framebuffer_access_denied",
        4 => "drm_format_or_geometry_unsupported",
        5 => "invalid_native_request",
        6 => "drm_capture_failed",
        7 => "drm_output_changed",
        8 => "too_many_drm_outputs",
        9 => "gbm_egl_import_unavailable",
        _ => "drm_capture_unavailable",
    }
}

pub(crate) fn native_error(error: u32) -> CaptureError {
    let code = match error {
        1 => "device_lost",
        2 => "output_unavailable",
        3 => "permission_denied",
        4 | 9 => "unsupported_pixel_format",
        5 => "invalid_request",
        6 => "capture_read_failed",
        7 => "access_lost",
        8 => "resource_exhausted",
        _ => "capture_unavailable",
    };
    CaptureError::new(code, error_reason(error))
}

// Prototype remains test-only until lifecycle and package integration are
// qualified. No production consumer silently switches providers.
#[cfg(all(test, feature = "isolated-helper", target_os = "linux", amaterasu_kms))]
mod isolated {
    use super::*;
    use crate::{
        helper_client::Client,
        helper_process::{self, Registry},
        open_task::{OpenTask, TaskPoll},
    };
    use std::io;

    pub(crate) fn convert_isolated_info(info: NativeInfo) -> NativeAvailability {
        NativeAvailability {
            native_supported: true,
            available: info.error == 0 && info.available != 0,
            framebuffer_export_supported: info.error == 0 && info.framebuffer_export_supported != 0,
            presentation_events: false,
            managed_virtual_output: false,
            reason: (info.error != 0).then_some(error_reason(info.error)),
        }
    }

    pub(crate) fn convert_isolated_outputs(
        records: &[NativeOutput],
    ) -> CaptureResult<(Vec<Output>, DesktopRect)> {
        let outputs = records
            .iter()
            .map(convert_native_output)
            .collect::<CaptureResult<Vec<_>>>()?;
        let rectangles = outputs
            .iter()
            .map(|output| {
                DesktopRect::new(output.left, output.top, output.width, output.height).map_err(
                    |_| {
                        CaptureError::new(
                            "invalid_geometry",
                            "DRM returned an invalid output rectangle",
                        )
                    },
                )
            })
            .collect::<CaptureResult<Vec<_>>>()?;
        let desktop = bounding_rectangle(&rectangles)?;
        Ok((outputs, desktop))
    }

    pub(crate) struct IsolatedOpenTask(OpenTask<CaptureResult<IsolatedSession>>);
    impl IsolatedOpenTask {
        pub fn spawn(registry: Registry, output: String) -> io::Result<Self> {
            OpenTask::spawn(move |cancellation| {
                validate_output_id(&output)?;
                if cancellation.cancelled() {
                    return Err(CaptureError::new("cancelled", "KMS open cancelled"));
                }
                let image = include_bytes!(concat!(env!("OUT_DIR"), "/amaterasu-kms-helper"));
                let (socket, receipt) =
                    helper_process::spawn_cancellable(&registry, image, &cancellation).map_err(
                        |error| CaptureError::new("capture_unavailable", error.to_string()),
                    )?;
                if let Err(error) = cancellation.register(&socket) {
                    receipt.stop();
                    return Err(CaptureError::new("capture_unavailable", error.to_string()));
                }
                let mut client = Client::new(socket, receipt)
                    .map_err(|error| CaptureError::new("capture_unavailable", error.to_string()))?;
                let records = client.enumerate()?;
                let (outputs, _) = convert_isolated_outputs(&records)?;
                let selected = outputs
                    .iter()
                    .find(|candidate| candidate.id == output && candidate.attached)
                    .ok_or_else(|| {
                        CaptureError::new("output_unavailable", "selected DRM output is not active")
                    })?;
                if !selected.framebuffer_export_supported {
                    return Err(CaptureError::new(
                        "unsupported_pixel_format",
                        selected
                            .readback_reason
                            .unwrap_or("DRM framebuffer is not exportable"),
                    ));
                }
                let opened = convert_native_output(&client.open(&output)?)?;
                if opened.id != output || !opened.framebuffer_export_supported {
                    return Err(CaptureError::new(
                        "output_unavailable",
                        "selected DRM output changed or cannot export its framebuffer",
                    ));
                }
                let records = client.enumerate()?;
                let (outputs, desktop) = convert_isolated_outputs(&records)?;
                let selected = outputs
                    .into_iter()
                    .find(|candidate| candidate.id == output)
                    .ok_or_else(|| {
                        CaptureError::new("output_unavailable", "selected DRM output disappeared")
                    })?;
                if !selected.framebuffer_export_supported
                    || !selected.attached
                    || (
                        selected.left,
                        selected.top,
                        selected.width,
                        selected.height,
                        selected.rotation,
                    ) != (
                        opened.left,
                        opened.top,
                        opened.width,
                        opened.height,
                        opened.rotation,
                    )
                {
                    return Err(CaptureError::new(
                        "access_lost",
                        "DRM output geometry changed while opening the capture stream",
                    ));
                }
                let output_rect =
                    DesktopRect::new(selected.left, selected.top, selected.width, selected.height)
                        .map_err(|_| {
                            CaptureError::new("invalid_geometry", "invalid DRM output bounds")
                        })?;
                let geometry = CaptureGeometry::new(output_rect, desktop).map_err(|_| {
                    CaptureError::new("invalid_geometry", "DRM output is outside desktop")
                })?;
                frame_layout(selected.width, selected.height)?;
                Ok(IsolatedSession {
                    client,
                    output: selected,
                    geometry: DesktopGeometry { capture: geometry },
                    opened_at: Instant::now(),
                    sequence: 0,
                    last_timestamp_us: 0,
                })
            })
            .map(Self)
        }
        pub fn cancel(&self) -> bool {
            self.0.cancel()
        }
        pub fn poll(&mut self) -> TaskPoll<CaptureResult<IsolatedSession>> {
            self.0.poll()
        }
    }

    pub(crate) struct IsolatedSession {
        client: Client,
        output: Output,
        geometry: DesktopGeometry,
        opened_at: Instant,
        sequence: u64,
        last_timestamp_us: u64,
    }
    impl IsolatedSession {
        pub fn output(&self) -> &Output {
            &self.output
        }
        pub fn retire(&mut self) -> CaptureResult<bool> {
            self.client.retire()
        }
    }
    impl CaptureSession for IsolatedSession {
        fn read(&mut self) -> CaptureResult<Frame> {
            let (stride, length) = frame_layout(self.output.width, self.output.height)?;
            let mut bytes = vec![0; length];
            self.client
                .read_frame(self.output.width, self.output.height, &mut bytes)?;
            self.sequence = self.sequence.checked_add(1).ok_or_else(|| {
                CaptureError::new("sequence_exhausted", "DRM capture sequence exhausted")
            })?;
            let elapsed = self.opened_at.elapsed().as_micros().min(u64::MAX as u128) as u64;
            self.last_timestamp_us =
                elapsed.max(self.last_timestamp_us.checked_add(1).ok_or_else(|| {
                    CaptureError::new("timestamp_exhausted", "DRM capture timestamp exhausted")
                })?);
            Ok(Frame {
                width: self.output.width,
                height: self.output.height,
                stride,
                timestamp_us: self.last_timestamp_us,
                sequence: self.sequence,
                bytes,
            })
        }
        fn geometry(&self) -> DesktopGeometry {
            self.geometry
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn record(id: &str, left: i32) -> NativeOutput {
            let mut output = NativeOutput {
                left,
                width: 1920,
                height: 1080,
                attached: 1,
                framebuffer_export_supported: 1,
                ..NativeOutput::default()
            };
            for (destination, byte) in output.id.iter_mut().zip(id.bytes()) {
                *destination = byte as _;
            }
            output.name[0] = b'D' as _;
            output.card[0] = b'c' as _;
            output
        }

        #[test]
        fn metadata_conversion_preserves_desktop_geometry_and_probe_failure() {
            let (outputs, desktop) = convert_isolated_outputs(&[
                record("kms:card0/conn=1/crtc=2/plane=3", -1920),
                record("kms:card0/conn=4/crtc=5/plane=6", 0),
            ])
            .unwrap();
            assert_eq!(outputs.len(), 2);
            assert_eq!((desktop.left, desktop.width), (-1920, 3840));
            let info = convert_isolated_info(NativeInfo {
                error: 3,
                ..NativeInfo::default()
            });
            assert!(!info.available);
            assert_eq!(info.reason, Some("drm_framebuffer_access_denied"));
            assert!(convert_isolated_outputs(&[record("invalid", 0)]).is_err());
        }
    }
}

#[cfg(all(test, feature = "isolated-helper", target_os = "linux", amaterasu_kms))]
pub(crate) use isolated::{
    IsolatedOpenTask, IsolatedSession, convert_isolated_info, convert_isolated_outputs,
};

#[cfg(test)]
mod tests {
    use super::{bounding_rectangle, error_reason};
    use amaterasu_media::DesktopRect;

    #[test]
    fn output_bounds_cover_negative_and_disjoint_crtc_positions() {
        let bounds = bounding_rectangle(&[
            DesktopRect::new(-1280, 0, 1280, 1024).unwrap(),
            DesktopRect::new(0, 80, 1920, 1080).unwrap(),
        ])
        .unwrap();
        assert_eq!(
            (bounds.left, bounds.top, bounds.width, bounds.height),
            (-1280, 0, 3200, 1160)
        );
    }

    #[test]
    fn empty_output_list_is_unavailable() {
        assert_eq!(
            bounding_rectangle(&[]).unwrap_err().code,
            "output_unavailable"
        );
    }

    #[test]
    fn native_error_reasons_are_stable() {
        assert_eq!(error_reason(3), "drm_framebuffer_access_denied");
        assert_eq!(error_reason(9), "gbm_egl_import_unavailable");
    }
}
