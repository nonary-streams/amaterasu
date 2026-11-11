//! Original byte codec for the backward-compatible Sunshine control ABI (3.6+).
//! Encode fields explicitly: no Rust/C++ padding or pointers cross the wire.
use crate::model::Specification;

pub const NAMESPACE: [u8; 16] = [
    0x84, 0x42, 0x86, 0xa2, 0xfe, 0x77, 0x36, 0x43, 0xa8, 0x28, 0x00, 0xfe, 0xec, 0x89, 0xeb, 0xac,
];
pub const LEASE_TIMEOUT_MS: u32 = 10_000;
pub const STATE_BYTES: usize = 1696;
const REQUESTED_RESOLUTION_CREATE_FLAG: u32 = 0x2;
const REQUESTED_RESOLUTION_STATE_FLAG: u32 = 0x4;

pub const fn ioctl(function: u32, access: u32) -> u32 {
    (0x22 << 16) | (access << 14) | (function << 2)
}
pub const VERSION: u32 = ioctl(0x900, 0);
pub const CREATE_OWNED: u32 = ioctl(0x90c, 3);
pub const RECLAIM: u32 = ioctl(0x90d, 3);
pub const REMOVE: u32 = ioctl(0x902, 3);
pub const FEED: u32 = ioctl(0x903, 3);
pub const QUERY_LEASE: u32 = ioctl(0x905, 1);
pub const QUERY_STATE: u32 = ioctl(0x908, 1);
pub const SET_PERMANENT_COUNT: u32 = ioctl(0x906, 3);
pub const QUERY_PERMANENT_COUNT: u32 = ioctl(0x907, 1);

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub struct Binding {
    pub adapter_low: u32,
    pub adapter_high: i32,
    pub target: u32,
}

pub fn check_version(bytes: &[u8]) -> Result<(), &'static str> {
    check(bytes, 24)?;
    if u16::from_le_bytes(bytes[16..18].try_into().unwrap()) != 3
        || u16::from_le_bytes(bytes[18..20].try_into().unwrap()) < 6
    {
        return Err("incompatible_driver");
    }
    Ok(())
}

pub fn minor_version(bytes: &[u8]) -> Result<u16, &'static str> {
    check_version(bytes)?;
    Ok(u16::from_le_bytes(bytes[18..20].try_into().unwrap()))
}

pub fn requested_resolution_catalog_supported(driver_minor: u16) -> bool {
    driver_minor >= 8
}

pub fn create(
    spec: &Specification,
    lease: u64,
    display: u64,
    capability: &[u8; 32],
    driver_minor: u16,
) -> Result<Vec<u8>, &'static str> {
    if driver_minor < 6 {
        return Err("incompatible_driver");
    }
    if spec.requested_resolution_only && !requested_resolution_catalog_supported(driver_minor) {
        return Err("unsupported_requested_resolution_catalog");
    }
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
    if spec.requested_resolution_only {
        put_u32(&mut bytes, 88, REQUESTED_RESOLUTION_CREATE_FLAG);
    }
    // Retain stable identity, default driver HDR luminance. No global manifest edits.
    bytes[96..128].copy_from_slice(capability);
    Ok(bytes)
}

pub fn permanent_count_request(count: u8) -> Result<Vec<u8>, &'static str> {
    if count > 4 {
        return Err("invalid_permanent_count");
    }
    let mut bytes = packet(76);
    for (offset, value) in [
        (16, u32::from(count)),
        (24, 1920),
        (28, 1080),
        (32, 600),
        (36, 340),
        (40, 60_000),
    ] {
        put_u32(&mut bytes, offset, value);
    }
    bytes[44..61].copy_from_slice(b"Amaterasu Display");
    Ok(bytes)
}

pub fn permanent_count(bytes: &[u8]) -> Result<u32, &'static str> {
    check(bytes, 80)?;
    let current = read_u32(bytes, 16);
    let maximum = read_u32(bytes, 20);
    let temporary = read_u32(bytes, 24);
    if maximum == 0 || maximum > 16 || current > maximum || temporary > 16 {
        return Err("invalid_driver_response");
    }
    Ok(current)
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

