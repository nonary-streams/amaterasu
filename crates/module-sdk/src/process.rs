//! Experimental provider-selected process SDK. No local spawn fallback is made.
//!
//! A successful submit confirms admission, not process creation. Retain the
//! exact operation ID and request for retries and poll until a terminal result.
//! Releasing an observation cannot retire native process or sink ownership.
use crate::{ModuleResult, NativeBinding, RuntimeClient};
use amaterasu_abi::{BorrowedBytes, NativeInterface, ReplySink, STATUS_OK, Status};
pub use amaterasu_process_api as api;
use std::{
    ffi::c_void,
    sync::{Arc, Mutex},
};

mod provider;
pub use provider::{ProcessFactory, Provider};

struct Inner {
    table: *const api::ProcessApi,
    binding: Option<NativeBinding>,
    admission: Mutex<Admission>,
    capabilities: api::Capabilities,
}
struct Admission {
    epoch: String,
    next: u64,
    prepared: Option<PreparedOperation>,
}
/// Immutable exact identity and payload retained across an admission retry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedOperation {
    operation_id: String,
    request: api::Request,
}
impl PreparedOperation {
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }
    pub fn request(&self) -> &api::Request {
        &self.request
    }
}
// SAFETY: constructors require the process contract's thread-safe callbacks;
// the final Arc releases the binding after all synchronous calls have ended.
unsafe impl Send for Inner {}
// SAFETY: the selected provider promises concurrent callbacks are safe.
unsafe impl Sync for Inner {}
impl Drop for Inner {
    fn drop(&mut self) {
        if self.binding.is_none() {
            // SAFETY: standalone construction transfers the unique table release.
            unsafe {
                let base = (*self.table).base;
                (base.release.expect("validated release"))(base.context);
            }
        }
    }
}

#[derive(Clone)]
pub struct Process {
    inner: Arc<Inner>,
}
impl Process {
    /// # Safety
    /// Trusted composition must bind this exact interface contract and keep the
    /// runtime and provider loaded until all consumer clones and calls retire.
    pub unsafe fn bind(runtime: &RuntimeClient, service: &str) -> ModuleResult<Self> {
        if !amaterasu_contracts::valid_name(service) || !service.ends_with("/1") {
            return Err("invalid process provider service".into());
        }
        // SAFETY: caller guarantees runtime and provider interface lifetimes.
        let binding = unsafe {
            runtime.bind_native_typed(
                service,
                api::CONTRACT,
                api::MAJOR,
                api::MINOR,
                size_of::<api::ProcessApi>() as u32,
            )?
        };
        // SAFETY: the typed native binding is the caller-verified process table.
        let table = unsafe { binding.table::<api::ProcessApi>()? } as *const _;
        Self::new(table, Some(binding))
    }

    /// # Safety
    /// `table` must be a readable complete thread-safe ProcessApi for this
    /// contract, uniquely transferred and valid until release. Provider code and
    /// its process-lifetime ownership must outlive every consumer binding.
    pub unsafe fn from_native_table(table: *const api::ProcessApi) -> ModuleResult<Self> {
        if table.is_null() || !(table as usize).is_multiple_of(align_of::<api::ProcessApi>()) {
            return Err("invalid process table".into());
        }
        // SAFETY: caller guarantees a readable native interface prefix.
        let base = unsafe { &(*table).base };
        if base.header.major != api::MAJOR
            || base.header.size < size_of::<api::ProcessApi>() as u32
            || base.release.is_none()
        {
            if base.header.size >= size_of::<NativeInterface>() as u32
                && let Some(release) = base.release
            {
                // SAFETY: the complete base prefix owns its release callback.
                unsafe { release(base.context) };
            }
            return Err("incompatible process table".into());
        }
        Self::new(table, None)
    }

