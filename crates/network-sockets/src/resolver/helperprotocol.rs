//! Bounded, versioned wire format shared by the DNS worker and its parent.
//!
//! This file depends only on the standard library so the worker can be compiled
//! independently for the provider's target architecture.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

pub const MAX_HOST_BYTES: usize = 253;
pub const MAX_ADDRESSES: usize = 256;
pub const MAX_ERROR_BYTES: usize = 256;
pub const MAX_REQUEST_BYTES: usize = 8 + 2 + 2 + MAX_HOST_BYTES;
pub const MAX_RESPONSE_BYTES: usize = 8 + 1 + 2 + MAX_ADDRESSES * 27;

const REQUEST_MAGIC: &[u8; 8] = b"AMDNSQ01";
const RESPONSE_MAGIC: &[u8; 8] = b"AMDNSR01";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Request {
    pub host: String,
    pub port: u16,
}

impl Request {
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        validate_host(&self.host)?;
        let mut bytes = Vec::with_capacity(12 + self.host.len());
        bytes.extend_from_slice(REQUEST_MAGIC);
        bytes.extend_from_slice(&(self.host.len() as u16).to_be_bytes());
        bytes.extend_from_slice(&self.port.to_be_bytes());
        bytes.extend_from_slice(self.host.as_bytes());
        Ok(bytes)
    }

    pub fn write_to(&self, mut writer: impl Write) -> io::Result<()> {
        writer.write_all(&self.encode()?)
    }

    pub fn read_from(mut reader: impl Read) -> io::Result<Self> {
        let mut header = [0; 12];
        reader.read_exact(&mut header)?;
        if &header[..8] != REQUEST_MAGIC {
            return Err(invalid_data("invalid resolver request version"));
        }
        let length = u16::from_be_bytes([header[8], header[9]]) as usize;
        if length == 0 || length > MAX_HOST_BYTES {
            return Err(invalid_data("resolver host exceeds its byte bound"));
        }
        let mut host = vec![0; length];
        reader.read_exact(&mut host)?;
        let mut extra = [0];
        if reader.read(&mut extra)? != 0 {
            return Err(invalid_data("trailing resolver request bytes"));
        }
        let host =
            String::from_utf8(host).map_err(|_| invalid_data("resolver host is not UTF-8"))?;
        validate_host(&host).map_err(|error| invalid_data(&error.to_string()))?;
        Ok(Self {
            host,
            port: u16::from_be_bytes([header[10], header[11]]),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Response {
    Addresses(Vec<SocketAddr>),
    Error(RemoteError),
}

impl Response {
    pub fn from_result(result: io::Result<Vec<SocketAddr>>) -> Self {
        match result {
            Ok(addresses) => Self::Addresses(addresses),
            Err(error) => Self::Error(RemoteError::from_io(&error)),
        }
    }

    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let mut bytes = RESPONSE_MAGIC.to_vec();
        match self {
            Self::Addresses(addresses) => {
                if addresses.len() > MAX_ADDRESSES {
                    return Err(invalid_data("resolver address count exceeds its bound"));
                }
                bytes.push(0);
                bytes.extend_from_slice(&(addresses.len() as u16).to_be_bytes());
                for (index, address) in addresses.iter().enumerate() {
                    if addresses[..index].contains(address) {
                        return Err(invalid_data("duplicate resolver address"));
                    }
                    match address {
                        SocketAddr::V4(address) => {
                            bytes.push(4);
                            bytes.extend_from_slice(&address.port().to_be_bytes());
                            bytes.extend_from_slice(&address.ip().octets());
                        }
                        SocketAddr::V6(address) => {
                            bytes.push(6);
                            bytes.extend_from_slice(&address.port().to_be_bytes());
                            bytes.extend_from_slice(&address.ip().octets());
                            bytes.extend_from_slice(&address.flowinfo().to_be_bytes());
                            bytes.extend_from_slice(&address.scope_id().to_be_bytes());
                        }
                    }
                }
            }
            Self::Error(error) => {
                if error.message.len() > MAX_ERROR_BYTES {
                    return Err(invalid_data("resolver error exceeds its byte bound"));
                }
                bytes.push(1);
                bytes.push(error.kind.code());
                bytes.extend_from_slice(&(error.message.len() as u16).to_be_bytes());
                bytes.extend_from_slice(error.message.as_bytes());
            }
        }
        Ok(bytes)
    }

    pub fn write_to(&self, mut writer: impl Write) -> io::Result<()> {
        writer.write_all(&self.encode()?)
    }

    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(invalid_data("resolver response exceeds its byte bound"));
        }
        let mut decoder = Decoder { bytes, offset: 0 };
        if decoder.take(8)? != RESPONSE_MAGIC {
            return Err(invalid_data("invalid resolver response version"));
        }
        let response = match decoder.byte()? {
            0 => {
                let count = decoder.u16()? as usize;
                if count > MAX_ADDRESSES {
                    return Err(invalid_data("resolver address count exceeds its bound"));
                }
                let mut addresses = Vec::with_capacity(count);
                for _ in 0..count {
                    let family = decoder.byte()?;
                    let port = decoder.u16()?;
                    let address = match family {
                        4 => {
                            let octets: [u8; 4] = decoder.take(4)?.try_into().unwrap();
                            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(octets), port))
                        }
                        6 => {
                            let octets: [u8; 16] = decoder.take(16)?.try_into().unwrap();
                            SocketAddr::V6(SocketAddrV6::new(
                                Ipv6Addr::from(octets),
                                port,
                                decoder.u32()?,
                                decoder.u32()?,
                            ))
                        }
                        _ => return Err(invalid_data("invalid resolver address family")),
                    };
                    if addresses.contains(&address) {
                        return Err(invalid_data("duplicate resolver address"));
                    }
                    addresses.push(address);
                }
                Self::Addresses(addresses)
            }
            1 => {
                let kind = ErrorKind::from_code(decoder.byte()?)?;
                let length = decoder.u16()? as usize;
                if length > MAX_ERROR_BYTES {
                    return Err(invalid_data("resolver error exceeds its byte bound"));
                }
                let message = std::str::from_utf8(decoder.take(length)?)
                    .map_err(|_| invalid_data("resolver error is not UTF-8"))?
                    .to_owned();
                Self::Error(RemoteError { kind, message })
            }
            _ => return Err(invalid_data("invalid resolver response kind")),
        };
        if decoder.offset != bytes.len() {
            return Err(invalid_data("trailing resolver response bytes"));
        }
        Ok(response)
    }
}

