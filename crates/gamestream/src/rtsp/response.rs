use super::{
    Error, MAX_BODY_BYTES, MAX_HEADER_BYTES, MAX_HEADERS, MAX_LINE_BYTES, header_value, token,
};
use std::fmt::Write;

/// Serialize one response. Framing headers are generated, never caller supplied.
/// The caller decides status and headers only after its actual operation finishes.
pub fn serialize_response(
    status: u16,
    cseq: u32,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<Vec<u8>, Error> {
    if body.len() > MAX_BODY_BYTES || headers.len() > MAX_HEADERS - 2 {
        return Err(Error::LimitExceeded);
    }
    let reason = reason(status).ok_or(Error::InvalidRequest)?;
    let mut text = format!(
        "RTSP/1.0 {status} {reason}\r\nCSeq: {cseq}\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (index, (name, value)) in headers.iter().enumerate() {
        validate_header(name, value)?;
        if headers[..index]
            .iter()
            .any(|(previous, _)| name.eq_ignore_ascii_case(previous))
        {
            return Err(Error::DuplicateHeader);
        }
        if text.len() + name.len() + value.len() + 6 > MAX_HEADER_BYTES {
            return Err(Error::LimitExceeded);
        }
        write!(text, "{name}: {value}\r\n").expect("String formatting");
    }
    text.push_str("\r\n");
    let mut response = text.into_bytes();
    response.extend_from_slice(body);
    Ok(response)
}

fn validate_header(name: &str, value: &str) -> Result<(), Error> {
    if !token(name) || !header_value(value) {
        return Err(Error::InvalidHeader);
    }
    if name.len() + value.len() + 2 > MAX_LINE_BYTES {
        return Err(Error::LimitExceeded);
    }
    if [
        "cseq",
        "content-length",
        "transfer-encoding",
        "trailer",
        "content-encoding",
    ]
    .iter()
    .any(|reserved| name.eq_ignore_ascii_case(reserved))
    {
        return Err(Error::UnsupportedFraming);
    }
    Ok(())
}

fn reason(status: u16) -> Option<&'static str> {
    Some(match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        408 => "Request Timeout",
        413 => "Request Entity Too Large",
        415 => "Unsupported Media Type",
        451 => "Parameter Not Understood",
        453 => "Not Enough Bandwidth",
        454 => "Session Not Found",
        455 => "Method Not Valid in This State",
        461 => "Unsupported Transport",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        505 => "RTSP Version Not Supported",
        _ => return None,
    })
}
