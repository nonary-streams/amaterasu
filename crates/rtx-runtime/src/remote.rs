//! Bounded random access to the pinned NVIDIA wheel through the selected network provider.
//!
//! The HTTP agent must use the Network SDK and enforce HTTPS, certificate
//! verification, redirect policy and finite connection/read timeouts. The wheel
//! is untrusted: the caller must verify each extracted file against pinned hashes.
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use ureq::Agent;

const MAX_WHEEL_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_TRANSFER_BYTES: u64 = 256 * 1024 * 1024;
const RANGE_BYTES: u64 = 1024 * 1024;
const READ_BUFFER_BYTES: usize = 64 * 1024;

/// A seekable, bounded HTTP source suitable for a ZIP reader.
///
/// Reads are served from a single one-MiB window. Each cache miss requires a
/// valid byte-range response for the exact requested interval. The source never
/// constructs its own network connector or follows a fallback transport.
pub struct RemoteWheel {
    agent: Agent,
    cancel: Arc<AtomicBool>,
    progress: Arc<AtomicU64>,
    length: u64,
    position: u64,
    transferred: u64,
    cache_start: u64,
    cache: Vec<u8>,
}

impl RemoteWheel {
    pub fn open(
        agent: &Agent,
        cancel: Arc<AtomicBool>,
        progress: Arc<AtomicU64>,
    ) -> io::Result<Self> {
        check_cancelled(&cancel)?;
        let response = agent
            .head(crate::WHEEL_URL)
            .header("accept-encoding", "identity")
            .call()
            .map_err(http_error)?;
        check_cancelled(&cancel)?;
        if response.status().as_u16() != 200 {
            return Err(invalid("wheel HEAD response must be HTTP 200"));
        }
        require_identity(response.headers())?;
        let length = header_u64(response.headers(), "content-length")?;
        if length == 0 || length > MAX_WHEEL_BYTES {
            return Err(invalid("wheel length outside permitted bounds"));
        }
        Ok(Self {
            agent: agent.clone(),
            cancel,
            progress,
            length,
            position: 0,
            transferred: 0,
            cache_start: 0,
            cache: Vec::new(),
        })
    }

    fn fetch_window(&mut self) -> io::Result<()> {
        check_cancelled(&self.cancel)?;
        let start = self.position;
        let amount = (self.length - start).min(RANGE_BYTES);
        if self
            .transferred
            .checked_add(amount)
            .is_none_or(|n| n > MAX_TRANSFER_BYTES)
        {
            return Err(invalid("wheel transfer exceeds permitted limit"));
        }
        let end = start + amount - 1;
        let mut response = self
            .agent
            .get(crate::WHEEL_URL)
            .header("accept-encoding", "identity")
            .header("range", format!("bytes={start}-{end}"))
            .call()
            .map_err(http_error)?;
        check_cancelled(&self.cancel)?;
        validate_range_response(
            response.status().as_u16(),
            response.headers(),
            start,
            end,
            self.length,
        )?;
        let mut data = Vec::with_capacity(amount as usize);
        let mut reader = response.body_mut().as_reader();
        let mut buffer = [0_u8; READ_BUFFER_BYTES];
        loop {
            check_cancelled(&self.cancel)?;
            let remaining = amount.saturating_add(1).saturating_sub(data.len() as u64);
            if remaining == 0 {
                return Err(invalid("range response exceeded expected length"));
            }
            let read_limit = remaining.min(buffer.len() as u64) as usize;
            let read = reader.read(&mut buffer[..read_limit])?;
            if read == 0 {
                break;
            }
            self.transferred = self.transferred.saturating_add(read as u64);
            self.progress.fetch_add(read as u64, Ordering::Relaxed);
            if self.transferred > MAX_TRANSFER_BYTES {
                return Err(invalid("wheel transfer exceeds permitted limit"));
            }
            data.extend_from_slice(&buffer[..read]);
        }
        if data.len() as u64 != amount {
            return Err(invalid("range response has incorrect length"));
        }
        self.cache_start = start;
        self.cache = data;
        Ok(())
    }
}