pub fn reclaim_request(display: u64, lease: u64, capability: &[u8; 32]) -> Vec<u8> {
    let mut bytes = packet(72);
    put_u64(&mut bytes, 16, display);
    put_u64(&mut bytes, 24, lease);
    put_u32(&mut bytes, 32, LEASE_TIMEOUT_MS);
    bytes[40..72].copy_from_slice(capability);
    bytes
}

pub fn reclaimed(bytes: &[u8], lease: u64, display: u64) -> Result<(), &'static str> {
    check(bytes, 48)?;
    if read_u64(bytes, 16) != lease
        || read_u64(bytes, 24) != display
        || read_u32(bytes, 36) != LEASE_TIMEOUT_MS
        || read_u32(bytes, 40) != 1
        || read_u32(bytes, 44) != 0
    {
        return Err("invalid_driver_response");
    }
    Ok(())
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

pub fn lease_display_count(bytes: &[u8], lease: u64) -> Result<u32, &'static str> {
    check(bytes, 48)?;
    if read_u64(bytes, 16) != lease || read_u32(bytes, 24) > 16 {
        return Err("invalid_driver_response");
    }
    Ok(read_u32(bytes, 24))
}

pub fn present(bytes: &[u8], lease: u64, display: u64) -> Result<bool, &'static str> {
    check(bytes, STATE_BYTES)?;
    let count = read_u32(bytes, 24) as usize;
    if count > 16 || read_u32(bytes, 16) > 16 || read_u32(bytes, 20) > 16 {
        return Err("invalid_driver_response");
    }
    let matches: Vec<_> = (0..count)
        .filter(|index| {
            let offset = 32 + 104 * index;
            read_u32(bytes, offset) == 2 && read_u64(bytes, offset + 16) == display
        })
        .collect();
    if matches.len() > 1 {
        return Err("invalid_driver_response");
    }
    Ok(matches
        .first()
        .is_some_and(|index| read_u64(bytes, 32 + 104 * index + 8) == lease))
}

pub fn occupied_by_other_lease(
    bytes: &[u8],
    lease: u64,
    display: u64,
) -> Result<bool, &'static str> {
    check(bytes, STATE_BYTES)?;
    let count = read_u32(bytes, 24) as usize;
    if count > 16 || read_u32(bytes, 16) > 16 || read_u32(bytes, 20) > 16 {
        return Err("invalid_driver_response");
    }
    let mut found = None;
    for index in 0..count {
        let offset = 32 + 104 * index;
        if read_u32(bytes, offset) == 2 && read_u64(bytes, offset + 16) == display {
            if found.is_some() {
                return Err("invalid_driver_response");
            }
            found = Some(read_u64(bytes, offset + 8));
        }
    }
    Ok(found.is_some_and(|found_lease| found_lease != lease))
}

