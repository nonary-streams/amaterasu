use super::{Error, MAX_ATTRIBUTES, MAX_BYTES, MAX_LINE_BYTES, MAX_LINES};
use std::collections::BTreeMap;

pub(super) struct Document<'a> {
    pub attributes: BTreeMap<&'a str, &'a str>,
    pub port: u16,
}

pub(super) fn parse(bytes: &[u8]) -> Result<Document<'_>, Error> {
    if bytes.len() > MAX_BYTES {
        return Err(Error::LimitExceeded);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| Error::InvalidSyntax)?;
    if !text.ends_with("\r\n") {
        return Err(Error::InvalidSyntax);
    }
    let mut fields = BTreeMap::new();
    let mut attributes = BTreeMap::new();
    for (index, line) in text[..text.len() - 2].split("\r\n").enumerate() {
        if index >= MAX_LINES || line.len() > MAX_LINE_BYTES {
            return Err(Error::LimitExceeded);
        }
        if !line.bytes().all(|byte| (b' '..=b'~').contains(&byte)) {
            return Err(Error::InvalidSyntax);
        }
        if index == 0 && line != "v=0" {
            return Err(Error::InvalidSyntax);
        }
        let (kind, value) = line.split_once('=').ok_or(Error::InvalidSyntax)?;
        if kind == "a" {
            insert_attribute(&mut attributes, value)?;
        } else if !matches!(kind, "v" | "o" | "s" | "t" | "m") {
            return Err(Error::Unsupported("SDP field"));
        } else if fields.insert(kind, value).is_some() {
            return Err(Error::DuplicateField);
        }
    }
    validate_session(&fields)?;
    let media = fields.get("m").ok_or(Error::Missing("m"))?;
    let mut parts = media.split_ascii_whitespace();
    if parts.next() != Some("video") {
        return Err(Error::Unsupported("media type"));
    }
    let port = parts
        .next()
        .and_then(decimal)
        .and_then(|value| u16::try_from(value).ok())
        .filter(|port| *port != 0)
        .ok_or(Error::Invalid("media port"))?;
    if parts.next().is_some() {
        return Err(Error::Unsupported("media transport profile"));
    }
    Ok(Document { attributes, port })
}

fn validate_session(fields: &BTreeMap<&str, &str>) -> Result<(), Error> {
    if fields.get("v") != Some(&"0") || fields.get("t") != Some(&"0 0") {
        return Err(Error::Invalid("session version or timing"));
    }
    if fields.get("s").is_none_or(|name| name.trim().is_empty()) {
        return Err(Error::Missing("s"));
    }
    let origin = fields.get("o").ok_or(Error::Missing("o"))?;
    let parts: Vec<_> = origin.split_ascii_whitespace().collect();
    if parts.len() != 6
        || parts[3] != "IN"
        || !matches!(parts[4], "IPv4" | "IPv6")
        || decimal(parts[1]).is_none()
        || decimal(parts[2]).is_none()
    {
        return Err(Error::Invalid("origin"));
    }
    Ok(())
}

fn insert_attribute<'a>(
    attributes: &mut BTreeMap<&'a str, &'a str>,
    value: &'a str,
) -> Result<(), Error> {
    if attributes.len() >= MAX_ATTRIBUTES {
        return Err(Error::LimitExceeded);
    }
    let (name, value) = value.split_once(':').ok_or(Error::InvalidSyntax)?;
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.[]".contains(&byte))
    {
        return Err(Error::InvalidSyntax);
    }
    if attributes.insert(name, value.trim_matches(' ')).is_some() {
        return Err(Error::DuplicateField);
    }
    Ok(())
}

pub(super) fn decimal(value: &str) -> Option<u32> {
    (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
}
