use super::*;

#[derive(Clone)]
pub(super) struct Assembly {
    slots: u16,
    pub(super) created_at: u64,
    total: usize,
    pieces: Vec<Option<(usize, Vec<u8>)>>,
}

impl Assembly {
    fn complete(data: Vec<u8>, now: u64) -> Self {
        Self {
            slots: 1,
            created_at: now,
            total: data.len(),
            pieces: vec![Some((0, data))],
        }
    }

    fn fragment(fragment: &Fragment, reliable: bool, now: u64) -> Self {
        Self {
            slots: if reliable {
                fragment.fragment_count as u16
            } else {
                1
            },
            created_at: now,
            total: fragment.total_length as usize,
            pieces: vec![None; fragment.fragment_count as usize],
        }
    }

    fn insert(&mut self, fragment: Fragment) -> Result<(), PeerError> {
        if self.total != fragment.total_length as usize
            || self.pieces.len() != fragment.fragment_count as usize
        {
            return Err(PeerError::InvalidPacket);
        }
        let offset = fragment.fragment_offset as usize;
        let end = offset + fragment.data.len();
        let index = fragment.fragment_number as usize;
        if let Some(existing) = &self.pieces[index] {
            return if existing.0 == offset && existing.1 == fragment.data {
                Ok(())
            } else {
                Err(PeerError::InvalidPacket)
            };
        }
        if self
            .pieces
            .iter()
            .flatten()
            .any(|(start, bytes)| offset < start + bytes.len() && *start < end)
        {
            return Err(PeerError::InvalidPacket);
        }
        self.pieces[index] = Some((offset, fragment.data));
        if self.pieces.iter().all(Option::is_some) && !self.ready() {
            return Err(PeerError::InvalidPacket);
        }
        Ok(())
    }

    fn ready(&self) -> bool {
        self.pieces.iter().all(Option::is_some)
            && self
                .pieces
                .iter()
                .flatten()
                .map(|(_, p)| p.len())
                .sum::<usize>()
                == self.total
    }

    fn identical_complete(&self, other: &Self) -> bool {
        self.slots == 1 && self.total == other.total && self.pieces == other.pieces
    }

    fn finish(self) -> Vec<u8> {
        let mut bytes = vec![0; self.total];
        for (offset, piece) in self.pieces.into_iter().flatten() {
            bytes[offset..offset + piece.len()].copy_from_slice(&piece);
        }
        bytes
    }
}

impl Peer {
    /// A rejected datagram has no effect, including its earlier valid commands.
    /// Staging clones only bounded state; this favors atomic admission over throughput.
    pub fn receive(&mut self, bytes: &[u8], now: u64) -> Result<ReceiveStatus, PeerError> {
        if now < self.now {
            return Err(PeerError::ClockRegression);
        }
        let packet = decode(bytes, &self.limits()).map_err(|_| PeerError::InvalidPacket)?;
        if packet.header.peer_id == UNCONNECTED_PEER {
            if self.state != State::Listening {
                return Ok(ReceiveStatus { associated: false });
            }
            if packet.commands.len() != 1 {
                return Err(PeerError::InvalidPacket);
            }
            let CommandBody::Connect(connect) = &packet.commands[0].body else {
                return Err(PeerError::InvalidPacket);
            };
            if connect.data != self.config.expected_connect_data {
                return Ok(ReceiveStatus { associated: false });
            }
        } else if packet.header.peer_id != self.config.local_peer_id
            || packet.header.session_id != self.incoming_session
            || matches!(self.state, State::Listening | State::TimedOut)
        {
            return Ok(ReceiveStatus { associated: false });
        }
        if self.state == State::Disconnected
            && (packet.commands.len() != 1
                || !matches!(packet.commands[0].body, CommandBody::Disconnect { .. }))
        {
            return Ok(ReceiveStatus { associated: false });
        }
        let mut staged = self.clone();
        staged.clock(now)?;
        for command in packet.commands {
            staged.command(command, packet.header.sent_time)?;
        }
        staged.check_receive_budget()?;
        staged.last_received = now;
        *self = staged;
        Ok(ReceiveStatus { associated: true })
    }

    fn check_receive_budget(&self) -> Result<(), PeerError> {
        let assemblies = self
            .channels
            .iter()
            .flat_map(|c| c.reliable.values().chain(c.unreliable.values()));
        let (count, bytes) =
            assemblies.fold((0, 0), |(n, size), assembly| (n + 1, size + assembly.total));
        if count > 128 || bytes > MAX_QUEUE_BYTES {
            Err(PeerError::Busy)
        } else {
            Ok(())
        }
    }

