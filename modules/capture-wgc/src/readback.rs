use crate::model::{CaptureError, CaptureResult, Frame, copy_bgra_rows, frame_layout};
use crate::staging::{StagingKey, StagingLifecycle};
use serde_json::{Value, json};
use std::sync::Arc;
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::WinRT::Direct3D11::CreateDirect3D11DeviceFromDXGIDevice;
use windows::core::Interface;

pub struct Readback {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    staging: StagingLifecycle<ID3D11Texture2D, PendingTexture>,
    sequence: u64,
}

struct PendingTexture {
    texture: ID3D11Texture2D,
    key: StagingKey,
    width: u32,
    height: u32,
    timestamp_us: u64,
    capture_pacing_timestamp_us: u64,
}

impl Readback {
    pub fn new() -> CaptureResult<(Self, IDirect3DDevice)> {
        let mut device = None;
        let mut context = None;
        // SAFETY: output slots are writable locals; official wrappers own the
        // returned interfaces. The worker serializes immediate-context access.
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
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
        let protection: ID3D11Multithread = context.cast().map_err(native_error)?;
        // SAFETY: WGC may use this device internally on its capture thread;
        // protection enables D3D11's synchronization before sharing the device.
        unsafe {
            let _ = protection.SetMultithreadProtected(true);
        }
        let dxgi_device: IDXGIDevice = device.cast().map_err(native_error)?;
        // SAFETY: the valid DXGI device is retained in Readback; the returned
        // inspectable transfers a new WinRT interface reference to the caller.
        let graphics_device = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device) }
            .map_err(native_error)?
            .cast()
            .map_err(native_error)?;
        Ok((
            Self {
                device,
                context,
                staging: StagingLifecycle::default(),
                sequence: 0,
            },
            graphics_device,
        ))
    }

    pub fn pending(&self) -> bool {
        self.staging.pending().is_some()
    }

    pub fn invalidate(&mut self) {
        self.staging.invalidate();
    }

    pub fn device_adapter(&self) -> Value {
        let result = (|| {
            let dxgi_device: IDXGIDevice = self.device.cast().ok()?;
            let adapter = unsafe { dxgi_device.GetAdapter().ok()? };
            let description = unsafe { adapter.GetDesc().ok()? };
            Some(json!({
                "luid": format!("{:08x}:{:08x}", description.AdapterLuid.HighPart as u32, description.AdapterLuid.LowPart),
                "vendor_id": description.VendorId,
                "device_id": description.DeviceId,
                "description": wide_string(&description.Description),
                "ownership": "wgc_readback_device",
            }))
        })();
        result.unwrap_or_else(|| json!({"ownership":"wgc_readback_device","available":false}))
    }

    pub fn submit(
        &mut self,
        source: &ID3D11Texture2D,
        width: u32,
        height: u32,
        timestamp_us: u64,
        capture_pacing_timestamp_us: u64,
    ) -> CaptureResult<()> {
        if self.pending() {
            return Err(CaptureError::new(
                "backend_error",
                "WGC readback is still pending",
            ));
        }
        if let Err(error) = frame_layout(width, height) {
            self.staging.invalidate();
            return Err(error);
        }
        let mut description = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: source is retained by the captured frame during this call;
        // description is writable stack storage initialized by GetDesc.
        unsafe { source.GetDesc(&mut description) };
        if description.Width != width || description.Height != height {
            self.staging.invalidate();
            return Err(CaptureError::new(
                "access_lost",
                "capture surface dimensions changed; reopen the output",
            ));
        }
        if description.Format != DXGI_FORMAT_B8G8R8A8_UNORM
            || description.SampleDesc.Count != 1
            || description.ArraySize != 1
            || description.MipLevels != 1
        {
            self.staging.invalidate();
            return Err(CaptureError::new(
                "unsupported_format",
                "WGC surface must be single-plane BGRA8",
            ));
        }
        description.Usage = D3D11_USAGE_STAGING;
        description.BindFlags = 0;
        description.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        description.MiscFlags = 0;
        let key = StagingKey {
            width,
            height,
            format: description.Format.0 as u32,
        };
        let mut created = None;
        // SAFETY: initialized staging descriptor and writable local COM slot.
        let texture = if let Some(texture) = self.staging.take_idle(key) {
            texture
        } else {
            if let Err(error) = unsafe {
                self.device
                    .CreateTexture2D(&description, None, Some(&mut created))
            } {
                self.staging.invalidate();
                return Err(native_error(error));
            }
            created.ok_or_else(|| {
                CaptureError::new("backend_error", "D3D11 returned no staging texture")
            })?
        };
        // SAFETY: source/destination match format and dimensions, are retained
        // through submission, and D3D11 tracks queued GPU resource dependencies.
        unsafe {
            self.context.CopyResource(&texture, source);
            self.context.Flush();
        }
        self.staging.submit(PendingTexture {
            texture,
            key,
            width,
            height,
            timestamp_us,
            capture_pacing_timestamp_us,
        });
        Ok(())
    }

    pub fn poll(&mut self) -> CaptureResult<Option<Frame>> {
        let pending = self.staging.pending().expect("pending WGC staging texture");
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: CPU-readable staging texture and writable map output slot.
        // DO_NOT_WAIT returns instead of waiting for GPU completion.
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
        let (stride, _) = match frame_layout(pending.width, pending.height) {
            Ok(layout) => layout,
            Err(error) => {
                drop(mapping);
                self.staging.invalidate();
                return Err(error);
            }
        };
        // SAFETY: each mapped row contains initialized pixel bytes through
        // Unmap. The helper reads only pixels, never uninitialized row padding.
        let bytes = unsafe {
            copy_bgra_rows(
                pending.width,
                pending.height,
                mapped.RowPitch,
                mapped.pData.cast::<u8>(),
            )
        };
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
            width: pending.width,
            height: pending.height,
            stride,
            bytes: Arc::new(bytes),
            timestamp_us: pending.timestamp_us,
            capture_pacing_timestamp_us: pending.capture_pacing_timestamp_us,
            sequence: self.sequence,
        };
        let completed = self
            .staging
            .take_pending()
            .expect("mapped WGC staging texture");
        self.staging.store_idle(completed.key, completed.texture);
        Ok(Some(result))
    }
}

fn wide_string(value: &[u16]) -> String {
    String::from_utf16_lossy(
        &value[..value
            .iter()
            .position(|word| *word == 0)
            .unwrap_or(value.len())],
    )
}

pub fn native_error(error: windows::core::Error) -> CaptureError {
    let code = match error.code() {
        DXGI_ERROR_ACCESS_LOST => "access_lost",
        DXGI_ERROR_DEVICE_REMOVED | DXGI_ERROR_DEVICE_RESET | DXGI_ERROR_DEVICE_HUNG => {
            "device_lost"
        }
        _ => "backend_error",
    };
    CaptureError::new(code, format!("WGC/D3D11: {error}"))
}

struct MappedTexture<'a> {
    context: &'a ID3D11DeviceContext,
    texture: &'a ID3D11Texture2D,
}

impl Drop for MappedTexture<'_> {
    fn drop(&mut self) {
        // SAFETY: constructed only after successful Map, with no pixel borrows
        // remaining when this guard runs.
        unsafe { self.context.Unmap(self.texture, 0) };
    }
}