/// Platform-independent error categories; raw OS error values are not portable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    NotFound,
    PermissionDenied,
    InvalidInput,
    TimedOut,
    Interrupted,
    InvalidData,
    Other,
}

impl ErrorKind {
    fn code(self) -> u8 {
        match self {
            Self::NotFound => 1,
            Self::PermissionDenied => 2,
            Self::InvalidInput => 3,
            Self::TimedOut => 4,
            Self::Interrupted => 5,
            Self::InvalidData => 6,
            Self::Other => 7,
        }
    }

    fn from_code(code: u8) -> io::Result<Self> {
        match code {
            1 => Ok(Self::NotFound),
            2 => Ok(Self::PermissionDenied),
            3 => Ok(Self::InvalidInput),
            4 => Ok(Self::TimedOut),
            5 => Ok(Self::Interrupted),
            6 => Ok(Self::InvalidData),
            7 => Ok(Self::Other),
            _ => Err(invalid_data("invalid resolver error kind")),
        }
    }

    fn io_kind(self) -> io::ErrorKind {
        match self {
            Self::NotFound => io::ErrorKind::NotFound,
            Self::PermissionDenied => io::ErrorKind::PermissionDenied,
            Self::InvalidInput => io::ErrorKind::InvalidInput,
            Self::TimedOut => io::ErrorKind::TimedOut,
            Self::Interrupted => io::ErrorKind::Interrupted,
            Self::InvalidData => io::ErrorKind::InvalidData,
            Self::Other => io::ErrorKind::Other,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteError {
    pub kind: ErrorKind,
    pub message: String,
}

impl RemoteError {
    pub fn from_io(error: &io::Error) -> Self {
        let kind = match error.kind() {
            io::ErrorKind::NotFound => ErrorKind::NotFound,
            io::ErrorKind::PermissionDenied => ErrorKind::PermissionDenied,
            io::ErrorKind::InvalidInput => ErrorKind::InvalidInput,
            io::ErrorKind::TimedOut => ErrorKind::TimedOut,
            io::ErrorKind::Interrupted => ErrorKind::Interrupted,
            io::ErrorKind::InvalidData => ErrorKind::InvalidData,
            _ => ErrorKind::Other,
        };
        let mut message = error.to_string();
        let mut length = message.len().min(MAX_ERROR_BYTES);
        while !message.is_char_boundary(length) {
            length -= 1;
        }
        message.truncate(length);
        Self { kind, message }
    }

    pub fn into_io(self) -> io::Error {
        io::Error::new(self.kind.io_kind(), self.message)
    }
}

fn validate_host(host: &str) -> io::Result<()> {
    if host.is_empty() || host.len() > MAX_HOST_BYTES || host.chars().any(char::is_control) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "resolver host must contain 1..=253 UTF-8 bytes without control characters",
        ));
    }
    Ok(())
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_owned())
}

