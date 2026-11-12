//! Read-only admission on the selected DXGI provider, before capture effects.
use crate::{backend::Backend, controller::Controller};
use amaterasu_media::{MAX_FRAME_BYTES, MEDIA_BACKEND_ERROR, MEDIA_BUSY};
use amaterasu_module_sdk::abi::{STATUS_INVALID_ARGUMENT, Status};
#[cfg(any(windows, test))]
use amaterasu_module_sdk::capture::SourceModeCapabilities;
use amaterasu_module_sdk::capture::{
    SourceClass, SourcePreflightProvider, SourcePreflightRequest, SourcePreflightResponse,
};
use std::sync::{Arc, Mutex};

#[cfg(windows)]
mod windows;
#[cfg(any(windows, target_os = "linux"))]
mod worker;

pub struct Provider<B: Backend> {
    controller: Arc<Mutex<Controller<B>>>,
    #[cfg(any(windows, target_os = "linux"))]
    worker: worker::Worker,
}
impl<B: Backend + 'static> Provider<B> {
    pub fn new(controller: Arc<Mutex<Controller<B>>>, backend: B) -> Result<Self, String> {
        #[cfg(any(windows, target_os = "linux"))]
        let worker = worker::Worker::start(Box::new(move |caller, request| {
            query(&backend, true, true, caller, request)
        }))?;
        #[cfg(not(any(windows, target_os = "linux")))]
        let _ = backend;
        Ok(Self {
            controller,
            #[cfg(any(windows, target_os = "linux"))]
            worker,
        })
    }

    pub fn stop_admission(&self) -> Result<(), Status> {
        #[cfg(any(windows, target_os = "linux"))]
        {
            self.worker.stop()
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            Ok(())
        }
    }

    pub fn quiesce(&self) -> Result<bool, Status> {
        #[cfg(any(windows, target_os = "linux"))]
        {
            self.worker.quiesce()
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            Ok(true)
        }
    }
}
impl<B: Backend + 'static> SourcePreflightProvider for Provider<B> {
    fn query(
        &self,
        caller: &str,
        request: &SourcePreflightRequest,
    ) -> Result<SourcePreflightResponse, Status> {
        request.validate().map_err(|_| STATUS_INVALID_ARGUMENT)?;
        if caller.is_empty() || caller.len() > 128 || caller.chars().any(char::is_control) {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let controller = match self.controller.try_lock() {
            Ok(controller) => controller,
            Err(std::sync::TryLockError::WouldBlock) => return Err(MEDIA_BUSY),
            Err(_) => return Err(MEDIA_BACKEND_ERROR),
        };
        let (enabled, capacity_available) = controller.preflight_state();
        drop(controller);
        if !enabled {
            return Ok(SourcePreflightResponse::unavailable(
                request,
                "capture_disabled",
            ));
        }
        if !capacity_available {
            return Ok(SourcePreflightResponse::unavailable(
                request,
                "capture_capacity_exhausted",
            ));
        }
        if let Some(response) = unsupported_mode(request) {
            return Ok(response);
        }
        #[cfg(any(windows, target_os = "linux"))]
        {
            self.worker.query(caller, request)
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            Ok(SourcePreflightResponse::unsupported(
                request,
                "unsupported_platform",
            ))
        }
    }
}