    fn connect(&mut self, connect: Connect) -> Result<(), PeerError> {
        if self.state != State::Listening {
            return Err(PeerError::InvalidPacket);
        }
        self.remote_peer = connect.outgoing_peer_id;
        self.outgoing_session = connect.incoming_session_id.wrapping_add(1) & 3;
        self.incoming_session = connect.outgoing_session_id.wrapping_add(1) & 3;
        self.mtu = self.config.mtu.min(connect.mtu.clamp(576, 4096) as usize);
        self.channels
            .truncate((connect.channel_count as usize).min(self.channels.len()));
        let verify = VerifyConnect {
            outgoing_peer_id: self.config.local_peer_id,
            incoming_session_id: self.outgoing_session,
            outgoing_session_id: self.incoming_session,
            mtu: self.mtu as u32,
            window_size: connect.window_size.clamp(4096, 65536),
            channel_count: self.channels.len() as u32,
            incoming_bandwidth: 0,
            outgoing_bandwidth: 0,
            packet_throttle_interval: connect.packet_throttle_interval,
            packet_throttle_acceleration: connect.packet_throttle_acceleration,
            packet_throttle_deceleration: connect.packet_throttle_deceleration,
            connect_id: connect.connect_id,
        };
        self.control_sequence = 1;
        self.queue(
            Command {
                channel_id: CONTROL_CHANNEL,
                reliable_sequence: 1,
                flags: FLAG_ACKNOWLEDGE,
                body: CommandBody::VerifyConnect(verify),
            },
            true,
        )?;
        self.connect = Some(connect);
        self.state = State::AwaitingVerifyAck;
        Ok(())
    }

    fn acknowledge(&mut self, channel: u8, sequence: u16, sent_time: u16) -> Result<(), PeerError> {
        let Some(index) = self.pending.iter().position(|p| {
            p.command.channel_id == channel
                && p.command.reliable_sequence == sequence
                && p.sent_times.contains(&sent_time)
        }) else {
            return Ok(());
        };
        let pending = self.pending.remove(index);
        match pending.command.body {
            CommandBody::VerifyConnect(_) if self.state == State::AwaitingVerifyAck => {
                self.state = State::Connected;
                self.event(Event::Connected {
                    channels: self.channels.len() as u8,
                    mtu: self.mtu,
                })?;
            }
            CommandBody::Disconnect { data } if self.state == State::Disconnecting => {
                self.terminal(State::Disconnected, Event::Disconnected { data })?;
            }
            _ => {}
        }
        Ok(())
    }

    fn command(&mut self, command: Command, sent_time: Option<u16>) -> Result<(), PeerError> {
        if self.state == State::Disconnected
            && !matches!(command.body, CommandBody::Disconnect { .. })
        {
            return Err(PeerError::NotConnected);
        }
        if let CommandBody::Connect(connect) = command.body {
            return self.connect(connect);
        }
        if let CommandBody::Acknowledge {
            received_sequence,
            received_sent_time,
        } = command.body
        {
            return self.acknowledge(command.channel_id, received_sequence, received_sent_time);
        }
        if !matches!(
            self.state,
            State::Connected | State::Disconnecting | State::Disconnected
        ) {
            return Err(PeerError::NotConnected);
        }
        if matches!(command.body, CommandBody::Disconnect { .. })
            && self.state != State::Disconnected
        {
            self.datagrams.clear();
            self.datagram_bytes = 0;
        }
        if command.flags & FLAG_ACKNOWLEDGE != 0 {
            self.queue(
                Command {
                    channel_id: command.channel_id,
                    reliable_sequence: command.reliable_sequence,
                    flags: 0,
                    body: CommandBody::Acknowledge {
                        received_sequence: command.reliable_sequence,
                        received_sent_time: sent_time.ok_or(PeerError::InvalidPacket)?,
                    },
                },
                false,
            )?;
        }
        let channel = command.channel_id;
        let baseline = command.reliable_sequence;
        match command.body {
            CommandBody::Disconnect { data } => {
                if self.state == State::Disconnected {
                    Ok(())
                } else {
                    self.terminal(State::Disconnected, Event::Disconnected { data })
                }
            }
            CommandBody::Ping
            | CommandBody::BandwidthLimit { .. }
            | CommandBody::ThrottleConfigure { .. } => Ok(()),
            CommandBody::SendReliable { data } => {
                self.reliable(channel, baseline, Assembly::complete(data, self.now))
            }
            CommandBody::SendFragment(fragment) => self.fragment(channel, baseline, fragment, true),
            CommandBody::SendUnreliable { sequence, data } => self.unreliable(
                channel,
                baseline,
                sequence,
                Assembly::complete(data, self.now),
            ),
            CommandBody::SendUnreliableFragment(fragment) => {
                self.fragment(channel, baseline, fragment, false)
            }
            CommandBody::SendUnsequenced { group, data } => self.unsequenced(channel, group, data),
            _ => Err(PeerError::InvalidPacket),
        }
    }

