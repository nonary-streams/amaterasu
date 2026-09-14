//! Explicitly unsafe bindings to provider-owned C interface tables.
use crate::{Command, ModuleResult, Response, RuntimeClient, abi};
use std::ffi::c_void;

/// One provider binding owned by this consumer. Drop releases through the bound
/// runtime endpoint, never through a caller-supplied identity. This value is not
/// Send/Sync: product interfaces must explicitly define their threading contracts.
/// Keep it alive while using any table reference, callback, buffer, or child object.
pub struct NativeBinding {
    endpoint: abi::HostApi,
    id: u64,
    interface: *const abi::NativeInterface,
}

impl NativeBinding {
    pub fn context(&self) -> *mut c_void {
        // SAFETY: bind checked the prefix and the provider keeps it live until Drop.
        unsafe { (*self.interface).context }
    }

    /// # Safety
    /// T must be the exact repr(C) table for the requested service/version, beginning
    /// with NativeInterface. Size/alignment checks do not prove field types, function
    /// signatures or protocol compatibility. Follow every callback's ownership and
    /// concurrency rules; references and asynchronous work must not outlive self.
    pub unsafe fn table<T>(&self) -> ModuleResult<&T> {
        // SAFETY: this live binding owns a validated prefix.
        let size = unsafe { (*self.interface).header.size as usize };
        if size < size_of::<T>() || !(self.interface as usize).is_multiple_of(align_of::<T>()) {
            return Err("native interface layout mismatch".into());
        }
        // SAFETY: caller guarantees T is the interface's exact advertised layout.
        Ok(unsafe { &*self.interface.cast::<T>() })
    }
}

impl Drop for NativeBinding {
    fn drop(&mut self) {
        // SAFETY: bind's contract requires the runtime/provider to remain alive until
        // this binding is dropped, after all operations and borrowed buffers finish.
        let runtime = unsafe { RuntimeClient::from_endpoint(self.endpoint) };
        let _ = runtime.send_native(Command::NativeRelease { binding: self.id });
    }
}

impl RuntimeClient {
    /// # Safety
    /// The factory descriptor is borrowed only during registration and copied by the
    /// runtime. Its context and callbacks must remain valid until all consumers are
    /// destroyed and runtime calls have stopped. Callbacks cannot unwind.
    /// The factory must honor NativeServiceFactory's output and principal contracts.
    /// Register once during provider start; consumer destruction precedes provider
    /// destruction through signed package dependencies. Grants are not a sandbox.
    pub unsafe fn register_native(
        &self,
        service: &str,
        factory: &abi::NativeServiceFactory,
    ) -> ModuleResult<()> {
        match self.send_native(Command::NativeRegister {
            service: service.into(),
            factory: factory as *const _ as usize,
        })? {
            Response::Ok => Ok(()),
            response => Err(format!("native registration failed: {response:?}")),
        }
    }

    /// # Safety
    /// Runtime and provider DLL must outlive the returned binding and every callback.
    /// Consumer code must obey the product interface's C layout, ownership and thread
    /// rules. Release all buffers/child objects before dropping the binding. The host
    /// must stop consumer modules before their providers. No Rust ABI crosses tables.
    pub unsafe fn bind_native(
        &self,
        service: &str,
        interface_major: u32,
        min_minor: u32,
        min_size: u32,
    ) -> ModuleResult<NativeBinding> {
        match self.send_native(Command::NativeBind {
            service: service.into(),
            interface_major,
            min_minor,
            min_size,
        })? {
            Response::NativeBound { binding, address } => {
                if address == 0 || !address.is_multiple_of(align_of::<abi::NativeInterface>()) {
                    let _ = self.send_native(Command::NativeRelease { binding });
                    return Err("runtime returned invalid native binding".into());
                }
                Ok(NativeBinding {
                    endpoint: self.endpoint,
                    id: binding,
                    interface: address as *const _,
                })
            }
            response => Err(format!("native binding failed: {response:?}")),
        }
    }
}
