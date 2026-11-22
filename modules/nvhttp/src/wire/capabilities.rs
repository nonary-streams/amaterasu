use amaterasu_module_sdk::{LogEmitter, LogLevel, network::Network};
use std::{
    io,
    net::SocketAddr,
    time::{Duration, Instant},
};

const NO_HARDWARE_ADDRESS: &str = "00:00:00:00:00:00";
const ADVISORY_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(super) struct WakeIdentity {
    pub(super) network: Option<Network>,
    pub(super) logger: Option<LogEmitter>,
    last_advisory: Option<Instant>,
}

impl WakeIdentity {
    pub(super) fn mac(&mut self, local: Option<SocketAddr>, now: Instant) -> Option<[u8; 6]> {
        let result = self.network.as_ref()?.wake_mac(local);
        match result {
            Ok(mac) => Some(mac),
            Err(error) => {
                if let Some(logger) = &self.logger
                    && self
                        .last_advisory
                        .is_none_or(|last| now.saturating_duration_since(last) >= ADVISORY_INTERVAL)
                {
                    let reason = match error.kind() {
                        io::ErrorKind::Unsupported => {
                            "selected network provider lacks Wake-on-LAN identity"
                        }
                        io::ErrorKind::NotFound => {
                            "selected network provider has no usable hardware address"
                        }
                        io::ErrorKind::InvalidData => {
                            "selected network provider returned an invalid hardware address"
                        }
                        io::ErrorKind::WouldBlock => {
                            "selected network provider's identity snapshot is busy"
                        }
                        _ => "selected network provider could not read Wake-on-LAN identity",
                    };
                    let _ = logger.emit(
                        LogLevel::Warning,
                        "nvhttp.serverinfo.wake_mac_unavailable",
                        &format!("{reason}. Advertising {NO_HARDWARE_ADDRESS}"),
                    );
                    self.last_advisory = Some(now);
                }
                None
            }
        }
    }
}

pub(super) fn format_mac(mac: Option<[u8; 6]>) -> String {
    let Some([a, b, c, d, e, f]) = mac else {
        return NO_HARDWARE_ADDRESS.to_owned();
    };
    format!("{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{f:02x}")
}
