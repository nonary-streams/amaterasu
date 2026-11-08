//! Bounded synchronous wire client for the provider-owned KMS process.
use crate::{
    helper_process::Receipt,
    model::{CaptureError, CaptureResult},
    native::{NativeInfo, NativeOutput},
};
use std::{
    io::{self, Read, Write},
    net::Shutdown,
    os::{fd::AsRawFd, unix::net::UnixStream},
    sync::Arc,
    time::{Duration, Instant},
};

const IO_DEADLINE: Duration = Duration::from_secs(5);
const REQUEST_SIZE: usize = 200;
const HEADER_SIZE: usize = 24;
const OUTPUT_SIZE: usize = 416;
const MAX_OUTPUTS: usize = 64;

pub(crate) struct Client {
    socket: UnixStream,
    receipt: Arc<Receipt>,
    retiring: bool,
}

impl Client {
    pub fn new(socket: UnixStream, receipt: Arc<Receipt>) -> io::Result<Self> {
        Self::new_with_setup(socket, receipt, |socket| socket.set_nonblocking(true))
    }

    fn new_with_setup(
        socket: UnixStream,
        receipt: Arc<Receipt>,
        setup: impl FnOnce(&UnixStream) -> io::Result<()>,
    ) -> io::Result<Self> {
        if let Err(error) = setup(&socket) {
            receipt.stop();
            return Err(error);
        }
        Ok(Self {
            socket,
            receipt,
            retiring: false,
        })
    }

    pub fn open(&mut self, output: &str) -> CaptureResult<NativeOutput> {
        let deadline = Instant::now() + IO_DEADLINE;
        let request = self.request(1, output.as_bytes())?;
        self.write(&request, deadline)?;
        let (length, width, height) = self.header(1, deadline)?;
        if length != OUTPUT_SIZE || width != 0 || height != 0 {
            return self.protocol_error();
        }
        let mut bytes = [0; OUTPUT_SIZE];
        self.read(&mut bytes, deadline)?;
        let Some(output) = decode_output(&bytes) else {
            return self.protocol_error();
        };
        Ok(output)
    }

    pub fn probe(&mut self) -> CaptureResult<NativeInfo> {
        let deadline = Instant::now() + IO_DEADLINE;
        let request = self.request(4, &[])?;
        self.write(&request, deadline)?;
        let (length, width, height) = self.header(4, deadline)?;
        if length != 16 || width != 0 || height != 0 {
            return self.protocol_error();
        }
        let mut bytes = [0; 16];
        self.read(&mut bytes, deadline)?;
        let info = NativeInfo {
            available: word(&bytes, 0),
            output_count: word(&bytes, 4),
            framebuffer_export_supported: word(&bytes, 8),
            error: word(&bytes, 12),
        };
        if info.available > 1
            || info.framebuffer_export_supported > 1
            || info.output_count as usize > MAX_OUTPUTS
        {
            return self.protocol_error();
        }
        Ok(info)
    }

    pub fn enumerate(&mut self) -> CaptureResult<Vec<NativeOutput>> {
        let deadline = Instant::now() + IO_DEADLINE;
        let request = self.request(5, &[])?;
        self.write(&request, deadline)?;
        let (length, width, height) = self.header(5, deadline)?;
        if width != 0
            || height != 0
            || !(4..=4 + MAX_OUTPUTS * OUTPUT_SIZE).contains(&length)
            || (length - 4) % OUTPUT_SIZE != 0
        {
            return self.protocol_error();
        }
        let mut count_bytes = [0; 4];
        self.read(&mut count_bytes, deadline)?;
        let count = word(&count_bytes, 0) as usize;
        if count > MAX_OUTPUTS || length != 4 + count * OUTPUT_SIZE {
            return self.protocol_error();
        }
        let mut outputs = Vec::with_capacity(count);
        for _ in 0..count {
            let mut bytes = [0; OUTPUT_SIZE];
            self.read(&mut bytes, deadline)?;
            let Some(output) = decode_output(&bytes) else {
                return self.protocol_error();
            };
            outputs.push(output);
        }
        Ok(outputs)
    }
    pub fn read_frame(&mut self, width: u32, height: u32, bytes: &mut [u8]) -> CaptureResult<()> {
        let deadline = Instant::now() + IO_DEADLINE;
        let request = self.request(2, &[])?;
        self.write(&request, deadline)?;
        let (length, actual_width, actual_height) = self.header(2, deadline)?;
        if length != bytes.len() || (actual_width, actual_height) != (width, height) {
            return self.protocol_error();
        }
        self.read(bytes, deadline)
    }

    pub fn retire(&mut self) -> CaptureResult<bool> {
        if !self.retiring {
            self.retiring = true;
            let _ = self.socket.shutdown(Shutdown::Both);
            self.receipt.stop();
        }
        self.receipt
            .poll()
            .map_err(|error| CaptureError::new("cleanup_failed", error.to_string()))
    }

