//! IPv4 IGD discovery and SOAP policy over the selected network provider.
use amaterasu_module_sdk::network::Network;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};
use xmltree::{Element, XMLNode};

use crate::ipv6::{
    find_child_recursive, http_get_until, http_post, soap_error_code, valid_http_path, xml_escape,
};
use crate::{
    Gateway, GatewayError, Mapping, MappingKey, OperationDeadline, Protocol, RefreshOutcome,
    RemovalOutcome,
};

/// Upper bound for one IPv4 discovery receive iteration.
pub const SEARCH_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_MAPPING_SCAN: u32 = 4096;
const MAX_DESCRIPTION_BYTES: usize = 256 * 1024;
const MAX_SOAP_BYTES: usize = 64 * 1024;
const DEVICE_TYPE: &str = "urn:schemas-upnp-org:device:InternetGatewayDevice:1";
const SERVICES: [&str; 3] = [
    "urn:schemas-upnp-org:service:WANPPPConnection:1",
    "urn:schemas-upnp-org:service:WANIPConnection:1",
    "urn:schemas-upnp-org:service:WANIPConnection:2",
];

/// Owns gateway protocol policy while the SDK owns connections and delivery.
pub struct IgdGateway {
    network: Network,
    address: SocketAddr,
    root_path: String,
    control_path: String,
    service: String,
    internal_address: Ipv4Addr,
    operation_deadline: OperationDeadline,
}

/// Discovers an IPv4 IGD using the caller's selected network provider.
pub fn discover_gateway(network: &Network) -> Result<IgdGateway, GatewayError> {
    discover_gateway_with_deadline(network, OperationDeadline::default())
}

/// Discovers a gateway while sharing a caller-restrictable shutdown budget.
pub fn discover_gateway_with_deadline(
    network: &Network,
    operation_deadline: OperationDeadline,
) -> Result<IgdGateway, GatewayError> {
    operation_deadline
        .remaining(Instant::now() + SEARCH_TIMEOUT)
        .map_err(|error| GatewayError::new(error.to_string()))?;
    let socket = network
        .bind_udp(SocketAddr::from(([0, 0, 0, 0], 0)))
        .map_err(|error| GatewayError::new(format!("IGD discovery socket failed: {error}")))?;
    socket.set_nonblocking(false).map_err(|error| {
        GatewayError::new(format!("IGD discovery socket setup failed: {error}"))
    })?;
    let request = format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {DEVICE_TYPE}\r\n\r\n"
    );
    socket
        .set_write_timeout(Some(
            operation_deadline
                .remaining(Instant::now() + SEARCH_TIMEOUT)
                .map_err(|error| GatewayError::new(error.to_string()))?,
        ))
        .and_then(|()| {
            socket.send_to(
                request.as_bytes(),
                SocketAddr::from(([239, 255, 255, 250], 1900)),
            )
        })
        .map_err(|error| GatewayError::new(format!("IGD discovery failed: {error}")))?;
    let deadline = Instant::now() + SEARCH_TIMEOUT;
    let mut buffer = [0; 8192];
    while Instant::now() < deadline {
        socket
            .set_read_timeout(Some(
                operation_deadline
                    .remaining(deadline)
                    .map_err(|error| GatewayError::new(error.to_string()))?,
            ))
            .map_err(|error| GatewayError::new(format!("IGD discovery timeout failed: {error}")))?;
        let (length, source) = match socket.recv_from(&mut buffer) {
            Ok(result) => result,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                break;
            }
            Err(error) => {
                return Err(GatewayError::new(format!(
                    "IGD discovery read failed: {error}"
                )));
            }
        };
        let Some((address, root_path)) = advertised_location(&buffer[..length], source) else {
            continue;
        };
        let description = match http_get_until(
            network,
            address,
            &root_path,
            MAX_DESCRIPTION_BYTES,
            deadline,
            &operation_deadline,
        ) {
            Ok(response) if response.status == 200 => response,
            _ => continue,
        };
        if let Ok(gateway) = IgdGateway::from_description(
            network.clone(),
            address,
            root_path,
            &description.body,
            operation_deadline.clone(),
        ) {
            return Ok(gateway);
        }
    }
    Err(GatewayError::new(
        "IGD discovery received no usable gateway",
    ))
}

