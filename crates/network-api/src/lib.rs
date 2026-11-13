//! Versioned provider-neutral network operations. No OS socket or Rust allocator
//! crosses this boundary. Addresses are values; handles belong to one binding.
use amaterasu_abi::{BorrowedBytes, NativeInterface, Status};
use std::ffi::c_void;
pub const MAJOR: u32 = 1;
pub const MINOR: u32 = 0;
pub const SERVICE: &str = "amaterasu.network/1";
/// Immutable interface-family identity, independent of the selected service alias.
pub const CONTRACT: &str = "amaterasu.network-interface/1";
pub const TCP: u64 = 1;
pub const UDP: u64 = 2;
pub const DNS: u64 = 4;
pub const BATCH: u64 = 8;
pub const MULTICAST: u64 = 16;
pub const INTERFACES: u64 = 32;
pub const MAX_ADDRESSES: usize = 256;
pub const MAX_BATCH: usize = 256;
pub const MAX_IO_BYTES: usize = 64 * 1024 * 1024;
pub const WOULD_BLOCK: Status = 100;
pub const TIMED_OUT: Status = 101;
pub const INTERRUPTED: Status = 102;
pub const CONNECTION_REFUSED: Status = 103;
pub const CONNECTION_RESET: Status = 104;
pub const CONNECTION_ABORTED: Status = 105;
pub const NOT_CONNECTED: Status = 106;
pub const ADDRESS_IN_USE: Status = 107;
pub const ADDRESS_UNAVAILABLE: Status = 108;
pub const PERMISSION_DENIED: Status = 109;
pub const UNSUPPORTED: Status = 110;
pub const NOT_FOUND: Status = 111;
pub const BROKEN_PIPE: Status = 112;
pub const INVALID_DATA: Status = 113;
pub const RESOURCE_EXHAUSTED: Status = 114;
pub const IO_ERROR: Status = 115;
pub const HOST_UNREACHABLE: Status = 116;
pub const NETWORK_UNREACHABLE: Status = 117;
pub const NONBLOCKING: u32 = 1;
pub const READ_TIMEOUT_MS: u32 = 2;
pub const WRITE_TIMEOUT_MS: u32 = 3;
pub const NODELAY: u32 = 4;
pub const BROADCAST: u32 = 5;
pub const TTL: u32 = 6;
pub const MULTICAST_IF_V6: u32 = 7;
pub const MULTICAST_HOPS_V6: u32 = 8;
pub const MULTICAST_LOOP_V4: u32 = 9;
pub const MULTICAST_LOOP_V6: u32 = 10;
pub const MULTICAST_TTL_V4: u32 = 11;
pub const IPV6_ONLY: u32 = 12;
pub const SOCKET_TOS: u32 = 13;
pub const SHUTDOWN_READ: u32 = 1;
pub const SHUTDOWN_WRITE: u32 = 2;
pub const SHUTDOWN_BOTH: u32 = 3;
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Address {
    pub family: u32,
    pub port: u16,
    pub reserved: u16,
    pub scope_id: u32,
    pub flowinfo: u32,
    pub bytes: [u8; 16],
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct InterfaceAddress {
    pub address: Address,
    pub interface_index: u32,
    pub reserved: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Capabilities {
    pub size: u32,
    pub version: u32,
    pub features: u64,
    pub max_io_bytes: u64,
    pub max_handles: u32,
    pub max_batch: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ListenOptions {
    pub size: u32,
    pub flags: u32,
    pub backlog: u32,
    pub reserved: u32,
}
pub const LISTEN_NONBLOCKING: u32 = 1;
pub const LISTEN_REUSE_ADDRESS: u32 = 2;
pub const LISTEN_IPV6_ONLY: u32 = 4;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Datagram {
    pub data: *const u8,
    pub len: usize,
}
/// All callbacks are thread-safe, cannot unwind, and use principal-bound handles.
/// Byte buffers are synchronous borrows. Read/write accept up to max_io_bytes;
/// TCP may return a partial count. UDP preserves whole datagram boundaries.
/// WouldBlock accepts no bytes. Batch returns the accepted prefix; errors with
/// a nonzero prefix are represented by success + count, so retry only the tail.
/// Calls with explicit timeouts run on consumer workers, never a host tick.
/// Factory release follows all concurrent calls and closes all binding handles.
#[repr(C)]
pub struct NetworkApi {
    pub base: NativeInterface,
    pub capabilities: unsafe extern "C" fn(*mut c_void, *mut Capabilities) -> Status,
    pub resolve: unsafe extern "C" fn(
        *mut c_void,
        BorrowedBytes,
        u16,
        u32,
        *mut Address,
        usize,
        *mut usize,
    ) -> Status,
    pub interfaces:
        unsafe extern "C" fn(*mut c_void, *mut InterfaceAddress, usize, *mut usize) -> Status,
    pub tcp_listen: unsafe extern "C" fn(*mut c_void, Address, ListenOptions, *mut u64) -> Status,
    /// Returns a blocking outbound stream. The timeout bounds connection setup;
    /// consumers set read/write timeouts or explicitly enable NONBLOCKING for IO.
    pub tcp_connect: unsafe extern "C" fn(*mut c_void, Address, u32, *mut u64) -> Status,
    pub udp_bind: unsafe extern "C" fn(*mut c_void, Address, *mut u64) -> Status,
    /// Accepted streams are nonblocking regardless of listener mode.
    pub accept: unsafe extern "C" fn(*mut c_void, u64, *mut u64, *mut Address) -> Status,
    pub local_addr: unsafe extern "C" fn(*mut c_void, u64, *mut Address) -> Status,
    pub peer_addr: unsafe extern "C" fn(*mut c_void, u64, *mut Address) -> Status,
    pub read: unsafe extern "C" fn(*mut c_void, u64, *mut u8, usize, *mut usize) -> Status,
    pub peek: unsafe extern "C" fn(*mut c_void, u64, *mut u8, usize, *mut usize) -> Status,
    pub write: unsafe extern "C" fn(*mut c_void, u64, *const u8, usize, *mut usize) -> Status,
    pub recv_from:
        unsafe extern "C" fn(*mut c_void, u64, *mut u8, usize, *mut usize, *mut Address) -> Status,
    pub send_to:
        unsafe extern "C" fn(*mut c_void, u64, *const u8, usize, Address, *mut usize) -> Status,
    pub connect_udp: unsafe extern "C" fn(*mut c_void, u64, Address) -> Status,
    pub set_option: unsafe extern "C" fn(*mut c_void, u64, u32, u64) -> Status,
    pub get_option: unsafe extern "C" fn(*mut c_void, u64, u32, *mut u64) -> Status,
    pub join_multicast: unsafe extern "C" fn(*mut c_void, u64, Address, u32, bool) -> Status,
    pub send_batch:
        unsafe extern "C" fn(*mut c_void, u64, *const Datagram, usize, *mut usize) -> Status,
    pub shutdown: unsafe extern "C" fn(*mut c_void, u64, u32) -> Status,
    pub close: unsafe extern "C" fn(*mut c_void, u64) -> Status,
}

/// Optional v1.1 tail. The v1 prefix remains valid for replacement providers.
pub const DIAGNOSTICS_MINOR: u32 = 1;
pub const DATAGRAM_DIAGNOSTICS: u64 = 64;
pub const KERNEL_RESIDENCE_REALTIME: u64 = 1;
pub const SOCKET_MEMORY: u64 = 2;
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ReceiveTiming {
    pub size: u32,
    pub version: u32,
    pub flags: u64,
    /// Kernel receive timestamp to userspace return, correlated in CLOCK_REALTIME.
    /// Unknown if timestamping is unavailable or the clock moved backwards.
    pub kernel_residence_us: u64,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SocketDiagnostics {
    pub size: u32,
    pub version: u32,
    pub flags: u64,
    /// OS memory charged to the receive queue; includes packet overhead.
    pub receive_memory_bytes: u64,
    pub receive_buffer_bytes: u64,
    pub send_memory_bytes: u64,
    pub send_buffer_bytes: u64,
    pub receive_drops: u64,
}
#[repr(C)]
pub struct NetworkApiV1_1 {
    pub v1: NetworkApi,
    pub recv_from_timed: unsafe extern "C" fn(
        *mut c_void,
        u64,
        *mut u8,
        usize,
        *mut usize,
        *mut Address,
        *mut ReceiveTiming,
    ) -> Status,
    pub socket_diagnostics:
        unsafe extern "C" fn(*mut c_void, u64, *mut SocketDiagnostics) -> Status,
}
/// Optional v1.2 tail for protocols that share a UDP port, such as mDNS.
pub const SHARED_UDP_MINOR: u32 = 2;
pub const SHARED_UDP: u64 = 128;
#[repr(C)]
pub struct NetworkApiV1_2 {
    pub v1_1: NetworkApiV1_1,
    pub udp_bind_shared: unsafe extern "C" fn(*mut c_void, Address, *mut u64) -> Status,
}
impl std::ops::Deref for NetworkApiV1_2 {
    type Target = NetworkApiV1_1;
    fn deref(&self) -> &Self::Target {
        &self.v1_1
    }
}
impl std::ops::DerefMut for NetworkApiV1_2 {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.v1_1
    }
}
impl std::ops::Deref for NetworkApiV1_1 {
    type Target = NetworkApi;
    fn deref(&self) -> &Self::Target {
        &self.v1
    }
}
impl std::ops::DerefMut for NetworkApiV1_1 {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.v1
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;

    #[test]
    fn public_c_layout_matches_supported_host_abi() {
        assert_eq!(size_of::<bool>(), 1);
        assert_eq!(size_of::<Address>(), 32);
        assert_eq!(std::mem::offset_of!(Address, port), 4);
        assert_eq!(std::mem::offset_of!(Address, scope_id), 8);
        assert_eq!(std::mem::offset_of!(Address, bytes), 16);
        assert_eq!(size_of::<InterfaceAddress>(), 40);
        assert_eq!(size_of::<Capabilities>(), 32);
        assert_eq!(std::mem::offset_of!(Capabilities, features), 8);
        assert_eq!(std::mem::offset_of!(Capabilities, max_io_bytes), 16);
        assert_eq!(size_of::<ListenOptions>(), 16);
        assert_eq!(size_of::<Datagram>(), 16);
        assert_eq!(size_of::<NetworkApi>(), 200);
        assert_eq!(size_of::<ReceiveTiming>(), 24);
        assert_eq!(size_of::<SocketDiagnostics>(), 56);
        assert_eq!(size_of::<NetworkApiV1_1>(), 216);
        assert_eq!(std::mem::offset_of!(NetworkApiV1_1, recv_from_timed), 200);
        assert_eq!(size_of::<NetworkApiV1_2>(), 224);
        assert_eq!(std::mem::offset_of!(NetworkApiV1_2, udp_bind_shared), 216);
        assert_eq!(std::mem::offset_of!(NetworkApi, capabilities), 32);
        assert_eq!(std::mem::offset_of!(NetworkApi, tcp_listen), 56);
        assert_eq!(std::mem::offset_of!(NetworkApi, read), 104);
        assert_eq!(std::mem::offset_of!(NetworkApi, send_batch), 176);
        assert_eq!(std::mem::offset_of!(NetworkApi, close), 192);
    }
}