    fn reliable(
        &mut self,
        channel: u8,
        sequence: u16,
        assembly: Assembly,
    ) -> Result<(), PeerError> {
        let state = &mut self.channels[channel as usize];
        let distance = sequence.wrapping_sub(state.received_reliable);
        if distance == 0 || distance >= 32768 {
            return Ok(());
        }
        if distance > RECEIVE_WINDOW {
            return Err(PeerError::Busy);
        }
        if let Some((&start, existing)) = state
            .reliable
            .iter()
            .find(|(start, value)| sequence.wrapping_sub(**start) < value.slots)
        {
            return if start == sequence && existing.identical_complete(&assembly) {
                Ok(())
            } else {
                Err(PeerError::InvalidPacket)
            };
        }
        state.reliable.insert(sequence, assembly);
        self.flush(channel)
    }

    fn fragment(
        &mut self,
        channel: u8,
        baseline: u16,
        fragment: Fragment,
        reliable: bool,
    ) -> Result<(), PeerError> {
        let state = &mut self.channels[channel as usize];
        let start = fragment.start_sequence;
        if reliable && baseline != start.wrapping_add(fragment.fragment_number as u16) {
            return Err(PeerError::InvalidPacket);
        }
        let distance =
            if reliable { start } else { baseline }.wrapping_sub(state.received_reliable);
        if distance >= 32768 || (reliable && distance == 0) {
            return Ok(());
        }
        if distance > RECEIVE_WINDOW
            || (reliable
                && distance as usize + fragment.fragment_count as usize - 1
                    > RECEIVE_WINDOW as usize)
        {
            return Err(PeerError::Busy);
        }
        if reliable {
            for (&other, assembly) in &state.reliable {
                if other != start
                    && (other.wrapping_sub(start) < fragment.fragment_count as u16
                        || start.wrapping_sub(other) < assembly.slots)
                {
                    return Err(PeerError::InvalidPacket);
                }
            }
            state
                .reliable
                .entry(start)
                .or_insert_with(|| Assembly::fragment(&fragment, true, self.now))
                .insert(fragment)?;
        } else {
            state
                .unreliable
                .entry((baseline, start))
                .or_insert_with(|| Assembly::fragment(&fragment, false, self.now))
                .insert(fragment)?;
        }
        self.flush(channel)
    }

    fn unreliable(
        &mut self,
        channel: u8,
        baseline: u16,
        sequence: u16,
        assembly: Assembly,
    ) -> Result<(), PeerError> {
        let state = &mut self.channels[channel as usize];
        let distance = baseline.wrapping_sub(state.received_reliable);
        if distance >= 32768 {
            return Ok(());
        }
        if distance > RECEIVE_WINDOW {
            return Err(PeerError::Busy);
        }
        state
            .unreliable
            .entry((baseline, sequence))
            .or_insert(assembly);
        self.flush(channel)
    }

    fn flush(&mut self, channel: u8) -> Result<(), PeerError> {
        loop {
            let state = &mut self.channels[channel as usize];
            let next = state.received_reliable.wrapping_add(1);
            if !state.reliable.get(&next).is_some_and(Assembly::ready) {
                break;
            }
            let assembly = state.reliable.remove(&next).unwrap();
            state.received_reliable = next.wrapping_add(assembly.slots - 1);
            state.received_unreliable = 0;
            self.event(Event::Receive {
                channel,
                delivery: Delivery::Reliable,
                data: assembly.finish(),
            })?;
        }
        let state = &mut self.channels[channel as usize];
        state.unreliable.retain(|(baseline, sequence), _| {
            let distance = baseline.wrapping_sub(state.received_reliable);
            distance < 32768 && (distance != 0 || *sequence > state.received_unreliable)
        });
        let baseline = state.received_reliable;
        let mut ready: Vec<_> = state
            .unreliable
            .iter()
            .filter(|((b, _), value)| *b == baseline && value.ready())
            .map(|(&(b, seq), _)| (seq, (b, seq)))
            .collect();
        ready.sort_unstable();
        for (_, key) in ready {
            let state = &mut self.channels[channel as usize];
            let assembly = state.unreliable.remove(&key).unwrap();
            state.received_unreliable = key.1;
            self.event(Event::Receive {
                channel,
                delivery: Delivery::Unreliable,
                data: assembly.finish(),
            })?;
        }
        Ok(())
    }

    fn unsequenced(&mut self, channel: u8, group: u16, data: Vec<u8>) -> Result<(), PeerError> {
        if self.unsequenced_seen.contains(&group) {
            return Ok(());
        }
        if let Some(latest) = self.unsequenced_latest {
            let forward = group.wrapping_sub(latest);
            if forward >= 32768 && latest.wrapping_sub(group) >= 1024 {
                return Ok(());
            }
            if forward < 32768 {
                self.unsequenced_latest = Some(group);
            }
        } else {
            self.unsequenced_latest = Some(group);
        }
        let latest = self.unsequenced_latest.unwrap();
        self.unsequenced_seen
            .retain(|seen| latest.wrapping_sub(*seen) < 1024);
        self.unsequenced_seen.push_back(group);
        self.event(Event::Receive {
            channel,
            delivery: Delivery::Unsequenced,
            data,
        })
    }
}