fn advertised_location(response: &[u8], source: SocketAddr) -> Option<(SocketAddr, String)> {
    let text = std::str::from_utf8(response).ok()?;
    if !text.lines().next()?.contains(" 200 ") {
        return None;
    }
    let location = text
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find_map(|(name, value)| name.eq_ignore_ascii_case("location").then(|| value.trim()))?;
    let (address, path) = parse_http_location(location)?;
    (source.ip() == address.ip()).then_some((address, path))
}

fn parse_http_location(location: &str) -> Option<(SocketAddr, String)> {
    let location = location.strip_prefix("http://")?;
    let end = location.find('/').unwrap_or(location.len());
    let authority = &location[..end];
    if authority.contains('@') {
        return None;
    }
    let address = if authority.contains(':') {
        authority.parse::<SocketAddr>().ok()?
    } else {
        SocketAddr::new(IpAddr::V4(authority.parse::<Ipv4Addr>().ok()?), 80)
    };
    if !address.is_ipv4()
        || address.port() == 0
        || address.ip().is_unspecified()
        || address.ip().is_multicast()
    {
        return None;
    }
    let path = if end == location.len() {
        "/"
    } else {
        &location[end..]
    };
    valid_http_path(path).then(|| (address, path.to_owned()))
}

fn service_control(root: &Element) -> Option<(String, String)> {
    if root.name == "service" {
        let service = root.get_child("serviceType")?.get_text()?.trim().to_owned();
        if SERVICES.contains(&service.as_str()) {
            let control = root.get_child("controlURL")?.get_text()?.trim().to_owned();
            return Some((service, control));
        }
    }
    root.children
        .iter()
        .filter_map(XMLNode::as_element)
        .find_map(service_control)
}

fn control_target(
    root: &Element,
    address: SocketAddr,
    root_path: &str,
    control: &str,
) -> Option<(SocketAddr, String)> {
    if control.starts_with("http://") {
        let target = parse_http_location(control)?;
        return (target.0.ip() == address.ip()).then_some(target);
    }
    let (address, base) = if let Some(base) = root.get_child("URLBase").and_then(Element::get_text)
    {
        let target = parse_http_location(base.trim())?;
        if target.0.ip() != address.ip() {
            return None;
        }
        target
    } else {
        (address, root_path.to_owned())
    };
    let path = if control.starts_with('/') {
        control.to_owned()
    } else {
        let prefix = base.rsplit_once('/').map_or("", |(prefix, _)| prefix);
        format!("{prefix}/{control}")
    };
    valid_http_path(&path).then_some((address, path))
}

