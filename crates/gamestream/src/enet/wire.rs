use super::*;

pub fn decode(bytes: &[u8], limits: &Limits) -> Result<Datagram, Error> {
    validate_limits(limits)?;
    if bytes.len() > limits.mtu {
        return Err(Error::DatagramTooLarge);
    }
    let mut reader = Reader { bytes, offset: 0 };
    let packed = reader.u16()?;
    if packed & 0x4000 != 0 {
        return Err(Error::UnsupportedCompression);
    }
    let header = Header {
        peer_id: packed & 0x0fff,
        session_id: ((packed >> 12) & 3) as u8,
        sent_time: if packed & 0x8000 != 0 {
            Some(reader.u16()?)
        } else {
            None
        },
    };
    let mut commands = Vec::new();
    while reader.offset < bytes.len() {
        if commands.len() == MAX_COMMANDS {
            return Err(Error::TooManyCommands);
        }
        commands.push(read_command(&mut reader)?);
    }
    let datagram = Datagram { header, commands };
    validate_datagram(&datagram, limits)?;
    Ok(datagram)
}

pub fn encode(datagram: &Datagram, limits: &Limits) -> Result<Vec<u8>, Error> {
    validate_limits(limits)?;
    validate_datagram(datagram, limits)?;
    let length = encoded_length(datagram);
    if length > limits.mtu {
        return Err(Error::DatagramTooLarge);
    }
    let mut bytes = Vec::with_capacity(length);
    let mut packed = datagram.header.peer_id | u16::from(datagram.header.session_id) << 12;
    if datagram.header.sent_time.is_some() {
        packed |= 0x8000;
    }
    put16(&mut bytes, packed);
    if let Some(time) = datagram.header.sent_time {
        put16(&mut bytes, time);
    }
    for command in &datagram.commands {
        write_command(&mut bytes, command);
    }
    Ok(bytes)
}

fn validate_limits(limits: &Limits) -> Result<(), Error> {
    if limits.checksum {
        return Err(Error::UnsupportedChecksum);
    }
    if !(576..=4096).contains(&limits.mtu)
        || limits.channels == 0
        || !(1..=1024).contains(&limits.max_fragments)
        || !(1..=131072).contains(&limits.max_message_bytes)
    {
        return Err(Error::InvalidLimits);
    }
    Ok(())
}

fn validate_datagram(datagram: &Datagram, limits: &Limits) -> Result<(), Error> {
    if datagram.header.peer_id > UNCONNECTED_PEER || datagram.header.session_id > 3 {
        return Err(Error::InvalidHeader);
    }
    if datagram.commands.is_empty() {
        return Err(Error::EmptyDatagram);
    }
    if datagram.commands.len() > MAX_COMMANDS {
        return Err(Error::TooManyCommands);
    }
    for command in &datagram.commands {
        validate_command(command, limits)?;
        if command.flags & FLAG_ACKNOWLEDGE != 0 && datagram.header.sent_time.is_none() {
            return Err(Error::InvalidHeader);
        }
    }
    Ok(())
}

