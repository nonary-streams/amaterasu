use super::{Backend, CaptureSession, TouchSurface};
use crate::format::{CaptureFormat, SourceEncoding, SourceInfo};
use crate::model::{CaptureError, CaptureResult, Frame, Output};
use crate::worker::{FrameSource, Worker};
use amaterasu_media::{CaptureGeometry, DesktopRect};
use amaterasu_module_sdk::capture::{EXACT_TARGET_PREFIX, exact_target_key, exact_target_selector};
use amaterasu_windows_capture_geometry::{
    GeometryError, SelectedOutput, resolve_target_matching, resolve_with,
    with_verified_physical_topology,
};
use serde_json::Value;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};
use windows::core::{IInspectable, Interface};

#[path = "monitors.rs"]
mod monitors;
#[path = "readback.rs"]
mod readback;
#[path = "source_clock.rs"]
mod source_clock;
use readback::{Readback, native_error};
use source_clock::SourceClock;

pub struct WindowsBackend {
    capture_cursor: Option<bool>,
}

impl WindowsBackend {
    pub fn new(capture_cursor: Option<bool>) -> Self {
        Self { capture_cursor }
    }
}

impl Backend for WindowsBackend {
    fn supported(&self) -> bool {
        true
    }

    fn enumerate(&self) -> CaptureResult<Vec<Output>> {
        with_verified_physical_topology(|| {
            monitors::enumerate()
                .map(|monitors| {
                    monitors
                        .into_iter()
                        .map(|monitor| monitor.metadata)
                        .collect()
                })
                .map_err(|_| GeometryError::Unavailable("GDI monitor enumeration failed"))
        })
        .map_err(capture_geometry_error)
    }

    fn open(&self, output: &str) -> CaptureResult<Box<dyn CaptureSession>> {
        self.open_with_format(output, CaptureFormat::default())
    }

    fn verify_source_format(&self, output: &str, format: CaptureFormat) -> CaptureResult<()> {
        let (_, monitor) = resolve_monitor(output, None)?;
        format.validate_dimensions(monitor.metadata.width, monitor.metadata.height)?;
        if format.color_proof {
            monitors::desktop_color(&monitor)?.verify(format)?;
        }
        Ok(())
    }

    fn open_with_format(
        &self,
        output: &str,
        format: CaptureFormat,
    ) -> CaptureResult<Box<dyn CaptureSession>> {
        let target_id = match resolve_monitor(output, None) {
            Ok((selection, _)) => Some(selection.target_id),
            Err(error)
                if error.code == "geometry_unavailable"
                    && !output.starts_with(EXACT_TARGET_PREFIX)
                    && !format.color_proof =>
            {
                None
            }
            Err(error) if error.code == "access_lost" => {
                return Err(CaptureError::new(
                    "not_found",
                    "monitor identity no longer enumerates; refresh outputs",
                ));
            }
            Err(error) => return Err(error),
        };
        let output_id = output.to_owned();
        let worker_output = output_id.clone();
        let worker_target = target_id.clone();
        let capture_cursor = self.capture_cursor;
        let (worker, (device_adapter, closed, source_info)) =
            Worker::start_with_metadata(move || {
                let monitor = match worker_target {
                    Some(target) => resolve_monitor(&worker_output, Some(&target))?.1,
                    None => find_monitor(&worker_output)?,
                };
                let session = Session::open(monitor, capture_cursor, format)?;
                let device_adapter = session.readback.device_adapter();
                let closed = session.closed.clone();
                let source_info = session.source_info;
                Ok((session, (device_adapter, closed, source_info)))
            })?;
        Ok(Box::new(WindowsSession {
            worker,
            output_id,
            target_id,
            device_adapter,
            closed,
            format,
            source_info,
        }))
    }
}

struct WindowsSession {
    worker: Worker,
    output_id: String,
    target_id: Option<String>,
    device_adapter: Value,
    closed: Arc<AtomicBool>,
    format: CaptureFormat,
    source_info: SourceInfo,
}

