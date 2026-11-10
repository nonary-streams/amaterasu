//! Repository retrieval through the Network SDK for one approved location.
//!
//! The location only says where bytes are. Every caller verifies what it
//! receives against the approved publisher key before using it.
use amaterasu_module_sdk::network::Network;
use amaterasu_package::PackageResult;
use amaterasu_package::repository::RepositoryTransport;
use std::{
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

const MAX_RESPONSE_BYTES: u64 = 256 * 1024 * 1024;

/// A bounded HTTPS client that refuses redirects, so a response cannot move
/// the download to another origin. Cancellation is observed between reads.
pub(crate) struct NetworkRepository {
    base: String,
    agent: ureq::Agent,
    cancelled: Arc<AtomicBool>,
    transferred: Arc<AtomicU64>,
}

impl NetworkRepository {
    /// `request_limit` bounds one whole request, so a caller fetching a small
    /// catalog does not wait as long as one downloading a component set.
    pub(crate) fn new(
        base: &str,
        network: Network,
        cancelled: Arc<AtomicBool>,
        transferred: Arc<AtomicU64>,
        request_limit: Duration,
    ) -> Self {
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .timeout_global(Some(request_limit))
            .timeout_resolve(Some(Duration::from_secs(10)))
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_send_request(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(15)))
            .timeout_recv_body(Some(Duration::from_secs(30)))
            .max_response_header_size(8192)
            .build();
        Self {
            base: base.trim_end_matches('/').into(),
            agent: amaterasu_module_sdk::network_http::agent(config, network),
            cancelled,
            transferred,
        }
    }

    fn check_cancelled(&self) -> PackageResult<()> {
        if self.cancelled.load(Ordering::Acquire) {
            Err("repository request cancelled".into())
        } else {
            Ok(())
        }
    }
}

impl RepositoryTransport for NetworkRepository {
    fn get(&self, relative: &str, maximum: u64) -> PackageResult<Vec<u8>> {
        self.check_cancelled()?;
        if maximum > MAX_RESPONSE_BYTES || !relative.split('/').all(valid_segment) {
            return Err("invalid repository request".into());
        }
        let response = self
            .agent
            .get(format!("{}/{relative}", self.base))
            .call()
            .map_err(|_| "repository download failed")?;
        if response.status().as_u16() != 200 {
            return Err("repository response was not successful".into());
        }
        let mut reader = response.into_body();
        let mut reader = reader.as_reader();
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 65536];
        loop {
            self.check_cancelled()?;
            let count = reader
                .read(&mut chunk)
                .map_err(|_| "repository response read failed")?;
            if count == 0 {
                break;
            }
            if bytes.len() as u64 + count as u64 > maximum {
                return Err("repository response exceeds byte limit".into());
            }
            bytes.extend_from_slice(&chunk[..count]);
            self.transferred.fetch_add(count as u64, Ordering::Relaxed);
        }
        Ok(bytes)
    }
}

fn valid_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value != "."
        && value != ".."
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_paths_and_cancelled_requests_fail_before_io() {
        for segment in ["", ".", "..", "../payload", "UPPER", "%2f"] {
            assert!(!valid_segment(segment));
        }
        assert!(valid_segment("amaterasu-linux-session-rtx-hdr"));

        let cancelled = NetworkRepository::new(
            "https://packages.example.test/",
            Network::test_provider(),
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicU64::new(0)),
            Duration::from_secs(1),
        );
        assert!(cancelled.get("catalog.json", 1024).is_err());

        let live = NetworkRepository::new(
            "https://packages.example.test/",
            Network::test_provider(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(0)),
            Duration::from_secs(1),
        );
        assert!(live.get("../catalog.json", 1024).is_err());
        assert!(live.get("catalog.json", MAX_RESPONSE_BYTES + 1).is_err());
    }
}
