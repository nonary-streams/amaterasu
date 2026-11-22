//! Versioned router mapping provider for optional listener exposure.
use crate::abi::{BorrowedBytes, NativeInterface, ReplySink, STATUS_OK};
use crate::{ModuleResult, NativeBinding, RuntimeClient};
use serde::{Deserialize, Serialize};
use std::ffi::c_void;
use std::net::IpAddr;

pub const SERVICE: &str = "amaterasu.router-mapping/1";
pub const CONTRACT: &str = "amaterasu.router-mapping-interface/1";
pub const MAJOR: u32 = 1;
pub const MINOR: u32 = 0;
pub const MAX_ENDPOINTS: usize = 64;
pub const MAX_REQUEST_BYTES: usize = 16 * 1024;
pub const MAX_REPLY_BYTES: usize = 2048;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    Tcp,
    Udp,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MappingEndpoint {
    pub address: IpAddr,
    pub port: u16,
    pub protocol: Protocol,
    pub active: bool,
}

impl MappingEndpoint {
    pub fn new(address: IpAddr, port: u16, protocol: Protocol, active: bool) -> Self {
        Self {
            address,
            port,
            protocol,
            active,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DesiredMappings {
    pub enabled: bool,
    pub ipv6_enabled: bool,
    pub endpoints: Vec<MappingEndpoint>,
}

impl DesiredMappings {
    pub fn validate(&self) -> ModuleResult<()> {
        if self.endpoints.len() > MAX_ENDPOINTS
            || self.endpoints.iter().any(|endpoint| endpoint.port == 0)
        {
            return Err("invalid router mapping endpoints".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub status: String,
    pub error: Option<String>,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            status: "disabled".into(),
            error: None,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Update { desired: DesiredMappings },
    Status,
    PrepareShutdown { remaining_ms: u64 },
    PollShutdown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    Accepted,
    Status { status: Status },
    Shutdown { ready: bool, error: Option<String> },
}

/// One live mapping lease per binding. Callbacks run synchronously on the host
/// thread and must never perform network I/O or join a live worker. Release
/// retires this binding and must finish its worker before returning.
#[repr(C)]
pub struct RouterMappingApi {
    pub base: NativeInterface,
    pub request:
        Option<unsafe extern "C" fn(*mut c_void, BorrowedBytes, ReplySink) -> crate::abi::Status>,
}

pub struct RouterMapping {
    binding: NativeBinding,
    desired: Option<DesiredMappings>,
}

impl RouterMapping {
    pub fn bind(runtime: &RuntimeClient, service: &str) -> ModuleResult<Self> {
        if !crate::network::valid_provider(service) {
            return Err("invalid router mapping provider service".into());
        }
        // SAFETY: the runtime requires the exact registered contract and signed
        // dependencies keep the provider live through the returned binding.
        let binding = unsafe {
            runtime.bind_native_typed(
                service,
                CONTRACT,
                MAJOR,
                MINOR,
                size_of::<RouterMappingApi>() as u32,
            )?
        };
        Ok(Self {
            binding,
            desired: None,
        })
    }

    pub fn update(&mut self, desired: &DesiredMappings) -> ModuleResult<Status> {
        if self.desired.as_ref() != Some(desired) {
            desired.validate()?;
            match self.request(Request::Update {
                desired: desired.clone(),
            })? {
                Response::Status { status } => {
                    self.desired = Some(desired.clone());
                    return Ok(status);
                }
                _ => return Err("unexpected router mapping update result".into()),
            }
        }
        match self.request(Request::Status)? {
            Response::Status { status } => Ok(status),
            _ => Err("unexpected router mapping status result".into()),
        }
    }

    pub fn prepare_shutdown(&mut self, remaining_ms: u64) -> ModuleResult<()> {
        match self.request(Request::PrepareShutdown { remaining_ms })? {
            Response::Accepted => Ok(()),
            _ => Err("unexpected router mapping shutdown acknowledgement".into()),
        }
    }

    pub fn poll_shutdown(&mut self) -> ModuleResult<bool> {
        match self.request(Request::PollShutdown)? {
            Response::Shutdown { ready, error: None } => Ok(ready),
            Response::Shutdown {
                error: Some(error), ..
            } => Err(error),
            _ => Err("unexpected router mapping shutdown result".into()),
        }
    }

    fn request(&mut self, request: Request) -> ModuleResult<Response> {
        let request = serde_json::to_vec(&request).map_err(|error| error.to_string())?;
        if request.len() > MAX_REQUEST_BYTES {
            return Err("router mapping request exceeds limit".into());
        }
        let mut response: Option<Vec<u8>> = None;
        let sink = ReplySink {
            context: (&mut response as *mut Option<Vec<u8>>).cast(),
            write: collect_response,
        };
        // SAFETY: the binding validated the exact interface. The borrowed
        // payload and reply sink outlive the synchronous provider call.
        let status = unsafe {
            let table = self.binding.table::<RouterMappingApi>()?;
            let request_callback = table.request.ok_or("router mapping callback unavailable")?;
            request_callback(self.binding.context(), BorrowedBytes::new(&request), sink)
        };
        if status != STATUS_OK {
            return Err("router mapping provider rejected request".into());
        }
        serde_json::from_slice(&response.ok_or("router mapping reply missing")?)
            .map_err(|_| "invalid router mapping reply".into())
    }
}

unsafe extern "C" fn collect_response(context: *mut c_void, payload: BorrowedBytes) {
    if payload.len <= MAX_REPLY_BYTES {
        // SAFETY: the local response slot is exclusively borrowed by the
        // callback, and provider bytes remain valid until this callback returns.
        if let Ok(bytes) = unsafe { payload.read() } {
            unsafe { *context.cast::<Option<Vec<u8>>>() = Some(bytes.to_vec()) };
        }
    }
}