impl CaptureSession for WindowsSession {
    fn quiesce(&mut self) -> CaptureResult<bool> {
        self.worker.quiesce()
    }

    fn source_info(&mut self) -> CaptureResult<Option<SourceInfo>> {
        self.validate_source()?;
        Ok(Some(self.source_info))
    }
    fn validate_source(&mut self) -> CaptureResult<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(CaptureError::new("access_lost", "capture monitor closed"));
        }
        if self.format.color_proof {
            let target = self.target_id.as_deref().ok_or_else(|| {
                CaptureError::new("access_lost", "high-depth source lost exact target binding")
            })?;
            let (_, monitor) = resolve_monitor(&self.output_id, Some(target))?;
            monitors::desktop_color(&monitor)
                .and_then(|color| color.verify(self.format))
                .map_err(|error| CaptureError::new("access_lost", error.message))?;
        }
        Ok(())
    }

    fn read(&mut self, wait_ms: u32) -> CaptureResult<Option<Frame>> {
        while_item_open(&self.closed, || self.worker.read(wait_ms))
    }

    fn geometry(&mut self) -> CaptureResult<Option<CaptureGeometry>> {
        let Some(target_id) = &self.target_id else {
            return Ok(None);
        };
        resolve_monitor(&self.output_id, Some(target_id))
            .map(|(selection, _)| Some(selection.geometry))
    }

    fn touch_surface(&mut self) -> CaptureResult<Option<TouchSurface>> {
        let Some(target_id) = &self.target_id else {
            return Ok(None);
        };
        let (_, monitor) = resolve_monitor(&self.output_id, Some(target_id))?;
        let width = monitor.metadata.width;
        let height = monitor.metadata.height;
        // The verified GDI/DXGI/CCD topology uses physical desktop pixels,
        // matching Windows absolute pointer coordinates for this monitor.
        Ok(Some(TouchSurface {
            width,
            height,
            logical_width: width,
            logical_height: height,
        }))
    }

    fn device_adapter(&self) -> Value {
        self.device_adapter.clone()
    }
}

// Declaration order drops every WinRT/D3D object before the apartment guard.
struct Session {
    session: GraphicsCaptureSession,
    pool: Direct3D11CaptureFramePool,
    item: GraphicsCaptureItem,
    readback: Readback,
    closed: Arc<AtomicBool>,
    closed_token: Option<i64>,
    size: SizeInt32,
    started: Instant,
    source_clock: SourceClock,
    monitor: monitors::Monitor,
    format: CaptureFormat,
    source_info: SourceInfo,
    _apartment: Apartment,
}

