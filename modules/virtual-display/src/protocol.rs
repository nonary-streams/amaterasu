//! Original byte codec for the published Sunshine control ABI (3.6/3.7).
//! Encode fields explicitly: no Rust/C++ padding or pointers cross the wire.
use crate::model::Specification;

pub const NAMESPACE: [u8; 16] = [
    0x84, 0x42, 0x86, 0xa2, 0xfe, 0x77, 0x36, 0x43, 0xa8, 0x28, 0x00, 0xfe, 0xec, 0x89, 0xeb, 0xac,
];
pub const LEASE_TIMEOUT_MS: u32 = 10_000;
pub const STATE_BYTES: usize = 1696;

pub const fn ioctl(function: u32, access: u32) -> u32 {
    (0x22 << 16) | (access << 14) | (function << 2)
}
pub const VERSION: u32 = ioctl(0x900, 0);
pub const CREATE_OWNED: u32 = ioctl(0x90c, 3);
pub const REMOVE: u32 = ioctl(0x902, 3);
pub const FEED: u32 = ioctl(0x903, 3);
pub const QUERY_LEASE: u32 = ioctl(0x905, 1);
pub const QUERY_STATE: u32 = ioctl(0x908, 1);

#[derive(Clone, Copy, Debug)]
pub struct Binding {
    pub adapter_low: u32,
    pub adapter_high: i32,
    pub target: u32,
}

pub fn check_version(bytes: &[u8]) -> Result<(), &'static str> {
    check(bytes, 24)?;
    if u16::from_le_bytes(bytes[16..18].try_into().unwrap()) != 3
        || !(6..=7).contains(&u16::from_le_bytes(bytes[18..20].try_into().unwrap()))
    {
        return Err("incompatible_driver");
    }
    Ok(())
}

pub fn create(spec: &Specification, lease: u64, display: u64, capability: &[u8; 32]) -> Vec<u8> {
    let mut bytes = packet(128);
    put_u64(&mut bytes, 16, lease);
    put_u64(&mut bytes, 24, display);
    for (offset, value) in [
        (32, spec.width),
        (36, spec.height),
        (40, 600),
        (44, 340),
        (48, spec.refresh_millihz),
        (52, LEASE_TIMEOUT_MS),
    ] {
        put_u32(&mut bytes, offset, value);
    }
    bytes[56..56 + spec.name.len()].copy_from_slice(spec.name.as_bytes());
    // Retain stable identity, default driver HDR luminance. No global manifest edits.
    bytes[96..128].copy_from_slice(capability);
    bytes
}

pub fn created(bytes: &[u8], lease: u64, display: u64) -> Result<Binding, &'static str> {
    check(bytes, 56)?;
    if read_u64(bytes, 16) != lease
        || read_u64(bytes, 24) != display
        || read_u32(bytes, 48) != LEASE_TIMEOUT_MS
    {
        return Err("invalid_driver_response");
    }
    Ok(Binding {
        adapter_low: read_u32(bytes, 32),
        adapter_high: read_u32(bytes, 36) as i32,
        target: read_u32(bytes, 40),
    })
}

pub fn lease_request(lease: u64) -> Vec<u8> {
    let mut bytes = packet(32);
    put_u64(&mut bytes, 16, lease);
    put_u32(&mut bytes, 24, LEASE_TIMEOUT_MS);
    bytes
}

pub fn remove_request(lease: u64, display: u64) -> Vec<u8> {
    let mut bytes = packet(32);
    put_u64(&mut bytes, 16, lease);
    put_u64(&mut bytes, 24, display);
    bytes
}

pub fn pending_departure(bytes: &[u8], lease: u64) -> Result<bool, &'static str> {
    check(bytes, 48)?;
    if read_u64(bytes, 16) != lease
        || read_u32(bytes, 36) > 1
        || read_u32(bytes, 24) > 16
        || read_u32(bytes, 40) > 16
    {
        return Err("invalid_driver_response");
    }
    Ok(read_u32(bytes, 40) != 0)
}

pub fn present(bytes: &[u8], lease: u64, display: u64) -> Result<bool, &'static str> {
    check(bytes, STATE_BYTES)?;
    let count = read_u32(bytes, 24) as usize;
    if count > 16 || read_u32(bytes, 16) > 16 || read_u32(bytes, 20) > 16 {
        return Err("invalid_driver_response");
    }
    Ok((0..count).any(|index| {
        let offset = 32 + 104 * index;
        read_u32(bytes, offset) == 2
            && read_u64(bytes, offset + 8) == lease
            && read_u64(bytes, offset + 16) == display
    }))
}