    fn request(&self, op: u32, output: &[u8]) -> CaptureResult<[u8; REQUEST_SIZE]> {
        if self.retiring || output.len() >= REQUEST_SIZE - 8 || output.contains(&0) {
            return Err(CaptureError::new(
                "capture_closed",
                "KMS helper request is invalid or retired",
            ));
        }
        let mut request = [0; REQUEST_SIZE];
        request[..4].copy_from_slice(b"AMK1");
        request[4..8].copy_from_slice(&op.to_le_bytes());
        request[8..8 + output.len()].copy_from_slice(output);
        Ok(request)
    }

    fn header(&mut self, op: u32, deadline: Instant) -> CaptureResult<(usize, u32, u32)> {
        let mut header = [0; HEADER_SIZE];
        self.read(&mut header, deadline)?;
        if &header[..4] != b"AMR1" || word(&header, 4) != op {
            return self.protocol_error();
        }
        let status = word(&header, 8);
        let length = word(&header, 12) as usize;
        let width = word(&header, 16);
        let height = word(&header, 20);
        if status != 0 {
            if length != 0 || width != 0 || height != 0 {
                return self.protocol_error();
            }
            return Err(crate::backend::native_error(status));
        }
        Ok((length, width, height))
    }

    fn read(&mut self, bytes: &mut [u8], deadline: Instant) -> CaptureResult<()> {
        transfer(&mut self.socket, bytes, deadline, false).map_err(|error| {
            self.abort();
            CaptureError::new(
                "capture_unavailable",
                format!("KMS helper read failed: {error}"),
            )
        })
    }

    fn write(&mut self, bytes: &[u8], deadline: Instant) -> CaptureResult<()> {
        let mut bytes = bytes.to_vec();
        transfer(&mut self.socket, &mut bytes, deadline, true).map_err(|error| {
            self.abort();
            CaptureError::new(
                "capture_unavailable",
                format!("KMS helper write failed: {error}"),
            )
        })
    }

    fn protocol_error<T>(&mut self) -> CaptureResult<T> {
        self.abort();
        Err(CaptureError::new(
            "capture_unavailable",
            "invalid KMS helper response",
        ))
    }

    fn abort(&mut self) {
        self.retiring = true;
        let _ = self.socket.shutdown(Shutdown::Both);
        self.receipt.stop();
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.abort();
    }
}

fn decode_output(bytes: &[u8; OUTPUT_SIZE]) -> Option<NativeOutput> {
    for field in [&bytes[32..224], &bytes[224..352], &bytes[352..416]] {
        let end = field.iter().position(|byte| *byte == 0)?;
        if field[end..].iter().any(|byte| *byte != 0) {
            return None;
        }
    }
    let mut output = NativeOutput {
        left: word(bytes, 0) as i32,
        top: word(bytes, 4) as i32,
        width: word(bytes, 8),
        height: word(bytes, 12),
        rotation: word(bytes, 16),
        attached: word(bytes, 20),
        framebuffer_export_supported: word(bytes, 24),
        error: word(bytes, 28),
        ..NativeOutput::default()
    };
    for (destination, source) in output.id.iter_mut().zip(&bytes[32..224]) {
        *destination = *source as _;
    }
    for (destination, source) in output.name.iter_mut().zip(&bytes[224..352]) {
        *destination = *source as _;
    }
    for (destination, source) in output.card.iter_mut().zip(&bytes[352..416]) {
        *destination = *source as _;
    }
    Some(output)
}

fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("fixed wire field"),
    )
}