impl Session {
    fn open(
        monitor: monitors::Monitor,
        capture_cursor: Option<bool>,
        format: CaptureFormat,
    ) -> CaptureResult<Self> {
        let apartment = Apartment::initialize()?;
        if !GraphicsCaptureSession::IsSupported().map_err(native_error)? {
            return Err(CaptureError::new(
                "unsupported_platform",
                "Windows Graphics Capture is unavailable",
            ));
        }
        let interop: IGraphicsCaptureItemInterop =
            windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
                .map_err(native_error)?;
        // SAFETY: this live monitor handle was resolved on this worker. The
        // official wrapper owns the returned GraphicsCaptureItem reference.
        let item: GraphicsCaptureItem =
            unsafe { interop.CreateForMonitor(monitor.handle) }.map_err(native_error)?;
        let size = item.Size().map_err(native_error)?;
        validate_size(size, format)?;
        let desktop_hdr = if format.color_proof {
            let color = monitors::desktop_color(&monitor)?;
            color.verify(format)?;
            color.hdr()?
        } else {
            false
        };
        if u32::try_from(size.Width).ok() != Some(monitor.metadata.width)
            || u32::try_from(size.Height).ok() != Some(monitor.metadata.height)
        {
            return Err(CaptureError::new(
                "access_lost",
                "WGC capture size differs from the selected physical monitor",
            ));
        }
        let (readback, graphics_device) = Readback::new(format.encoding())?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &graphics_device,
            match format.encoding() {
                SourceEncoding::Bgra8 => DirectXPixelFormat::B8G8R8A8UIntNormalized,
                SourceEncoding::Rgba16FloatScRgb => DirectXPixelFormat::R16G16B16A16Float,
            },
            2,
            size,
        )
        .map_err(native_error)?;
        let session = match pool.CreateCaptureSession(&item) {
            Ok(session) => session,
            Err(error) => {
                let _ = pool.Close();
                return Err(native_error(error));
            }
        };
        // Do not retain temporary COM references outside Session once its
        // apartment guard is moved: an initialization error must release all
        // COM objects before RoUninitialize.
        drop(graphics_device);
        drop(interop);
        let closed = Arc::new(AtomicBool::new(false));
        let started = Instant::now();
        let mut result = Self {
            session,
            pool,
            item,
            readback,
            closed,
            closed_token: None,
            size,
            started,
            source_clock: SourceClock::start(),
            source_info: SourceInfo {
                width: size.Width as u32,
                height: size.Height as u32,
                encoding: format.encoding(),
                desktop_hdr,
            },
            monitor,
            format,
            _apartment: apartment,
        };
        result.configure(capture_cursor)?;
        result.session.StartCapture().map_err(native_error)?;
        Ok(result)
    }

    fn configure(&mut self, capture_cursor: Option<bool>) -> CaptureResult<()> {
        if let Some(enabled) = capture_cursor {
            self.session
                .SetIsCursorCaptureEnabled(enabled)
                .map_err(|error| {
                    CaptureError::new(
                        "unsupported_cursor",
                        format!("requested cursor capture setting failed: {error}"),
                    )
                })?;
        }
        let closed = self.closed.clone();
        self.closed_token = Some(
            self.item
                .Closed(
                    &TypedEventHandler::<GraphicsCaptureItem, IInspectable>::new(move |_, _| {
                        closed.store(true, Ordering::Release);
                        Ok(())
                    }),
                )
                .map_err(native_error)?,
        );
        Ok(())
    }

    fn acquire(&mut self) -> CaptureResult<bool> {
        let Some(frame) = next_frame(&self.pool)? else {
            return Ok(false);
        };
        let frame = CapturedFrame(frame);
        let size = frame.0.ContentSize().map_err(native_error)?;
        if size != self.size {
            return Err(CaptureError::new(
                "access_lost",
                "monitor content size changed; enumerate and reopen",
            ));
        }
        let surface = frame.0.Surface().map_err(native_error)?;
        let access: IDirect3DDxgiInterfaceAccess = surface.cast().map_err(native_error)?;
        // SAFETY: WinRT surface exposes the official DXGI access interface;
        // the returned texture owns a reference through readback submission.
        let texture = unsafe { access.GetInterface() }.map_err(native_error)?;
        let observed_us = self.started.elapsed().as_micros().min(u64::MAX as u128) as u64;
        // WinRT supplies the compositor's QPC time. If it fails to supply one,
        // retain only the local observation time, with no source-time claim.
        let source_time = frame.0.SystemRelativeTime().ok().map(|time| time.Duration);
        let timestamp = self.source_clock.map(source_time, observed_us);
        self.readback.submit(
            &texture,
            size.Width as u32,
            size.Height as u32,
            observed_us,
            timestamp,
        )?;
        Ok(true)
    }
}

