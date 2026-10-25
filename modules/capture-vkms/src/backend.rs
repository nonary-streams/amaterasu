//! The platform operations the controller needs, behind one trait so the
//! controller is testable without DRM or a helper process.
use crate::{
    discovery::{self, Discovery, DisplayProbe, OutputBinding, kms::KmsProbe},
    geometry::{DesktopLayout, LayoutOutput, WaylandLayout},
    model::{CaptureError, CaptureResult},
    output_name::OutputName,
    presenter::{FrameSource, SourceInterrupt},
};
use amaterasu_vkms_frame_helper::{
    channel::ChannelShutdown,
    client::{ClientError, FrameExport, Presentation},
    process::{self, HelperProcess},
    protocol::Failure,
};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

pub trait Backend: Send {
    fn discover(&self) -> CaptureResult<Discovery>;
    fn resolve(&self, output: OutputName) -> CaptureResult<OutputBinding>;
    /// Starts the helper once to prove it is usable and holds CAP_SYS_ADMIN.
    fn probe_helper(&self, helper: &str) -> CaptureResult<()>;
    /// Starts a helper for `binding`, opens its card and exports one frame
    /// to prove the whole path works before a stream is reported open.
    fn connect(&self, helper: &str, binding: &OutputBinding)
    -> CaptureResult<Box<dyn FrameSource>>;
    /// CLOCK_MONOTONIC nanoseconds, the driver's presentation clock.
    fn monotonic_now_ns(&self) -> u64;
    /// Dispatches compositor events without blocking; true when the output
    /// layout changed since the previous call.
    fn poll_layout(&self) -> bool;
    /// The compositor's live outputs, or `None` without a Wayland display.
    fn desktop_layout(&self) -> Option<Vec<LayoutOutput>>;
}

pub struct NativeBackend {
    probe: KmsProbe,
    layout: Mutex<WaylandLayout>,
}

impl NativeBackend {
    pub fn new() -> Self {
        Self {
            probe: KmsProbe::new(),
            layout: Mutex::new(WaylandLayout::new()),
        }
    }

    fn with_layout<T>(&self, operation: impl FnOnce(&mut WaylandLayout) -> T) -> T {
        let mut layout = self
            .layout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        operation(&mut layout)
    }
}

impl Backend for NativeBackend {
    fn discover(&self) -> CaptureResult<Discovery> {
        discovery::discover(&self.probe as &dyn DisplayProbe)
    }

    fn resolve(&self, output: OutputName) -> CaptureResult<OutputBinding> {
        discovery::resolve(&self.probe, output)
    }

    fn probe_helper(&self, helper: &str) -> CaptureResult<()> {
        start_helper(helper).map(drop)
    }

    fn connect(
        &self,
        helper: &str,
        binding: &OutputBinding,
    ) -> CaptureResult<Box<dyn FrameSource>> {
        let mut process = start_helper(helper)?;
        process
            .client()
            .open_card(binding.device.major, binding.device.minor)
            .map_err(|error| open_card_error(binding, &error))?;
        match process.client().frame(binding.crtc_id) {
            Ok(_) => {}
            Err(error) => return Err(first_frame_error(helper, binding, &error)),
        }
        let shutdown = process.client().shutdown_handle().map_err(|error| {
            CaptureError::new(
                "frame_helper_failed",
                format!("the frame helper socket cannot be shared: {error}"),
            )
        })?;
        Ok(Box::new(HelperSource {
            process,
            shutdown: Arc::new(shutdown),
        }))
    }

    fn monotonic_now_ns(&self) -> u64 {
        monotonic_now_ns()
    }

    fn poll_layout(&self) -> bool {
        self.with_layout(|layout| layout.poll())
    }

    fn desktop_layout(&self) -> Option<Vec<LayoutOutput>> {
        self.with_layout(|layout| layout.outputs())
    }
}

/// A helper process serving one stream, used only by its presentation
/// thread. The shutdown handle lets another thread end a pending WAIT.
struct HelperSource {
    process: HelperProcess,
    shutdown: Arc<ChannelShutdown>,
}

impl FrameSource for HelperSource {
    fn wait(
        &mut self,
        crtc_id: u32,
        last_sequence: u64,
        timeout_ms: u32,
    ) -> Result<Presentation, ClientError> {
        self.process
            .client()
            .wait(crtc_id, last_sequence, timeout_ms)
    }

    fn frame(&mut self, crtc_id: u32) -> Result<FrameExport, ClientError> {
        self.process.client().frame(crtc_id)
    }

    fn interrupter(&self) -> Arc<dyn SourceInterrupt> {
        self.shutdown.clone()
    }
}

impl Drop for HelperSource {
    fn drop(&mut self) {
        // The shutdown handle holds a second reference to the socket, so
        // closing the client alone would not show the helper end of stream.
        self.shutdown.shutdown();
    }
}

impl SourceInterrupt for ChannelShutdown {
    fn interrupt(&self) {
        self.shutdown();
    }
}