#[cfg(any(windows, target_os = "linux"))]
pub(super) fn query<B: Backend>(
    backend: &B,
    enabled: bool,
    capacity_available: bool,
    caller: &str,
    request: &SourcePreflightRequest,
) -> Result<SourcePreflightResponse, Status> {
    request.validate().map_err(|_| STATUS_INVALID_ARGUMENT)?;
    if caller.is_empty() || caller.len() > 128 || caller.chars().any(char::is_control) {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    if !enabled {
        return Ok(SourcePreflightResponse::unavailable(
            request,
            "capture_disabled",
        ));
    }
    if !capacity_available {
        return Ok(SourcePreflightResponse::unavailable(
            request,
            "capture_capacity_exhausted",
        ));
    }
    if let Some(response) = unsupported_mode(request) {
        return Ok(response);
    }
    #[cfg(windows)]
    {
        Ok(windows::query(backend, request))
    }
    #[cfg(not(windows))]
    {
        let _ = backend;
        Ok(SourcePreflightResponse::unsupported(
            request,
            "unsupported_platform",
        ))
    }
}

fn unsupported_mode(request: &SourcePreflightRequest) -> Option<SourcePreflightResponse> {
    if request.source_class == SourceClass::Synthetic {
        return Some(SourcePreflightResponse::unsupported(
            request,
            "source_class_unsupported",
        ));
    }
    let high_depth = request.mode.hdr || request.mode.bit_depth == Some(10);
    let pixels = u64::from(request.mode.width) * u64::from(request.mode.height);
    if (high_depth
        && (!request.mode.width.is_multiple_of(2) || !request.mode.height.is_multiple_of(2)))
        || if high_depth {
            pixels * 8 > 2 * MAX_FRAME_BYTES as u64 || pixels * 3 > MAX_FRAME_BYTES as u64
        } else {
            pixels * 4 > MAX_FRAME_BYTES as u64
        }
    {
        return Some(SourcePreflightResponse::unsupported(
            request,
            "source_frame_too_large",
        ));
    }
    None
}

#[cfg(any(windows, test))]
fn ready(request: &SourcePreflightRequest) -> SourcePreflightResponse {
    // Describe this observed, requested size precisely. A later mode needs its
    // own query and cannot infer a larger Cartesian range past the frame budget.
    SourcePreflightResponse::ready(
        request,
        vec![SourceModeCapabilities {
            source_class: request.source_class,
            min_width: request.mode.width,
            max_width: request.mode.width,
            min_height: request.mode.height,
            max_height: request.mode.height,
            min_refresh_millihz: 1000,
            max_refresh_millihz: 1_000_000,
            sdr: true,
            hdr: true,
            bit_depths: vec![8, 10],
            format_open: true,
        }],
    )
}

#[cfg(any(windows, test))]
pub(super) struct OutputObservation<'a> {
    pub id: &'a str,
    pub source_name: &'a str,
    pub attached: bool,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}
#[cfg(any(windows, test))]
pub(super) struct ExactGeometry {
    pub source_name: String,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}
