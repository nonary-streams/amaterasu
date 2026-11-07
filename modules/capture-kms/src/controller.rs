use crate::{
    backend::{Backend, CaptureSession, NativeBackend},
    model::{
        CaptureError, CaptureResult, Frame, MAX_OUTPUTS, MAX_STREAMS, Operation, Output,
        validate_operation_id,
    },
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Instant};

struct Stream {
    owner: String,
    output: Output,
    operation_id: Option<String>,
    session: Box<dyn CaptureSession>,
    opened_at: Instant,
    sequence: u64,
    last_timestamp_us: u64,
    frame_readback_verified: bool,
}

pub struct Controller<B: Backend = NativeBackend> {
    backend: B,
    enabled: bool,
    streams: BTreeMap<u64, Stream>,
    opened_stream: Option<u64>,
}

impl<B: Backend> Controller<B> {
    pub fn new(backend: B, enabled: bool) -> Self {
        Self {
            backend,
            enabled,
            streams: BTreeMap::new(),
            opened_stream: None,
        }
    }

    pub fn handle(
        &mut self,
        caller: &str,
        operation: Operation,
        guard: &mut impl FnMut() -> CaptureResult<()>,
    ) -> Value {
        self.opened_stream = None;
        let result = match operation {
            Operation::Status => Ok(self.status(caller)),
            Operation::Enumerate { offset, limit } => self.enumerate(offset, limit),
            Operation::Open {
                output,
                operation_id,
            } => self.open(caller, output, operation_id, guard),
            Operation::Lookup { operation_id } => self.lookup(caller, &operation_id),
            Operation::Close { stream } => self.close(caller, stream).map(|()| json!({"ok":true})),
        };
        result.unwrap_or_else(CaptureError::payload)
    }

    pub fn take_opened_stream(&mut self) -> Option<u64> {
        self.opened_stream.take()
    }

    #[cfg(test)]
    fn with_backend<C: Backend>(self, backend: C) -> Controller<C> {
        Controller {
            backend,
            enabled: self.enabled,
            streams: self.streams,
            opened_stream: self.opened_stream,
        }
    }

    pub fn shutdown(&mut self) {
        self.enabled = false;
        self.streams.clear();
    }

    pub fn close(&mut self, caller: &str, id: u64) -> CaptureResult<()> {
        self.authorize(caller, id)?;
        self.streams.remove(&id);
        Ok(())
    }

    pub fn read(&mut self, caller: &str, id: u64) -> CaptureResult<Frame> {
        self.authorize(caller, id)?;
        let stream = self.streams.get_mut(&id).expect("authorized DRM stream");
        let mut frame = stream.session.read()?;
        stream.sequence = stream.sequence.checked_add(1).ok_or_else(|| {
            CaptureError::new("sequence_exhausted", "DRM capture sequence exhausted")
        })?;
        let elapsed_us = stream.opened_at.elapsed().as_micros().min(u64::MAX as u128) as u64;
        stream.last_timestamp_us =
            elapsed_us.max(stream.last_timestamp_us.checked_add(1).ok_or_else(|| {
                CaptureError::new("timestamp_exhausted", "DRM capture timestamp exhausted")
            })?);
        frame.sequence = stream.sequence;
        frame.timestamp_us = stream.last_timestamp_us;
        stream.frame_readback_verified = true;
        Ok(frame)
    }

    pub fn geometry(
        &self,
        caller: &str,
        id: u64,
    ) -> CaptureResult<amaterasu_media::CaptureGeometry> {
        self.authorize(caller, id)?;
        Ok(self
            .streams
            .get(&id)
            .expect("authorized DRM stream")
            .session
            .geometry()
            .capture)
    }

