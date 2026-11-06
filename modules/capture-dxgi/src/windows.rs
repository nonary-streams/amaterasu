use super::{Backend, CaptureSession, TouchSurface};
use crate::cursor::{CursorState, Shape, ShapeKind};
use crate::model::{
    CaptureError, CaptureResult, Frame, Output, frame_layout, orient_bgra_rows, verify_output_mode,
};
use crate::staging::{StagingKey, StagingLifecycle};
use amaterasu_media::{CaptureGeometry, DesktopRect};
use amaterasu_module_sdk::capture::{EXACT_TARGET_PREFIX, exact_target_key, exact_target_selector};
use amaterasu_windows_capture_geometry::{
    GeometryError, SelectedOutput, resolve_target_matching, resolve_with,
    with_verified_physical_topology,
};
use serde_json::{Value, json};
use std::time::Instant;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::Interface;

pub struct WindowsBackend;

struct NativeOutput {
    metadata: Output,
    monitor_handle: usize,
    adapter: IDXGIAdapter1,
    output: IDXGIOutput,
}

impl Backend for WindowsBackend {
    fn supported(&self) -> bool {
        true
    }

    fn enumerate(&self) -> CaptureResult<Vec<Output>> {
        with_verified_physical_topology(|| {
            enumerate_outputs()
                .map(|outputs| outputs.into_iter().map(|output| output.metadata).collect())
                .map_err(|_| GeometryError::Unavailable("DXGI output enumeration failed"))
        })
        .map_err(capture_geometry_error)
    }

    fn open(&self, id: &str) -> CaptureResult<Box<dyn CaptureSession>> {
        let (target_id, output) = match resolve_output(id, None) {
            Ok((selection, output)) => (Some(selection.target_id), output),
            Err(error)
                if error.code == "geometry_unavailable" && !id.starts_with(EXACT_TARGET_PREFIX) =>
            {
                (None, find_output(id)?)
            }
            Err(error) if error.code == "access_lost" => {
                return Err(CaptureError::new(
                    "not_found",
                    "output identity no longer enumerates; refresh outputs",
                ));
            }
            Err(error) => return Err(error),
        };
        if !output.metadata.attached {
            return Err(CaptureError::new(
                "output_detached",
                "output is not attached to the desktop",
            ));
        }
        if !matches!(
            output.metadata.rotation,
            value if value == DXGI_MODE_ROTATION_IDENTITY.0 as u32
                || value == DXGI_MODE_ROTATION_ROTATE90.0 as u32
                || value == DXGI_MODE_ROTATION_ROTATE180.0 as u32
                || value == DXGI_MODE_ROTATION_ROTATE270.0 as u32
        ) {
            return Err(CaptureError::new(
                "unsupported_rotation",
                "DXGI reported an unknown output rotation",
            ));
        }
        frame_layout(output.metadata.width, output.metadata.height)?;
        Ok(Box::new(Session::open(output, target_id)?))
    }
}

fn enumerate_outputs() -> CaptureResult<Vec<NativeOutput>> {
    // SAFETY: COM wrappers own returned interface references. No raw pointers
    // escape this call; the factory and adapters remain alive during enumeration.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(native_error)?;
        let mut result = Vec::new();
        for adapter_index in 0..32 {
            let adapter = match factory.EnumAdapters1(adapter_index) {
                Ok(adapter) => adapter,
                Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => return Ok(result),
                Err(error) => return Err(native_error(error)),
            };
            let description = adapter.GetDesc1().map_err(native_error)?;
            let adapter_id = format!(
                "{:08x}:{:08x}",
                description.AdapterLuid.HighPart as u32, description.AdapterLuid.LowPart
            );
            let mut output_enumeration_complete = false;
            for output_index in 0..=64 {
                let output = match adapter.EnumOutputs(output_index) {
                    Ok(output) => output,
                    Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => {
                        output_enumeration_complete = true;
                        break;
                    }
                    Err(error) => return Err(native_error(error)),
                };
                if result.len() >= 64 {
                    return Err(CaptureError::new(
                        "resource_exhausted",
                        "output enumeration exceeds 64 outputs",
                    ));
                }
                result.push(describe_output(&adapter, &adapter_id, output)?);
            }
            if !output_enumeration_complete {
                return Err(CaptureError::new(
                    "resource_exhausted",
                    "output enumeration exceeds 64 outputs per adapter",
                ));
            }
        }
        Err(CaptureError::new(
            "resource_exhausted",
            "adapter enumeration exceeds 32 adapters",
        ))
    }
}

