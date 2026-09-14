use super::{
    Error, MAX_BODY_BYTES, MAX_BUFFER_BYTES, MAX_HEADER_BYTES, MAX_HEADERS, MAX_LINE_BYTES, Method,
    Request, decimal, header_value, token,
};

struct Head {
    request: Request,
    bytes: usize,
    body_bytes: usize,
}

/// One connection's framing state. Drain `next_request` between reads. The caller
/// owns socket deadlines, session authorization and cross-request CSeq policy.
#[derive(Default)]
pub struct Decoder {
    buffer: Vec<u8>,
    scan_from: usize,
    head: Option<Head>,
    poisoned: bool,
}

impl Decoder {
    /// Append a bounded chunk. Oversized input is rejected before allocation.
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        if bytes.len() > MAX_BUFFER_BYTES - self.buffer.len() {
            return self.fail(Error::LimitExceeded);
        }
        self.buffer.extend_from_slice(bytes);
        Ok(())
    }

    pub fn next_request(&mut self) -> Result<Option<Request>, Error> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        match self.decode() {
            Ok(request) => Ok(request),
            Err(error) => self.fail(error),
        }
    }

    /// Call after draining complete requests at EOF. Partial messages are errors.
    pub fn finish(&mut self) -> Result<(), Error> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        if self.buffer.is_empty() {
            Ok(())
        } else {
            self.fail(Error::Truncated)
        }
    }

    pub fn buffered_bytes(&self) -> usize {
        self.buffer.len()
    }

    fn fail<T>(&mut self, error: Error) -> Result<T, Error> {
        self.poisoned = true;
        self.buffer.clear();
        self.head = None;
        Err(error)
    }

    fn decode(&mut self) -> Result<Option<Request>, Error> {
        if self.head.is_none() {
            let end = self.buffer[self.scan_from..]
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|offset| self.scan_from + offset + 4);
            let Some(end) = end else {
                if self.buffer.len() >= MAX_HEADER_BYTES {
                    return Err(Error::LimitExceeded);
                }
                self.scan_from = self.buffer.len().saturating_sub(3);
                return Ok(None);
            };
            if end > MAX_HEADER_BYTES {
                return Err(Error::LimitExceeded);
            }
            self.head = Some(parse_head(&self.buffer[..end])?);
        }
        let head = self.head.as_ref().expect("parsed header");
        let length = head.bytes + head.body_bytes;
        if self.buffer.len() < length {
            return Ok(None);
        }
        let mut head = self.head.take().expect("complete header");
        head.request.body = self.buffer[head.bytes..length].to_vec();
        self.buffer.drain(..length);
        self.scan_from = 0;
        Ok(Some(head.request))
    }
}

fn parse_head(bytes: &[u8]) -> Result<Head, Error> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error::InvalidHeader)?;
    let mut lines = text[..text.len() - 4].split("\r\n");
    let (method, target) = request_line(lines.next().ok_or(Error::InvalidRequest)?)?;
    let mut headers = Vec::<(String, String)>::new();
    for line in lines {
        if headers.len() >= MAX_HEADERS || line.len() > MAX_LINE_BYTES {
            return Err(Error::LimitExceeded);
        }
        let (name, value) = parse_header(line)?;
        if headers.iter().any(|(key, _)| key == &name) {
            return Err(Error::DuplicateHeader);
        }
        if matches!(
            name.as_str(),
            "transfer-encoding" | "trailer" | "content-encoding"
        ) {
            return Err(Error::UnsupportedFraming);
        }
        headers.push((name, value));
    }
    let value = |name| {
        headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    let cseq = value("cseq")
        .and_then(decimal)
        .ok_or(Error::InvalidSequence)?;
    let body_bytes = match value("content-length") {
        Some(value) => decimal(value).ok_or(Error::InvalidLength)? as usize,
        None => 0,
    };
    if body_bytes > MAX_BODY_BYTES {
        return Err(Error::LimitExceeded);
    }
    Ok(Head {
        request: Request {
            method,
            target,
            cseq,
            headers,
            body: Vec::new(),
        },
        bytes: bytes.len(),
        body_bytes,
    })
}

fn request_line(line: &str) -> Result<(Method, String), Error> {
    if line.len() > MAX_LINE_BYTES {
        return Err(Error::LimitExceeded);
    }
    let mut parts = line.split(' ');
    let method = parts.next().ok_or(Error::InvalidRequest)?;
    let target = parts.next().ok_or(Error::InvalidRequest)?;
    let version = parts.next().ok_or(Error::InvalidRequest)?;
    if parts.next().is_some()
        || target.is_empty()
        || !target.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(Error::InvalidRequest);
    }
    if version != "RTSP/1.0" {
        return Err(Error::UnsupportedVersion);
    }
    Ok((Method::parse(method)?, target.into()))
}

fn parse_header(line: &str) -> Result<(String, String), Error> {
    let (name, value) = line.split_once(':').ok_or(Error::InvalidHeader)?;
    if !token(name) || !header_value(value) {
        return Err(Error::InvalidHeader);
    }
    Ok((
        name.to_ascii_lowercase(),
        value.trim_matches([' ', '\t']).into(),
    ))
}
