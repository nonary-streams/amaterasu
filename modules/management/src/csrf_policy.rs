//! Explicit administrator-selected exceptions to the cookie CSRF origin/header check.
//! These origins confer no authentication, CORS, transport, or peer authority.
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeSet;
use std::net::{Ipv4Addr, Ipv6Addr};

pub(crate) const MAX_ORIGINS: usize = 16;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub(crate) struct CsrfOrigins(Vec<String>);

impl CsrfOrigins {
    pub(crate) fn as_slice(&self) -> &[String] {
        &self.0
    }

    pub(crate) fn permits(&self, origin: Option<&str>) -> bool {
        origin.is_some_and(|origin| self.0.iter().any(|allowed| allowed == origin))
    }
}

impl<'de> Deserialize<'de> for CsrfOrigins {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let origins = Vec::<String>::deserialize(deserializer)?;
        let mut unique = BTreeSet::new();
        if origins.len() > MAX_ORIGINS
            || origins
                .iter()
                .any(|origin| !valid_origin(origin) || !unique.insert(origin))
        {
            return Err(serde::de::Error::custom(
                "CSRF exceptions require at most 16 distinct exact HTTPS origins",
            ));
        }
        Ok(Self(origins))
    }
}

pub(crate) fn valid_origin(origin: &str) -> bool {
    if origin.len() > 256 {
        return false;
    }
    let Some(authority) = origin.strip_prefix("https://") else {
        return false;
    };
    if let Some(ipv6) = authority.strip_prefix('[') {
        let Some((address, suffix)) = ipv6.split_once(']') else {
            return false;
        };
        return address.parse::<Ipv6Addr>().is_ok() && valid_port_suffix(suffix);
    }
    let (host, suffix) = authority
        .split_once(':')
        .map_or((authority, ""), |(host, port)| (host, port));
    if !suffix.is_empty() && !valid_port(suffix) || authority.ends_with(':') {
        return false;
    }
    if host.parse::<Ipv4Addr>().is_ok() {
        return true;
    }
    // Do not accept ambiguous numeric host spellings interpreted as IPv4 by
    // browser URL parsers. DNS names use ordinary ASCII labels only.
    !host.is_empty()
        && host.len() <= 253
        && !host.rsplit('.').next().is_some_and(numeric_host_label)
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

fn numeric_host_label(label: &str) -> bool {
    label.bytes().all(|byte| byte.is_ascii_digit())
        || label
            .strip_prefix("0x")
            .or_else(|| label.strip_prefix("0X"))
            .is_some_and(|number| number.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn valid_port_suffix(suffix: &str) -> bool {
    suffix.is_empty() || suffix.strip_prefix(':').is_some_and(valid_port)
}

fn valid_port(port: &str) -> bool {
    !port.is_empty()
        && port.bytes().all(|byte| byte.is_ascii_digit())
        && port.parse::<u16>().is_ok_and(|port| port != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn origins_are_bounded_exact_https_authorities() {
        for origin in [
            "https://localhost",
            "https://example.com:47990",
            "https://127.0.0.1:443",
            "https://[::1]",
            "https://[fd00::1]:47990",
        ] {
            assert!(valid_origin(origin), "{origin}");
        }
        for origin in [
            "",
            "*",
            "https://*",
            "http://localhost",
            "HTTPS://localhost",
            "https://",
            "https://user@host",
            "https://host/",
            "https://host?q=1",
            "https://host#fragment",
            "https://host:",
            "https://host:0",
            "https://host:65536",
            "https://host:443:1",
            "https://[::1",
            "https://[::1]/",
            "https://[::1]:",
            "https://[not-an-ip]",
            "https://host..invalid",
            "https://-host",
            "https://host-",
            "https://127.1",
            "https://2130706433",
            "https://host\r\n",
            "https://0x7f000001",
            "https://0x7f.0.0.1",
            "https://host.123",
            "https://0x",
            "https://1.0X",
        ] {
            assert!(!valid_origin(origin), "{origin}");
        }
        assert!(
            serde_json::from_value::<CsrfOrigins>(json!(["https://host", "https://host"])).is_err()
        );
        assert!(serde_json::from_value::<CsrfOrigins>(json!(vec!["https://host"; 17])).is_err());
        let origins: CsrfOrigins = serde_json::from_value(json!(["https://host:47990"])).unwrap();
        assert!(origins.permits(Some("https://host:47990")));
        for origin in [
            None,
            Some("https://host"),
            Some("https://host:47990.evil"),
            Some("https://host:47990/path"),
        ] {
            assert!(!origins.permits(origin));
        }
    }
}