fn validate_command(command: &Command, limits: &Limits) -> Result<(), Error> {
    use CommandBody::*;
    let expected_flags = match &command.body {
        Acknowledge { .. } | SendUnreliable { .. } | SendUnreliableFragment(_) => 0,
        SendUnsequenced { .. } => FLAG_UNSEQUENCED,
        Disconnect { .. } => {
            if !matches!(command.flags, 0 | FLAG_ACKNOWLEDGE | FLAG_UNSEQUENCED) {
                return Err(Error::InvalidFlags);
            }
            command.flags
        }
        _ => FLAG_ACKNOWLEDGE,
    };
    if command.flags != expected_flags {
        return Err(Error::InvalidFlags);
    }
    match &command.body {
        Acknowledge { .. } => {
            if command.channel_id != CONTROL_CHANNEL && command.channel_id >= limits.channels {
                return Err(Error::InvalidChannel);
            }
        }
        SendReliable { data } | SendUnreliable { data, .. } | SendUnsequenced { data, .. } => {
            validate_payload(command.channel_id, data, limits)?;
        }
        SendFragment(fragment) | SendUnreliableFragment(fragment) => {
            validate_payload(command.channel_id, &fragment.data, limits)?;
            validate_fragment(fragment, limits)?;
            if matches!(command.body, SendFragment(_))
                && command.reliable_sequence
                    != fragment
                        .start_sequence
                        .wrapping_add(fragment.fragment_number as u16)
            {
                return Err(Error::InvalidFragment);
            }
        }
        _ if command.channel_id != CONTROL_CHANNEL => return Err(Error::InvalidChannel),
        Connect(value) => validate_handshake(&connect_fields(value), true, limits)?,
        VerifyConnect(value) => validate_handshake(value, false, limits)?,
        _ => {}
    }
    Ok(())
}

fn validate_payload(channel: u8, bytes: &[u8], limits: &Limits) -> Result<(), Error> {
    if channel >= limits.channels {
        return Err(Error::InvalidChannel);
    }
    if bytes.len() > limits.max_message_bytes as usize || bytes.len() > u16::MAX as usize {
        return Err(Error::PayloadTooLarge);
    }
    Ok(())
}

fn validate_fragment(fragment: &Fragment, limits: &Limits) -> Result<(), Error> {
    let end = u64::from(fragment.fragment_offset) + fragment.data.len() as u64;
    if fragment.fragment_count == 0
        || fragment.fragment_count > limits.max_fragments
        || fragment.fragment_number >= fragment.fragment_count
        || fragment.total_length == 0
        || fragment.total_length > limits.max_message_bytes
        || fragment.fragment_count > fragment.total_length
        || fragment.data.is_empty()
        || end > u64::from(fragment.total_length)
        || (fragment.fragment_number == 0 && fragment.fragment_offset != 0)
        || (fragment.fragment_number + 1 == fragment.fragment_count
            && end != u64::from(fragment.total_length))
    {
        return Err(Error::InvalidFragment);
    }
    Ok(())
}

fn validate_handshake(value: &VerifyConnect, initial: bool, limits: &Limits) -> Result<(), Error> {
    let session_valid = |id| id <= 3 || (initial && id == 255);
    if value.outgoing_peer_id >= UNCONNECTED_PEER
        || !session_valid(value.incoming_session_id)
        || !session_valid(value.outgoing_session_id)
        || !(576..=4096).contains(&value.mtu)
        || !(4096..=65536).contains(&value.window_size)
        || value.channel_count == 0
        || value.channel_count > u32::from(limits.channels)
    {
        return Err(Error::InvalidHandshake);
    }
    Ok(())
}

fn read_command(reader: &mut Reader<'_>) -> Result<Command, Error> {
    let code = reader.u8()?;
    if code & 0x30 != 0 {
        return Err(Error::InvalidFlags);
    }
    let channel_id = reader.u8()?;
    let reliable_sequence = reader.u16()?;
    use CommandBody::*;
    let body = match code & 15 {
        1 => Acknowledge {
            received_sequence: reader.u16()?,
            received_sent_time: reader.u16()?,
        },
        2 => {
            let fields = read_handshake(reader)?;
            Connect(with_connect_data(fields, reader.u32()?))
        }
        3 => VerifyConnect(read_handshake(reader)?),
        4 => Disconnect {
            data: reader.u32()?,
        },
        5 => Ping,
        6 => SendReliable {
            data: reader.payload()?,
        },
        7 => SendUnreliable {
            sequence: reader.u16()?,
            data: reader.payload()?,
        },
        8 => SendFragment(read_fragment(reader)?),
        9 => SendUnsequenced {
            group: reader.u16()?,
            data: reader.payload()?,
        },
        10 => BandwidthLimit {
            incoming: reader.u32()?,
            outgoing: reader.u32()?,
        },
        11 => ThrottleConfigure {
            interval: reader.u32()?,
            acceleration: reader.u32()?,
            deceleration: reader.u32()?,
        },
        12 => SendUnreliableFragment(read_fragment(reader)?),
        _ => return Err(Error::UnknownCommand),
    };
    Ok(Command {
        channel_id,
        reliable_sequence,
        flags: code & 0xc0,
        body,
    })
}

