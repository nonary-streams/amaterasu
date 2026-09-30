use crate::invalid_configured_bind;
use std::net::{IpAddr, SocketAddr};

/// Checks the endpoint reported by a transport against its requested bind.
/// A wildcard request may remain a wildcard or resolve to a concrete local IP;
/// a requested fixed IP or port must never be substituted by the provider.
pub fn validate_bound_endpoint(
    actual: SocketAddr,
    requested: Option<SocketAddr>,
    peer: IpAddr,
) -> Result<(), &'static str> {
    if actual.port() == 0
        || invalid_configured_bind(actual.ip())
        || actual.is_ipv4() != peer.is_ipv4()
    {
        return Err("invalid_bind");
    }
    if let Some(requested) = requested
        && (requested.is_ipv4() != actual.is_ipv4()
            || (requested.port() != 0 && requested.port() != actual.port())
            || (!requested.ip().is_unspecified() && requested.ip() != actual.ip()))
    {
        return Err("invalid_bind");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
