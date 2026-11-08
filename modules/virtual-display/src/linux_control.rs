//! Direct bindings to the pinned libvirtualdisplay Linux broker client.
//! The broker remains the connector ownership authority shared with Vibeshine.
#[cfg(amaterasu_libvirtualdisplay)]
use std::ffi::c_char;
use std::{
    ffi::CString,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlError {
    pub status: u32,
    pub native_error: i32,
    pub detail: String,
}

pub struct LinuxControlClient {
    socket: PathBuf,
}
impl Default for LinuxControlClient {
    fn default() -> Self {
        Self {
            socket: PathBuf::from("/run/vibeshine/vkms-control.sock"),
        }
    }
}
impl LinuxControlClient {
    pub fn new(socket: PathBuf) -> Result<Self, ControlError> {
        if !socket.is_absolute() {
            return Err(invalid_request());
        }
        Ok(Self { socket })
    }
    pub fn socket_path(&self) -> &Path {
        &self.socket
    }
    pub fn query_connector(&self, output: &str) -> Result<bool, ControlError> {
        self.request(output, 0)
    }
    pub fn set_connector(&self, output: &str, connected: bool) -> Result<bool, ControlError> {
        self.request(output, if connected { 1 } else { 2 })
    }
    #[cfg(not(amaterasu_libvirtualdisplay))]
    pub fn request_mode(
        &self,
        _output: &str,
        _width: u32,
        _height: u32,
        _refresh: u32,
    ) -> Result<(), String> {
        Err("libvirtualdisplay was not linked".into())
    }
    #[cfg(amaterasu_libvirtualdisplay)]
    pub fn request_mode(
        &self,
        output: &str,
        width: u32,
        height: u32,
        refresh: u32,
    ) -> Result<(), String> {
        self.request_mode_from_peer(output, width, height, refresh, 0)
    }

    #[cfg(amaterasu_libvirtualdisplay)]
    fn request_mode_from_peer(
        &self,
        output: &str,
        width: u32,
        height: u32,
        refresh: u32,
        expected_uid: u32,
    ) -> Result<(), String> {
        use std::io::{Read, Write};
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        use std::os::unix::net::UnixStream;
        use std::time::Duration;
        if super::linux_manager::connectors::connector_id(output).is_none()
            || !(64..=8192).contains(&width)
            || !(64..=8192).contains(&height)
            || !(1000..=1_000_000).contains(&refresh)
        {
            return Err("invalid requested display mode".into());
        }
        let metadata = std::fs::symlink_metadata(&self.socket).map_err(|e| e.to_string())?;
        if !metadata.file_type().is_socket()
            || metadata.uid() != expected_uid
            || metadata.mode() & 0o007 != 0
        {
            return Err("untrusted display mode broker socket".into());
        }
        let mut stream = UnixStream::connect(&self.socket).map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(Duration::from_secs(8)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_secs(8)))
            .map_err(|e| e.to_string())?;
        let mut credentials = std::mem::MaybeUninit::<libc::ucred>::uninit();
        let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: the socket is live and the kernel writes at most the supplied credential buffer length.
        let status = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                credentials.as_mut_ptr().cast(),
                &mut length,
            )
        };
        if status != 0 || length as usize != std::mem::size_of::<libc::ucred>() {
            return Err("display mode broker credentials unavailable".into());
        }
        // SAFETY: a successful SO_PEERCRED call initialized the complete credential structure.
        if unsafe { credentials.assume_init() }.uid != expected_uid {
            return Err("untrusted display mode broker peer".into());
        }
        let args = format!("{output} {width} {height} {refresh}");
        stream
            .write_all(format!("mode {args}\n").as_bytes())
            .map_err(|e| e.to_string())?;
        let mut reply = Vec::new();
        stream
            .take(129)
            .read_to_end(&mut reply)
            .map_err(|e| e.to_string())?;
        if reply != format!("OK mode {args}\n").as_bytes() {
            return Err(format!(
                "display mode admission rejected: {:?}",
                String::from_utf8_lossy(&reply)
            ));
        }
        Ok(())
    }
    fn request(&self, output: &str, action: u32) -> Result<bool, ControlError> {
        use std::os::unix::ffi::OsStrExt;
        let socket =
            CString::new(self.socket.as_os_str().as_bytes()).map_err(|_| invalid_request())?;
        let output = CString::new(output).map_err(|_| invalid_request())?;
        #[cfg(amaterasu_libvirtualdisplay)]
        {
            let mut reply = NativeResult {
                status: 0,
                connected: -1,
                native_error: 0,
                detail: [0; 192],
            };
            // SAFETY: both C strings and the fixed-layout writable result remain
            // live through the synchronous library call. It retains no pointer.
            let status = unsafe {
                amaterasu_linux_display_control(
                    socket.as_ptr(),
                    output.as_ptr(),
                    action,
                    &mut reply,
                )
            };
            if status == 0 && reply.status == 0 && matches!(reply.connected, 0 | 1) {
                return Ok(reply.connected == 1);
            }
            let end = reply
                .detail
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(reply.detail.len());
            let bytes: Vec<u8> = reply.detail[..end].iter().map(|byte| *byte as u8).collect();
            Err(ControlError {
                status: reply.status,
                native_error: reply.native_error,
                detail: String::from_utf8_lossy(&bytes).into_owned(),
            })
        }
        #[cfg(not(amaterasu_libvirtualdisplay))]
        {
            let _ = (socket, output, action);
            Err(ControlError {
                status: 2,
                native_error: 0,
                detail: "libvirtualdisplay was not linked".into(),
            })
        }
    }
}
fn invalid_request() -> ControlError {
    ControlError {
        status: 1,
        native_error: 22,
        detail: "invalid display control request".into(),
    }
}

#[cfg(amaterasu_libvirtualdisplay)]
#[repr(C)]
struct NativeResult {
    status: u32,
    connected: i32,
    native_error: i32,
    detail: [c_char; 192],
}
#[cfg(amaterasu_libvirtualdisplay)]
unsafe extern "C" {
    fn amaterasu_linux_display_control(
        socket: *const c_char,
        output: *const c_char,
        action: u32,
        result: *mut NativeResult,
    ) -> i32;
}

#[cfg(all(test, amaterasu_libvirtualdisplay))]
mod tests;