fn read_handshake(reader: &mut Reader<'_>) -> Result<VerifyConnect, Error> {
    Ok(VerifyConnect {
        outgoing_peer_id: reader.u16()?,
        incoming_session_id: reader.u8()?,
        outgoing_session_id: reader.u8()?,
        mtu: reader.u32()?,
        window_size: reader.u32()?,
        channel_count: reader.u32()?,
        incoming_bandwidth: reader.u32()?,
        outgoing_bandwidth: reader.u32()?,
        packet_throttle_interval: reader.u32()?,
        packet_throttle_acceleration: reader.u32()?,
        packet_throttle_deceleration: reader.u32()?,
        connect_id: reader.take(4)?.try_into().expect("four checked bytes"),
    })
}

fn read_fragment(reader: &mut Reader<'_>) -> Result<Fragment, Error> {
    let start_sequence = reader.u16()?;
    let length = reader.u16()?;
    let fragment_count = reader.u32()?;
    let fragment_number = reader.u32()?;
    let total_length = reader.u32()?;
    let fragment_offset = reader.u32()?;
    Ok(Fragment {
        start_sequence,
        fragment_count,
        fragment_number,
        total_length,
        fragment_offset,
        data: reader.take(usize::from(length))?.to_vec(),
    })
}

fn command_code(body: &CommandBody) -> u8 {
    use CommandBody::*;
    match body {
        Acknowledge { .. } => 1,
        Connect(_) => 2,
        VerifyConnect(_) => 3,
        Disconnect { .. } => 4,
        Ping => 5,
        SendReliable { .. } => 6,
        SendUnreliable { .. } => 7,
        SendFragment(_) => 8,
        SendUnsequenced { .. } => 9,
        BandwidthLimit { .. } => 10,
        ThrottleConfigure { .. } => 11,
        SendUnreliableFragment(_) => 12,
    }
}

fn write_command(bytes: &mut Vec<u8>, command: &Command) {
    bytes.push(command_code(&command.body) | command.flags);
    bytes.push(command.channel_id);
    put16(bytes, command.reliable_sequence);
    use CommandBody::*;
    match &command.body {
        Acknowledge {
            received_sequence,
            received_sent_time,
        } => {
            put16(bytes, *received_sequence);
            put16(bytes, *received_sent_time);
        }
        Connect(value) => {
            write_handshake(bytes, &connect_fields(value));
            put32(bytes, value.data);
        }
        VerifyConnect(value) => write_handshake(bytes, value),
        Disconnect { data } => put32(bytes, *data),
        Ping => {}
        SendReliable { data } => write_payload(bytes, data),
        SendUnreliable { sequence, data } => {
            put16(bytes, *sequence);
            write_payload(bytes, data);
        }
        SendUnsequenced { group, data } => {
            put16(bytes, *group);
            write_payload(bytes, data);
        }
        SendFragment(value) | SendUnreliableFragment(value) => write_fragment(bytes, value),
        BandwidthLimit { incoming, outgoing } => {
            put32(bytes, *incoming);
            put32(bytes, *outgoing);
        }
        ThrottleConfigure {
            interval,
            acceleration,
            deceleration,
        } => {
            put32(bytes, *interval);
            put32(bytes, *acceleration);
            put32(bytes, *deceleration);
        }
    }
}