impl IgdGateway {
    fn from_description(
        network: Network,
        address: SocketAddr,
        root_path: String,
        bytes: &[u8],
        operation_deadline: OperationDeadline,
    ) -> Result<Self, GatewayError> {
        let root = Element::parse(bytes)
            .map_err(|error| GatewayError::new(format!("invalid IGD description: {error}")))?;
        let (service, control) = service_control(&root)
            .ok_or_else(|| GatewayError::new("IGD description has no mapping service"))?;
        let (address, control_path) = control_target(&root, address, &root_path, &control)
            .ok_or_else(|| GatewayError::new("IGD description has an invalid control URL"))?;
        operation_deadline
            .remaining(Instant::now() + SEARCH_TIMEOUT)
            .map_err(|error| GatewayError::new(error.to_string()))?;
        let internal_address = local_ipv4_route(&network, address)?;
        Ok(Self {
            network,
            address,
            root_path,
            control_path,
            service,
            internal_address,
            operation_deadline,
        })
    }
    /// IPv4 interface selected by the provider route to this gateway.
    pub fn internal_address(&self) -> Ipv4Addr {
        self.internal_address
    }
    /// Identity retained across refreshes of the same gateway and host route.
    pub fn identity(&self) -> String {
        format!(
            "{}|{}|{}",
            self.address, self.root_path, self.internal_address
        )
    }
    fn action(&self, action: &str, parameters: &[(&str, String)]) -> Result<Element, GatewayError> {
        let body = soap_envelope(&self.service, action, parameters);
        let header = format!("\"{}#{action}\"", self.service);
        let response = http_post(
            &self.network,
            self.address,
            &self.control_path,
            &[
                ("Content-Type", "text/xml; charset=\"utf-8\""),
                ("SOAPAction", &header),
            ],
            Some(body.as_bytes()),
            MAX_SOAP_BYTES,
            &self.operation_deadline,
        )?;
        let root = Element::parse(response.body.as_slice())
            .map_err(|error| GatewayError::new(format!("invalid IGD SOAP response: {error}")))?;
        if response.status != 200 || find_child_recursive(&root, "Fault").is_some() {
            return Err(GatewayError::new(format!(
                "IGD rejected {action} with fault {}: {}",
                text(&root, "errorCode")?,
                text(&root, "errorDescription")?
            )));
        }
        if find_child_recursive(&root, &format!("{action}Response")).is_none() {
            return Err(GatewayError::new(format!(
                "IGD SOAP response omitted {action}Response"
            )));
        }
        Ok(root)
    }
    fn add(&self, mapping: &Mapping) -> Result<(), GatewayError> {
        self.action(
            "AddPortMapping",
            &[
                ("RemoteHost", mapping.remote_host.clone()),
                ("ExternalPort", mapping.external_port.to_string()),
                ("Protocol", protocol_name(mapping.protocol).into()),
                ("InternalPort", mapping.internal_port.to_string()),
                ("InternalClient", mapping.internal_address.to_string()),
                ("Enabled", if mapping.enabled { "1" } else { "0" }.into()),
                ("PortMappingDescription", mapping.description.clone()),
                ("LeaseDuration", mapping.lease_duration_seconds.to_string()),
            ],
        )?;
        Ok(())
    }
}

fn local_ipv4_route(network: &Network, gateway: SocketAddr) -> Result<Ipv4Addr, GatewayError> {
    let socket = network
        .bind_udp(SocketAddr::from(([0, 0, 0, 0], 0)))
        .map_err(|error| GatewayError::new(format!("failed to select an IGD route: {error}")))?;
    socket
        .connect(gateway)
        .map_err(|error| GatewayError::new(format!("failed to select an IGD route: {error}")))?;
    let address = socket
        .local_addr()
        .map_err(|error| GatewayError::new(format!("failed to inspect the IGD route: {error}")))?
        .ip();
    let IpAddr::V4(address) = address else {
        return Err(GatewayError::new("IGD discovery selected a non-IPv4 route"));
    };
    if address.is_loopback() || address.is_unspecified() {
        return Err(GatewayError::new(
            "IGD discovery selected a non-routable local IPv4 address",
        ));
    }
    Ok(address)
}

