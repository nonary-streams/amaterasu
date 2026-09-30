use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AddressFamily {
    Ipv4,
    Both,
}

impl AddressFamily {
    pub const fn supports(self, address: IpAddr) -> bool {
        matches!(self, Self::Both) || address.is_ipv4()
    }
}

/// Selects public listener IPs while retaining the legacy address when no new
/// address or family was configured. Ports remain the caller's responsibility.
#[derive(Clone, Copy, Debug)]
pub struct ListenPolicy<'a> {
    pub family: Option<AddressFamily>,
    pub address: Option<&'a str>,
    pub legacy_address: IpAddr,
    pub allow_remote: bool,
}

impl ListenPolicy<'_> {
    pub fn effective_family(self) -> AddressFamily {
        self.family.unwrap_or_else(|| {
            let address = self
                .address
                .and_then(|configured| configured.trim().parse::<IpAddr>().ok())
                .unwrap_or(self.legacy_address);
            if address.is_ipv4() {
                AddressFamily::Ipv4
            } else {
                AddressFamily::Both
            }
        })
    }

    pub fn resolve_ips(self) -> Result<Vec<IpAddr>, &'static str> {
        let family = self.effective_family();
        let addresses = match self.address {
            Some(configured) if !configured.trim().is_empty() => vec![
                configured
                    .trim()
                    .parse::<IpAddr>()
                    .map_err(|_| "invalid_bind_address")?,
            ],
            None if self.family.is_none() => vec![self.legacy_address],
            Some(_)
                if self.family.is_none() && family == AddressFamily::Ipv4 && !self.allow_remote =>
            {
                vec![self.legacy_address]
            }
            _ => default_addresses(family, self.allow_remote),
        };
        for address in &addresses {
            if !family.supports(*address) {
                return Err("address_family_mismatch");
            }
            if !self.allow_remote && !address.is_loopback() {
                return Err("remote_binding_requires_allow_remote");
            }
            if invalid_configured_bind(*address) {
                return Err("invalid_bind_address");
            }
        }
        Ok(addresses)
    }
}

fn default_addresses(family: AddressFamily, allow_remote: bool) -> Vec<IpAddr> {
    let ipv4 = IpAddr::V4(if allow_remote {
        Ipv4Addr::UNSPECIFIED
    } else {
        Ipv4Addr::LOCALHOST
    });
    match family {
        AddressFamily::Ipv4 => vec![ipv4],
        AddressFamily::Both => vec![
            ipv4,
            IpAddr::V6(if allow_remote {
                Ipv6Addr::UNSPECIFIED
            } else {
                Ipv6Addr::LOCALHOST
            }),
        ],
    }
}

/// Wildcards are valid only after the caller has permitted remote listening.
pub fn invalid_configured_bind(address: IpAddr) -> bool {
    address.is_multicast()
        || address == IpAddr::V4(Ipv4Addr::BROADCAST)
        || matches!(address, IpAddr::V6(address) if address.to_ipv4_mapped().is_some())
}

pub fn valid_peer(address: IpAddr) -> bool {
    !address.is_unspecified() && !invalid_configured_bind(address)
}

pub fn validate_local_bind(
    address: IpAddr,
    peer: IpAddr,
    allow_remote: bool,
) -> Result<(), &'static str> {
    if address.is_ipv4() != peer.is_ipv4()
        || invalid_configured_bind(address)
        || (!allow_remote && !address.is_loopback())
    {
        return Err("invalid_bind");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
