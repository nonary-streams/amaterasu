//! Platform name service runs only inside the disposable resolver helper.
use std::{
    ffi::{CStr, c_char, c_int, c_void},
    io,
    net::IpAddr,
};

#[cfg(any(unix, windows))]
pub fn lookup(address: IpAddr) -> io::Result<String> {
    #[cfg(windows)]
    type SocketLength = c_int;
    #[cfg(unix)]
    type SocketLength = u32;
    #[cfg_attr(windows, link(name = "ws2_32"))]
    unsafe extern "system" {
        fn getnameinfo(
            address: *const c_void,
            length: SocketLength,
            host: *mut c_char,
            host_length: u32,
            service: *mut c_char,
            service_length: u32,
            flags: c_int,
        ) -> c_int;
    }
    // std initializes Winsock for the process before name-service use.
    #[cfg(windows)]
    let _winsock = std::net::UdpSocket::bind("127.0.0.1:0")?;
    #[repr(C)]
    struct AddressV4 {
        family: u16,
        port: u16,
        ip: u32,
        padding: [u8; 8],
    }
    #[repr(C)]
    struct AddressV6 {
        family: u16,
        port: u16,
        flow: u32,
        ip: [u8; 16],
        scope: u32,
    }
    fn family(value: u16, length: u8) -> u16 {
        // BSD sockaddr begins with length and a one-byte address family.
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        ))]
        {
            u16::from_ne_bytes([length, value as u8])
        }
        #[cfg(not(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        )))]
        {
            let _ = length;
            value
        }
    }
    #[cfg(windows)]
    const AF_INET6: u16 = 23;
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    const AF_INET6: u16 = 30;
    #[cfg(any(target_os = "freebsd", target_os = "dragonfly"))]
    const AF_INET6: u16 = 28;
    #[cfg(any(target_os = "openbsd", target_os = "netbsd"))]
    const AF_INET6: u16 = 24;
    #[cfg(not(any(
        windows,
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    )))]
    const AF_INET6: u16 = 10;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    const NAME_REQUIRED: c_int = 8;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    const NAME_REQUIRED: c_int = 4;
    let mut host = [0 as c_char; 1025];
    let result = match address {
        IpAddr::V4(ip) => {
            let address = AddressV4 {
                family: family(2, 16),
                port: 0,
                ip: u32::from_ne_bytes(ip.octets()),
                padding: [0; 8],
            };
            // SAFETY: the platform sockaddr layout and its exact length match
            // AF_INET. Both buffers live throughout the synchronous libc call.
            unsafe {
                getnameinfo(
                    std::ptr::from_ref(&address).cast(),
                    size_of::<AddressV4>() as SocketLength,
                    host.as_mut_ptr(),
                    host.len() as u32,
                    std::ptr::null_mut(),
                    0,
                    NAME_REQUIRED,
                )
            }
        }
        IpAddr::V6(ip) => {
            let address = AddressV6 {
                family: family(AF_INET6, 28),
                port: 0,
                flow: 0,
                ip: ip.octets(),
                scope: 0,
            };
            // SAFETY: the platform sockaddr layout and its exact length match
            // AF_INET6. No borrowed pointer escapes this helper call.
            unsafe {
                getnameinfo(
                    std::ptr::from_ref(&address).cast(),
                    size_of::<AddressV6>() as SocketLength,
                    host.as_mut_ptr(),
                    host.len() as u32,
                    std::ptr::null_mut(),
                    0,
                    NAME_REQUIRED,
                )
            }
        }
    };
    if result != 0 {
        return Err(io::ErrorKind::NotFound.into());
    }
    // SAFETY: successful getnameinfo writes a terminated name into host. Its
    // final byte was also initialized to zero, bounding the scan to this buffer.
    let hostname = unsafe { CStr::from_ptr(host.as_ptr()) }
        .to_str()
        .map_err(|_| io::ErrorKind::InvalidData)?
        .trim_end_matches('.');
    if hostname.is_empty()
        || hostname.len() > 253
        || hostname.chars().any(char::is_control)
        || hostname.parse::<IpAddr>().is_ok()
    {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(hostname.into())
}

#[cfg(not(any(unix, windows)))]
pub fn lookup(_address: IpAddr) -> io::Result<String> {
    Err(io::ErrorKind::Unsupported.into())
}
