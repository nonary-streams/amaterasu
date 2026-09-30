//! Explicit portal/PipeWire qualification. Pixel bytes are inspected synchronously
//! and reduced to dimensions plus a nonblack-pixel count before the callback ends.
use amaterasu_media::{
    CAPTURE_INTERFACE_MINOR, CaptureApi, CaptureGeometry, CpuFrame, FrameSink, MEDIA_BUSY,
    MEDIA_CLOSED, MEDIA_PENDING, MEDIA_UNSUPPORTED, PIXEL_BGRA8,
};
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient, abi};
use serde_json::{Value, json};
use std::{
    ffi::c_void,
    mem::{MaybeUninit, size_of},
    time::{Duration, Instant},
};

const CAPTURE: &str = "amaterasu.capture-pipewire/1";
// The development host caps one live console request at 30 seconds. Keep the
// opt-in user picker and first-frame waits bounded so close/status cleanup can
// finish before that outer request expires. The provider's own user-configured
// selection timeout remains independently configurable from 1 to 300 seconds.
const SELECTION_WAIT: Duration = Duration::from_secs(15);
const FRAME_WAIT: Duration = Duration::from_secs(8);

#[derive(Clone, Copy)]
enum ControlStage {
    Status,
    Enumerate,
}

pub struct ControlStatusProbe {
    request: u64,
    pending: u64,
    stage: ControlStage,
    status_passed: bool,
    native_supported: bool,
    available: bool,
    availability_reason: Option<String>,
}

impl ControlStatusProbe {
    pub fn start(runtime: &RuntimeClient, request: u64) -> ModuleResult<Self> {
        verify_native_interface(runtime)?;
        let pending = request_service(
            runtime,
            request,
            json!({"action":"status"}),
            "native-pipewire-control-status",
        )?;
        Ok(Self {
            request,
            pending,
            stage: ControlStage::Status,
            status_passed: false,
            native_supported: false,
            available: false,
            availability_reason: None,
        })
    }

    pub fn tick(&mut self, runtime: &RuntimeClient) -> ModuleResult<bool> {
        let payload = match runtime.send_ephemeral(Command::TakeResult {
            request: self.pending,
        })? {
            Response::Pending => return Ok(false),
            Response::Result { payload } if payload["ok"] == true => payload,
            Response::Result { payload } => {
                let code = payload["error"]["code"]
                    .as_str()
                    .or_else(|| payload["code"].as_str())
                    .unwrap_or("capture_control_failed");
                return self.finish(runtime, false, Some(code));
            }
            Response::Error { code, .. } => {
                return self.finish(runtime, false, Some(&code));
            }
            _ => {
                return self.finish(runtime, false, Some("capture_control_response_invalid"));
            }
        };

        match self.stage {
            ControlStage::Status => {
                if payload["enabled"] != true || payload["selection_state"] != "permission_required"
                {
                    return self.finish(runtime, false, Some("capture_status_invalid"));
                }
                let Some(native_supported) = payload["native_supported"].as_bool() else {
                    return self.finish(runtime, false, Some("capture_status_invalid"));
                };
                let Some(available) = payload["availability"]["available"].as_bool() else {
                    return self.finish(runtime, false, Some("capture_status_invalid"));
                };
                self.native_supported = native_supported;
                self.available = available;
                self.status_passed = true;
                self.availability_reason = payload["availability"]["reason"]
                    .as_str()
                    .filter(|reason| safe_availability_reason(reason))
                    .map(str::to_owned);
                self.pending = request_service(
                    runtime,
                    self.request,
                    json!({"action":"enumerate"}),
                    "native-pipewire-control-enumerate",
                )?;
                self.stage = ControlStage::Enumerate;
                Ok(false)
            }
            ControlStage::Enumerate => {
                let Some(outputs) = payload["outputs"].as_array() else {
                    return self.finish(runtime, false, Some("capture_enumeration_invalid"));
                };
                if !outputs.is_empty()
                    || payload["geometry_supported"] != false
                    || !payload["enumeration_id"].is_null()
                {
                    return self.finish(runtime, false, Some("capture_enumeration_not_empty"));
                }
                self.finish(runtime, true, None)
            }
        }
    }