    fn enumerate(&self, offset: Option<usize>, limit: Option<usize>) -> CaptureResult<Value> {
        let (outputs, _) = self.backend.enumerate()?;
        let offset = offset.unwrap_or(0);
        let limit = limit.unwrap_or(MAX_OUTPUTS).min(MAX_OUTPUTS);
        if offset > outputs.len() {
            return Err(CaptureError::new(
                "invalid_request",
                "output offset is out of range",
            ));
        }
        let end = offset.saturating_add(limit).min(outputs.len());
        Ok(json!({
            "ok":true,
            "outputs":outputs[offset..end],
            "total":outputs.len(),
            "next_offset":(end < outputs.len()).then_some(end),
            "presentation_events":false,
            "managed_virtual_output":false,
            "display_composition":"active_primary_plane_only",
            "cursor_and_overlay_planes_captured":false,
            "crtc_color_pipeline_captured":false,
        }))
    }

    fn open(
        &mut self,
        caller: &str,
        output: String,
        operation_id: Option<String>,
        guard: &mut impl FnMut() -> CaptureResult<()>,
    ) -> CaptureResult<Value> {
        if !self.enabled {
            return Err(CaptureError::new(
                "capture_disabled",
                "DRM capture requires enabled startup policy",
            ));
        }
        if let Some(operation_id) = operation_id.as_ref() {
            validate_operation_id(operation_id)?;
            if let Some((stream, existing)) = self.find_operation(caller, operation_id) {
                if existing.output.id != output {
                    return Err(CaptureError::new(
                        "operation_conflict",
                        "operation ID already selected a different DRM output",
                    ));
                }
                return Ok(open_reply(stream, existing));
            }
        }
        if self.streams.len() >= MAX_STREAMS {
            return Err(CaptureError::new(
                "resource_exhausted",
                "at most four DRM streams may be open",
            ));
        }
        let (outputs, _) = self.backend.enumerate()?;
        let selected = outputs
            .into_iter()
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
        let id = self.next_stream_id()?;
        guard()?;
        let session = self.backend.open(&output)?;
        let geometry = session.geometry().capture;
        guard()?;
        self.streams.insert(
            id,
            Stream {
                owner: caller.into(),
                output: selected,
                operation_id,
                session,
                opened_at: Instant::now(),
                sequence: 0,
                last_timestamp_us: 0,
                frame_readback_verified: false,
            },
        );
        self.opened_stream = Some(id);
        let stream = self.streams.get(&id).expect("new DRM stream inserted");
        Ok(open_reply_with_geometry(id, stream, geometry))
    }

    fn status(&self, caller: &str) -> Value {
        let streams: Vec<_> = self
            .streams
            .iter()
            .filter(|(_, stream)| stream.owner == caller)
            .map(|(id, stream)| stream_reply(*id, stream))
            .collect();
        let availability = self.backend.availability();
        json!({
            "ok":true,
            "enabled":self.enabled,
            "native_supported":availability.native_supported,
            "available":availability.available,
            "framebuffer_export_supported":availability.framebuffer_export_supported,
            "presentation_events":availability.presentation_events,
            "managed_virtual_output":availability.managed_virtual_output,
            "display_composition":"active_primary_plane_only",
            "cursor_and_overlay_planes_captured":false,
            "crtc_color_pipeline_captured":false,
            "timestamp_source":"monotonic_acquisition",
            "pixel_path":"gbm_egl_gles_cpu_bgra",
            "reason":availability.reason,
            "streams":streams,
        })
    }

    fn next_stream_id(&self) -> CaptureResult<u64> {
        for _ in 0..4 {
            let mut random = [0; 8];
            getrandom::fill(&mut random)
                .map_err(|error| CaptureError::new("entropy_error", error.to_string()))?;
            let id = u64::from_le_bytes(random);
            if id != 0 && !self.streams.contains_key(&id) {
                return Ok(id);
            }
        }
        Err(CaptureError::new(
            "entropy_error",
            "DRM stream ID collision",
        ))
    }

    fn lookup(&self, caller: &str, operation: &str) -> CaptureResult<Value> {
        validate_operation_id(operation)?;
        self.find_operation(caller, operation)
            .map(|(id, stream)| open_reply(id, stream))
            .ok_or_else(|| CaptureError::new("not_found", "DRM stream was not found"))
    }

    fn find_operation(&self, caller: &str, operation: &str) -> Option<(u64, &Stream)> {
        self.streams
            .iter()
            .find(|(_, stream)| {
                stream.owner == caller && stream.operation_id.as_deref() == Some(operation)
            })
            .map(|(id, stream)| (*id, stream))
    }

