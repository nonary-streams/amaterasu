//! Process hardening performed before the helper serves any request.
use amaterasu_vkms_frame_helper::{HELPER_SOCKET_FD, channel::SeqpacketChannel, exit_status};
use std::{
    io,
    os::fd::{FromRawFd, OwnedFd},
};

/// Forbids privilege gain through exec, ptrace attachment and core dumps,
/// and clears the ambient capability set.
pub fn harden_process() -> Result<(), u8> {
    // SAFETY: these prctl options take integer arguments only.
    let hardened = unsafe {
        libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0
            && libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) == 0
            && libc::prctl(
                libc::PR_CAP_AMBIENT,
                libc::PR_CAP_AMBIENT_CLEAR_ALL,
                0,
                0,
                0,
            ) == 0
    };
    if hardened {
        Ok(())
    } else {
        Err(exit_status::HARDENING)
    }
}

/// Refuses root identities: the helper's only privilege is one capability.
pub fn refuse_root_identity() -> Result<(), u8> {
    let (mut real, mut effective, mut saved) = (0, 0, 0);
    let (mut real_group, mut effective_group, mut saved_group) = (0, 0, 0);
    // SAFETY: each pointer names a writable id on this stack frame.
    let read = unsafe {
        libc::getresuid(&mut real, &mut effective, &mut saved) == 0
            && libc::getresgid(&mut real_group, &mut effective_group, &mut saved_group) == 0
    };
    let any_root = [real, effective, saved].contains(&0)
        || [real_group, effective_group, saved_group].contains(&0);
    if !read || any_root {
        return Err(exit_status::ROOT_IDENTITY);
    }
    Ok(())
}

/// Adopts the inherited control socket, marks it close-on-exec and checks
/// that the peer runs as the same user.
pub fn adopt_control_socket() -> Result<SeqpacketChannel, u8> {
    // SAFETY: F_GETFD only queries the descriptor number.
    if unsafe { libc::fcntl(HELPER_SOCKET_FD, libc::F_GETFD) } < 0 {
        return Err(exit_status::SOCKET);
    }
    // SAFETY: the launch contract hands descriptor 3 to this process and
    // nothing else in the helper refers to it.
    let socket = unsafe { OwnedFd::from_raw_fd(HELPER_SOCKET_FD) };
    // SAFETY: F_SETFD only changes this descriptor's flags.
    if unsafe { libc::fcntl(HELPER_SOCKET_FD, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
        return Err(exit_status::SOCKET);
    }
    let channel = SeqpacketChannel::from_owned(socket).map_err(|_| exit_status::SOCKET)?;
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    match channel.peer_uid() {
        Ok(peer) if peer == uid => Ok(channel),
        _ => Err(exit_status::SOCKET),
    }
}

/// Closes every inherited descriptor above the control socket.
pub fn close_inherited_descriptors() -> io::Result<()> {
    let first = HELPER_SOCKET_FD as libc::c_uint + 1;
    // SAFETY: close_range closes descriptors this process does not use; the
    // helper has created no descriptors of its own above the socket yet.
    let result = unsafe { libc::syscall(libc::SYS_close_range, first, libc::c_uint::MAX, 0) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