/// Verifies, spawns and greets the helper; refuses one without CAP_SYS_ADMIN.
fn start_helper(helper: &str) -> CaptureResult<HelperProcess> {
    let executable = process::open_verified_helper(Path::new(helper))
        .map_err(|problem| CaptureError::frame_helper_unavailable(problem.describe()))?;
    let mut helper_process = HelperProcess::spawn(executable).map_err(|error| {
        CaptureError::frame_helper_unavailable(format!("cannot start the frame helper: {error}"))
    })?;
    let hello = match helper_process.client().hello() {
        Ok(hello) => hello,
        Err(ClientError::Closed) => {
            let reason = helper_process.startup_refusal();
            return Err(CaptureError::frame_helper_unavailable(reason));
        }
        Err(error) => {
            return Err(CaptureError::frame_helper_unavailable(format!(
                "the frame helper did not answer: {error:?}"
            )));
        }
    };
    if !hello.admin_permitted {
        return Err(CaptureError::permission_denied(helper));
    }
    Ok(helper_process)
}

pub fn open_card_error(binding: &OutputBinding, error: &ClientError) -> CaptureError {
    match error {
        ClientError::Refused {
            failure: Failure::WrongDriver | Failure::DeviceRejected,
            ..
        } => CaptureError::driver_unavailable(format!(
            "{} is not a vibeshine_drm primary node",
            binding.card
        )),
        ClientError::Refused {
            failure: Failure::DeviceUnavailable,
            os_error,
        } => CaptureError::driver_unavailable(format!(
            "the frame helper cannot open {}: {}",
            binding.card,
            std::io::Error::from_raw_os_error(*os_error)
        )),
        error => helper_failed(error),
    }
}

pub fn first_frame_error(
    helper: &str,
    binding: &OutputBinding,
    error: &ClientError,
) -> CaptureError {
    match error.failure() {
        Some(Failure::PermissionDenied) => CaptureError::permission_denied(helper),
        Some(Failure::CrtcNotFound) => CaptureError::new(
            "output_unavailable",
            format!("CRTC {} of {} is gone", binding.crtc_id, binding.output),
        ),
        _ => helper_failed(error),
    }
}

fn helper_failed(error: &ClientError) -> CaptureError {
    let detail = match error {
        ClientError::Refused { failure, os_error } => {
            format!("{} (errno {os_error})", failure.code())
        }
        other => format!("{other:?}"),
    };
    CaptureError::new(
        "frame_helper_failed",
        format!("the frame helper failed: {detail}"),
    )
}

pub fn monotonic_now_ns() -> u64 {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes one timespec on this stack frame.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    (now.tv_sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(now.tv_nsec as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::DisplayMode;
    use amaterasu_vkms_frame_helper::drm_node::DeviceNumber;

    fn binding() -> OutputBinding {
        OutputBinding {
            output: OutputName::new(9).unwrap(),
            card: "card3".into(),
            device: DeviceNumber {
                major: 226,
                minor: 3,
            },
            connector_id: 109,
            crtc_id: 41,
            mode: DisplayMode {
                width: 1920,
                height: 1080,
                refresh_hz: 60,
            },
        }
    }

    fn refused(failure: Failure, os_error: i32) -> ClientError {
        ClientError::Refused { failure, os_error }
    }

    #[test]
    fn card_open_failures_map_to_driver_unavailable() {
        for failure in [Failure::WrongDriver, Failure::DeviceRejected] {
            assert_eq!(
                open_card_error(&binding(), &refused(failure, 0)).code,
                "driver_unavailable"
            );
        }
        let denied = open_card_error(
            &binding(),
            &refused(Failure::DeviceUnavailable, libc::EACCES),
        );
        assert_eq!(denied.code, "driver_unavailable");
        assert!(denied.message.contains("card3"));
        assert_eq!(
            open_card_error(&binding(), &ClientError::TimedOut).code,
            "frame_helper_failed"
        );
    }

    #[test]
    fn first_frame_failures_explain_the_operator_fix() {
        let denied = first_frame_error(
            "/opt/h",
            &binding(),
            &refused(Failure::PermissionDenied, 13),
        );
        assert_eq!(denied.code, "permission_denied");
        assert!(
            denied
                .message
                .contains("sudo setcap cap_sys_admin+p /opt/h")
        );
        assert_eq!(
            first_frame_error("/opt/h", &binding(), &refused(Failure::CrtcNotFound, 2)).code,
            "output_unavailable"
        );
        assert_eq!(
            first_frame_error("/opt/h", &binding(), &ClientError::Closed).code,
            "frame_helper_failed"
        );
    }

    #[test]
    fn an_unusable_helper_path_is_frame_helper_unavailable() {
        for path in ["relative/helper", "/nonexistent/amaterasu-helper", "/"] {
            let error = start_helper(path).err().unwrap();
            assert_eq!(error.code, "frame_helper_unavailable", "{path}");
        }
    }

    #[test]
    fn the_monotonic_clock_advances() {
        let first = monotonic_now_ns();
        assert!(first > 0);
        assert!(monotonic_now_ns() >= first);
    }
}
