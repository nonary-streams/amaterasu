//! Repository bytes use the selected Network provider, never native sockets.
use amaterasu_module_sdk::network::Network;
use amaterasu_package::{PackageResult, repository::RepositoryTransport};
use std::{
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
pub struct Transport {
    base: String,
    agent: ureq::Agent,
    cancelled: Arc<AtomicBool>,
}
impl Transport {
    pub fn new(base: &str, network: Network, cancelled: Arc<AtomicBool>) -> Self {
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(120)))
            .timeout_resolve(Some(Duration::from_secs(2)))
            .timeout_connect(Some(Duration::from_secs(2)))
            .max_response_header_size(8192)
            .build();
        Self {
            base: base.trim_end_matches('/').into(),
            agent: amaterasu_module_sdk::network_http::agent_cancellable(
                config,
                network,
                cancelled.clone(),
            ),
            cancelled,
        }
    }
}
impl RepositoryTransport for Transport {
    fn get(&self, relative: &str, maximum: u64) -> PackageResult<Vec<u8>> {
        if !relative.split('/').all(|part| {
            !part.is_empty()
                && !matches!(part, "." | "..")
                && part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
        }) {
            return Err("plugin_invalid_repository_path".into());
        }
        if self.cancelled.load(Ordering::Acquire) {
            return Err("plugin_preparation_cancelled".into());
        }
        let response = self
            .agent
            .get(format!("{}/{}", self.base, relative))
            .call()
            .map_err(|e| e.to_string())?;
        if response.status().as_u16() != 200 {
            return Err("plugin_download_failed".into());
        }
        let mut body = response.into_body();
        let mut input = body.as_reader();
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 65536];
        loop {
            if self.cancelled.load(Ordering::Acquire) {
                return Err("plugin_preparation_cancelled".into());
            }
            let count = input.read(&mut chunk).map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            if bytes.len() as u64 + count as u64 > maximum {
                return Err("plugin_download_too_large".into());
            }
            bytes.extend_from_slice(&chunk[..count]);
        }
        Ok(bytes)
    }
}