fn resolve_monitor(
    output_id: &str,
    expected_target: Option<&str>,
) -> CaptureResult<(
    amaterasu_windows_capture_geometry::CaptureSelection,
    monitors::Monitor,
)> {
    let exact = if output_id.starts_with(EXACT_TARGET_PREFIX) {
        if exact_target_key(output_id).is_none() {
            return Err(CaptureError::new(
                "invalid_request",
                "invalid exact capture selector",
            ));
        }
        Some(
            resolve_target_matching(|target| {
                exact_target_selector(&target.to_ascii_lowercase())
                    .is_ok_and(|selector| selector == output_id)
            })
            .map_err(capture_geometry_error)?,
        )
    } else {
        None
    };
    if let Some((selection, _)) = &exact
        && expected_target
            .is_some_and(|expected| !expected.eq_ignore_ascii_case(&selection.target_id))
    {
        return Err(capture_geometry_error(GeometryError::TargetChanged));
    }
    let target = exact
        .as_ref()
        .map(|(selection, _)| selection.target_id.as_str())
        .or(expected_target);
    resolve_with(target, || {
        let mut monitors = monitors::enumerate()
            .map_err(|_| GeometryError::Unavailable("GDI monitor enumeration failed"))?
            .into_iter()
            .filter(|monitor| {
                exact.as_ref().map_or_else(
                    || monitor.metadata.id == output_id,
                    |(_, selected)| {
                        monitor
                            .metadata
                            .device_name
                            .eq_ignore_ascii_case(&selected.source_name)
                            && monitor.handle.0 as usize == selected.monitor_handle
                    },
                )
            });
        let monitor = monitors.next().ok_or(GeometryError::TargetChanged)?;
        if monitors.next().is_some() {
            return Err(GeometryError::Unavailable(
                "capture selector matches multiple GDI monitors",
            ));
        }
        let rectangle = DesktopRect::new(
            monitor.metadata.left,
            monitor.metadata.top,
            monitor.metadata.width,
            monitor.metadata.height,
        )
        .map_err(|_| GeometryError::Unavailable("WGC selected an invalid monitor rectangle"))?;
        Ok((
            SelectedOutput {
                source_name: monitor.metadata.device_name.clone(),
                monitor_handle: monitor.handle.0 as usize,
                rectangle,
            },
            monitor,
        ))
    })
    .map_err(capture_geometry_error)
}

fn find_monitor(output_id: &str) -> CaptureResult<monitors::Monitor> {
    monitors::enumerate()?
        .into_iter()
        .find(|monitor| monitor.metadata.id == output_id)
        .ok_or_else(|| {
            CaptureError::new(
                "not_found",
                "monitor identity no longer enumerates; refresh outputs",
            )
        })
}

fn capture_geometry_error(error: GeometryError) -> CaptureError {
    match error {
        GeometryError::TargetChanged => CaptureError::new(
            "access_lost",
            "captured monitor no longer resolves to the same physical display",
        ),
        GeometryError::Unavailable(message) => CaptureError::new("geometry_unavailable", message),
    }
}

impl FrameSource for Session {
    fn poll(&mut self) -> CaptureResult<Option<Frame>> {
        let closed = self.closed.clone();
        let result = while_item_open(&closed, || {
            if self.format.color_proof {
                monitors::desktop_color(&self.monitor)
                    .and_then(|color| color.verify(self.format))
                    .map_err(|error| CaptureError::new("access_lost", error.message))?;
            }
            if !self.readback.pending() && !self.acquire()? {
                return Ok(None);
            }
            let frame = self.readback.poll()?;
            if self.format.color_proof {
                monitors::desktop_color(&self.monitor)
                    .and_then(|color| color.verify(self.format))
                    .map_err(|error| CaptureError::new("access_lost", error.message))?;
            }
            Ok(frame)
        });
        if result.is_err() {
            self.readback.invalidate();
        }
        result
    }
}