fn describe_output(
    adapter: &IDXGIAdapter1,
    adapter_id: &str,
    output: IDXGIOutput,
) -> CaptureResult<NativeOutput> {
    // SAFETY: the COM wrapper retains the output through GetDesc and owns the
    // returned description by value. Adapter ownership is cloned into the result.
    let adapter_description = unsafe { adapter.GetDesc1() }.map_err(native_error)?;
    let output_description = unsafe { output.GetDesc() }.map_err(native_error)?;
    let device_name = wide_string(&output_description.DeviceName);
    let rectangle = output_description.DesktopCoordinates;
    let width = rectangle_extent(rectangle.left, rectangle.right)?;
    let height = rectangle_extent(rectangle.top, rectangle.bottom)?;
    let metadata = Output {
        id: format!("{adapter_id}/{device_name}"),
        adapter: adapter_id.into(),
        adapter_vendor_id: adapter_description.VendorId,
        adapter_device_id: adapter_description.DeviceId,
        adapter_description: wide_string(&adapter_description.Description),
        device_name,
        attached: output_description.AttachedToDesktop.as_bool(),
        left: rectangle.left,
        top: rectangle.top,
        width,
        height,
        rotation: output_description.Rotation.0 as u32,
    };
    Ok(NativeOutput {
        metadata,
        monitor_handle: output_description.Monitor.0 as usize,
        adapter: adapter.clone(),
        output,
    })
}

fn rectangle_extent(start: i32, end: i32) -> CaptureResult<u32> {
    u32::try_from(i64::from(end) - i64::from(start))
        .ok()
        .filter(|extent| *extent != 0)
        .ok_or_else(|| CaptureError::new("backend_error", "DXGI returned an invalid output span"))
}

fn wide_string(value: &[u16]) -> String {
    String::from_utf16_lossy(
        &value[..value
            .iter()
            .position(|word| *word == 0)
            .unwrap_or(value.len())],
    )
}

fn native_error(error: windows::core::Error) -> CaptureError {
    let code = match error.code() {
        DXGI_ERROR_ACCESS_LOST => "access_lost",
        DXGI_ERROR_DEVICE_REMOVED | DXGI_ERROR_DEVICE_RESET | DXGI_ERROR_DEVICE_HUNG => {
            "device_lost"
        }
        _ => "backend_error",
    };
    CaptureError::new(code, format!("DXGI/D3D11: {error}"))
}

struct PendingTexture {
    texture: ID3D11Texture2D,
    key: StagingKey,
    width: u32,
    height: u32,
    timestamp_us: u64,
    cursor: Option<(Shape, i32, i32)>,
}

struct Session {
    output_id: String,
    target_id: Option<String>,
    adapter: Output,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    staging: StagingLifecycle<ID3D11Texture2D, PendingTexture>,
    started: Instant,
    sequence: u64,
    cursor: CursorState,
}