#[cfg(any(windows, test))]
pub(super) fn verify_outputs<'a>(
    outputs: impl IntoIterator<Item = OutputObservation<'a>>,
    route: &str,
    exact: Option<&ExactGeometry>,
    request: &SourcePreflightRequest,
) -> Result<(), &'static str> {
    let mut matching = outputs.into_iter().filter(|output| {
        output.attached
            && exact.map_or_else(
                || output.id == route,
                |exact| {
                    output.source_name.eq_ignore_ascii_case(&exact.source_name)
                        && output.left == exact.left
                        && output.top == exact.top
                        && output.width == exact.width
                        && output.height == exact.height
                },
            )
    });
    let output = matching.next().ok_or("exact_source_unavailable")?;
    if matching.next().is_some() {
        return Err("ambiguous_exact_source");
    }
    if output.width != request.mode.width || output.height != request.mode.height {
        return Err("source_mode_changed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_module_sdk::capture::SourceMode;
    #[cfg(not(windows))]
    use amaterasu_module_sdk::capture::SourceReadiness;
    struct NoEffectsBackend;
    impl Backend for NoEffectsBackend {
        fn supported(&self) -> bool {
            panic!("disabled query must not inspect backend")
        }
        fn enumerate(&self) -> crate::model::CaptureResult<Vec<crate::model::Output>> {
            panic!("disabled query must not enumerate")
        }
        fn open(
            &self,
            _: &str,
        ) -> crate::model::CaptureResult<Box<dyn crate::backend::CaptureSession>> {
            panic!("source preflight must never open capture")
        }
    }
    fn request() -> SourcePreflightRequest {
        SourcePreflightRequest {
            source_class: SourceClass::ManagedOutput,
            route: None,
            mode: SourceMode {
                width: 1920,
                height: 1080,
                refresh_millihz: 60_000,
                hdr: false,
                bit_depth: None,
            },
        }
    }
    #[test]
    fn actual_source_formats_support_sdr_hdr_and_bounded_p010_storage() {
        let mut request = request();
        assert!(unsupported_mode(&request).is_none());
        ready(&request).validate(&request).unwrap();
        request.mode.hdr = true;
        assert!(unsupported_mode(&request).is_none());
        assert!(ready(&request).capabilities[0].hdr);
        request.mode.hdr = false;
        request.mode.bit_depth = Some(10);
        assert!(unsupported_mode(&request).is_none());
        assert!(ready(&request).capabilities[0].bit_depths.contains(&10));
        assert!(ready(&request).capabilities[0].format_open);
        request.mode.bit_depth = None;
        request.mode.width = 8192;
        request.mode.height = 8192;
        assert_eq!(
            unsupported_mode(&request).unwrap().reason.as_deref(),
            Some("source_frame_too_large")
        );
        request.source_class = SourceClass::Synthetic;
        assert_eq!(
            unsupported_mode(&request).unwrap().reason.as_deref(),
            Some("source_class_unsupported")
        );
    }
    #[test]
    fn query_binding_reports_disabled_and_busy_without_opening_a_source() {
        let controller = Arc::new(Mutex::new(Controller::new(NoEffectsBackend, false)));
        let provider = Provider::new(controller.clone(), NoEffectsBackend).unwrap();
        let request = request();
        assert_eq!(
            provider
                .query("consumer", &request)
                .unwrap()
                .reason
                .as_deref(),
            Some("capture_disabled")
        );
        assert!(provider.query("", &request).is_err());
        let _guard = controller.lock().unwrap();
        assert_eq!(
            provider.query("consumer", &request).unwrap_err(),
            MEDIA_BUSY
        );
    }
    #[test]
    fn exact_readiness_rejects_missing_ambiguous_detached_or_changed_sources() {
        let mut request = request();
        request.source_class = SourceClass::DesktopOutput;
        let target = ExactGeometry {
            source_name: "DISPLAY2".into(),
            left: 1920,
            top: 0,
            width: 1920,
            height: 1080,
        };
        let output = || OutputObservation {
            id: "native-two",
            source_name: "DISPLAY2",
            attached: true,
            left: 1920,
            top: 0,
            width: 1920,
            height: 1080,
        };
        assert!(verify_outputs([output()], "opaque-exact", Some(&target), &request).is_ok());
        assert_eq!(
            verify_outputs([output()], "missing-native-id", None, &request),
            Err("exact_source_unavailable")
        );
        assert_eq!(
            verify_outputs(
                [output(), output()],
                "opaque-exact",
                Some(&target),
                &request
            ),
            Err("ambiguous_exact_source")
        );
        let mut detached = output();
        detached.attached = false;
        assert_eq!(
            verify_outputs([detached], "opaque-exact", Some(&target), &request),
            Err("exact_source_unavailable")
        );
        request.mode.width = 1280;
        assert_eq!(
            verify_outputs([output()], "opaque-exact", Some(&target), &request),
            Err("source_mode_changed")
        );
    }
    #[test]
    #[cfg(not(windows))]
    fn an_enabled_non_windows_provider_does_not_claim_source_readiness() {
        let controller = Arc::new(Mutex::new(Controller::new(NoEffectsBackend, true)));
        let provider = Provider::new(controller, NoEffectsBackend).unwrap();
        let response = loop {
            match provider.query("consumer", &request()) {
                Ok(response) => break response,
                Err(MEDIA_BUSY) => std::thread::yield_now(),
                Err(error) => panic!("unexpected preflight failure: {error:?}"),
            }
        };
        assert_eq!(response.state, SourceReadiness::Unsupported);
        assert!(response.capabilities.is_empty());
    }

    #[cfg(not(any(windows, target_os = "linux")))]
    #[test]
    fn unsupported_platform_needs_no_worker_to_quiesce() {
        let controller = Arc::new(Mutex::new(Controller::new(NoEffectsBackend, true)));
        let provider = Provider::new(controller, NoEffectsBackend).unwrap();
        assert_eq!(
            provider.query("", &request()).unwrap_err(),
            STATUS_INVALID_ARGUMENT
        );
        assert_eq!(
            provider.query("consumer", &request()).unwrap().state,
            SourceReadiness::Unsupported
        );
        assert!(provider.stop_admission().is_ok());
        assert!(provider.quiesce().unwrap());
    }
}