impl Read for RemoteWheel {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        check_cancelled(&self.cancel)?;
        if output.is_empty() || self.position >= self.length {
            return Ok(0);
        }
        let cache_end = self.cache_start + self.cache.len() as u64;
        if self.position < self.cache_start || self.position >= cache_end {
            self.fetch_window()?;
        }
        let offset = (self.position - self.cache_start) as usize;
        let available = &self.cache[offset..];
        let count = output.len().min(available.len());
        output[..count].copy_from_slice(&available[..count]);
        self.position += count as u64;
        Ok(count)
    }
}

impl Seek for RemoteWheel {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        check_cancelled(&self.cancel)?;
        let absolute = match position {
            SeekFrom::Start(value) => i128::from(value),
            SeekFrom::Current(value) => i128::from(self.position) + i128::from(value),
            SeekFrom::End(value) => i128::from(self.length) + i128::from(value),
        };
        self.position = u64::try_from(absolute)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid wheel seek"))?;
        Ok(self.position)
    }
}

fn validate_range_response(
    status: u16,
    headers: &ureq::http::HeaderMap,
    start: u64,
    end: u64,
    total: u64,
) -> io::Result<()> {
    if status != 206 {
        return Err(invalid("wheel range response must be HTTP 206"));
    }
    require_identity(headers)?;
    let expected = format!("bytes {start}-{end}/{total}");
    let mut ranges = headers.get_all("content-range").iter();
    let actual = ranges.next().and_then(|value| value.to_str().ok());
    if actual != Some(expected.as_str()) || ranges.next().is_some() {
        return Err(invalid("wheel content range mismatch"));
    }
    if header_u64(headers, "content-length")? != end - start + 1 {
        return Err(invalid("wheel range content length mismatch"));
    }
    Ok(())
}

fn require_identity(headers: &ureq::http::HeaderMap) -> io::Result<()> {
    if headers
        .get_all("content-encoding")
        .iter()
        .any(|value| value.as_bytes() != b"identity")
    {
        return Err(invalid("encoded wheel responses are unsupported"));
    }
    Ok(())
}

fn header_u64(headers: &ureq::http::HeaderMap, name: &str) -> io::Result<u64> {
    let mut values = headers.get_all(name).iter();
    let value = values
        .next()
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| invalid("missing or invalid HTTP length"))?;
    if values.next().is_some() {
        return Err(invalid("duplicate HTTP length"));
    }
    Ok(value)
}

fn check_cancelled(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "runtime download cancelled",
        ))
    } else {
        Ok(())
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn http_error(error: ureq::Error) -> io::Error {
    io::Error::other(format!("wheel request failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ureq::http::{HeaderMap, HeaderValue};

    fn range_headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("content-range", HeaderValue::from_static("bytes 10-19/50"));
        headers.insert("content-length", HeaderValue::from_static("10"));
        headers
    }

    #[test]
    fn only_exact_identity_range_response_is_accepted() {
        let headers = range_headers();
        assert!(validate_range_response(206, &headers, 10, 19, 50).is_ok());
        assert!(validate_range_response(200, &headers, 10, 19, 50).is_err());
        assert!(validate_range_response(206, &headers, 10, 19, 51).is_err());
        let mut wrong = headers.clone();
        wrong.insert("content-length", HeaderValue::from_static("11"));
        assert!(validate_range_response(206, &wrong, 10, 19, 50).is_err());
        wrong = headers.clone();
        wrong.insert("content-encoding", HeaderValue::from_static("gzip"));
        assert!(validate_range_response(206, &wrong, 10, 19, 50).is_err());
        wrong = headers.clone();
        wrong.append("content-range", HeaderValue::from_static("bytes 10-19/50"));
        assert!(validate_range_response(206, &wrong, 10, 19, 50).is_err());
        wrong = headers;
        wrong.append("content-length", HeaderValue::from_static("10"));
        assert!(validate_range_response(206, &wrong, 10, 19, 50).is_err());
    }

    #[test]
    fn cancellation_is_observable() {
        let cancel = AtomicBool::new(false);
        assert!(check_cancelled(&cancel).is_ok());
        cancel.store(true, Ordering::Release);
        assert_eq!(
            check_cancelled(&cancel).unwrap_err().kind(),
            io::ErrorKind::ConnectionAborted
        );
    }
}
