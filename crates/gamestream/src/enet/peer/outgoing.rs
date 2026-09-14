use super::*;

impl Peer {
    pub fn can_send(&self, channel: u8, len: usize, delivery: Delivery) -> Result<(), PeerError> {
        if self.state != State::Connected {
            return Err(PeerError::NotConnected);
        }
        if channel as usize >= self.channels.len() {
            return Err(PeerError::InvalidChannel);
        }
        let delivery = self.effective_delivery(channel, delivery);
        if len > MAX_MESSAGE_BYTES {
            return Err(PeerError::MessageTooLarge);
        }
        let single = self.mtu
            - if delivery == Delivery::Reliable {
                10
            } else {
                12
            };
        let count = if len <= single {
            1
        } else {
            if delivery == Delivery::Unsequenced {
                return Err(PeerError::MessageTooLarge);
            }
            len.div_ceil(self.mtu - 28)
        };
        if count > MAX_FRAGMENTS {
            return Err(PeerError::MessageTooLarge);
        }
        if delivery == Delivery::Reliable {
            let next = self.channels[channel as usize]
                .sent_reliable
                .wrapping_add(1);
            if self.pending.iter().any(|pending| {
                pending.command.channel_id == channel
                    && next.wrapping_sub(pending.command.reliable_sequence) as usize + count
                        > RECEIVE_WINDOW as usize
            }) {
                return Err(PeerError::Busy);
            }
        }
        if self.datagrams.len() + count > MAX_DATAGRAMS
            || self.datagram_bytes + len + count * 28 > MAX_QUEUE_BYTES
            || (delivery == Delivery::Reliable && self.pending.len() + count > MAX_PENDING)
        {
            return Err(PeerError::Busy);
        }
        Ok(())
    }

    pub fn send(
        &mut self,
        channel: u8,
        payload: &[u8],
        delivery: Delivery,
        now: u64,
    ) -> Result<(), PeerError> {
        self.can_send(channel, payload.len(), delivery)?;
        let mut staged = self.clone();
        staged.clock(now)?;
        staged.send_inner(channel, payload, delivery)?;
        *self = staged;
        Ok(())
    }

    // ENet promotes the next unreliable message to reliable at sequence 65535.
    // The reliable baseline then advances and resets the unreliable counter.
    fn effective_delivery(&self, channel: u8, delivery: Delivery) -> Delivery {
        if delivery == Delivery::Unreliable
            && self.channels[channel as usize].sent_unreliable == u16::MAX
        {
            Delivery::Reliable
        } else {
            delivery
        }
    }

    fn send_inner(
        &mut self,
        channel_id: u8,
        payload: &[u8],
        delivery: Delivery,
    ) -> Result<(), PeerError> {
        let delivery = self.effective_delivery(channel_id, delivery);
        let single = self.mtu
            - if delivery == Delivery::Reliable {
                10
            } else {
                12
            };
        let channel = &mut self.channels[channel_id as usize];
        let start = if delivery == Delivery::Reliable {
            channel.sent_reliable.wrapping_add(1)
        } else {
            channel.sent_unreliable.wrapping_add(1)
        };
        let baseline = channel.sent_reliable;
        let fragmented = payload.len() > single;
        let chunk = if fragmented {
            self.mtu - 28
        } else {
            payload.len().max(1)
        };
        let count = payload.len().div_ceil(chunk).max(1);
        if delivery == Delivery::Reliable {
            channel.sent_reliable = baseline.wrapping_add(count as u16);
            channel.sent_unreliable = 0;
        } else if delivery == Delivery::Unreliable {
            channel.sent_unreliable = start;
        }
        if delivery == Delivery::Unsequenced {
            self.unsequenced_sent = self.unsequenced_sent.wrapping_add(1);
        }
        for number in 0..count {
            let offset = number * chunk;
            let data = payload[offset..payload.len().min(offset + chunk)].to_vec();
            let sequence = if delivery == Delivery::Reliable {
                start.wrapping_add(number as u16)
            } else {
                baseline
            };
            let body = if fragmented {
                let fragment = Fragment {
                    start_sequence: start,
                    fragment_count: count as u32,
                    fragment_number: number as u32,
                    total_length: payload.len() as u32,
                    fragment_offset: offset as u32,
                    data,
                };
                if delivery == Delivery::Reliable {
                    CommandBody::SendFragment(fragment)
                } else {
                    CommandBody::SendUnreliableFragment(fragment)
                }
            } else {
                match delivery {
                    Delivery::Reliable => CommandBody::SendReliable { data },
                    Delivery::Unreliable => CommandBody::SendUnreliable {
                        sequence: start,
                        data,
                    },
                    Delivery::Unsequenced => CommandBody::SendUnsequenced {
                        group: self.unsequenced_sent,
                        data,
                    },
                }
            };
            let flags = match delivery {
                Delivery::Reliable => FLAG_ACKNOWLEDGE,
                Delivery::Unsequenced => FLAG_UNSEQUENCED,
                _ => 0,
            };
            self.queue(
                Command {
                    channel_id,
                    reliable_sequence: sequence,
                    flags,
                    body,
                },
                delivery == Delivery::Reliable,
            )?;
        }
        Ok(())
    }

