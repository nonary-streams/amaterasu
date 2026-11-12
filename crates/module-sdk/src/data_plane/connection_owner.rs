//! Isolated, not routed, connection-owner adapter for bounded duplex transfer.
//!
//! Trusted composition and the TLS owner establish `Binding`. The caller must
//! serialize connection effects and observed cancellation on its owner thread.
//! Entitlement checks are point-in-time and do not reserve authorization.
//! This adapter neither authenticates a peer nor owns TLS, sockets or workers.
use crate::{
    abi::{STATUS_INVALID_ARGUMENT, STATUS_OK, Status},
    data_plane::{
        Channel, DuplexPair, Hub, OwnedChunk,
        abi::{BUSY, CANCELLED, CLOSED, Description, EMPTY, ENDED, TOO_LARGE},
    },
};
use std::io::{self, Write};

/// Values established by the trusted connection and composition owners, not
/// fields chosen by the remote peer. Certificate is the verified peer identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub connection_id: u64,
    pub peer_certificate: Vec<u8>,
    pub consumer_principal: String,
}

/// Local output state; EOF means the producer finished and queued data drained,
/// not that the remote peer received it.
#[derive(Debug, PartialEq, Eq)]
pub enum OutputProgress {
    Pending,
    Eof,
}

/// Host-selected limits for the response and each staged transfer chunk.
pub struct BufferLimits {
    pub response: usize,
    pub staging: usize,
}

/// All methods are called by one connection owner. It must stop effects after
/// observing denial or disconnect before `signal_cancel`, unblock external operations,
/// signal cancellation before joining channel waiters, then drop this object.
/// Never drop from either channel's receive callback.
pub struct DuplexOwner {
    binding: Binding,
    pair: DuplexPair,
    response: Vec<u8>,
    response_offset: usize,
    response_flushed: bool,
    pending_input: Option<OwnedChunk>,
    next_input: u64,
    output: Vec<u8>,
    output_offset: usize,
    output_limit: usize,
    closed: bool,
}

impl DuplexOwner {
    /// Called only after TLS verification, trusted provider selection, authority
    /// approval and protocol-specific response validation. Bounds are local
    /// policy, not peer input. Initial plaintext must include every byte already
    /// consumed beyond the request; failure leaves the connection unupgraded.
    pub fn new(
        hub: &Hub,
        binding: Binding,
        input: Description,
        output: Description,
        response: Vec<u8>,
        buffered: Vec<u8>,
        limits: BufferLimits,
    ) -> Result<Self, Status> {
        let response_limit = limits.response;
        let staging_limit = limits.staging;
        if binding.connection_id == 0
            || binding.peer_certificate.is_empty()
            || binding.peer_certificate.len() > 16 * 1024
            || response.is_empty()
            || response.len() > response_limit
            || response_limit == 0
            || staging_limit == 0
            || buffered.len() > staging_limit
            || buffered.len() as u64 > input.limits.max_chunk_bytes
            || output.limits.max_chunk_bytes > staging_limit as u64
            || input.limits.max_metadata_bytes != 0
            || output.limits.max_metadata_bytes != 0
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let pair = DuplexPair::create(hub, &binding.consumer_principal, input, output)?;
        let binding = Binding {
            peer_certificate: binding.peer_certificate.into_boxed_slice().into_vec(),
            ..binding
        };
        let pending_input = (!buffered.is_empty()).then(|| OwnedChunk::with_bytes(1, buffered));
        Ok(Self {
            binding,
            pair,
            response: response.into_boxed_slice().into_vec(),
            response_offset: 0,
            response_flushed: false,
            pending_input,
            next_input: 1,
            output: Vec::new(),
            output_offset: 0,
            output_limit: staging_limit,
            closed: false,
        })
    }
    pub fn binding(&self) -> &Binding {
        &self.binding
    }
    pub fn pair(&self) -> &DuplexPair {
        &self.pair
    }

    /// Called by the connection owner on every tick, including while idle.
    /// A native consumer can release either channel independently; once that
    /// cancellation is observed, retire both directions before further effects.
    pub fn check_channels(&mut self) -> Result<(), Status> {
        if self.closed {
            return Err(CLOSED);
        }
        if self.pair.input().snapshot().phase == CANCELLED
            || self.pair.output().snapshot().phase == CANCELLED
        {
            self.signal_cancel();
            return Err(CLOSED);
        }
        Ok(())
    }

