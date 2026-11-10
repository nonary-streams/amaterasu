//! OS timing stays inside the selected network provider. No descriptor escapes.
use super::*;

pub(super) fn enable_timestamps(socket: &Socket) {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let enabled: libc::c_int = 1;
        // SAFETY: a live socket and a correctly sized immutable integer are borrowed
        // by setsockopt. Failure leaves timing unavailable without rejecting IO.
        let _ = unsafe {
            libc::setsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_TIMESTAMPNS,
                std::ptr::from_ref(&enabled).cast(),
                size_of_val(&enabled) as libc::socklen_t,
            )
        };
    }
    #[cfg(not(target_os = "linux"))]
    let _ = socket;
}

pub(super) unsafe extern "C" fn recv_from_timed(
    context: *mut c_void,
    id: u64,
    buffer: *mut u8,
    len: usize,
    count: *mut usize,
    peer: *mut net::Address,
    timing: *mut net::ReceiveTiming,
) -> Status {
    guarded(|| {
        let binding = unsafe { binding(context)? };
        if count.is_null() || peer.is_null() || timing.is_null() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let buffer = unsafe { write_buffer(buffer, len)? };
        let resource = resource(binding, id)?;
        let SocketResource::Udp(socket) = &resource.socket else {
            return Err(net::UNSUPPORTED);
        };
        let (size, address, observed) = receive(socket, buffer).map_err(io_status)?;
        // SAFETY: the caller supplied writable synchronous outputs validated above.
        unsafe {
            output(count, size)?;
            output(peer, to_address(address))?;
            output(timing, observed)
        }
    })
}

fn empty_timing() -> net::ReceiveTiming {
    net::ReceiveTiming {
        size: size_of::<net::ReceiveTiming>() as u32,
        version: 1,
        ..Default::default()
    }
}

#[cfg(not(target_os = "linux"))]
fn receive(
    socket: &UdpSocket,
    buffer: &mut [u8],
) -> io::Result<(usize, SocketAddr, net::ReceiveTiming)> {
    socket
        .recv_from(buffer)
        .map(|(n, peer)| (n, peer, empty_timing()))
}

#[cfg(target_os = "linux")]
fn receive(
    socket: &UdpSocket,
    buffer: &mut [u8],
) -> io::Result<(usize, SocketAddr, net::ReceiveTiming)> {
    use std::{
        os::fd::AsRawFd,
        time::{SystemTime, UNIX_EPOCH},
    };
    // SAFETY: zero is a valid empty sockaddr_storage and msghdr. recvmsg only
    // writes within the supplied address, ancillary and exclusive payload buffers.
    let mut address = socket2::SockAddrStorage::zeroed();
    let mut header: libc::msghdr = unsafe { std::mem::zeroed() };
    let mut control = [0usize; 16];
    let mut vector = libc::iovec {
        iov_base: buffer.as_mut_ptr().cast(),
        iov_len: buffer.len(),
    };
    header.msg_name = std::ptr::from_mut(unsafe { address.view_as::<libc::sockaddr>() }).cast();
    header.msg_namelen = address.size_of();
    header.msg_iov = &mut vector;
    header.msg_iovlen = 1;
    header.msg_control = control.as_mut_ptr().cast();
    header.msg_controllen = size_of_val(&control);
    // SAFETY: socket and every msghdr borrow remain live through this synchronous call.
    let size = unsafe { libc::recvmsg(socket.as_raw_fd(), &mut header, 0) };
    if size < 0 {
        return Err(io::Error::last_os_error());
    }
    let received = SystemTime::now();
    if header.msg_namelen > address.size_of() {
        return Err(io::ErrorKind::InvalidData.into());
    }
    // SAFETY: recvmsg initialized a bounded sockaddr_storage and its reported length.
    let peer = unsafe { SockAddr::new(address, header.msg_namelen) }
        .as_socket()
        .ok_or(io::ErrorKind::InvalidData)?;
    let mut observed = empty_timing();
    if header.msg_flags & libc::MSG_CTRUNC == 0 {
        // SAFETY: libc walks aligned, initialized ancillary bytes bounded by msghdr.
        let mut message = unsafe { libc::CMSG_FIRSTHDR(&header) };
        while !message.is_null() {
            // SAFETY: CMSG_FIRSTHDR/NXTHDR returned a complete ancillary header.
            let item = unsafe { &*message };
            if item.cmsg_level == libc::SOL_SOCKET
                && item.cmsg_type == libc::SCM_TIMESTAMPNS
                && item.cmsg_len
                    >= unsafe { libc::CMSG_LEN(size_of::<libc::timespec>() as u32) } as usize
            {
                // SAFETY: cmsg_len establishes initialized timestamp bytes; unaligned
                // read avoids assuming additional alignment beyond the CMSG contract.
                let stamp = unsafe {
                    std::ptr::read_unaligned(libc::CMSG_DATA(message).cast::<libc::timespec>())
                };
                if stamp.tv_sec >= 0
                    && (0..1_000_000_000).contains(&stamp.tv_nsec)
                    && let Some(at) = UNIX_EPOCH
                        .checked_add(Duration::new(stamp.tv_sec as u64, stamp.tv_nsec as u32))
                    && let Ok(age) = received.duration_since(at)
                {
                    observed.flags |= net::KERNEL_RESIDENCE_REALTIME;
                    observed.kernel_residence_us = age.as_micros().min(u128::from(u64::MAX)) as u64;
                }
            }
            // SAFETY: same initialized bounded msghdr, and message belongs to it.
            message = unsafe { libc::CMSG_NXTHDR(&header, message) };
        }
    }
    Ok((size as usize, peer, observed))
}

pub(super) unsafe extern "C" fn socket_diagnostics(
    context: *mut c_void,
    id: u64,
    out: *mut net::SocketDiagnostics,
) -> Status {
    guarded(|| {
        let binding = unsafe { binding(context)? };
        if out.is_null() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let resource = resource(binding, id)?;
        let SocketResource::Udp(socket) = &resource.socket else {
            return Err(net::UNSUPPORTED);
        };
        let observed = socket_memory(socket).map_err(io_status)?;
        // SAFETY: caller's output remains writable throughout this synchronous callback.
        unsafe { output(out, observed) }
    })
}

#[cfg(target_os = "linux")]
fn socket_memory(socket: &UdpSocket) -> io::Result<net::SocketDiagnostics> {
    use std::os::fd::AsRawFd;
    let mut info = [0u32; 9];
    let mut length = size_of_val(&info) as libc::socklen_t;
    // SAFETY: live socket, correctly sized initialized output array and length.
    let status = unsafe {
        libc::getsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_MEMINFO,
            info.as_mut_ptr().cast(),
            &mut length,
        )
    };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    if (length as usize) < size_of_val(&info) {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(net::SocketDiagnostics {
        size: size_of::<net::SocketDiagnostics>() as u32,
        version: 1,
        flags: net::SOCKET_MEMORY,
        receive_memory_bytes: info[0].into(),
        receive_buffer_bytes: info[1].into(),
        send_memory_bytes: info[5].into(),
        send_buffer_bytes: info[3].into(),
        receive_drops: info[8].into(),
    })
}
#[cfg(not(target_os = "linux"))]
fn socket_memory(_socket: &UdpSocket) -> io::Result<net::SocketDiagnostics> {
    Ok(net::SocketDiagnostics {
        size: size_of::<net::SocketDiagnostics>() as u32,
        version: 1,
        ..Default::default()
    })
}
