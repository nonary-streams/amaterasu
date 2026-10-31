use std::{io, net::UdpSocket};
/// Return a datagram prefix, never a partial datagram. The Linux path preserves
/// UDP segmentation offload without joining the input buffers. Unsupported
/// platform/layouts fall back locally; no OS handle leaks to consumer plugins.
pub(super) fn send(socket: &UdpSocket, buffers: &[&[u8]]) -> io::Result<usize> {
    #[cfg(target_os = "linux")]
    if let Some(result) = send_linux(socket, buffers) {
        return result;
    }
    let mut count = 0;
    for bytes in buffers {
        match socket.send(bytes) {
            Ok(size) if size == bytes.len() => count += 1,
            Ok(_) => {
                return if count > 0 {
                    Ok(count)
                } else {
                    Err(io::Error::from(io::ErrorKind::WriteZero))
                };
            }
            Err(_) if count > 0 => return Ok(count),
            Err(error) => return Err(error),
        }
    }
    Ok(count)
}
#[cfg(target_os = "linux")]
fn send_linux(socket: &UdpSocket, buffers: &[&[u8]]) -> Option<io::Result<usize>> {
    use std::os::fd::AsRawFd;
    if buffers.len() < 2 || buffers.len() > 64 {
        return None;
    }
    let segment = buffers[0].len();
    if segment == 0
        || segment > u16::MAX as usize
        || buffers
            .iter()
            .take(buffers.len() - 1)
            .any(|b| b.len() != segment)
        || buffers.last().is_some_and(|b| b.len() > segment)
    {
        return None;
    }
    let total: usize = buffers.iter().map(|b| b.len()).sum();
    if total > 65535 {
        return None;
    }
    #[repr(C, align(8))]
    struct Control([u8; 24]);
    let mut control = Control([0; 24]);
    let header_bytes =
        std::mem::size_of::<libc::cmsghdr>().next_multiple_of(std::mem::size_of::<usize>());
    let message_length = header_bytes + 2;
    let control_length = message_length.next_multiple_of(std::mem::size_of::<usize>());
    // SAFETY: suitably aligned bounded control storage holds CMSG header + u16.
    unsafe {
        control
            .0
            .as_mut_ptr()
            .cast::<libc::cmsghdr>()
            .write(libc::cmsghdr {
                cmsg_len: message_length,
                cmsg_level: libc::IPPROTO_UDP,
                cmsg_type: 103,
            });
        control
            .0
            .as_mut_ptr()
            .add(header_bytes)
            .cast::<u16>()
            .write(segment as u16)
    }
    let mut vectors: Vec<libc::iovec> = buffers
        .iter()
        .map(|b| libc::iovec {
            iov_base: b.as_ptr().cast_mut().cast(),
            iov_len: b.len(),
        })
        .collect();
    // SAFETY: zero is a valid uninitialized msghdr; pointers set below remain live.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = vectors.as_mut_ptr();
    message.msg_iovlen = vectors.len();
    message.msg_control = control.0.as_mut_ptr().cast();
    message.msg_controllen = control_length;
    // SAFETY: live connected socket and immutable borrowed input throughout sendmsg.
    let sent = unsafe { libc::sendmsg(socket.as_raw_fd(), &message, 0) };
    if sent >= 0 {
        return Some(if sent as usize == total {
            Ok(buffers.len())
        } else {
            Err(io::Error::from(io::ErrorKind::WriteZero))
        });
    }
    let error = io::Error::last_os_error();
    if matches!(error.raw_os_error(), Some(22 | 38 | 90 | 92 | 95)) {
        None
    } else {
        Some(Err(error))
    }
}

/// Windows segmentation changes a socket option. The caller serializes every
/// send operation on this resource while this function sets/sends/restores it.
/// If restoration fails the resource is poisoned before returning; an accepted
/// prefix is still reported exactly once, preventing a retry of sent datagrams.
#[cfg(windows)]
pub(super) fn send_serialized(
    socket: &UdpSocket,
    buffers: &[&[u8]],
    healthy: &mut bool,
) -> io::Result<usize> {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Networking::WinSock::{
        IPPROTO_UDP, SOCKET, UDP_SEND_MSG_SIZE, WSAGetLastError, setsockopt,
    };
    if buffers.len() < 2 || buffers.len() > 64 {
        return send(socket, buffers);
    }
    let segment = buffers[0].len();
    let total: usize = buffers.iter().map(|b| b.len()).sum();
    if segment == 0
        || segment > u16::MAX as usize
        || total > 65535
        || buffers
            .iter()
            .take(buffers.len() - 1)
            .any(|b| b.len() != segment)
        || buffers.last().is_some_and(|b| b.len() > segment)
    {
        return send(socket, buffers);
    }
    let raw = socket.as_raw_socket() as SOCKET;
    let size = segment as u32;
    // SAFETY: socket is live and this option uses the documented DWORD layout.
    if unsafe {
        setsockopt(
            raw,
            IPPROTO_UDP,
            UDP_SEND_MSG_SIZE,
            (&size as *const u32).cast(),
            size_of::<u32>() as i32,
        )
    } != 0
    {
        let code = unsafe { WSAGetLastError() };
        return if matches!(code, 10022 | 10040 | 10042 | 10045 | 10047) {
            send(socket, buffers)
        } else {
            Err(io::Error::from_raw_os_error(code))
        };
    }
    let mut joined = Vec::with_capacity(total);
    for bytes in buffers {
        joined.extend_from_slice(bytes);
    }
    let result = socket.send(&joined);
    let zero = 0u32;
    // SAFETY: resets the same option while the caller's send lock is held.
    if unsafe {
        setsockopt(
            raw,
            IPPROTO_UDP,
            UDP_SEND_MSG_SIZE,
            (&zero as *const u32).cast(),
            size_of::<u32>() as i32,
        )
    } != 0
    {
        *healthy = false;
    }
    match result {
        Ok(count) if count == total => Ok(buffers.len()),
        Ok(_) => {
            *healthy = false;
            Err(io::Error::from(io::ErrorKind::WriteZero))
        }
        Err(error) => Err(error),
    }
}