fn write_handshake(bytes: &mut Vec<u8>, value: &VerifyConnect) {
    put16(bytes, value.outgoing_peer_id);
    bytes.push(value.incoming_session_id);
    bytes.push(value.outgoing_session_id);
    for number in [
        value.mtu,
        value.window_size,
        value.channel_count,
        value.incoming_bandwidth,
        value.outgoing_bandwidth,
        value.packet_throttle_interval,
        value.packet_throttle_acceleration,
        value.packet_throttle_deceleration,
    ] {
        put32(bytes, number);
    }
    bytes.extend_from_slice(&value.connect_id);
}

fn write_fragment(bytes: &mut Vec<u8>, value: &Fragment) {
    put16(bytes, value.start_sequence);
    put16(bytes, value.data.len() as u16);
    for number in [
        value.fragment_count,
        value.fragment_number,
        value.total_length,
        value.fragment_offset,
    ] {
        put32(bytes, number);
    }
    bytes.extend_from_slice(&value.data);
}

fn write_payload(bytes: &mut Vec<u8>, data: &[u8]) {
    put16(bytes, data.len() as u16);
    bytes.extend_from_slice(data);
}
fn put16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_be_bytes());
}
fn put32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn encoded_length(datagram: &Datagram) -> usize {
    let header = if datagram.header.sent_time.is_some() {
        4
    } else {
        2
    };
    header
        + datagram
            .commands
            .iter()
            .map(|command| {
                use CommandBody::*;
                match &command.body {
                    Acknowledge { .. } | Disconnect { .. } => 8,
                    Connect(_) => 48,
                    VerifyConnect(_) => 44,
                    Ping => 4,
                    SendReliable { data } => 6 + data.len(),
                    SendUnreliable { data, .. } | SendUnsequenced { data, .. } => 8 + data.len(),
                    SendFragment(value) | SendUnreliableFragment(value) => 24 + value.data.len(),
                    BandwidthLimit { .. } => 12,
                    ThrottleConfigure { .. } => 16,
                }
            })
            .sum::<usize>()
}

fn connect_fields(value: &Connect) -> VerifyConnect {
    VerifyConnect {
        outgoing_peer_id: value.outgoing_peer_id,
        incoming_session_id: value.incoming_session_id,
        outgoing_session_id: value.outgoing_session_id,
        mtu: value.mtu,
        window_size: value.window_size,
        channel_count: value.channel_count,
        incoming_bandwidth: value.incoming_bandwidth,
        outgoing_bandwidth: value.outgoing_bandwidth,
        packet_throttle_interval: value.packet_throttle_interval,
        packet_throttle_acceleration: value.packet_throttle_acceleration,
        packet_throttle_deceleration: value.packet_throttle_deceleration,
        connect_id: value.connect_id,
    }
}

fn with_connect_data(value: VerifyConnect, data: u32) -> Connect {
    Connect {
        outgoing_peer_id: value.outgoing_peer_id,
        incoming_session_id: value.incoming_session_id,
        outgoing_session_id: value.outgoing_session_id,
        mtu: value.mtu,
        window_size: value.window_size,
        channel_count: value.channel_count,
        incoming_bandwidth: value.incoming_bandwidth,
        outgoing_bandwidth: value.outgoing_bandwidth,
        packet_throttle_interval: value.packet_throttle_interval,
        packet_throttle_acceleration: value.packet_throttle_acceleration,
        packet_throttle_deceleration: value.packet_throttle_deceleration,
        connect_id: value.connect_id,
        data,
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], Error> {
        let end = self.offset.checked_add(count).ok_or(Error::Truncated)?;
        let bytes = self.bytes.get(self.offset..end).ok_or(Error::Truncated)?;
        self.offset = end;
        Ok(bytes)
    }
    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_be_bytes(
            self.take(2)?.try_into().expect("two checked bytes"),
        ))
    }
    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().expect("four checked bytes"),
        ))
    }
    fn payload(&mut self) -> Result<Vec<u8>, Error> {
        let length = self.u16()?;
        Ok(self.take(usize::from(length))?.to_vec())
    }
}
