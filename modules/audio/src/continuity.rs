//! Stream-level packet continuity shared by platform capture workers.
use crate::model::{AudioResult, Packet};
use amaterasu_media::{AUDIO_DISCONTINUITY, AUDIO_TIMESTAMP_ESTIMATED};
use std::time::Instant;

/// Preserves the stream-level packet clock when a capture source replaces its
/// per-device QPC timeline after endpoint recovery.
pub struct Continuity {
    next_sequence: u64,
    last_timestamp: Option<u64>,
    last_arrival: Option<Instant>,
    recovery_pending: bool,
    segment_origin: Option<(u64, u64)>,
}

impl Continuity {
    pub fn new() -> Self {
        Self {
            next_sequence: 0,
            last_timestamp: None,
            last_arrival: None,
            recovery_pending: false,
            segment_origin: None,
        }
    }

    pub fn begin_recovery(&mut self) {
        self.recovery_pending = true;
        self.segment_origin = None;
    }

    /// Returns true when this is the first packet from a recovered capture.
    pub fn normalize(&mut self, packet: &mut Packet) -> AudioResult<bool> {
        let arrival = Instant::now();
        let recovered_first = self.recovery_pending;
        let mut timestamp = if self.recovery_pending {
            let elapsed = self
                .last_arrival
                .map(|last_arrival| {
                    arrival
                        .saturating_duration_since(last_arrival)
                        .as_micros()
                        .min(u128::from(u64::MAX)) as u64
                })
                .unwrap_or(0)
                .max(1);
            let output_origin = self
                .last_timestamp
                .unwrap_or(packet.timestamp_us)
                .saturating_add(elapsed);
            self.segment_origin = Some((packet.timestamp_us, output_origin));
            packet.flags |= AUDIO_DISCONTINUITY | AUDIO_TIMESTAMP_ESTIMATED;
            self.recovery_pending = false;
            output_origin
        } else if let Some((source_origin, output_origin)) = self.segment_origin {
            match packet.timestamp_us.checked_sub(source_origin) {
                Some(delta) => output_origin.saturating_add(delta),
                None => {
                    packet.flags |= AUDIO_TIMESTAMP_ESTIMATED;
                    output_origin
                }
            }
        } else {
            packet.timestamp_us
        };

        if let Some(last_timestamp) = self.last_timestamp
            && timestamp < last_timestamp
        {
            timestamp = last_timestamp;
            packet.flags |= AUDIO_TIMESTAMP_ESTIMATED;
        }

        packet.timestamp_us = timestamp;
        packet.sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or("sequence_exhausted")?;
        self.last_timestamp = Some(timestamp);
        self.last_arrival = Some(arrival);
        Ok(recovered_first)
    }
}
