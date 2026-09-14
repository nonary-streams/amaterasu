//! RFC 6716 packet framing validation, not entropy decoding or audio validation.
use super::AudioError;

pub(super) fn decoded_frames(packet: &[u8]) -> Result<u32, AudioError> {
    let toc = *packet.first().ok_or(AudioError::InvalidOpus)?;
    let configuration = toc >> 3;
    let frame_samples = match configuration {
        0..=11 => [480, 960, 1920, 2880][usize::from(configuration & 3)],
        12..=15 => [480, 960][usize::from(configuration & 1)],
        _ => [120, 240, 480, 960][usize::from(configuration & 3)],
    };
    let count = match toc & 3 {
        0 => 1,
        1 => {
            if !(packet.len() - 1).is_multiple_of(2) {
                return Err(AudioError::InvalidOpus);
            }
            2
        }
        2 => {
            let mut offset = 1;
            let first_length = read_length(packet, &mut offset)?;
            if first_length > packet.len() - offset {
                return Err(AudioError::InvalidOpus);
            }
            2
        }
        _ => multiple_frames(packet)?,
    };
    let samples = frame_samples * count;
    if samples > 5760 {
        return Err(AudioError::InvalidOpus);
    }
    Ok(samples)
}

fn read_length(packet: &[u8], offset: &mut usize) -> Result<usize, AudioError> {
    let first = *packet.get(*offset).ok_or(AudioError::InvalidOpus)?;
    *offset += 1;
    if first < 252 {
        return Ok(usize::from(first));
    }
    let second = *packet.get(*offset).ok_or(AudioError::InvalidOpus)?;
    *offset += 1;
    Ok(usize::from(first) + 4 * usize::from(second))
}

fn multiple_frames(packet: &[u8]) -> Result<u32, AudioError> {
    let control = *packet.get(1).ok_or(AudioError::InvalidOpus)?;
    let count = usize::from(control & 63);
    if !(1..=48).contains(&count) {
        return Err(AudioError::InvalidOpus);
    }
    let mut offset = 2;
    let mut padding = 0;
    if control & 64 != 0 {
        loop {
            let amount = *packet.get(offset).ok_or(AudioError::InvalidOpus)?;
            offset += 1;
            padding += if amount == 255 {
                254
            } else {
                usize::from(amount)
            };
            if padding > packet.len() - offset {
                return Err(AudioError::InvalidOpus);
            }
            if amount != 255 {
                break;
            }
        }
    }
    let end = packet.len() - padding;
    let data = &packet[..end];
    if control & 128 == 0 {
        if !(end - offset).is_multiple_of(count) {
            return Err(AudioError::InvalidOpus);
        }
    } else {
        let mut described_bytes = 0;
        for _ in 1..count {
            described_bytes += read_length(data, &mut offset)?;
        }
        if described_bytes > end - offset {
            return Err(AudioError::InvalidOpus);
        }
    }
    Ok(count as u32)
}