    fn new(table: *const api::ProcessApi, binding: Option<NativeBinding>) -> ModuleResult<Self> {
        // Create an owned guard before the fallible epoch call, so failure also
        // releases a standalone table or runtime binding exactly once.
        // The guard exists before capability or epoch calls so errors release it.
        let mut inner = Inner {
            table,
            binding,
            admission: Mutex::new(Admission {
                epoch: String::new(),
                next: 1,
                prepared: None,
            }),
            capabilities: api::Capabilities {
                managed_groups: false,
                detached: false,
                shared_output: false,
                elevated: false,
                elevated_output: false,
                max_payload_bytes: 0,
                max_queued_operations: 0,
                max_operation_receipts: 0,
                max_processes: 0,
                max_reservations: 0,
                max_sinks: 0,
                max_epochs: 0,
            },
        };
        // SAFETY: the selected contract owns this synchronous table.
        let capabilities =
            receive_bytes(|sink| unsafe { ((*table).capabilities)((*table).base.context, sink) })
                .map_err(|status| format!("process capabilities failed: {status}"))?;
        let capabilities: api::Capabilities =
            serde_json::from_slice(&capabilities).map_err(|_| "invalid process capabilities")?;
        capabilities
            .validate()
            .map_err(|_| "invalid process capabilities")?;
        // SAFETY: construction established the live synchronous interface.
        let bytes = receive_bytes(|sink| unsafe { ((*table).epoch)((*table).base.context, sink) })
            .map_err(|status| format!("process epoch failed: {status}"))?;
        let epoch: String = serde_json::from_slice(&bytes).map_err(|_| "invalid process epoch")?;
        if !api::valid_operation_id(&format!("{epoch}:1")) {
            return Err("invalid process epoch".into());
        }
        inner
            .admission
            .lock()
            .map_err(|_| "process admission unavailable")?
            .epoch = epoch;
        inner.capabilities = capabilities;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    pub fn capabilities(&self) -> &api::Capabilities {
        &self.inner.capabilities
    }

    /// Retain one immutable operation identity before attempting admission.
    pub fn prepare(&self, request: &api::Request) -> Result<PreparedOperation, Status> {
        if request.validate().is_err() {
            return Err(amaterasu_abi::STATUS_INVALID_ARGUMENT);
        }
        if serde_json::to_vec(request)
            .map_err(|_| amaterasu_abi::STATUS_INVALID_ARGUMENT)?
            .len()
            > self.inner.capabilities.max_payload_bytes as usize
            || !supports(self.capabilities(), request)
        {
            return Err(api::INVALID_STATE);
        }
        let mut admission = self
            .inner
            .admission
            .lock()
            .map_err(|_| amaterasu_abi::STATUS_INTERNAL_ERROR)?;
        if admission.prepared.is_some() {
            return Err(api::INVALID_STATE);
        }
        admission
            .next
            .checked_add(1)
            .ok_or(api::RESOURCE_EXHAUSTED)?;
        let prepared = PreparedOperation {
            operation_id: format!("{}:{}", admission.epoch, admission.next),
            request: request.clone(),
        };
        admission.prepared = Some(prepared.clone());
        Ok(prepared)
    }

    /// Submit the prepared operation. Non-OK proves no work was admitted;
    /// keep this same prepared identity to retry or explicitly discard it.
    /// The provider must not reenter this binding's admission methods while its
    /// synchronous submit callback runs; admission is serialized across clones.
    pub fn submit(&self, prepared: &PreparedOperation) -> Result<(), Status> {
        let bytes = serde_json::to_vec(&prepared.request)
            .map_err(|_| amaterasu_abi::STATUS_INVALID_ARGUMENT)?;
        let mut admission = self
            .inner
            .admission
            .lock()
            .map_err(|_| amaterasu_abi::STATUS_INTERNAL_ERROR)?;
        if admission.prepared.as_ref() != Some(prepared) {
            return Err(api::INVALID_STATE);
        }
        // SAFETY: all buffers are live for the synchronous callback.
        let status = unsafe {
            ((*self.inner.table).submit)(
                (*self.inner.table).base.context,
                BorrowedBytes::new(prepared.operation_id.as_bytes()),
                BorrowedBytes::new(&bytes),
            )
        };
        if status == STATUS_OK {
            admission.next += 1;
            admission.prepared = None;
            Ok(())
        } else {
            Err(status)
        }
    }

    /// Discard only a locally prepared, not-yet-admitted operation.
    pub fn discard_prepared(&self, prepared: &PreparedOperation) -> Result<(), Status> {
        let mut admission = self
            .inner
            .admission
            .lock()
            .map_err(|_| amaterasu_abi::STATUS_INTERNAL_ERROR)?;
        if admission.prepared.as_ref() != Some(prepared) {
            return Err(api::INVALID_STATE);
        }
        admission.prepared = None;
        Ok(())
    }

    pub fn poll(&self, operation_id: &str) -> Result<api::Response, Status> {
        if !api::valid_operation_id(operation_id) {
            return Err(amaterasu_abi::STATUS_INVALID_ARGUMENT);
        }
        // SAFETY: operation bytes and sink remain valid until callback returns.
        receive(|sink| unsafe {
            ((*self.inner.table).poll)(
                (*self.inner.table).base.context,
                BorrowedBytes::new(operation_id.as_bytes()),
                sink,
            )
        })
    }

    /// Read cached process facts without allocating a new operation identity.
    pub fn inspect(&self, process: &api::ProcessToken) -> Result<api::Response, Status> {
        if !api::valid_identity(&process.0) {
            return Err(amaterasu_abi::STATUS_INVALID_ARGUMENT);
        }
        // SAFETY: token and reply sink are borrowed only synchronously.
        receive(|sink| unsafe {
            ((*self.inner.table).inspect)(
                (*self.inner.table).base.context,
                BorrowedBytes::new(process.0.as_bytes()),
                sink,
            )
        })
    }

    pub fn release_operation(&self, operation_id: &str) -> Result<(), Status> {
        if !api::valid_operation_id(operation_id) {
            return Err(amaterasu_abi::STATUS_INVALID_ARGUMENT);
        }
        // SAFETY: input is borrowed only during this synchronous call.
        let status = unsafe {
            ((*self.inner.table).release_operation)(
                (*self.inner.table).base.context,
                BorrowedBytes::new(operation_id.as_bytes()),
            )
        };
        if status == STATUS_OK {
            Ok(())
        } else {
            Err(status)
        }
    }
}

fn supports(capabilities: &api::Capabilities, request: &api::Request) -> bool {
    let kind = |kind| match kind {
        api::ProcessKind::Managed => capabilities.managed_groups,
        api::ProcessKind::Detached => capabilities.detached,
    };
    match request {
        api::Request::Reserve { kind: value } => kind(*value),
        api::Request::OpenSink { .. } | api::Request::CloseSink { .. } => {
            capabilities.shared_output
        }
        api::Request::Spawn {
            kind: value,
            command,
            ..
        } => {
            let output = matches!(command.stdout, api::OutputTarget::Sink { .. })
                || matches!(command.stderr, api::OutputTarget::Sink { .. });
            kind(*value)
                && (!output || capabilities.shared_output)
                && (!command.elevated || capabilities.elevated)
                && (!(command.elevated && output) || capabilities.elevated_output)
        }
        api::Request::StopManaged { .. } | api::Request::ForceManaged { .. } => {
            capabilities.managed_groups
        }
        _ => true,
    }
}

fn receive(call: impl FnOnce(ReplySink) -> Status) -> Result<api::Response, Status> {
    let bytes = receive_bytes(call)?;
    let response: api::Response =
        serde_json::from_slice(&bytes).map_err(|_| amaterasu_abi::STATUS_INTERNAL_ERROR)?;
    response
        .validate()
        .map_err(|_| amaterasu_abi::STATUS_INTERNAL_ERROR)?;
    Ok(response)
}

fn receive_bytes(call: impl FnOnce(ReplySink) -> Status) -> Result<Vec<u8>, Status> {
    struct Output {
        bytes: Vec<u8>,
        valid: bool,
        calls: usize,
    }
    unsafe extern "C" fn copy(context: *mut c_void, bytes: BorrowedBytes) {
        // SAFETY: the synchronous caller lends an exclusive Output pointer.
        let output = unsafe { &mut *context.cast::<Output>() };
        output.calls += 1;
        if output.calls != 1 || bytes.len > api::MAX_PAYLOAD_BYTES {
            output.valid = false;
            return;
        }
        // SAFETY: provider lends readable bytes for this callback only.
        match unsafe { bytes.read() } {
            Ok(bytes) => output.bytes.extend_from_slice(bytes),
            Err(_) => output.valid = false,
        }
    }
    let mut output = Output {
        bytes: Vec::new(),
        valid: true,
        calls: 0,
    };
    let sink = ReplySink {
        context: (&mut output as *mut Output).cast(),
        write: copy,
    };
    let status = call(sink);
    if status != STATUS_OK {
        return Err(status);
    }
    if !output.valid || output.calls != 1 {
        return Err(amaterasu_abi::STATUS_INTERNAL_ERROR);
    }
    Ok(output.bytes)
}