impl Gateway for IgdGateway {
    fn find_mappings(&mut self, key: MappingKey) -> Result<Vec<Mapping>, GatewayError> {
        let mut mappings = Vec::new();
        for index in 0..MAX_MAPPING_SCAN {
            let response = match self.action(
                "GetGenericPortMappingEntry",
                &[("PortMappingIndex", index.to_string())],
            ) {
                Ok(response) => response,
                Err(error) if soap_error_code(&error) == Some(713) => return Ok(mappings),
                Err(error) => return Err(error),
            };
            let mapping = mapping_from_response(&response)?;
            if mapping.external_port == key.external_port && mapping.protocol == key.protocol {
                mappings.push(mapping);
            }
        }
        Err(GatewayError::new(format!(
            "IGD mapping table exceeded the {MAX_MAPPING_SCAN}-entry inspection bound"
        )))
    }
    fn create_if_absent(&mut self, mapping: &Mapping) -> Result<(), GatewayError> {
        if !self.find_mappings(mapping.key())?.is_empty() {
            return Err(GatewayError::new(
                "IGD mapping appeared before creation; it was preserved",
            ));
        }
        self.add(mapping)
    }
    fn remove_if_matches(&mut self, mapping: &Mapping) -> Result<RemovalOutcome, GatewayError> {
        let current = self.find_mappings(mapping.key())?;
        if current.is_empty() {
            return Ok(RemovalOutcome::Missing);
        }
        if !current
            .iter()
            .any(|entry| entry.has_same_owned_attributes(mapping))
        {
            return Ok(RemovalOutcome::Changed);
        }
        self.action(
            "DeletePortMapping",
            &[
                ("RemoteHost", mapping.remote_host.clone()),
                ("ExternalPort", mapping.external_port.to_string()),
                ("Protocol", protocol_name(mapping.protocol).into()),
            ],
        )?;
        Ok(RemovalOutcome::Removed)
    }
    fn remove_if_matches_until(
        &mut self,
        mapping: &Mapping,
        deadline: Instant,
    ) -> Result<RemovalOutcome, GatewayError> {
        self.operation_deadline.restrict(deadline);
        self.remove_if_matches(mapping)
    }
    fn refresh_if_matches(&mut self, mapping: &Mapping) -> Result<RefreshOutcome, GatewayError> {
        let current = self.find_mappings(mapping.key())?;
        if current.is_empty() {
            return Ok(RefreshOutcome::Missing);
        }
        if !current
            .iter()
            .any(|entry| entry.has_same_owned_attributes(mapping))
        {
            return Ok(RefreshOutcome::Changed);
        }
        self.add(mapping)?;
        Ok(RefreshOutcome::Refreshed)
    }
}

fn protocol_name(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Tcp => "TCP",
        Protocol::Udp => "UDP",
    }
}
fn text(root: &Element, name: &str) -> Result<String, GatewayError> {
    find_child_recursive(root, name)
        .map(|element| {
            element
                .get_text()
                .map_or_else(String::new, |value| value.trim().to_owned())
        })
        .ok_or_else(|| GatewayError::new(format!("IGD response omitted {name}")))
}
fn parse<T: std::str::FromStr>(root: &Element, name: &str) -> Result<T, GatewayError> {
    text(root, name)?
        .parse()
        .map_err(|_| GatewayError::new(format!("IGD response has invalid {name}")))
}
fn mapping_from_response(root: &Element) -> Result<Mapping, GatewayError> {
    let protocol = match text(root, "NewProtocol")?.as_str() {
        "TCP" => Protocol::Tcp,
        "UDP" => Protocol::Udp,
        _ => return Err(GatewayError::new("IGD response has invalid NewProtocol")),
    };
    let enabled = match text(root, "NewEnabled")?.as_str() {
        "1" | "true" => true,
        "0" | "false" => false,
        _ => return Err(GatewayError::new("IGD response has invalid NewEnabled")),
    };
    Ok(Mapping {
        external_port: parse(root, "NewExternalPort")?,
        protocol,
        internal_address: parse(root, "NewInternalClient")?,
        internal_port: parse(root, "NewInternalPort")?,
        remote_host: text(root, "NewRemoteHost")?,
        enabled,
        description: text(root, "NewPortMappingDescription")?,
        lease_duration_seconds: parse(root, "NewLeaseDuration")?,
    })
}
fn soap_envelope(service: &str, action: &str, parameters: &[(&str, String)]) -> String {
    let mut body = format!(
        "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><u:{action} xmlns:u=\"{service}\">"
    );
    for (name, value) in parameters {
        body.push_str(&format!("<New{name}>{}</New{name}>", xml_escape(value)));
    }
    body.push_str(&format!("</u:{action}></s:Body></s:Envelope>"));
    body
}

#[cfg(test)]
mod tests;