struct Decoder<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Decoder<'a> {
    fn take(&mut self, count: usize) -> io::Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(count)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| invalid_data("truncated resolver response"))?;
        let result = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(result)
    }

    fn byte(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trip_preserves_utf8_host_and_port() {
        let request = Request {
            host: "é.example".into(),
            port: 47989,
        };
        let mut bytes = Vec::new();
        request.write_to(&mut bytes).unwrap();
        assert_eq!(Request::read_from(bytes.as_slice()).unwrap(), request);
        assert!(bytes.len() <= MAX_REQUEST_BYTES);
        let largest = Request {
            host: "a".repeat(MAX_HOST_BYTES),
            port: u16::MAX,
        };
        let bytes = largest.encode().unwrap();
        assert_eq!(bytes.len(), MAX_REQUEST_BYTES);
        assert_eq!(Request::read_from(bytes.as_slice()).unwrap(), largest);
    }

    #[test]
    fn requests_reject_invalid_hosts_versions_lengths_and_trailing_bytes() {
        for host in [
            "".to_owned(),
            "a".repeat(254),
            "a\0b".into(),
            "a\nb".into(),
            "a\u{85}b".into(),
        ] {
            assert!(Request { host, port: 0 }.encode().is_err());
        }
        let valid = Request {
            host: "127.0.0.1".into(),
            port: 0,
        }
        .encode()
        .unwrap();
        for length in 0..valid.len() {
            assert!(Request::read_from(&valid[..length]).is_err());
        }
        let mut wrong_version = valid.clone();
        wrong_version[7] = b'2';
        assert!(Request::read_from(wrong_version.as_slice()).is_err());
        let mut oversized_length = valid.clone();
        oversized_length[8..10].copy_from_slice(&u16::MAX.to_be_bytes());
        assert!(Request::read_from(oversized_length.as_slice()).is_err());
        let mut trailing = valid.clone();
        trailing.push(0);
        assert!(Request::read_from(trailing.as_slice()).is_err());
        let mut bad_utf8 = valid;
        bad_utf8[12] = 0xff;
        assert!(Request::read_from(bad_utf8.as_slice()).is_err());
    }

    #[test]
    fn response_round_trip_preserves_ipv6_flow_and_scope() {
        let response = Response::Addresses(vec![
            "127.0.0.1:47989".parse().unwrap(),
            SocketAddr::V6(SocketAddrV6::new("fe80::1".parse().unwrap(), 48010, 37, 9)),
        ]);
        let mut bytes = Vec::new();
        response.write_to(&mut bytes).unwrap();
        assert_eq!(Response::decode(&bytes).unwrap(), response);
    }

    #[test]
    fn maximum_response_is_bounded_and_round_trips() {
        let response = Response::Addresses(
            (0..MAX_ADDRESSES)
                .map(|index| {
                    SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, index as u16, 0, 0))
                })
                .collect(),
        );
        let bytes = response.encode().unwrap();
        assert_eq!(bytes.len(), MAX_RESPONSE_BYTES);
        assert_eq!(Response::decode(&bytes).unwrap(), response);
    }

    #[test]
    fn responses_reject_truncation_trailing_bytes_invalid_families_and_duplicate_addresses() {
        let response = Response::Addresses(vec!["127.0.0.1:1".parse().unwrap()]);
        let bytes = response.encode().unwrap();
        for length in 0..bytes.len() {
            assert!(Response::decode(&bytes[..length]).is_err());
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(Response::decode(&trailing).is_err());
        let mut bad_family = bytes.clone();
        bad_family[11] = 9;
        assert!(Response::decode(&bad_family).is_err());
        let mut duplicate = bytes.clone();
        duplicate[10] = 2;
        duplicate.extend_from_slice(&bytes[11..]);
        assert!(Response::decode(&duplicate).is_err());
        assert!(
            Response::Addresses(vec!["127.0.0.1:1".parse().unwrap(); 2])
                .encode()
                .is_err()
        );
        assert!(Response::decode(&vec![0; MAX_RESPONSE_BYTES + 1]).is_err());
    }

    #[test]
    fn remote_errors_preserve_stable_kinds_and_bound_utf8_messages() {
        for kind in [
            io::ErrorKind::NotFound,
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::InvalidInput,
            io::ErrorKind::TimedOut,
            io::ErrorKind::Interrupted,
            io::ErrorKind::InvalidData,
            io::ErrorKind::Other,
        ] {
            let response = Response::from_result(Err(io::Error::new(kind, "é".repeat(200))));
            let bytes = response.encode().unwrap();
            let Response::Error(error) = Response::decode(&bytes).unwrap() else {
                panic!("error expected")
            };
            assert_eq!(error.message.len(), MAX_ERROR_BYTES);
            assert_eq!(error.into_io().kind(), kind);
        }
    }

    #[test]
    fn remote_errors_reject_unknown_kinds_oversized_lengths_and_invalid_utf8() {
        let mut bytes = Response::from_result(Err(io::Error::other("x")))
            .encode()
            .unwrap();
        bytes[9] = 255;
        assert!(Response::decode(&bytes).is_err());
        bytes[9] = 7;
        bytes[10..12].copy_from_slice(&257u16.to_be_bytes());
        assert!(Response::decode(&bytes).is_err());
        bytes[10..12].copy_from_slice(&1u16.to_be_bytes());
        bytes[12] = 0xff;
        assert!(Response::decode(&bytes).is_err());
    }
}