pub fn matches_specification(
    bytes: &[u8],
    lease: u64,
    display: u64,
    spec: &Specification,
    driver_minor: u16,
) -> Result<bool, &'static str> {
    if driver_minor < 6 {
        return Err("incompatible_driver");
    }
    if spec.requested_resolution_only && !requested_resolution_catalog_supported(driver_minor) {
        return Err("unsupported_requested_resolution_catalog");
    }
    check(bytes, STATE_BYTES)?;
    let count = read_u32(bytes, 24) as usize;
    if count > 16 || read_u32(bytes, 16) > 16 || read_u32(bytes, 20) > 16 {
        return Err("invalid_driver_response");
    }
    for index in 0..count {
        let offset = 32 + 104 * index;
        if read_u32(bytes, offset) == 2
            && read_u64(bytes, offset + 8) == lease
            && read_u64(bytes, offset + 16) == display
        {
            let name = &bytes[offset + 72..offset + 104];
            let end = name
                .iter()
                .position(|byte| *byte == 0)
                .ok_or("invalid_driver_response")?;
            return Ok(read_u32(bytes, offset + 52) == spec.width
                && (!requested_resolution_catalog_supported(driver_minor)
                    || (read_u32(bytes, offset + 4) & REQUESTED_RESOLUTION_STATE_FLAG != 0)
                        == spec.requested_resolution_only)
                && read_u32(bytes, offset + 56) == spec.height
                && read_u32(bytes, offset + 60) == 600
                && read_u32(bytes, offset + 64) == 340
                && read_u32(bytes, offset + 68) == spec.refresh_millihz
                && &name[..end] == spec.name.as_bytes());
        }
    }
    Ok(false)
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
    fn permanent_count_codec_is_bounded_and_validates_independent_readback() {
        let request = permanent_count_request(2).unwrap();
        assert_eq!(request.len(), 76);
        assert_eq!(&request[..16], &NAMESPACE);
        assert_eq!(read_u32(&request, 16), 2);
        assert_eq!(read_u32(&request, 20), 0);
        assert_eq!(read_u32(&request, 24), 1920);
        assert_eq!(read_u32(&request, 28), 1080);
        assert_eq!(read_u32(&request, 40), 60_000);
        assert_eq!(&request[44..62], b"Amaterasu Display\0");
        assert!(permanent_count_request(5).is_err());
        assert_eq!(read_u32(&permanent_count_request(0).unwrap(), 16), 0);

        let mut observed = packet(80);
        put_u32(&mut observed, 16, 2);
        put_u32(&mut observed, 20, 4);
        assert_eq!(permanent_count(&observed), Ok(2));
        put_u32(&mut observed, 16, 5);
        assert!(permanent_count(&observed).is_err());
        put_u32(&mut observed, 16, 2);
        put_u32(&mut observed, 24, 17);
        assert!(permanent_count(&observed).is_err());
        put_u32(&mut observed, 24, 0);
        observed[0] ^= 1;
        assert!(permanent_count(&observed).is_err());
        for length in [0, 16, 79, 81] {
            assert!(permanent_count(&vec![0; length]).is_err());
        }
    }

    #[test]
    fn fixed_wire_layout_uses_documented_offsets_and_owned_capability() {
        let spec = Specification {
            requested_resolution_only: false,
            hdr_enabled: false,
            identity: "00000000-0000-0000-0000-000000000001".into(),
            width: 1920,
            height: 1080,
            refresh_millihz: 59940,
            name: "Example".into(),
        };
        let encoded = create(&spec, 0x6000000000000001, 42, &[0x7c; 32], 7).unwrap();
        assert_eq!(encoded.len(), 128);
        assert_eq!(&encoded[..16], &NAMESPACE);
        assert_eq!(read_u64(&encoded, 16), 0x6000000000000001);
        assert_eq!(read_u64(&encoded, 24), 42);
        assert_eq!(read_u32(&encoded, 48), 59940);
        assert_eq!(&encoded[56..64], b"Example\0");
        assert_eq!(&encoded[96..], &[0x7c; 32]);
        assert_eq!(CREATE_OWNED, 0x22e430);
        assert_eq!(RECLAIM, 0x22e434);
        let reclaim = reclaim_request(42, 0x6000000000000002, &[0x7c; 32]);
        assert_eq!(reclaim.len(), 72);
        assert_eq!(read_u64(&reclaim, 16), 42);
        assert_eq!(read_u64(&reclaim, 24), 0x6000000000000002);
        assert_eq!(read_u32(&reclaim, 32), LEASE_TIMEOUT_MS);
        assert_eq!(&reclaim[40..], &[0x7c; 32]);
        assert_eq!(remove_request(9, 42).len(), 32);
        assert_eq!(read_u64(&remove_request(9, 42), 24), 42);
    }
    #[test]
    fn additive_driver_minor_versions_preserve_the_owned_display_contract() {
        let mut version = packet(24);
        version[16..18].copy_from_slice(&3u16.to_le_bytes());
        for minor in [6u16, 7, 8, 9, u16::MAX] {
            version[18..20].copy_from_slice(&minor.to_le_bytes());
            assert!(check_version(&version).is_ok(), "rejected 3.{minor}");
        }
        for major in [0u16, 2, 4] {
            version[16..18].copy_from_slice(&major.to_le_bytes());
            assert!(check_version(&version).is_err());
        }
        version[16..18].copy_from_slice(&3u16.to_le_bytes());
        version[0] ^= 1;
        assert!(check_version(&version).is_err());
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
        let mut reclaimed_reply = packet(48);
        put_u64(&mut reclaimed_reply, 16, 10);
        put_u64(&mut reclaimed_reply, 24, 11);
        put_u32(&mut reclaimed_reply, 36, LEASE_TIMEOUT_MS);
        put_u32(&mut reclaimed_reply, 40, 1);
        reclaimed(&reclaimed_reply, 10, 11).unwrap();
        put_u32(&mut reclaimed_reply, 40, 2);
        assert!(reclaimed(&reclaimed_reply, 10, 11).is_err());
    }

    #[test]
    fn private_catalog_create_requires_actual_producer_support() {
        let mut specification = Specification {
            requested_resolution_only: false,
            hdr_enabled: false,
            identity: "00000000-0000-0000-0000-000000000001".into(),
            width: 1920,
            height: 1080,
            refresh_millihz: 60_000,
            name: "Private display".into(),
        };
        for minor in [6, 7, 8, 9] {
            let request = create(&specification, 7, 8, &[0x7c; 32], minor).unwrap();
            assert_eq!(read_u32(&request, 88), 0);
        }
        specification.requested_resolution_only = true;
        for minor in [6, 7] {
            assert_eq!(
                create(&specification, 7, 8, &[0x7c; 32], minor),
                Err("unsupported_requested_resolution_catalog")
            );
        }
        for minor in [8, 9] {
            let request = create(&specification, 7, 8, &[0x7c; 32], minor).unwrap();
            assert_eq!(read_u32(&request, 88), REQUESTED_RESOLUTION_CREATE_FLAG);
            assert_eq!(read_u32(&request, 32), 1920);
            assert_eq!(read_u32(&request, 36), 1080);
            assert_eq!(&request[96..], &[0x7c; 32]);
        }
    }

    #[test]
    fn state_requires_unique_exact_specification() {
        let spec = Specification {
            requested_resolution_only: false,
            hdr_enabled: false,
            identity: "00000000-0000-0000-0000-000000000001".into(),
            width: 1920,
            height: 1080,
            refresh_millihz: 60000,
            name: "Example".into(),
        };
        let mut state = packet(STATE_BYTES);
        put_u32(&mut state, 24, 1);
        put_u32(&mut state, 32, 2);
        put_u64(&mut state, 40, 7);
        put_u64(&mut state, 48, 8);
        put_u32(&mut state, 84, spec.width);
        put_u32(&mut state, 88, spec.height);
        put_u32(&mut state, 92, 600);
        put_u32(&mut state, 96, 340);
        put_u32(&mut state, 100, spec.refresh_millihz);
        state[104..111].copy_from_slice(b"Example");
        assert!(present(&state, 7, 8).unwrap());
        assert!(occupied_by_other_lease(&state, 9, 8).unwrap());
        assert!(!occupied_by_other_lease(&state, 7, 8).unwrap());
        assert!(matches_specification(&state, 7, 8, &spec, 8).unwrap());
        let private = Specification {
            requested_resolution_only: true,
            ..spec.clone()
        };
        assert!(!matches_specification(&state, 7, 8, &private, 8).unwrap());
        put_u32(&mut state, 36, REQUESTED_RESOLUTION_STATE_FLAG);
        assert!(matches_specification(&state, 7, 8, &private, 8).unwrap());
        assert!(!matches_specification(&state, 7, 8, &spec, 8).unwrap());
        // Older producers do not define the additive catalog state bit.
        assert!(matches_specification(&state, 7, 8, &spec, 7).unwrap());
        assert!(matches_specification(&state, 7, 8, &private, 7).is_err());
        put_u32(&mut state, 84, 1280);
        assert!(!matches_specification(&state, 7, 8, &private, 8).unwrap());
        put_u32(&mut state, 84, spec.width);
        put_u32(&mut state, 36, 0);
        state[104] = b'X';
        assert!(!matches_specification(&state, 7, 8, &spec, 8).unwrap());
        put_u32(&mut state, 24, 2);
        put_u32(&mut state, 136, 2);
        put_u64(&mut state, 152, 8);
        assert!(present(&state, 7, 8).is_err());
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
