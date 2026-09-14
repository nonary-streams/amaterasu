use super::{Backend, CaptureSession};
use crate::model::{CaptureError, CaptureResult, Frame, Output, frame_layout};
use crate::worker::{FrameSource, Worker};
use amaterasu_media::{CaptureGeometry, DesktopRect};
use amaterasu_windows_capture_geometry::{GeometryError, SelectedOutput, resolve_with};
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
use readback::{Readback, native_error};

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
        Ok(monitors::enumerate()?
            .into_iter()
            .map(|monitor| monitor.metadata)
            .collect())
    }

    fn open(&self, output: &str) -> CaptureResult<Box<dyn CaptureSession>> {
        let target_id = match resolve_monitor(output, None) {
            Ok((selection, _)) => Some(selection.target_id),
            Err(error) if error.code == "geometry_unavailable" => None,
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
        let worker = Worker::start(move || {
            let monitor = match worker_target {
                Some(target) => resolve_monitor(&worker_output, Some(&target))?.1,
                None => find_monitor(&worker_output)?,
            };
            Session::open(monitor, capture_cursor)
        })?;
        Ok(Box::new(WindowsSession {
            worker,
            output_id,
            target_id,
        }))
    }
}

struct WindowsSession {
    worker: Worker,
    output_id: String,
    target_id: Option<String>,
}

impl CaptureSession for WindowsSession {
    fn read(&mut self, wait_ms: u32) -> CaptureResult<Option<Frame>> {
        self.worker.read(wait_ms)
    }

    fn geometry(&mut self) -> CaptureResult<Option<CaptureGeometry>> {
        let Some(target_id) = &self.target_id else {
            return Ok(None);
        };
        resolve_monitor(&self.output_id, Some(target_id))
            .map(|(selection, _)| Some(selection.geometry))
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
    _apartment: Apartment,
}

impl Session {
    fn open(monitor: monitors::Monitor, capture_cursor: Option<bool>) -> CaptureResult<Self> {
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
        validate_size(size)?;
        let (readback, graphics_device) = Readback::new()?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &graphics_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
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
        let mut result = Self {
            session,
            pool,
            item,
            readback,
            closed,
            closed_token: None,
            size,
            started: Instant::now(),
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
        let timestamp = self.started.elapsed().as_micros().min(u64::MAX as u128) as u64;
        self.readback
            .submit(&texture, size.Width as u32, size.Height as u32, timestamp)?;
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
    resolve_with(expected_target, || {
        let monitor = monitors::enumerate()
            .map_err(|_| GeometryError::Unavailable("GDI monitor enumeration failed"))?
            .into_iter()
            .find(|monitor| monitor.metadata.id == output_id)
            .ok_or(GeometryError::TargetChanged)?;
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
        if self.closed.load(Ordering::Acquire) {
            return Err(CaptureError::new(
                "access_lost",
                "capture monitor closed; enumerate and reopen",
            ));
        }
        if !self.readback.pending() && !self.acquire()? {
            return Ok(None);
        }
        self.readback.poll()
    }
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

fn validate_size(size: SizeInt32) -> CaptureResult<()> {
    let width = u32::try_from(size.Width).unwrap_or(0);
    let height = u32::try_from(size.Height).unwrap_or(0);
    frame_layout(width, height).map(|_| ())
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