    fn authorize(&self, caller: &str, id: u64) -> CaptureResult<()> {
        if self
            .streams
            .get(&id)
            .is_some_and(|stream| stream.owner == caller)
        {
            Ok(())
        } else {
            Err(CaptureError::new(
                "not_found",
                "DRM stream was not found for this caller",
            ))
        }
    }
}

fn stream_reply(id: u64, stream: &Stream) -> Value {
    json!({
        "stream":id,
        "output":stream.output.id,
        "frame_readback_verified":stream.frame_readback_verified,
    })
}

fn open_reply(id: u64, stream: &Stream) -> Value {
    let mut reply = stream_reply(id, stream);
    reply["ok"] = json!(true);
    reply
}

fn open_reply_with_geometry(
    id: u64,
    stream: &Stream,
    geometry: amaterasu_media::CaptureGeometry,
) -> Value {
    let mut reply = open_reply(id, stream);
    reply["desktop_geometry"] = json!(geometry);
    reply
}

#[cfg(test)]
mod tests {
    use super::Controller;
    use crate::{
        backend::{Backend, CaptureSession},
        model::{
            CaptureError, CaptureResult, DesktopGeometry, NativeAvailability, Operation, Output,
        },
    };
    use amaterasu_media::{CaptureGeometry, DesktopRect};
    use serde_json::Value;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct MetadataBackend {
        opened: Arc<AtomicUsize>,
        dropped: Arc<AtomicUsize>,
    }

    impl Backend for MetadataBackend {
        fn availability(&self) -> NativeAvailability {
            NativeAvailability {
                native_supported: true,
                available: true,
                framebuffer_export_supported: true,
                presentation_events: false,
                managed_virtual_output: false,
                reason: None,
            }
        }

        fn enumerate(&self) -> CaptureResult<(Vec<Output>, DesktopRect)> {
            Ok((vec![output()], DesktopRect::new(0, 0, 2, 2).unwrap()))
        }

