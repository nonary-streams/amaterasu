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

    /// Registers an interface-family identity independently of the service alias.
    ///
    /// # Safety
    /// The factory must implement the exact contract's C layout, concurrency,
    /// ownership and callbacks. All register_native lifetime rules also apply.
    pub unsafe fn register_native_typed(
        &self,
        service: &str,
        contract: &str,
        factory: &abi::NativeServiceFactory,
    ) -> ModuleResult<()> {
        match self.send_native(Command::NativeRegisterTyped {
            service: service.into(),
            factory: factory as *const _ as usize,
            contract: contract.into(),
        })? {
            Response::Ok => Ok(()),
            response => Err(format!("typed native registration failed: {response:?}")),
        }
    }

    /// Binds only a factory registered for this exact interface family. The
    /// runtime rejects mismatches before calling the provider's factory.
    ///
    /// # Safety
    /// All bind_native lifetime and native contract requirements apply. Contract
    /// names identify trusted provider promises; they do not sandbox native code.
    pub unsafe fn bind_native_typed(
        &self,
        service: &str,
        contract: &str,
        interface_major: u32,
        min_minor: u32,
        min_size: u32,
    ) -> ModuleResult<NativeBinding> {
        self.bind_native_command(Command::NativeBindTyped {
            service: service.into(),
            contract: contract.into(),
            interface_major,
            min_minor,
            min_size,
        })
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
        self.bind_native_command(Command::NativeBind {
            service: service.into(),
            interface_major,
            min_minor,
            min_size,
        })
    }

    fn bind_native_command(&self, command: Command) -> ModuleResult<NativeBinding> {
        match self.send_native(command)? {
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