    /// Retry previously buffered input before accepting another socket read.
    pub fn flush_input(&mut self) -> Result<(), Status> {
        self.check_channels()?;
        if let Some(chunk) = self.pending_input.take() {
            match self.pair.input().push(chunk) {
                Ok(()) => match self.next_input.checked_add(1) {
                    Some(next) => self.next_input = next,
                    None => {
                        self.signal_cancel();
                        return Err(CLOSED);
                    }
                },
                Err((status, chunk)) => {
                    self.pending_input = Some(chunk);
                    if status != BUSY {
                        self.signal_cancel();
                    }
                    return Err(status);
                }
            }
        }
        Ok(())
    }
    /// Takes one bounded socket read only when previous input was admitted.
    pub fn input(&mut self, bytes: Vec<u8>) -> Result<(), (Status, Vec<u8>)> {
        if self.check_channels().is_err() {
            return Err((CLOSED, bytes));
        }
        if self.pending_input.is_some() {
            return Err((BUSY, bytes));
        }
        if bytes.is_empty()
            || bytes.len() > self.output_limit
            || bytes.len() as u64 > self.pair.input().description().limits.max_chunk_bytes
        {
            self.signal_cancel();
            return Err((TOO_LARGE, bytes));
        }
        self.pending_input = Some(OwnedChunk::with_bytes(self.next_input, bytes));
        // Ownership moves into bounded staging; flush reports admission errors.
        Ok(())
    }
    pub fn finish_input(&mut self) -> Result<(), Status> {
        self.flush_input()?;
        match self.pair.input().finish() {
            Ok(()) => Ok(()),
            Err(status) => {
                self.signal_cancel();
                Err(status)
            }
        }
    }
    /// Whether all approved response plaintext has been accepted by the TLS
    /// writer. The owner must still drain TLS ciphertext before acknowledging.
    pub fn response_written(&self) -> bool {
        !self.closed && self.response_offset == self.response.len()
    }

    /// Acknowledge only after the connection owner has flushed the complete
    /// response through TLS and its underlying transport, on the owner thread.
    pub fn response_flushed(&mut self) -> Result<(), Status> {
        if self.check_channels().is_err() || self.response_offset != self.response.len() {
            self.signal_cancel();
            return Err(STATUS_INVALID_ARGUMENT);
        }
        self.response_flushed = true;
        Ok(())
    }

    /// Write at most one nonblocking step. Response bytes always precede plugin
    /// output; each output chunk is fully staged before accepting it, so partial
    /// writes never cause duplicate transfer delivery. The caller serializes this
    /// step with observed cancellation and must close on denial or I/O failure.
    /// Authorization is point-in-time; revocation can race the following write.
    pub fn write_step(
        &mut self,
        writer: &mut impl Write,
        authorize: impl FnOnce(&Binding) -> bool,
    ) -> io::Result<OutputProgress> {
        let result = self.write_step_inner(writer, authorize);
        if result.is_err() {
            self.signal_cancel();
        }
        result
    }