fn check(bytes: &[u8], length: usize) -> Result<(), &'static str> {
    if bytes.len() != length || bytes[..16] != NAMESPACE {
        Err("invalid_driver_response")
    } else {
        Ok(())
    }
}
fn packet(size: usize) -> Vec<u8> {
    let mut bytes = vec![0; size];
    bytes[..16].copy_from_slice(&NAMESPACE);
    bytes
}
fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_wire_layout_uses_documented_offsets_and_owned_capability() {
        let spec = Specification {
            identity: "00000000-0000-0000-0000-000000000001".into(),
            width: 1920,
            height: 1080,
            refresh_millihz: 59940,
            name: "Example".into(),
        };
        let encoded = create(&spec, 0x6000000000000001, 42, &[0x7c; 32]);
        assert_eq!(encoded.len(), 128);
        assert_eq!(&encoded[..16], &NAMESPACE);
        assert_eq!(read_u64(&encoded, 16), 0x6000000000000001);
        assert_eq!(read_u64(&encoded, 24), 42);
        assert_eq!(read_u32(&encoded, 48), 59940);
        assert_eq!(&encoded[56..64], b"Example\0");
        assert_eq!(&encoded[96..], &[0x7c; 32]);
        assert_eq!(CREATE_OWNED, 0x22e430);
        assert_eq!(remove_request(9, 42).len(), 32);
        assert_eq!(read_u64(&remove_request(9, 42), 24), 42);
    }
    #[test]
    fn malformed_and_incompatible_responses_fail_closed() {
        let mut version = packet(24);
        version[16..18].copy_from_slice(&3u16.to_le_bytes());
        version[18..20].copy_from_slice(&7u16.to_le_bytes());
        check_version(&version).unwrap();
        version[18] = 5;
        assert!(check_version(&version).is_err());
        for size in [0, 15, 23, 25] {
            assert!(check_version(&vec![0; size]).is_err());
        }
        let mut state = packet(STATE_BYTES);
        put_u32(&mut state, 24, 17);
        assert!(present(&state, 1, 1).is_err());
        let mut reply = packet(56);
        put_u64(&mut reply, 16, 7);
        put_u64(&mut reply, 24, 8);
        put_u32(&mut reply, 48, LEASE_TIMEOUT_MS);
        created(&reply, 7, 8).unwrap();
        assert!(created(&reply, 7, 9).is_err());
        reply[0] ^= 1;
        assert!(created(&reply, 7, 8).is_err());
    }
}

pub fn parse_interfaces(list: &[u16]) -> Result<Vec<String>, &'static str> {
    if list.len() < 2 || list[list.len() - 2..] != [0, 0] {
        return Err("invalid_device_interfaces");
    }
    let mut result = Vec::new();
    for path in list.split(|unit| *unit == 0) {
        if path.is_empty() {
            break;
        }
        if result.len() >= 8 || path.len() > 4096 {
            return Err("invalid_device_interfaces");
        }
        result.push(String::from_utf16(path).map_err(|_| "invalid_device_interfaces")?);
    }
    Ok(result)
}

pub fn display_path(units: &[u16]) -> Result<Option<String>, &'static str> {
    let end = units
        .iter()
        .position(|unit| *unit == 0)
        .ok_or("invalid_display_identity")?;
    if end == 0 {
        return Ok(None);
    }
    String::from_utf16(&units[..end])
        .map(Some)
        .map_err(|_| "invalid_display_identity")
}

#[cfg(test)]
mod native_string_tests {
    use super::*;
    #[test]
    fn malformed_native_strings_fail_closed() {
        assert_eq!(parse_interfaces(&[65, 0, 0]).unwrap(), vec!["A"]);
        assert!(parse_interfaces(&[65, 0]).is_err());
        assert!(parse_interfaces(&[0xd800, 0, 0]).is_err());
        assert!(parse_interfaces(&vec![65; 8193]).is_err());
        assert_eq!(display_path(&[65, 0]).unwrap(), Some("A".into()));
        assert_eq!(display_path(&[0]).unwrap(), None);
        assert!(display_path(&[65]).is_err());
        assert!(display_path(&[0xd800, 0]).is_err());
    }
}
