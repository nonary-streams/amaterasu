//! Draft C ABI. No Rust allocator, trait object, or collection crosses a boundary.
//! All calls are synchronous transport operations; feature work is queued by the runtime.
use std::ffi::c_void;

pub const ABI_MAJOR: u32 = 1;
pub const ABI_MINOR: u32 = 0;
pub const MAX_MESSAGE: usize = 1_048_576;
pub type Status = i32;
pub const STATUS_OK: Status = 0;
pub const STATUS_INVALID_ARGUMENT: Status = 1;
pub const STATUS_INTERNAL_ERROR: Status = 2;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AbiHeader {
    pub size: u32,
    pub major: u32,
    pub minor: u32,
}
impl AbiHeader {
    pub const fn of<T>() -> Self {
        Self {
            size: size_of::<T>() as u32,
            major: ABI_MAJOR,
            minor: ABI_MINOR,
        }
    }
    pub fn supports<T>(&self) -> bool {
        self.major == ABI_MAJOR && self.size as usize >= size_of::<T>()
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BorrowedBytes {
    pub ptr: *const u8,
    pub len: usize,
}
impl BorrowedBytes {
    pub fn new(bytes: &[u8]) -> Self {
        Self {
            ptr: bytes.as_ptr(),
            len: bytes.len(),
        }
    }
    /// # Safety
    /// The producer must supply a readable allocation lasting for this call.
    pub unsafe fn read<'a>(self) -> Result<&'a [u8], Status> {
        if self.len > MAX_MESSAGE || (self.ptr.is_null() && self.len != 0) {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        if self.len == 0 {
            return Ok(&[]);
        }
        Ok(unsafe { std::slice::from_raw_parts(self.ptr, self.len) })
    }
}

/// Borrowed reply bytes are valid only inside the callback. The caller copies them.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ReplySink {
    pub context: *mut c_void,
    pub write: unsafe extern "C" fn(*mut c_void, BorrowedBytes),
}
impl ReplySink {
    /// # Safety
    /// The callback and context must remain valid until the operation returns.
    pub unsafe fn send(self, bytes: &[u8]) {
        unsafe { (self.write)(self.context, BorrowedBytes::new(bytes)) };
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct HostApi {
    pub header: AbiHeader,
    /// An endpoint identity created by the runtime, never supplied in message JSON.
    pub context: *mut c_void,
    pub invoke: unsafe extern "C" fn(*mut c_void, BorrowedBytes, ReplySink) -> Status,
}
impl HostApi {
    /// # Safety
    /// This table and its runtime endpoint must remain alive for the call. Do not use after shutdown.
    pub unsafe fn call(&self, input: &[u8]) -> Result<Vec<u8>, Status> {
        unsafe extern "C" fn copy(context: *mut c_void, bytes: BorrowedBytes) {
            if let Ok(bytes) = unsafe { bytes.read() } {
                unsafe { &mut *(context as *mut Vec<u8>) }.extend_from_slice(bytes);
            }
        }
        if !self.header.supports::<Self>() || input.len() > MAX_MESSAGE {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let mut output = Vec::new();
        let reply = ReplySink {
            context: &mut output as *mut _ as *mut c_void,
            write: copy,
        };
        let result = unsafe { (self.invoke)(self.context, BorrowedBytes::new(input), reply) };
        if result == STATUS_OK {
            Ok(output)
        } else {
            Err(result)
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ModuleApi {
    pub header: AbiHeader,
    pub create: unsafe extern "C" fn(*const HostApi, *mut *mut c_void) -> Status,
    /// Must be bounded and nonblocking. The external supervisor enforces process health.
    pub tick: unsafe extern "C" fn(*mut c_void) -> Status,
    pub health: unsafe extern "C" fn(*mut c_void) -> Status,
    /// Joins all module-owned threads and releases callbacks before returning.
    pub destroy: unsafe extern "C" fn(*mut c_void),
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RuntimeApi {
    pub header: AbiHeader,
    pub create: unsafe extern "C" fn(BorrowedBytes, *mut *mut c_void) -> Status,
    /// Restricted bootstrap call. Returns only identities configured at runtime creation.
    pub endpoint: unsafe extern "C" fn(*mut c_void, BorrowedBytes, *mut HostApi) -> Status,
    pub destroy: unsafe extern "C" fn(*mut c_void),
}

pub type ModuleEntry = unsafe extern "C" fn() -> *const ModuleApi;
pub type RuntimeEntry = unsafe extern "C" fn() -> *const RuntimeApi;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versions_and_sizes_are_checked() {
        let mut header = AbiHeader::of::<HostApi>();
        assert!(header.supports::<HostApi>());
        header.major += 1;
        assert!(!header.supports::<HostApi>());
        header.major = ABI_MAJOR;
        header.size = 0;
        assert!(!header.supports::<HostApi>());
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn rust_layout_matches_the_public_c_header() {
        assert_eq!(size_of::<AbiHeader>(), 12);
        assert_eq!(size_of::<BorrowedBytes>(), 16);
        assert_eq!(size_of::<ReplySink>(), 16);
        assert_eq!(size_of::<HostApi>(), 32);
        assert_eq!(size_of::<ModuleApi>(), 48);
        assert_eq!(size_of::<RuntimeApi>(), 40);
        assert_eq!(size_of::<NativeInterface>(), 32);
        assert_eq!(size_of::<NativeInterfaceQuery>(), 24);
        assert_eq!(size_of::<NativeServiceFactory>(), 32);
        assert_eq!(std::mem::offset_of!(NativeInterface, context), 16);
        assert_eq!(std::mem::offset_of!(NativeServiceFactory, bind), 24);
        assert_eq!(std::mem::offset_of!(ModuleApi, create), 16);
    }
}

/// Common prefix of a provider-owned native interface table. `header` describes
/// the complete product table, using that interface's own major/minor version.
/// The table and its context remain live until `release(context)` returns.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NativeInterface {
    pub header: AbiHeader,
    pub context: *mut c_void,
    pub release: Option<unsafe extern "C" fn(*mut c_void)>,
}

/// A requested interface layout. Its header uses the generic Amaterasu ABI version.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NativeInterfaceQuery {
    pub header: AbiHeader,
    pub interface_major: u32,
    pub min_minor: u32,
    pub min_size: u32,
}

/// Registration descriptor borrowed for one register call; the runtime copies it.
/// Its context and callback code must remain live until all consumers stop.
/// `bind` borrows the authenticated endpoint principal for the duration of the call;
/// the provider must copy it into the new context if needed after return. On success
/// output transfers one binding to the runtime. On failure output must remain null.
/// Callbacks must not unwind, and must synchronize any shared state themselves.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NativeServiceFactory {
    pub header: AbiHeader,
    pub context: *mut c_void,
    pub bind: Option<
        unsafe extern "C" fn(
            *mut c_void,
            BorrowedBytes,
            *const NativeInterfaceQuery,
            *mut *const NativeInterface,
        ) -> Status,
    >,
}