impl Session {
    fn open(output: NativeOutput, target_id: Option<String>) -> CaptureResult<Self> {
        let output_id = output.metadata.id.clone();
        let adapter = output.metadata.clone();
        let mut device = None;
        let mut context = None;
        // SAFETY: stack output slots are writable for the synchronous call;
        // windows-rs takes ownership of returned COM references. Each immediate
        // context is accessed exclusively under the controller mutex.
        unsafe {
            D3D11CreateDevice(
                &output.adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .map_err(native_error)?;
        }
        let device =
            device.ok_or_else(|| CaptureError::new("backend_error", "D3D11 returned no device"))?;
        let context = context
            .ok_or_else(|| CaptureError::new("backend_error", "D3D11 returned no context"))?;
        let output: IDXGIOutput1 = output.output.cast().map_err(native_error)?;
        // SAFETY: the device was created on this output's adapter and remains
        // owned by Session for the entire duplication lifetime.
        let duplication = unsafe { output.DuplicateOutput(&device) }.map_err(native_error)?;
        Ok(Self {
            output_id,
            target_id,
            adapter,
            device,
            context,
            duplication,
            staging: StagingLifecycle::default(),
            started: Instant::now(),
            sequence: 0,
            cursor: CursorState::default(),
        })
    }

    fn acquire(&mut self, wait_ms: u32) -> CaptureResult<bool> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource = None;
        // SAFETY: both writable output slots live through the call. The wait
        // is validated to <=16 ms by the controller (native ABI uses zero).
        match unsafe {
            self.duplication
                .AcquireNextFrame(wait_ms, &mut info, &mut resource)
        } {
            Ok(()) => (),
            Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(false),
            Err(error) => {
                self.staging.invalidate();
                return Err(native_error(error));
            }
        }
        // The guard releases ownership on every error and unwind path.
        let frame = AcquiredFrame {
            duplication: self.duplication.clone(),
            released: false,
        };
        let result = self.capture_frame(resource, &info);
        let release = frame.release();
        if let Err(error) = release {
            self.staging.invalidate();
            return Err(error);
        }
        let (pending, cursor) = match result {
            Ok(value) => value,
            Err(error) => {
                self.staging.invalidate();
                return Err(error);
            }
        };
        self.cursor = cursor;
        self.staging.submit(pending);
        Ok(true)
    }

    fn capture_frame(
        &mut self,
        resource: Option<IDXGIResource>,
        info: &DXGI_OUTDUPL_FRAME_INFO,
    ) -> CaptureResult<(PendingTexture, CursorState)> {
        let shape = (info.PointerShapeBufferSize != 0)
            .then(|| self.read_pointer_shape(info.PointerShapeBufferSize))
            .transpose()?;
        let position = (info.LastMouseUpdateTime != 0).then(|| {
            info.PointerPosition.Visible.as_bool().then_some((
                info.PointerPosition.Position.x,
                info.PointerPosition.Position.y,
            ))
        });
        let mut cursor = self.cursor.clone();
        cursor.update(shape, position);
        let mut pending = self.copy_resource(resource)?;
        pending.cursor = cursor.snapshot();
        Ok((pending, cursor))
    }

    fn read_pointer_shape(&self, size: u32) -> CaptureResult<Shape> {
        const MAX_POINTER_BYTES: u32 = 2 * 1024 * 1024;
        if size > MAX_POINTER_BYTES {
            return Err(CaptureError::new(
                "invalid_frame",
                "DXGI pointer shape exceeds 2 MiB",
            ));
        }
        let mut bytes = vec![0; size as usize];
        let mut required = 0;
        let mut info = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
        // SAFETY: the duplication frame is owned until capture_frame returns.
        // The vector remains allocated during the call; DXGI writes at most
        // its supplied length and reports how many bytes are initialized.
        unsafe {
            self.duplication.GetFramePointerShape(
                size,
                bytes.as_mut_ptr().cast(),
                &mut required,
                &mut info,
            )
        }
        .map_err(native_error)?;
        if required > size {
            return Err(CaptureError::new(
                "invalid_frame",
                "DXGI pointer shape exceeded its announced buffer",
            ));
        }
        bytes.truncate(required as usize);
        let kind = match info.Type as i32 {
            value if value == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR.0 => ShapeKind::Color,
            value if value == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR.0 => {
                ShapeKind::MaskedColor
            }
            value if value == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0 => ShapeKind::Monochrome,
            _ => {
                return Err(CaptureError::new(
                    "unsupported_format",
                    "DXGI returned an unknown pointer shape",
                ));
            }
        };
        Shape::new(kind, info.Width, info.Height, info.Pitch, bytes)
    }