        fn open(&self, output_id: &str) -> CaptureResult<Box<dyn CaptureSession>> {
            if output_id != "kms:card0/conn=1/crtc=2/plane=3" {
                return Err(CaptureError::new(
                    "output_unavailable",
                    "missing test output",
                ));
            }
            self.opened.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(NoReadbackSession {
                dropped: self.dropped.clone(),
            }))
        }
    }

    struct NoReadbackSession {
        dropped: Arc<AtomicUsize>,
    }

    impl CaptureSession for NoReadbackSession {
        fn read(&mut self) -> CaptureResult<crate::model::Frame> {
            Err(CaptureError::new(
                "capture_unavailable",
                "no DRM device is connected in this unit-test backend",
            ))
        }

        fn geometry(&self) -> DesktopGeometry {
            DesktopGeometry {
                capture: CaptureGeometry::new(
                    DesktopRect::new(0, 0, 2, 2).unwrap(),
                    DesktopRect::new(0, 0, 2, 2).unwrap(),
                )
                .unwrap(),
            }
        }
    }

    impl Drop for NoReadbackSession {
        fn drop(&mut self) {
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn output() -> Output {
        Output {
            id: "kms:card0/conn=1/crtc=2/plane=3".into(),
            adapter: "/dev/dri/card0".into(),
            adapter_vendor_id: None,
            adapter_device_id: None,
            adapter_description: Some("DRM/KMS primary plane".into()),
            device_name: "DP-1 (/dev/dri/card0)".into(),
            attached: true,
            framebuffer_export_supported: true,
            readback_reason: None,
            left: 0,
            top: 0,
            width: 2,
            height: 2,
            rotation: 1,
        }
    }

    fn controller(
        enabled: bool,
    ) -> (
        Controller<MetadataBackend>,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
    ) {
        let opened = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        (
            Controller::new(
                MetadataBackend {
                    opened: opened.clone(),
                    dropped: dropped.clone(),
                },
                enabled,
            ),
            opened,
            dropped,
        )
    }

    fn request(controller: &mut Controller<MetadataBackend>, operation: Operation) -> Value {
        controller.handle("console.one", operation, &mut || Ok(()))
    }

    #[test]
    fn module_quiescence_releases_capture_before_reporting_ready() {
        use amaterasu_module_sdk::{ManagedModule, RuntimeClient, abi::*};
        use std::{ffi::c_void, sync::Mutex};

        unsafe extern "C" fn unused_host(
            _context: *mut c_void,
            _request: BorrowedBytes,
            _reply: ReplySink,
        ) -> Status {
            STATUS_INTERNAL_ERROR
        }

        let (mut controller, _, dropped) = controller(true);
        let reply = request(
            &mut controller,
            Operation::Open {
                output: output().id,
                operation_id: Some("shutdown".into()),
            },
        );
        let stream = reply["stream"].as_u64().unwrap();
        let controller = Arc::new(Mutex::new(
            controller.with_backend(crate::backend::NativeBackend::new()),
        ));
        let factory = crate::native::Factory::new(controller.clone());
        // SAFETY: the stateless endpoint stays live through this fixture.
        // Shutdown must not dispatch requests or touch a real DRM device.
        let runtime = unsafe {
            RuntimeClient::from_endpoint(HostApi {
                header: AbiHeader::of::<HostApi>(),
                context: std::ptr::null_mut(),
                invoke: unused_host,
            })
        };
        let mut module = crate::CaptureModule {
            runtime,
            controller,
            _factory: factory,
            shutting_down: false,
        };

        module.prepare_shutdown(20_000).unwrap();
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        assert!(
            module
                .controller
                .lock()
                .unwrap()
                .geometry("console.one", stream)
                .is_ok()
        );
        let shared = module.controller.clone();
        let held = shared.lock().unwrap();
        assert_eq!(module.quiesce(), Ok(false));
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        drop(held);
        assert_eq!(module.quiesce(), Ok(true));
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert_eq!(module.quiesce(), Ok(true));
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn stream_leases_are_caller_scoped_and_release_native_session() {
        let (mut controller, opened, dropped) = controller(true);
        let reply = request(
            &mut controller,
            Operation::Open {
                output: output().id,
                operation_id: Some("launch.one".into()),
            },
        );
        assert_eq!(reply["ok"], true);
        let stream = reply["stream"].as_u64().unwrap();
        assert_eq!(opened.load(Ordering::SeqCst), 1);
        assert_eq!(
            controller.read("console.two", stream).unwrap_err().code,
            "not_found"
        );
        assert_eq!(
            controller.read("console.one", stream).unwrap_err().code,
            "capture_unavailable"
        );
        controller.close("console.one", stream).unwrap();
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn same_operation_is_idempotent_without_reopening_the_device() {
        let (mut controller, opened, _) = controller(true);
        let first = request(
            &mut controller,
            Operation::Open {
                output: output().id,
                operation_id: Some("launch.one".into()),
            },
        );
        let second = request(
            &mut controller,
            Operation::Open {
                output: output().id,
                operation_id: Some("launch.one".into()),
            },
        );
        assert_eq!(first["stream"], second["stream"]);
        assert_eq!(opened.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn disabled_capture_does_not_open_a_device() {
        let (mut controller, opened, _) = controller(false);
        let reply = request(
            &mut controller,
            Operation::Open {
                output: output().id,
                operation_id: None,
            },
        );
        assert_eq!(reply["error"]["code"], "capture_disabled");
        assert_eq!(opened.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn denied_frame_access_never_returns_pixel_success() {
        let (mut controller, _, _) = controller(true);
        let reply = request(
            &mut controller,
            Operation::Open {
                output: output().id,
                operation_id: None,
            },
        );
        let stream = reply["stream"].as_u64().unwrap();
        assert_eq!(
            controller.read("console.one", stream).unwrap_err().code,
            "capture_unavailable"
        );
        let status = request(&mut controller, Operation::Status);
        assert_eq!(status["streams"][0]["frame_readback_verified"], false);
        assert_eq!(status["display_composition"], "active_primary_plane_only");
        assert_eq!(status["cursor_and_overlay_planes_captured"], false);
        assert_eq!(status["crtc_color_pipeline_captured"], false);
    }
}