fn while_item_open<T>(
    closed: &AtomicBool,
    poll: impl FnOnce() -> CaptureResult<T>,
) -> CaptureResult<T> {
    let check_open = || {
        if closed.load(Ordering::Acquire) {
            Err(CaptureError::new(
                "access_lost",
                "capture monitor closed; enumerate and reopen",
            ))
        } else {
            Ok(())
        }
    };
    check_open()?;
    let result = poll()?;
    // A queued GPU readback may complete after GraphicsCaptureItem.Closed.
    // Never hand those pixels to a stream that must now be reopened.
    check_open()?;
    Ok(result)
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(token) = self.closed_token.take() {
            let _ = self.item.RemoveClosed(token);
        }
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

fn validate_size(size: SizeInt32, format: CaptureFormat) -> CaptureResult<()> {
    let width = u32::try_from(size.Width).unwrap_or(0);
    let height = u32::try_from(size.Height).unwrap_or(0);
    format.validate_dimensions(width, height)
}

fn next_frame(pool: &Direct3D11CaptureFramePool) -> CaptureResult<Option<Direct3D11CaptureFrame>> {
    let mut pointer = std::ptr::null_mut();
    // SAFETY: use the official generated vtable, not a handwritten ABI. The
    // high-level wrapper treats a successful null result as E_POINTER; here
    // null means an empty queue, distinct from a real failing HRESULT. A
    // non-null result transfers exactly one owned COM reference.
    unsafe {
        (Interface::vtable(pool).TryGetNextFrame)(Interface::as_raw(pool), &mut pointer)
            .ok()
            .map_err(native_error)?;
        if pointer.is_null() {
            Ok(None)
        } else {
            Ok(Some(Direct3D11CaptureFrame::from_raw(pointer)))
        }
    }
}

struct CapturedFrame(Direct3D11CaptureFrame);

impl Drop for CapturedFrame {
    fn drop(&mut self) {
        let _ = self.0.Close();
    }
}

struct Apartment(PhantomData<Rc<()>>);

impl Apartment {
    fn initialize() -> CaptureResult<Self> {
        // SAFETY: called only on a newly spawned resource-owning worker.
        // Matching RoUninitialize runs on this same thread after resource drops.
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.map_err(native_error)?;
        Ok(Self(PhantomData))
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: paired with successful RoInitialize on this worker.
        unsafe { RoUninitialize() };
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use std::sync::{atomic::AtomicUsize, mpsc};

    struct DelayedFrame {
        release: mpsc::Receiver<()>,
        dropped: Arc<AtomicUsize>,
    }

    impl FrameSource for DelayedFrame {
        fn poll(&mut self) -> CaptureResult<Option<Frame>> {
            self.release.recv().unwrap();
            Ok(Some(Frame {
                encoding: SourceEncoding::Bgra8,
                width: 1,
                height: 1,
                stride: 4,
                timestamp_us: 40_000,
                capture_pacing_timestamp_us: 10_000,
                sequence: 1,
                bytes: Arc::new(vec![1, 2, 3, 4]),
            }))
        }
    }

    impl Drop for DelayedFrame {
        fn drop(&mut self) {
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn closed_item_discards_a_frame_completed_during_readback() {
        let closed = AtomicBool::new(false);
        let result = while_item_open(&closed, || {
            closed.store(true, Ordering::Release);
            Ok(Some(42))
        });

        assert!(matches!(result, Err(error) if error.code == "access_lost"));
    }

    #[test]
    fn closed_item_never_starts_a_new_readback() {
        let closed = AtomicBool::new(true);
        let result = while_item_open(&closed, || -> CaptureResult<Option<u32>> {
            panic!("closed capture must not poll its readback")
        });

        assert!(matches!(result, Err(error) if error.code == "access_lost"));
    }

    #[test]
    fn monitor_close_discards_delayed_readback_and_joins_worker() {
        let (release, gate) = mpsc::channel();
        let dropped = Arc::new(AtomicUsize::new(0));
        let worker_dropped = dropped.clone();
        let worker = Worker::start(move || {
            Ok(DelayedFrame {
                release: gate,
                dropped: worker_dropped,
            })
        })
        .unwrap();
        let closed = Arc::new(AtomicBool::new(false));
        let mut session = WindowsSession {
            worker,
            output_id: "test-output".into(),
            target_id: None,
            device_adapter: Value::Null,
            closed: closed.clone(),
            format: CaptureFormat::default(),
            source_info: SourceInfo {
                width: 1,
                height: 1,
                encoding: SourceEncoding::Bgra8,
                desktop_hdr: false,
            },
        };

        assert!(session.read(0).unwrap().is_none());
        closed.store(true, Ordering::Release);
        release.send(()).unwrap();
        assert_eq!(session.read(1_000).unwrap_err().code, "access_lost");
        assert_eq!(session.read(0).unwrap_err().code, "access_lost");
        drop(session);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }
}
