//! Protocol-owned wait times use the peer's injected monotonic millisecond clock.
use super::*;
use amaterasu_timing_metrics::LatencyMetric;
use serde::Serialize;

#[derive(Clone, Default, Serialize)]
pub struct ProtocolTiming {
    pub receive_to_delivery_us: LatencyMetric,
    pub reassembly_us: LatencyMetric,
    pub complete_to_delivery_us: LatencyMetric,
    pub event_queue_us: LatencyMetric,
    pub datagram_queue_us: LatencyMetric,
    pub reliable_ack_us: LatencyMetric,
    pub unretransmitted_ack_us: LatencyMetric,
    pub retransmitted_commands: u64,
    pub expired_assemblies: u64,
}

#[derive(Serialize)]
pub struct ChannelBacklog {
    pub channel: usize,
    pub reliable_assemblies: usize,
    pub unreliable_assemblies: usize,
    pub complete_waiting_for_baseline: usize,
    pub incomplete_assemblies: usize,
    pub oldest_assembly_us: Option<u64>,
}

#[derive(Serialize)]
pub struct ProtocolWindow {
    pub timing_us: ProtocolTiming,
    pub channels: Vec<ChannelBacklog>,
    pub queued_events: usize,
    pub oldest_event_us: Option<u64>,
    pub queued_datagrams: usize,
    pub oldest_datagram_us: Option<u64>,
    pub pending_reliable_commands: usize,
    pub oldest_unacknowledged_us: Option<u64>,
    pub scope: &'static str,
    pub clock_resolution_us: u64,
}

fn age_us(now: u64, then: u64) -> u64 {
    now.saturating_sub(then).saturating_mul(1000)
}

impl Peer {
    pub fn take_timing(&mut self) -> ProtocolWindow {
        let channels = self
            .channels
            .iter()
            .enumerate()
            .filter_map(|(channel, state)| {
                let assemblies: Vec<_> = state
                    .reliable
                    .values()
                    .chain(state.unreliable.values())
                    .collect();
                if assemblies.is_empty() {
                    return None;
                }
                Some(ChannelBacklog {
                    channel,
                    reliable_assemblies: state.reliable.len(),
                    unreliable_assemblies: state.unreliable.len(),
                    complete_waiting_for_baseline: assemblies.iter().filter(|a| a.ready()).count(),
                    incomplete_assemblies: assemblies.iter().filter(|a| !a.ready()).count(),
                    oldest_assembly_us: assemblies
                        .iter()
                        .map(|a| age_us(self.now, a.created_at))
                        .max(),
                })
            })
            .collect();
        ProtocolWindow {
            timing_us: std::mem::take(&mut self.timing),
            channels,
            queued_events: self.events.len(),
            oldest_event_us: self.events.front().map(|(_, at)| age_us(self.now, *at)),
            queued_datagrams: self.datagrams.len(),
            oldest_datagram_us: self.datagrams.front().map(|(_, at)| age_us(self.now, *at)),
            pending_reliable_commands: self.pending.len(),
            oldest_unacknowledged_us: self
                .pending
                .iter()
                .map(|p| age_us(self.now, p.first_sent))
                .max(),
            scope: "host_protocol_first_arrival_reassembly_ordering_event_and_outbound_queues_not_network_transit",
            clock_resolution_us: 1000,
        }
    }

    pub(super) fn observe_assembly(&mut self, assembly: &incoming::Assembly) {
        self.timing
            .receive_to_delivery_us
            .record(age_us(self.now, assembly.created_at));
        if let Some(ready) = assembly.ready_at {
            self.timing
                .reassembly_us
                .record(age_us(ready, assembly.created_at));
            self.timing
                .complete_to_delivery_us
                .record(age_us(self.now, ready));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{connected, packet, reliable};

    #[test]
    fn reorder_age_survives_window_reset_and_is_separate_from_event_queue() {
        let mut peer = connected();
        peer.receive(&packet(vec![reliable(2, b"second")], 10), 10)
            .unwrap();
        peer.poll(100).unwrap();
        let waiting = peer.take_timing();
        assert_eq!(waiting.channels[0].complete_waiting_for_baseline, 1);
        assert_eq!(waiting.channels[0].oldest_assembly_us, Some(90_000));
        assert_eq!(waiting.timing_us.receive_to_delivery_us.calls, 0);
        peer.receive(&packet(vec![reliable(1, b"first")], 120), 120)
            .unwrap();
        let delivered = peer.take_timing();
        assert!(delivered.channels.is_empty());
        assert_eq!(delivered.timing_us.complete_to_delivery_us.calls, 2);
        assert_eq!(delivered.timing_us.complete_to_delivery_us.max_us, 110_000);
        assert_eq!(delivered.timing_us.reassembly_us.max_us, 0);
        peer.poll(130).unwrap();
        peer.pop_event().unwrap();
        peer.pop_event().unwrap();
        let drained = peer.take_timing();
        assert_eq!(drained.timing_us.event_queue_us.calls, 2);
        assert_eq!(drained.timing_us.event_queue_us.max_us, 10_000);
        assert_eq!(drained.queued_events, 0);
    }
}