    fn finish(
        &self,
        runtime: &RuntimeClient,
        passed: bool,
        code: Option<&str>,
    ) -> ModuleResult<bool> {
        let availability = if self.available {
            "available"
        } else {
            "unavailable"
        };
        let payload = json!({
            "ok": passed,
            "control_proof": if passed { "passed" } else { "failed" },
            "native_api_bound": true,
            "status_passed": self.status_passed,
            "empty_enumeration": passed && matches!(self.stage, ControlStage::Enumerate),
            "native_supported": self.native_supported,
            "availability": availability,
            "availability_reason": self.availability_reason,
            "source_selection": "not_requested",
            "code": code,
        });
        let result = runtime.send_ephemeral(Command::Complete {
            request: self.request,
            payload,
        })?;
        Ok(matches!(result, Response::Ok | Response::Error { .. }))
    }
}

fn safe_availability_reason(reason: &str) -> bool {
    matches!(
        reason,
        "session_bus_unavailable"
            | "portal_unavailable"
            | "native_dependencies_unavailable"
            | "unsupported_platform"
    )
}

fn verify_native_interface(runtime: &RuntimeClient) -> ModuleResult<()> {
    let binding = unsafe {
        runtime.bind_native(
            CAPTURE,
            1,
            CAPTURE_INTERFACE_MINOR,
            size_of::<CaptureApi>() as u32,
        )
    }?;
    // SAFETY: the signed fixture profile promises the exact capture-v1.1 table.
    let api = unsafe { binding.table::<CaptureApi>() }?;
    let mut geometry = MaybeUninit::<CaptureGeometry>::uninit();
    // SAFETY: output storage is aligned/live; the nonexistent stream is read-only.
    let status = unsafe { (api.geometry)(api.base.context, u64::MAX, geometry.as_mut_ptr()) };
    if status != MEDIA_CLOSED {
        return Err("PipeWire native ABI probe returned an unexpected status".into());
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Stage {
    Select,
    Poll,
    Open,
    Capture,
    Cancel,
    Close,
    Status,
}

#[derive(Default)]
struct FrameObservation {
    frames: u32,
    width: u32,
    height: u32,
    stride: u32,
    format: u32,
    nonblack_pixels: u64,
}

pub struct CaptureProbe {
    request: u64,
    pending: u64,
    stage: Stage,
    operation: String,
    source_type: String,
    width: u32,
    height: u32,
    stream: Option<u64>,
    binding: Option<amaterasu_module_sdk::NativeBinding>,
    observation: FrameObservation,
    selection_deadline: Instant,
    frame_deadline: Option<Instant>,
    next_poll: Instant,
    selection_active: bool,
    success: bool,
    classification: String,
    error: Option<String>,
    final_status: Option<Value>,
}

impl CaptureProbe {
    pub fn start(runtime: &RuntimeClient, request: u64, source_type: &str) -> ModuleResult<Self> {
        if !matches!(source_type, "monitor" | "window") {
            return Err("invalid PipeWire test source type".into());
        }
        let operation = format!("native-pipewire-selection-{request}");
        let pending = request_service(
            runtime,
            request,
            json!({"action":"select_source","source_type":source_type,
                "operation_id":operation}),
            "native-pipewire-select",
        )?;
        Ok(Self {
            request,
            pending,
            stage: Stage::Select,
            operation,
            source_type: source_type.into(),
            width: 0,
            height: 0,
            stream: None,
            binding: None,
            observation: FrameObservation::default(),
            selection_deadline: Instant::now() + SELECTION_WAIT,
            frame_deadline: None,
            next_poll: Instant::now(),
            selection_active: false,
            success: false,
            classification: "failed".into(),
            error: None,
            final_status: None,
        })
    }

    pub fn tick(&mut self, runtime: &RuntimeClient) -> ModuleResult<bool> {
        if matches!(self.stage, Stage::Capture) {
            return self.capture_frame(runtime);
        }
        if matches!(self.stage, Stage::Poll) && Instant::now() < self.next_poll {
            return Ok(false);
        }
        if matches!(self.stage, Stage::Poll) && Instant::now() >= self.selection_deadline {
            return self.fail(runtime, "selection_timeout", "skipped");
        }
        let payload = match runtime.send_ephemeral(Command::TakeResult {
            request: self.pending,
        })? {
            Response::Pending => return Ok(false),
            Response::Result { payload } if payload["ok"] == true => payload,
            Response::Result { payload } => {
                let code = payload["error"]["code"]
                    .as_str()
                    .or_else(|| payload["code"].as_str())
                    .unwrap_or("capture_request_failed");
                if matches!(self.stage, Stage::Close | Stage::Cancel) {
                    self.success = false;
                    self.classification = "failed".into();
                    self.error = Some(code.into());
                    return self.request_status(runtime);
                }
                let classification = classify_selection_error(code);
                return self.fail(runtime, code, classification);
            }
            Response::Error { code, .. } => {
                if matches!(self.stage, Stage::Close | Stage::Cancel) {
                    self.success = false;
                    self.classification = "failed".into();
                    self.error = Some(code);
                    return self.request_status(runtime);
                }
                let classification = classify_selection_error(&code);
                return self.fail(runtime, &code, classification);
            }
            response => {
                return self.fail(
                    runtime,
                    &format!("capture_control_failed:{response:?}"),
                    "failed",
                );
            }
        };
        match self.stage {
            Stage::Select => {
                self.selection_active = true;
                self.poll_selection(runtime)
            }
            Stage::Poll => self.handle_selection(runtime, payload),
            Stage::Open => self.opened(runtime, payload),
            Stage::Cancel => self.complete(runtime),
            Stage::Close => self.request_status(runtime),
            Stage::Status => self.finish(runtime, payload),
            Stage::Capture => unreachable!("capture is handled before control polling"),
        }
    }

    fn poll_selection(&mut self, runtime: &RuntimeClient) -> ModuleResult<bool> {
        self.pending = request_service(
            runtime,
            self.request,
            json!({"action":"poll_selection","operation_id":self.operation}),
            "native-pipewire-poll",
        )?;
        self.stage = Stage::Poll;
        self.next_poll = Instant::now() + Duration::from_millis(250);
        Ok(false)
    }

    fn handle_selection(&mut self, runtime: &RuntimeClient, payload: Value) -> ModuleResult<bool> {
        match payload["selection_state"].as_str() {
            Some("selecting") => self.poll_selection(runtime),
            Some("denied") => self.fail(runtime, "source_selection_denied", "denied"),
            Some("failed") => {
                let code = payload["error"].as_str().unwrap_or("capture_failed");
                self.fail(runtime, code, classify_selection_error(code))
            }
            Some("ready") => {
                let Some(output) = payload["output"]["id"].as_str() else {
                    return self.fail(runtime, "selected_output_missing", "failed");
                };
                let Some(width) = payload["output"]["width"]
                    .as_u64()
                    .and_then(|value| u32::try_from(value).ok())
                else {
                    return self.fail(runtime, "selected_width_missing", "failed");
                };
                let Some(height) = payload["output"]["height"]
                    .as_u64()
                    .and_then(|value| u32::try_from(value).ok())
                else {
                    return self.fail(runtime, "selected_height_missing", "failed");
                };
                self.width = width;
                self.height = height;
                self.pending = request_service(
                    runtime,
                    self.request,
                    json!({"action":"open","output":output,
                        "operation_id":format!("native-pipewire-open-{}", self.request)}),
                    "native-pipewire-open",
                )?;
                self.stage = Stage::Open;
                self.frame_deadline = Some(Instant::now() + FRAME_WAIT);
                Ok(false)
            }
            _ => self.fail(runtime, "unknown_selection_state", "failed"),
        }
    }

    fn opened(&mut self, runtime: &RuntimeClient, payload: Value) -> ModuleResult<bool> {
        let Some(stream) = payload["stream"].as_u64() else {
            return self.fail(runtime, "capture_stream_missing", "failed");
        };
        self.stream = Some(stream);
        let binding = unsafe {
            runtime.bind_native(
                CAPTURE,
                1,
                CAPTURE_INTERFACE_MINOR,
                size_of::<CaptureApi>() as u32,
            )
        }?;
        // SAFETY: the signed test profile grants this exact capture-v1.1 table.
        let api = unsafe { binding.table::<CaptureApi>() }?;
        let mut geometry = std::mem::MaybeUninit::<CaptureGeometry>::uninit();
        // SAFETY: output storage is aligned and retained through the synchronous call.
        let geometry_status =
            unsafe { (api.geometry)(api.base.context, stream, geometry.as_mut_ptr()) };
        if geometry_status != MEDIA_UNSUPPORTED {
            self.binding = Some(binding);
            return self.fail(runtime, "portal_geometry_must_be_unsupported", "failed");
        }
        self.binding = Some(binding);
        self.stage = Stage::Capture;
        Ok(false)
    }

    fn capture_frame(&mut self, runtime: &RuntimeClient) -> ModuleResult<bool> {
        if self
            .frame_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return self.fail(runtime, "first_frame_timeout", "failed");
        }
        let binding = self.binding.as_ref().expect("capture binding exists");
        let api = unsafe { binding.table::<CaptureApi>() }?;
        let sink = FrameSink {
            context: (&mut self.observation as *mut FrameObservation).cast(),
            write: inspect_frame,
        };
        // SAFETY: the binding and test-owned observation live through this synchronous call.
        let status = unsafe {
            (api.read_frame)(api.base.context, self.stream.expect("opened stream"), sink)
        };
        if self.observation.frames > 0 {
            if status != abi::STATUS_OK {
                return self.fail(runtime, "captured_frame_not_accepted", "failed");
            }
            if self.observation.width != self.width
                || self.observation.height != self.height
                || self.observation.stride != self.width.saturating_mul(4)
                || self.observation.format != PIXEL_BGRA8
            {
                return self.fail(runtime, "captured_frame_metadata_mismatch", "failed");
            }
            if self.observation.nonblack_pixels == 0 {
                return self.fail(runtime, "captured_frame_is_black", "failed");
            }
            self.success = true;
            self.classification = "passed".into();
            return self.close(runtime, None);
        }
        if matches!(status, MEDIA_PENDING | MEDIA_BUSY) {
            return Ok(false);
        }
        self.fail(runtime, &format!("capture_read_status_{status}"), "failed")
    }

    fn fail(
        &mut self,
        runtime: &RuntimeClient,
        code: &str,
        classification: &str,
    ) -> ModuleResult<bool> {
        self.success = false;
        self.classification = classification.into();
        self.error = Some(code.into());
        if self.stream.is_some() {
            self.close(runtime, Some(code))
        } else if self.selection_active {
            self.cancel_selection(runtime)
        } else {
            self.complete(runtime)
        }
    }

    fn cancel_selection(&mut self, runtime: &RuntimeClient) -> ModuleResult<bool> {
        self.pending = request_service(
            runtime,
            self.request,
            json!({"action":"cancel_selection","operation_id":self.operation}),
            "native-pipewire-cancel",
        )?;
        self.stage = Stage::Cancel;
        Ok(false)
    }

    fn close(&mut self, runtime: &RuntimeClient, error: Option<&str>) -> ModuleResult<bool> {
        self.binding = None;
        if let Some(error) = error {
            self.error = Some(error.into());
        }
        let pending = request_service(
            runtime,
            self.request,
            json!({"action":"close","stream":self.stream.expect("opened stream")}),
            "native-pipewire-close",
        )?;
        self.pending = pending;
        self.stage = Stage::Close;
        Ok(false)
    }

    fn request_status(&mut self, runtime: &RuntimeClient) -> ModuleResult<bool> {
        self.pending = request_service(
            runtime,
            self.request,
            json!({"action":"status"}),
            "native-pipewire-status",
        )?;
        self.stage = Stage::Status;
        Ok(false)
    }

    fn finish(&mut self, runtime: &RuntimeClient, status: Value) -> ModuleResult<bool> {
        self.final_status = Some(status.clone());
        if status["active"].as_u64() != Some(0) {
            self.success = false;
            self.classification = "failed".into();
            self.error
                .get_or_insert_with(|| "capture_still_active".into());
        }
        if self.success
            && (status["frames_copied"].as_u64().unwrap_or(0) == 0
                || status["frames_accepted"].as_u64().unwrap_or(0) == 0)
        {
            self.success = false;
            self.classification = "failed".into();
            self.error = Some("capture_counters_incomplete".into());
        }
        self.complete(runtime)
    }

    fn complete(&self, runtime: &RuntimeClient) -> ModuleResult<bool> {
        let status = self.final_status.as_ref();
        let result = runtime.send_ephemeral(Command::Complete {
            request: self.request,
            payload: json!({
                "ok": self.success,
                "qualification": self.classification,
                "code": self.error,
                "source_type": self.source_type,
                "width": self.observation.width,
                "height": self.observation.height,
                "format": if self.observation.frames == 0 { Value::Null } else { json!("bgra8") },
                "nonblack_pixels": self.observation.nonblack_pixels,
                "frames_copied": status.and_then(|value| value["frames_copied"].as_u64()),
                "frames_accepted": status.and_then(|value| value["frames_accepted"].as_u64()),
                "active": status.and_then(|value| value["active"].as_u64()),
                "geometry_supported": false,
            }),
        })?;
        Ok(matches!(result, Response::Ok | Response::Error { .. }))
    }
}

fn classify_selection_error(code: &str) -> &'static str {
    match code {
        "session_bus_unavailable"
        | "portal_unavailable"
        | "native_dependencies_unavailable"
        | "native_worker_unavailable"
        | "source_selection_timeout" => "unavailable",
        "source_selection_denied" => "denied",
        "capture_disabled" | "selection_permission_denied" | "capture_permission_denied" => {
            "failed"
        }
        _ => "failed",
    }
}