    fn write_step_inner(
        &mut self,
        writer: &mut impl Write,
        authorize: impl FnOnce(&Binding) -> bool,
    ) -> io::Result<OutputProgress> {
        if self.check_channels().is_err() {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        if !authorize(&self.binding) {
            self.signal_cancel();
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        if self.response_offset < self.response.len() {
            write_some(writer, &self.response, &mut self.response_offset)?;
            return Ok(OutputProgress::Pending);
        }
        if !self.response_flushed {
            return Ok(OutputProgress::Pending);
        }
        if self.output_offset == self.output.len() {
            self.output.clear();
            self.output_offset = 0;
            let status = stage_output(self.pair.output(), &mut self.output, self.output_limit);
            if status == EMPTY {
                return Ok(OutputProgress::Pending);
            }
            if status == CLOSED {
                return if self.pair.output().snapshot().phase == ENDED {
                    Ok(OutputProgress::Eof)
                } else {
                    Err(io::ErrorKind::BrokenPipe.into())
                };
            }
            if status != STATUS_OK {
                return Err(io::Error::other(format!("transfer output: {status}")));
            }
        }
        write_some(writer, &self.output, &mut self.output_offset)?;
        Ok(OutputProgress::Pending)
    }
    /// Stop socket effects first; signal before joining transfer waiters.
    pub fn signal_cancel(&mut self) {
        self.closed = true;
        self.response.clear();
        self.pending_input = None;
        self.output.clear();
        self.pair.signal_cancel();
    }
}

fn write_some(writer: &mut impl Write, bytes: &[u8], offset: &mut usize) -> io::Result<()> {
    match writer.write(&bytes[*offset..]) {
        Ok(0) => Err(io::ErrorKind::WriteZero.into()),
        Ok(n) => {
            *offset += n;
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(()),
        Err(e) => Err(e),
    }
}
fn stage_output(channel: &Channel, output: &mut Vec<u8>, limit: usize) -> Status {
    // SAFETY: all segment pointers are borrowed only during this callback;
    // payload is copied before return and no descriptor escapes.
    unsafe {
        channel.receive(|chunk| {
            let mut length = 0usize;
            for segment in std::slice::from_raw_parts(chunk.segments, chunk.segment_count as usize)
            {
                let Some(next) = length.checked_add(segment.len) else {
                    return TOO_LARGE;
                };
                if next > limit {
                    return TOO_LARGE;
                }
                length = next;
            }
            if chunk.metadata_len != 0 {
                return STATUS_INVALID_ARGUMENT;
            }
            output.reserve(length);
            for segment in std::slice::from_raw_parts(chunk.segments, chunk.segment_count as usize)
            {
                output.extend_from_slice(std::slice::from_raw_parts(segment.data, segment.len));
            }
            STATUS_OK
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_plane::{
        abi::{Limits, SINK, SOURCE, Schema},
        description,
    };
    fn desc(direction: u32) -> Description {
        let schema = Schema {
            namespace: 1,
            value: 1,
            version: 1,
        };
        description(
            direction,
            schema,
            schema,
            Limits {
                max_chunk_bytes: 4,
                max_queued_bytes: 4,
                max_record_bytes: 4,
                max_queued_chunks: 1,
                max_segments: 1,
                max_metadata_bytes: 0,
                reserved: 0,
            },
        )
        .unwrap()
    }
    fn owner(buffered: &[u8]) -> DuplexOwner {
        DuplexOwner::new(
            &Hub::new(2, 8).unwrap(),
            Binding {
                connection_id: 3,
                peer_certificate: vec![9],
                consumer_principal: "consumer".into(),
            },
            desc(SOURCE),
            desc(SINK),
            b"OK".to_vec(),
            buffered.to_vec(),
            BufferLimits {
                response: 8,
                staging: 4,
            },
        )
        .unwrap()
    }
    struct Partial(Vec<u8>);
    impl Write for Partial {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let n = bytes.len().min(1);
            self.0.extend_from_slice(&bytes[..n]);
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn response_precedes_partial_output_and_authority_denial_closes_pair() {
        let mut owner = owner(b"");
        owner
            .pair()
            .output()
            .push(OwnedChunk::with_bytes(1, b"data".to_vec()))
            .unwrap();
        let output = owner.pair().output().clone();
        let mut wire = Partial(Vec::new());
        for _ in 0..2 {
            owner
                .write_step(&mut wire, |b| {
                    b.connection_id == 3
                        && b.peer_certificate == [9]
                        && b.consumer_principal == "consumer"
                })
                .unwrap();
        }
        owner.write_step(&mut wire, |_| true).unwrap();
        assert_eq!(wire.0, b"OK");
        owner.response_flushed().unwrap();
        for _ in 0..4 {
            owner.write_step(&mut wire, |_| true).unwrap();
        }
        assert_eq!(wire.0, b"OKdata");
        assert_eq!(
            owner.write_step(&mut wire, |_| false).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            output
                .push(OwnedChunk::with_bytes(2, b"x".to_vec()))
                .unwrap_err()
                .0,
            CLOSED
        );
    }
    #[test]
    fn buffered_input_precedes_later_reads_under_backpressure() {
        let mut owner = owner(b"pre");
        assert_eq!(owner.input(b"next".to_vec()).unwrap_err().0, BUSY);
        owner.flush_input().unwrap();
        owner.input(b"next".to_vec()).unwrap();
        assert_eq!(owner.flush_input(), Err(BUSY));
        let mut received = Vec::new();
        assert_eq!(
            stage_output(owner.pair().input(), &mut received, 4),
            STATUS_OK
        );
        assert_eq!(received, b"pre");
        owner.flush_input().unwrap();
        received.clear();
        assert_eq!(
            stage_output(owner.pair().input(), &mut received, 4),
            STATUS_OK
        );
        assert_eq!(received, b"next");
        owner.finish_input().unwrap();
    }
    #[test]
    fn observed_revocation_stops_later_owner_effects() {
        let mut owner = owner(b"");
        let mut wire = Partial(Vec::new());
        owner.write_step(&mut wire, |_| true).unwrap();
        assert_eq!(wire.0, b"O");
        assert_eq!(
            owner.write_step(&mut wire, |_| false).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            owner.write_step(&mut wire, |_| true).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        assert_eq!(wire.0, b"O");
    }

    #[test]
    fn directional_eof_is_distinct_from_cancellation() {
        let mut owner = owner(b"");
        let mut wire = Partial(Vec::new());
        for _ in 0..2 {
            owner.write_step(&mut wire, |_| true).unwrap();
        }
        owner.response_flushed().unwrap();
        owner.pair().output().finish().unwrap();
        assert_eq!(
            owner.write_step(&mut wire, |_| true).unwrap(),
            OutputProgress::Eof
        );
        owner.signal_cancel();
        assert_eq!(
            owner.write_step(&mut wire, |_| true).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::ConnectionReset.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn fatal_io_and_invalid_flush_signal_both_channels() {
        let mut connection = owner(b"");
        let input = connection.pair().input().clone();
        let output = connection.pair().output().clone();
        assert_eq!(
            connection
                .write_step(&mut Broken, |_| true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::ConnectionReset
        );
        assert_eq!(input.snapshot().phase, crate::data_plane::abi::CANCELLED);
        assert_eq!(output.snapshot().phase, crate::data_plane::abi::CANCELLED);
        let mut connection = owner(b"");
        assert!(connection.response_flushed().is_err());
        assert_eq!(
            connection.pair().output().snapshot().phase,
            crate::data_plane::abi::CANCELLED
        );
    }
    #[test]
    fn idle_consumer_cancellation_stops_both_directions() {
        let mut connection = owner(b"");
        let output = connection.pair().output().clone();
        connection.pair().input().cancel();
        assert_eq!(connection.check_channels(), Err(CLOSED));
        assert_eq!(output.snapshot().phase, CANCELLED);
        assert_eq!(
            connection
                .write_step(&mut Vec::new(), |_| true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[test]
    fn fatal_input_admission_signals_other_direction() {
        let mut connection = owner(b"pre");
        let output = connection.pair().output().clone();
        connection.pair().input().cancel();
        assert_eq!(connection.flush_input(), Err(CLOSED));
        assert_eq!(output.snapshot().phase, crate::data_plane::abi::CANCELLED);
    }
    #[test]
    fn oversized_remainder_and_metadata_profile_fail_before_upgrade() {
        let hub = Hub::new(2, 8).unwrap();
        let binding = Binding {
            connection_id: 1,
            peer_certificate: vec![1],
            consumer_principal: "consumer".into(),
        };
        let limits = || BufferLimits {
            response: 8,
            staging: 8,
        };
        assert!(
            DuplexOwner::new(
                &hub,
                binding.clone(),
                desc(SOURCE),
                desc(SINK),
                vec![1],
                vec![1; 5],
                limits()
            )
            .is_err()
        );
        let mut metadata_output = desc(SINK);
        metadata_output.limits.max_metadata_bytes = 1;
        assert!(
            DuplexOwner::new(
                &hub,
                binding.clone(),
                desc(SOURCE),
                metadata_output,
                vec![1],
                vec![],
                limits()
            )
            .is_err()
        );
        // Failed handoff did not reserve either channel.
        assert!(
            DuplexOwner::new(
                &hub,
                binding,
                desc(SOURCE),
                desc(SINK),
                vec![1],
                vec![],
                limits()
            )
            .is_ok()
        );
    }

    #[test]
    fn invalid_binding_and_unbounded_staging_are_rejected() {
        let hub = Hub::new(2, 8).unwrap();
        let binding = Binding {
            connection_id: 0,
            peer_certificate: vec![1],
            consumer_principal: "consumer".into(),
        };
        assert!(
            DuplexOwner::new(
                &hub,
                binding,
                desc(SOURCE),
                desc(SINK),
                vec![1],
                vec![],
                BufferLimits {
                    response: 8,
                    staging: 4
                }
            )
            .is_err()
        );
        let binding = Binding {
            connection_id: 1,
            peer_certificate: vec![1],
            consumer_principal: "consumer".into(),
        };
        assert!(
            DuplexOwner::new(
                &hub,
                binding,
                desc(SOURCE),
                desc(SINK),
                vec![1],
                vec![1; 5],
                BufferLimits {
                    response: 8,
                    staging: 4
                }
            )
            .is_err()
        );
    }
}