    fn copy_resource(&mut self, resource: Option<IDXGIResource>) -> CaptureResult<PendingTexture> {
        let source: ID3D11Texture2D = resource
            .ok_or_else(|| CaptureError::new("backend_error", "DXGI returned no frame resource"))?
            .cast()
            .map_err(native_error)?;
        let mut description = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: source is a valid texture; description is a writable local.
        unsafe { source.GetDesc(&mut description) };
        if let Err(error) = frame_layout(description.Width, description.Height) {
            self.staging.invalidate();
            return Err(error);
        }
        if description.Format != DXGI_FORMAT_B8G8R8A8_UNORM
            || description.SampleDesc.Count != 1
            || description.ArraySize != 1
            || description.MipLevels != 1
        {
            self.staging.invalidate();
            return Err(CaptureError::new(
                "unsupported_format",
                "duplication texture must be single-plane BGRA8",
            ));
        }
        description.Usage = D3D11_USAGE_STAGING;
        description.BindFlags = 0;
        description.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        description.MiscFlags = 0;
        let key = StagingKey {
            width: description.Width,
            height: description.Height,
            format: description.Format.0 as u32,
        };
        let mut created = None;
        // SAFETY: description is initialized and destination output slot lives
        // through creation. CopyResource retains GPU dependencies internally;
        // source is not used again after duplication ReleaseFrame.
        let texture = if let Some(texture) = self.staging.take_idle(key) {
            texture
        } else {
            unsafe {
                self.device
                    .CreateTexture2D(&description, None, Some(&mut created))
            }
            .map_err(native_error)?;
            created.ok_or_else(|| {
                CaptureError::new("backend_error", "D3D11 returned no staging texture")
            })?
        };
        unsafe {
            self.context.CopyResource(&texture, &source);
            self.context.Flush();
        }
        Ok(PendingTexture {
            texture,
            key,
            width: description.Width,
            height: description.Height,
            timestamp_us: self.started.elapsed().as_micros().min(u64::MAX as u128) as u64,
            cursor: None,
        })
    }

    fn read_pending(&mut self) -> CaptureResult<Option<Frame>> {
        let pending = self.staging.pending().expect("pending texture");
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: pending texture is CPU-readable staging memory. Map writes a
        // borrowed pointer valid until Unmap; the guard encloses every read.
        match unsafe {
            self.context.Map(
                &pending.texture,
                0,
                D3D11_MAP_READ,
                D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32,
                Some(&mut mapped),
            )
        } {
            Ok(()) => (),
            Err(error) if error.code() == DXGI_ERROR_WAS_STILL_DRAWING => return Ok(None),
            Err(error) => {
                self.staging.invalidate();
                return Err(native_error(error));
            }
        }
        let mapping = MappedTexture {
            context: &self.context,
            texture: &pending.texture,
        };
        let (stride, _) = frame_layout(self.adapter.width, self.adapter.height)?;
        // SAFETY: D3D11 guarantees initialized pixel bytes at each mapped
        // texture row. The map guard outlives all copying, and row padding
        // is never included in borrowed slices or the returned frame.
        let bytes = unsafe {
            orient_bgra_rows(
                pending.width,
                pending.height,
                mapped.RowPitch,
                mapped.pData.cast::<u8>(),
                self.adapter.width,
                self.adapter.height,
                self.adapter.rotation,
            )
        };
        let bytes = bytes.and_then(|mut bytes| {
            if let Some((shape, x, y)) = &pending.cursor {
                shape.compose(&mut bytes, self.adapter.width, self.adapter.height, *x, *y)?;
            }
            Ok(bytes)
        });
        drop(mapping);
        let bytes = match bytes {
            Ok(bytes) => bytes,
            Err(error) => {
                self.staging.invalidate();
                return Err(error);
            }
        };
        self.sequence = self.sequence.saturating_add(1);
        let result = Frame {
            width: self.adapter.width,
            height: self.adapter.height,
            stride,
            bytes,
            timestamp_us: pending.timestamp_us,
            sequence: self.sequence,
        };
        let completed = self.staging.take_pending().expect("mapped pending texture");
        self.staging.store_idle(completed.key, completed.texture);
        Ok(Some(result))
    }
}

impl CaptureSession for Session {
    fn read(&mut self, wait_ms: u32) -> CaptureResult<Option<Frame>> {
        if self.staging.pending().is_none() && !self.acquire(wait_ms)? {
            return Ok(None);
        }
        self.read_pending()
    }

    fn geometry(&mut self) -> CaptureResult<Option<CaptureGeometry>> {
        let Some(target_id) = &self.target_id else {
            return Ok(None);
        };
        let (selection, output) = resolve_output(&self.output_id, Some(target_id))?;
        verify_output_mode(&self.adapter, &output.metadata, selection.geometry)?;
        Ok(Some(selection.geometry))
    }