fn request_service(
    runtime: &RuntimeClient,
    parent: u64,
    payload: Value,
    operation: &str,
) -> ModuleResult<u64> {
    match runtime.send_ephemeral(Command::Request {
        service: CAPTURE.into(),
        payload,
        timeout_ms: 5000,
        operation_id: format!("{operation}-{parent}"),
        parent: Some(parent),
    })? {
        Response::Accepted { id } => Ok(id),
        response => Err(format!("capture_control_request_rejected:{response:?}")),
    }
}

unsafe extern "C" fn inspect_frame(context: *mut c_void, frame: *const CpuFrame) -> abi::Status {
    if context.is_null() || frame.is_null() {
        return abi::STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: capture provider borrows the frame and test-owned observation only for this call.
    let observation = unsafe { &mut *context.cast::<FrameObservation>() };
    let frame = unsafe { &*frame };
    if frame.validate().is_err() || frame.width == 0 || frame.height == 0 {
        return abi::STATUS_INVALID_ARGUMENT;
    }
    let Ok(bytes) = (unsafe { frame.bytes() }) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    let row_bytes = match frame.width.checked_mul(4) {
        Some(row_bytes) if row_bytes <= frame.stride => row_bytes as usize,
        _ => return abi::STATUS_INVALID_ARGUMENT,
    };
    let mut nonblack_pixels = 0u64;
    for row in 0..frame.height as usize {
        let Some(start) = row.checked_mul(frame.stride as usize) else {
            return abi::STATUS_INVALID_ARGUMENT;
        };
        let Some(end) = start.checked_add(row_bytes) else {
            return abi::STATUS_INVALID_ARGUMENT;
        };
        let Some(pixels) = bytes.get(start..end) else {
            return abi::STATUS_INVALID_ARGUMENT;
        };
        nonblack_pixels += pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| pixel[..3].iter().any(|channel| *channel != 0))
            .count() as u64;
    }
    observation.frames = observation.frames.saturating_add(1);
    observation.width = frame.width;
    observation.height = frame.height;
    observation.stride = frame.stride;
    observation.format = frame.format;
    observation.nonblack_pixels = nonblack_pixels;
    abi::STATUS_OK
}
