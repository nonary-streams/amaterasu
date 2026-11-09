//! Socket admission keeps the trusted destination policy outside ENet parsing.
use super::*;
use std::io;

impl Stream {
    pub(super) fn read_socket(&mut self, now: u64) {
        // Receive the entire standard UDP payload before parsing. A short buffer
        // makes Windows report WSAEMSGSIZE before the sender can be filtered.
        let mut buffer = [0; 65_536];
        for _ in 0..32 {
            let Some(socket) = self.socket.as_ref() else {
                return;
            };
            let receive_started = Instant::now();
            let (length, sender, timestamp) = match socket.recv_from_timed(&mut buffer) {
                Ok(received) => received,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if ignorable_datagram_error(&error) => {
                    self.rejected_datagrams = self.rejected_datagrams.saturating_add(1);
                    continue;
                }
                Err(_) => {
                    self.fail("udp_receive_failed");
                    return;
                }
            };
            self.timing.socket_receive_us.record_since(receive_started);
            if sender.ip() != self.authorized_ip
                || self
                    .authorized_port
                    .is_some_and(|port| sender.port() != port)
                || self.remote.is_some_and(|remote| remote != sender)
            {
                self.rejected_datagrams = self.rejected_datagrams.saturating_add(1);
                continue;
            }
            let kernel_residence_us = timestamp
                .filter(|t| {
                    t.flags & amaterasu_module_sdk::network::abi::KERNEL_RESIDENCE_REALTIME != 0
                })
                .map(|timestamp| timestamp.kernel_residence_us);
            if let Some(residence_us) = kernel_residence_us {
                self.timing.kernel_residence_us.record(residence_us);
            } else {
                self.timing.kernel_timing_unknown =
                    self.timing.kernel_timing_unknown.saturating_add(1);
            }
            if let Some((_, trace)) = &self.flow_trace {
                // Messages completed by this tick are traced from the oldest
                // datagram it read, the longest any of them can have waited.
                let arrived = trace.arrival(kernel_residence_us);
                self.tick_arrival = Some(
                    self.tick_arrival
                        .map_or(arrived, |oldest| oldest.min(arrived)),
                );
            }
            let arrived = Instant::now();
            if let Some(previous) = self.previous_datagram.replace(arrived) {
                self.timing
                    .datagram_arrival_gap_us
                    .record(amaterasu_module_sdk::diagnostics::elapsed_us(previous));
            }
            match self.peer.receive(&buffer[..length], now) {
                Ok(status) if status.associated => {
                    // CONNECT data is only an admission hint. No control plaintext
                    // is delivered and no native send is allowed until GCM verifies.
                    self.remote.get_or_insert(sender);
                }
                Ok(_) | Err(_) => {
                    self.rejected_datagrams = self.rejected_datagrams.saturating_add(1);
                }
            }
        }
        self.timing.receive_budget_exhausted =
            self.timing.receive_budget_exhausted.saturating_add(1);
    }

    pub(super) fn flush_socket(&mut self) {
        let Some(remote) = self.remote else {
            return;
        };
        for _ in 0..32 {
            let Some(socket) = self.socket.as_ref() else {
                return;
            };
            if self.pending_datagram.is_none() {
                self.pending_datagram = self.peer.pop_datagram();
            }
            let Some(packet) = self.pending_datagram.as_ref() else {
                return;
            };
            match socket.send_to(packet, remote) {
                Ok(length) if length == packet.len() => {
                    self.pending_datagram = None;
                    self.sent_datagrams = self.sent_datagrams.saturating_add(1);
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    self.timing.socket_send_retries =
                        self.timing.socket_send_retries.saturating_add(1);
                    return;
                }
                _ => {
                    self.fail("udp_send_failed");
                    return;
                }
            }
        }
    }
}

fn ignorable_datagram_error(error: &io::Error) -> bool {
    // Providers normalize oversized datagrams and delayed UDP ICMP resets into
    // portable conditions. They are not authenticated peer messages; the
    // authenticated idle/handshake deadlines still bound resource lifetime.
    matches!(
        error.kind(),
        io::ErrorKind::InvalidData | io::ErrorKind::ConnectionReset
    )
}