    fn touch_surface(&mut self) -> CaptureResult<Option<TouchSurface>> {
        let Some(target_id) = &self.target_id else {
            return Ok(None);
        };
        let (selection, output) = resolve_output(&self.output_id, Some(target_id))?;
        verify_output_mode(&self.adapter, &output.metadata, selection.geometry)?;
        // The verified topology runs in physical per-monitor DPI coordinates.
        // Windows absolute pointer input uses those same physical desktop units.
        let width = output.metadata.width;
        let height = output.metadata.height;
        Ok(Some(TouchSurface {
            width,
            height,
            logical_width: width,
            logical_height: height,
        }))
    }

    fn device_adapter(&self) -> Value {
        json!({
            "luid": self.adapter.adapter,
            "vendor_id": self.adapter.adapter_vendor_id,
            "device_id": self.adapter.adapter_device_id,
            "description": self.adapter.adapter_description,
            "ownership": "dxgi_output_adapter",
        })
    }
}

fn find_output(output_id: &str) -> CaptureResult<NativeOutput> {
    enumerate_outputs()?
        .into_iter()
        .find(|output| output.metadata.id == output_id)
        .ok_or_else(|| {
            CaptureError::new(
                "not_found",
                "output identity no longer enumerates; refresh outputs",
            )
        })
}

fn resolve_output(
    output_id: &str,
    expected_target: Option<&str>,
) -> CaptureResult<(
    amaterasu_windows_capture_geometry::CaptureSelection,
    NativeOutput,
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
        let mut outputs = enumerate_outputs()
            .map_err(|_| GeometryError::Unavailable("DXGI output enumeration failed"))?
            .into_iter()
            .filter(|output| {
                exact.as_ref().map_or_else(
                    || output.metadata.id == output_id,
                    |(_, selected)| {
                        output
                            .metadata
                            .device_name
                            .eq_ignore_ascii_case(&selected.source_name)
                            && output.monitor_handle == selected.monitor_handle
                    },
                )
            });
        let output = outputs.next().ok_or(GeometryError::TargetChanged)?;
        if outputs.next().is_some() {
            return Err(GeometryError::Unavailable(
                "capture selector matches multiple DXGI outputs",
            ));
        }
        let rectangle = DesktopRect::new(
            output.metadata.left,
            output.metadata.top,
            output.metadata.width,
            output.metadata.height,
        )
        .map_err(|_| GeometryError::Unavailable("DXGI selected an invalid output rectangle"))?;
        Ok((
            SelectedOutput {
                source_name: output.metadata.device_name.clone(),
                monitor_handle: output.monitor_handle,
                rectangle,
            },
            output,
        ))
    })
    .map_err(capture_geometry_error)
}

fn capture_geometry_error(error: GeometryError) -> CaptureError {
    match error {
        GeometryError::TargetChanged => CaptureError::new(
            "access_lost",
            "captured output no longer resolves to the same physical display",
        ),
        GeometryError::Unavailable(message) => CaptureError::new("geometry_unavailable", message),
    }
}

struct AcquiredFrame {
    duplication: IDXGIOutputDuplication,
    released: bool,
}
impl AcquiredFrame {
    fn release(mut self) -> CaptureResult<()> {
        // SAFETY: this guard exists only after a successful acquire and is
        // consumed exactly once. Mark it released even when DXGI reports an
        // error so Drop cannot issue a second ReleaseFrame.
        let result = unsafe { self.duplication.ReleaseFrame() }.map_err(native_error);
        self.released = true;
        result
    }
}

impl Drop for AcquiredFrame {
    fn drop(&mut self) {
        // SAFETY: the unconsumed guard owns this acquired duplication frame.
        if !self.released {
            let _ = unsafe { self.duplication.ReleaseFrame() };
        }
    }
}

struct MappedTexture<'a> {
    context: &'a ID3D11DeviceContext,
    texture: &'a ID3D11Texture2D,
}

impl Drop for MappedTexture<'_> {
    fn drop(&mut self) {
        // SAFETY: constructed only after successful Map; no borrowed pixel
        // slice escapes the scope that owns this guard.
        unsafe { self.context.Unmap(self.texture, 0) };
    }
}
