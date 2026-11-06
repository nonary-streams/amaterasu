//! A nonmutating capability/readiness query on the selected capture provider.
//! A ready observation is not source ownership, a lease, or proof of delivery.
mod factory;
pub use factory::{SourcePreflightFactory, SourcePreflightProvider};

use crate::{ModuleResult, NativeBinding, RuntimeClient, abi::*, media::native_contracts};
use amaterasu_media::{
    CAPTURE_API_V1_11_SIZE, CAPTURE_SOURCE_PREFLIGHT_INTERFACE_MINOR, CaptureApiPrefix,
    CaptureApiV1_11, INTERFACE_MAJOR, MEDIA_BACKEND_ERROR, MEDIA_UNSUPPORTED,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::ffi::c_void;

pub const MAX_SOURCE_PREFLIGHT_BYTES: usize = 8 * 1024;
pub const MAX_SOURCE_CAPABILITIES: usize = 16;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceClass {
    ManagedOutput,
    DesktopOutput,
    Synthetic,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceMode {
    pub width: u32,
    pub height: u32,
    pub refresh_millihz: u32,
    pub hdr: bool,
    /// Known only after stream negotiation. Absence never means eight-bit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u8>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourcePreflightRequest {
    pub source_class: SourceClass,
    /// A provider-defined bounded output selector. `None` queries a class
    /// before a future managed output exists; it never authorizes fallback.
    pub route: Option<String>,
    pub mode: SourceMode,
}

impl SourcePreflightRequest {
    pub fn validate(&self) -> ModuleResult<()> {
        if !(1..=8192).contains(&self.mode.width)
            || !(1..=8192).contains(&self.mode.height)
            || !(1_000..=1_000_000).contains(&self.mode.refresh_millihz)
            || self
                .mode
                .bit_depth
                .is_some_and(|depth| !matches!(depth, 8 | 10) || (self.mode.hdr && depth != 10))
            || self.route.as_ref().is_some_and(|route| {
                route.is_empty() || route.len() > 512 || route.chars().any(char::is_control)
            })
        {
            return Err("invalid source preflight request".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceModeCapabilities {
    pub source_class: SourceClass,
    pub min_width: u32,
    pub max_width: u32,
    pub min_height: u32,
    pub max_height: u32,
    pub min_refresh_millihz: u32,
    pub max_refresh_millihz: u32,
    pub sdr: bool,
    pub hdr: bool,
    /// Accepted negotiated depths; empty is unknown, never implied ten-bit.
    #[serde(default)]
    pub bit_depths: Vec<u8>,
    /// The provider accepts an explicit `source_format` on Open. A false
    /// value preserves its original Open schema; it is never sent a new field.
    #[serde(default)]
    pub format_open: bool,
}

impl SourceModeCapabilities {
    pub fn supports(&self, request: &SourcePreflightRequest) -> bool {
        self.source_class == request.source_class
            && (self.min_width..=self.max_width).contains(&request.mode.width)
            && (self.min_height..=self.max_height).contains(&request.mode.height)
            && (self.min_refresh_millihz..=self.max_refresh_millihz)
                .contains(&request.mode.refresh_millihz)
            && request.mode.bit_depth.is_none_or(|depth| {
                self.bit_depths.contains(&depth)
                    || (depth == 8 && self.bit_depths.is_empty() && !request.mode.hdr)
            })
            && if request.mode.hdr { self.hdr } else { self.sdr }
    }

    fn valid(&self) -> bool {
        self.min_width > 0
            && self.min_width <= self.max_width
            && self.max_width <= 8192
            && self.min_height > 0
            && self.min_height <= self.max_height
            && self.max_height <= 8192
            && self.min_refresh_millihz >= 1_000
            && self.min_refresh_millihz <= self.max_refresh_millihz
            && self.max_refresh_millihz <= 1_000_000
            && (self.sdr || self.hdr)
            && self.bit_depths.len() <= 2
            && self.bit_depths.iter().all(|depth| matches!(depth, 8 | 10))
            && (self.bit_depths.len() != 2 || self.bit_depths[0] != self.bit_depths[1])
            && (!self.format_open || !self.bit_depths.is_empty())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceReadiness {
    Ready,
    Unavailable,
    Unsupported,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourcePreflightResponse {
    pub request: SourcePreflightRequest,
    pub state: SourceReadiness,
    pub reason: Option<String>,
    pub capabilities: Vec<SourceModeCapabilities>,
}

impl SourcePreflightResponse {
    pub fn unavailable(request: &SourcePreflightRequest, reason: &str) -> Self {
        Self {
            request: request.clone(),
            state: SourceReadiness::Unavailable,
            reason: Some(reason.into()),
            capabilities: Vec::new(),
        }
    }
    pub fn unsupported(request: &SourcePreflightRequest, reason: &str) -> Self {
        Self {
            state: SourceReadiness::Unsupported,
            ..Self::unavailable(request, reason)
        }
    }
    pub fn ready(
        request: &SourcePreflightRequest,
        capabilities: Vec<SourceModeCapabilities>,
    ) -> Self {
        Self {
            request: request.clone(),
            state: SourceReadiness::Ready,
            reason: None,
            capabilities,
        }
    }
    pub fn validate(&self, expected: &SourcePreflightRequest) -> ModuleResult<()> {
        expected.validate()?;
        if self.request != *expected
            || self.capabilities.len() > MAX_SOURCE_CAPABILITIES
            || self
                .capabilities
                .iter()
                .any(|capability| !capability.valid())
            || self.reason.as_ref().is_some_and(|reason| {
                reason.is_empty()
                    || reason.len() > 128
                    || !reason.bytes().all(|byte| {
                        byte.is_ascii_lowercase()
                            || byte.is_ascii_digit()
                            || matches!(byte, b'_' | b'-' | b'.')
                    })
            })
        {
            return Err("invalid source preflight response".into());
        }
        match self.state {
            SourceReadiness::Ready
                if self.reason.is_none()
                    && self
                        .capabilities
                        .iter()
                        .any(|capability| capability.supports(expected)) =>
            {
                Ok(())
            }
            SourceReadiness::Unavailable | SourceReadiness::Unsupported
                if self.reason.is_some() =>
            {
                Ok(())
            }
            _ => Err("source preflight readiness is inconsistent".into()),
        }
    }
    pub fn is_ready(&self) -> bool {
        self.state == SourceReadiness::Ready
    }
}

/// Public broker request for Video's composition-selected source route. The
/// route key selects no arbitrary native service; Video resolves it from signed settings.
pub fn source_preflight_payload(
    route_key: &str,
    request: &SourcePreflightRequest,
) -> ModuleResult<Value> {
    request.validate()?;
    if route_key.is_empty() || route_key.len() > 128 || route_key.chars().any(char::is_control) {
        return Err("invalid source route key".into());
    }
    Ok(
        json!({"action":"preflight_source","route_key":route_key,"source_class":request.source_class,"route":request.route,"mode":request.mode}),
    )
}

pub fn decode_video_source_preflight(
    payload: &Value,
    expected: &SourcePreflightRequest,
) -> ModuleResult<SourcePreflightResponse> {
    if payload["ok"] != true {
        return Err("source preflight request failed".into());
    }
    if serde_json::to_vec(payload)
        .map_err(|error| error.to_string())?
        .len()
        > MAX_SOURCE_PREFLIGHT_BYTES
    {
        return Err("source preflight response is too large".into());
    }
    let response: SourcePreflightResponse = serde_json::from_value(payload["preflight"].clone())
        .map_err(|_| "invalid source preflight response")?;
    response.validate(expected)?;
    Ok(response)
}

/// One typed binding to the selected Capture family, including its optional
/// complete v1.11 tail. Missing/old providers return an explicit bind error.
pub struct SourcePreflightConnection {
    binding: NativeBinding,
}
impl SourcePreflightConnection {
    /// # Safety
    /// The selected signed provider must obey the Capture native contract and
    /// the runtime must outlive this binding and every synchronous query.
    pub unsafe fn bind(runtime: &RuntimeClient, service: &str) -> ModuleResult<Self> {
        // SAFETY: caller establishes trusted composition and provider/runtime lifetime.
        let binding = unsafe {
            runtime.bind_native_typed(
                service,
                native_contracts::CAPTURE,
                INTERFACE_MAJOR,
                CAPTURE_SOURCE_PREFLIGHT_INTERFACE_MINOR,
                CAPTURE_API_V1_11_SIZE,
            )
        }?;
        Ok(Self { binding })
    }
    pub fn query(
        &self,
        request: &SourcePreflightRequest,
    ) -> Result<SourcePreflightResponse, Status> {
        // SAFETY: construction binds the exact Capture family, full v1.11 table,
        // and retains the runtime/provider through this synchronous call.
        let api =
            unsafe { self.binding.table::<CaptureApiPrefix>() }.map_err(|_| MEDIA_UNSUPPORTED)?;
        unsafe { source_preflight(api, request) }
    }
}

struct Reply {
    bytes: Vec<u8>,
    calls: u8,
    invalid: bool,
}
unsafe extern "C" fn receive(context: *mut c_void, bytes: BorrowedBytes) {
    // SAFETY: the synchronous query retains this exclusive stack-owned sink.
    let reply = unsafe { &mut *context.cast::<Reply>() };
    reply.calls = reply.calls.saturating_add(1);
    if reply.calls != 1 || bytes.len > MAX_SOURCE_PREFLIGHT_BYTES {
        reply.invalid = true;
        return;
    }
    // SAFETY: provider guarantees a live immutable byte borrow until callback return.
    match unsafe { bytes.read() } {
        Ok(bytes) => reply.bytes.extend_from_slice(bytes),
        Err(_) => reply.invalid = true,
    }
}

/// # Safety
/// `api` must be the prefix of an authentic live Capture-family binding with
/// its complete advertised table allocated; it must outlive this query.
pub unsafe fn source_preflight(
    api: &CaptureApiPrefix,
    request: &SourcePreflightRequest,
) -> Result<SourcePreflightResponse, Status> {
    request.validate().map_err(|_| STATUS_INVALID_ARGUMENT)?;
    let header = api.base.header;
    if header.major != INTERFACE_MAJOR
        || header.minor < CAPTURE_SOURCE_PREFLIGHT_INTERFACE_MINOR
        || header.size < CAPTURE_API_V1_11_SIZE
    {
        return Err(MEDIA_UNSUPPORTED);
    }
    let bytes = serde_json::to_vec(request).map_err(|_| STATUS_INVALID_ARGUMENT)?;
    if bytes.len() > MAX_SOURCE_PREFLIGHT_BYTES {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    // SAFETY: caller establishes identity and lifetime; size and minor cover the tail.
    let table = unsafe { &*std::ptr::from_ref(api).cast::<CaptureApiV1_11>() };
    let mut reply = Reply {
        bytes: Vec::new(),
        calls: 0,
        invalid: false,
    };
    let sink = ReplySink {
        context: (&mut reply as *mut Reply).cast(),
        write: receive,
    };
    // SAFETY: input bytes, stack sink and selected binding remain live throughout the callback.
    let status =
        unsafe { (table.source_preflight)(api.base.context, BorrowedBytes::new(&bytes), sink) };
    if status != STATUS_OK {
        return Err(status);
    }
    if reply.invalid || reply.calls != 1 {
        return Err(MEDIA_BACKEND_ERROR);
    }
    let response: SourcePreflightResponse =
        serde_json::from_slice(&reply.bytes).map_err(|_| MEDIA_BACKEND_ERROR)?;
    response
        .validate(request)
        .map_err(|_| MEDIA_BACKEND_ERROR)?;
    Ok(response)
}

#[cfg(test)]
mod tests;