    pub(super) fn queue(&mut self, command: Command, reliable: bool) -> Result<(), PeerError> {
        if reliable && self.pending.len() == MAX_PENDING {
            return Err(PeerError::Busy);
        }
        let transmit_now = !reliable || self.window_available(&command);
        if transmit_now {
            self.transmit(&command)?;
        }
        if reliable {
            self.pending.push(Pending {
                command,
                first_sent: self.now,
                last_sent: self.now,
                attempts: u8::from(transmit_now),
                sent_times: if transmit_now {
                    vec![self.now as u16]
                } else {
                    Vec::new()
                },
            });
        }
        Ok(())
    }

    fn window_available(&self, command: &Command) -> bool {
        let in_flight: usize = self
            .pending
            .iter()
            .filter(|pending| pending.attempts != 0)
            .map(|pending| reliable_bytes(&pending.command))
            .sum();
        let window = self
            .connect
            .as_ref()
            .map_or(65536, |connect| connect.window_size.clamp(4096, 65536))
            as usize;
        in_flight + reliable_bytes(command) <= window
    }

    fn transmit(&mut self, command: &Command) -> Result<(), PeerError> {
        let datagram = Datagram {
            header: Header {
                peer_id: self.remote_peer,
                session_id: self.outgoing_session,
                sent_time: Some(self.now as u16),
            },
            commands: vec![command.clone()],
        };
        let bytes = encode(&datagram, &self.limits()).map_err(|_| PeerError::InvalidPacket)?;
        if self.datagrams.len() == MAX_DATAGRAMS
            || self.datagram_bytes + bytes.len() > MAX_QUEUE_BYTES
        {
            return Err(PeerError::Busy);
        }
        self.datagram_bytes += bytes.len();
        self.datagrams.push_back(bytes);
        self.last_sent = self.now;
        Ok(())
    }

    pub fn poll(&mut self, now: u64) -> Result<(), PeerError> {
        self.clock(now)?;
        if matches!(self.state, State::Disconnected | State::TimedOut) {
            return Ok(());
        }
        let stalled_reliable = self.channels.iter().any(|channel| {
            channel
                .reliable
                .values()
                .any(|assembly| now - assembly.created_at >= 5000)
        });
        for channel in &mut self.channels {
            channel
                .unreliable
                .retain(|_, assembly| now - assembly.created_at < 5000);
        }
        let expired = match self.state {
            State::Listening | State::AwaitingVerifyAck => {
                now - self.started >= self.config.handshake_timeout_ms
            }
            _ => now - self.last_received >= self.config.idle_timeout_ms,
        };
        if stalled_reliable
            || expired
            || self
                .pending
                .iter()
                .any(|p| now - p.first_sent >= self.config.idle_timeout_ms)
        {
            return self.terminal(State::TimedOut, Event::TimedOut);
        }
        let mut budget = 16;
        let pending_count = self.pending.len();
        let retry_start = self.retry_cursor;
        for offset in 0..pending_count {
            let index = (retry_start + offset) % pending_count;
            self.retry_cursor = (index + 1) % pending_count;
            let pending = &self.pending[index];
            let delay = self
                .config
                .retransmit_ms
                .saturating_mul(1 << pending.attempts.saturating_sub(1).min(3))
                .min(2000);
            if (pending.attempts == 0 && !self.window_available(&pending.command))
                || (pending.attempts != 0 && now - pending.last_sent < delay)
            {
                continue;
            }
            if pending.attempts >= 16 {
                return self.terminal(State::TimedOut, Event::TimedOut);
            }
            let command = pending.command.clone();
            match self.transmit(&command) {
                Ok(()) => {}
                Err(PeerError::Busy) => break,
                Err(error) => return Err(error),
            }
            let pending = &mut self.pending[index];
            pending.last_sent = now;
            pending.attempts += 1;
            pending.sent_times.push(now as u16);
            budget -= 1;
            if budget == 0 {
                break;
            }
        }
        if self.state == State::Connected
            && now - self.last_sent >= self.config.ping_interval_ms
            && !self
                .pending
                .iter()
                .any(|p| matches!(p.command.body, CommandBody::Ping))
        {
            let sequence = self.control_sequence.wrapping_add(1);
            let result = self.queue(
                Command {
                    channel_id: CONTROL_CHANNEL,
                    reliable_sequence: sequence,
                    flags: FLAG_ACKNOWLEDGE,
                    body: CommandBody::Ping,
                },
                true,
            );
            match result {
                Ok(()) => self.control_sequence = sequence,
                Err(PeerError::Busy) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    pub fn disconnect(&mut self, data: u32, now: u64) -> Result<(), PeerError> {
        if self.state != State::Connected {
            return Err(PeerError::NotConnected);
        }
        self.clock(now)?;
        let sequence = self.control_sequence.wrapping_add(1);
        self.queue(
            Command {
                channel_id: CONTROL_CHANNEL,
                reliable_sequence: sequence,
                flags: FLAG_ACKNOWLEDGE,
                body: CommandBody::Disconnect { data },
            },
            true,
        )?;
        self.control_sequence = sequence;
        self.state = State::Disconnecting;
        Ok(())
    }
}

fn reliable_bytes(command: &Command) -> usize {
    match &command.body {
        CommandBody::SendReliable { data } => data.len(),
        CommandBody::SendFragment(fragment) => fragment.data.len(),
        _ => 0,
    }
}
