//! Batch admission is an optional provider capability. Socket syscalls and
//! platform segmentation options belong to the selected network implementation.
use amaterasu_module_sdk::network::{Network, UdpSocket, abi};
use std::io;

pub(super) enum BatchError {
    Unsupported,
    Retry,
    Failed(io::Error),
}

pub(super) struct BatchSender {
    max_datagrams: usize,
}
impl BatchSender {
    pub(super) fn new(network: &Network) -> Self {
        let capabilities = network.capabilities();
        Self {
            max_datagrams: if network.supports(abi::BATCH) {
                capabilities.max_batch as usize
            } else {
                0
            },
        }
    }

    /// Limit the proposed group before calling the provider. A provider's
    /// smaller declared capacity is not a partial write or backpressure.
    pub(super) fn max_datagrams(&self) -> usize {
        self.max_datagrams.max(1)
    }

    pub(super) fn send(
        &self,
        socket: &UdpSocket,
        payload: &[u8],
        segment_size: usize,
    ) -> Result<usize, BatchError> {
        if self.max_datagrams == 0 {
            return Err(BatchError::Unsupported);
        }
        if segment_size == 0 || !payload.len().is_multiple_of(segment_size) {
            return Err(BatchError::Failed(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid datagram batch shape",
            )));
        }
        let datagrams: Vec<&[u8]> = payload
            .chunks(segment_size)
            .take(self.max_datagrams)
            .collect();
        match socket.send_batch(&datagrams) {
            Ok(count) if count <= datagrams.len() => Ok(count * segment_size),
            Ok(_) => Err(BatchError::Failed(io::Error::new(
                io::ErrorKind::InvalidData,
                "provider accepted more datagrams than submitted",
            ))),
            Err(error) if error.kind() == io::ErrorKind::Unsupported => {
                Err(BatchError::Unsupported)
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                Err(BatchError::Retry)
            }
            Err(error) => Err(BatchError::Failed(error)),
        }
    }
}