fn transfer(
    socket: &mut UnixStream,
    bytes: &mut [u8],
    deadline: Instant,
    write: bool,
) -> io::Result<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let timeout = remaining
            .as_millis()
            .saturating_add(1)
            .min(i32::MAX as u128) as i32;
        let mut descriptor = libc::pollfd {
            fd: socket.as_raw_fd(),
            events: if write { libc::POLLOUT } else { libc::POLLIN },
            revents: 0,
        };
        // SAFETY: the descriptor points to one initialized pollfd for this call.
        let ready = unsafe { libc::poll(&mut descriptor, 1, timeout) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if ready == 0 {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let result = if write {
            socket.write(&bytes[offset..])
        } else {
            socket.read(&mut bytes[offset..])
        };
        match result {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(count) => offset += count,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn response(op: u32, length: u32, width: u32, height: u32) -> [u8; HEADER_SIZE] {
        let mut bytes = [0; HEADER_SIZE];
        bytes[..4].copy_from_slice(b"AMR1");
        bytes[4..8].copy_from_slice(&op.to_le_bytes());
        bytes[12..16].copy_from_slice(&length.to_le_bytes());
        bytes[16..20].copy_from_slice(&width.to_le_bytes());
        bytes[20..24].copy_from_slice(&height.to_le_bytes());
        bytes
    }

    #[test]
    fn open_and_read_validate_and_transfer_bounded_frames() {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        let worker = thread::spawn(move || {
            let mut request = [0; REQUEST_SIZE];
            peer.read_exact(&mut request).unwrap();
            assert_eq!(&request[..8], b"AMK1\x01\0\0\0");
            let mut output = [0; OUTPUT_SIZE];
            output[8..12].copy_from_slice(&2u32.to_le_bytes());
            output[12..16].copy_from_slice(&1u32.to_le_bytes());
            output[32..35].copy_from_slice(b"id\0");
            peer.write_all(&response(1, OUTPUT_SIZE as u32, 0, 0))
                .unwrap();
            peer.write_all(&output).unwrap();
            peer.read_exact(&mut request).unwrap();
            assert_eq!(&request[..8], b"AMK1\x02\0\0\0");
            peer.write_all(&response(2, 8, 2, 1)).unwrap();
            peer.write_all(&[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        });
        let receipt = crate::helper_process::test_receipt();
        let mut client = Client::new(socket, receipt.clone()).unwrap();
        let output = client.open("id").unwrap();
        assert_eq!((output.width, output.height), (2, 1));
        let mut bytes = [0; 8];
        client.read_frame(2, 1, &mut bytes).unwrap();
        assert_eq!(bytes, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(!client.retire().unwrap());
        drop(client);
        assert!(receipt.is_stopping());
        worker.join().unwrap();
    }

    #[test]
    fn malformed_frame_length_stops_the_owned_child() {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        let worker = thread::spawn(move || {
            let mut request = [0; REQUEST_SIZE];
            peer.read_exact(&mut request).unwrap();
            peer.write_all(&response(2, u32::MAX, 2, 1)).unwrap();
        });
        let receipt = crate::helper_process::test_receipt();
        let mut client = Client::new(socket, receipt.clone()).unwrap();
        assert!(client.read_frame(2, 1, &mut [0; 8]).is_err());
        assert!(receipt.is_stopping());
        worker.join().unwrap();
    }

    #[test]
    fn failed_socket_setup_requests_child_retirement() {
        let (socket, _peer) = UnixStream::pair().unwrap();
        let receipt = crate::helper_process::test_receipt();
        assert!(
            Client::new_with_setup(socket, receipt.clone(), |_| Err(io::Error::other(
                "injected setup failure"
            )))
            .is_err()
        );
        assert!(receipt.is_stopping());
    }

    #[test]
    fn metadata_responses_are_bounded_and_decoded() {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        let worker = thread::spawn(move || {
            let mut request = [0; REQUEST_SIZE];
            peer.read_exact(&mut request).unwrap();
            assert_eq!(&request[..8], b"AMK1\x04\0\0\0");
            peer.write_all(&response(4, 16, 0, 0)).unwrap();
            for value in [1u32, 1, 1, 0] {
                peer.write_all(&value.to_le_bytes()).unwrap();
            }
            peer.read_exact(&mut request).unwrap();
            assert_eq!(&request[..8], b"AMK1\x05\0\0\0");
            peer.write_all(&response(5, (4 + OUTPUT_SIZE) as u32, 0, 0))
                .unwrap();
            peer.write_all(&1u32.to_le_bytes()).unwrap();
            let mut output = [0; OUTPUT_SIZE];
            output[8..12].copy_from_slice(&1920u32.to_le_bytes());
            output[32..35].copy_from_slice(b"id\0");
            peer.write_all(&output).unwrap();
        });
        let mut client = Client::new(socket, crate::helper_process::test_receipt()).unwrap();
        let info = client.probe().unwrap();
        assert_eq!((info.available, info.output_count), (1, 1));
        let outputs = client.enumerate().unwrap();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].width, 1920);
        worker.join().unwrap();
    }

    #[test]
    fn malformed_metadata_count_and_padding_stop_child() {
        for malformed_padding in [false, true] {
            let (socket, mut peer) = UnixStream::pair().unwrap();
            let worker = thread::spawn(move || {
                let mut request = [0; REQUEST_SIZE];
                peer.read_exact(&mut request).unwrap();
                peer.write_all(&response(5, (4 + OUTPUT_SIZE) as u32, 0, 0))
                    .unwrap();
                peer.write_all(&(if malformed_padding { 1u32 } else { 65 }).to_le_bytes())
                    .unwrap();
                if malformed_padding {
                    let mut output = [0; OUTPUT_SIZE];
                    output[33] = b'x';
                    peer.write_all(&output).unwrap();
                }
            });
            let receipt = crate::helper_process::test_receipt();
            let mut client = Client::new(socket, receipt.clone()).unwrap();
            assert!(client.enumerate().is_err());
            assert!(receipt.is_stopping());
            worker.join().unwrap();
        }
    }
}
