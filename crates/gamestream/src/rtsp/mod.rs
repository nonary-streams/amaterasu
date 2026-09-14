//! Strict, bounded RTSP/1.0 framing. Framing errors poison a decoder: close its transport.
mod decoder;
pub mod negotiation;
mod response;

pub use decoder::Decoder;
pub use response::serialize_response;

pub const MAX_HEADER_BYTES: usize = 16 * 1024;
pub const MAX_BODY_BYTES: usize = 64 * 1024;
pub const MAX_HEADERS: usize = 64;
pub const MAX_LINE_BYTES: usize = 2048;
pub const MAX_BUFFER_BYTES: usize = 2 * (MAX_HEADER_BYTES + MAX_BODY_BYTES);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    LimitExceeded,
    InvalidRequest,
    UnsupportedVersion,
    UnsupportedMethod,
    InvalidHeader,
    DuplicateHeader,
    InvalidSequence,
    InvalidLength,
    UnsupportedFraming,
    Truncated,
    Poisoned,
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "RTSP protocol error: {self:?}")
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Method {
    Options,
    Describe,
    Setup,
    Announce,
    Play,
    Teardown,
}

impl Method {
    fn parse(value: &str) -> Result<Self, Error> {
        match value {
            "OPTIONS" => Ok(Self::Options),
            "DESCRIBE" => Ok(Self::Describe),
            "SETUP" => Ok(Self::Setup),
            "ANNOUNCE" => Ok(Self::Announce),
            "PLAY" => Ok(Self::Play),
            "TEARDOWN" => Ok(Self::Teardown),
            _ => Err(Error::UnsupportedMethod),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct Request {
    pub method: Method,
    /// Opaque target: parsing does not authorize a URL, session, or stream ID.
    pub target: String,
    pub cseq: u32,
    /// Names are normalized to ASCII lowercase; duplicate names are rejected.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

fn token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

fn header_value(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte == b'\t' || (b' '..=b'~').contains(&byte))
}

fn decimal(value: &str) -> Option<u32> {
    (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
}

#[cfg(test)]
mod tests;
