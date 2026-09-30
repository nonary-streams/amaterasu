//! UPnP listener selection and owned gateway-mapping lifecycle.
use amaterasu_module_sdk::{Command, Response, RuntimeClient};
use amaterasu_network_policy::GameStreamPorts;
use amaterasu_upnp::{
    IgdGateway, MappingEndpoint, MappingManager, Protocol, RECOMMENDED_REFRESH_INTERVAL,
    discover_gateway,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const MANAGEMENT_SERVICE: &str = "amaterasu.management/1";
const EXPOSURE_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const EXPOSURE_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const CLEANUP_RETRY_INTERVAL: Duration = Duration::from_secs(5);
const MAX_RUNTIME_ERROR_CHARS: usize = 256;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ManagementExposureSnapshot {
    pub(crate) wan_enabled: bool,
    pub(crate) tls_address: Option<SocketAddr>,
}

/// Polls the management module for the live WAN policy and TLS socket.
pub(crate) struct ManagementExposure {
    pending: Option<u64>,
    cancelling: bool,
    sequence: u64,
    next_request: Instant,
    snapshot: ManagementExposureSnapshot,
}

impl Default for ManagementExposure {
    fn default() -> Self {
        Self {
            pending: None,
            cancelling: false,
            sequence: 0,
            next_request: Instant::now(),
            snapshot: ManagementExposureSnapshot::default(),
        }
    }
}

impl ManagementExposure {
    pub(crate) fn tick(&mut self, runtime: &RuntimeClient) {
        if let Some(request) = self.pending {
            if self.cancelling {
                self.cancel_pending(runtime, request);
                return;
            }
            match runtime.send_ephemeral(Command::TakeResult { request }) {
                Ok(Response::Pending) => return,
                Ok(Response::Result { payload }) => {
                    self.snapshot = parse_management_exposure(&payload);
                    self.pending = None;
                    self.next_request = Instant::now() + EXPOSURE_REFRESH_INTERVAL;
                }
                Ok(Response::Error { code, .. }) if code == "not_found" => {
                    self.snapshot = ManagementExposureSnapshot::default();
                    self.pending = None;
                    self.next_request = Instant::now() + EXPOSURE_REFRESH_INTERVAL;
                }
                _ => {
                    self.snapshot = ManagementExposureSnapshot::default();
                    self.cancelling = true;
                    self.cancel_pending(runtime, request);
                    return;
                }
            }
            return;
        }

        if Instant::now() < self.next_request {
            return;
        }
        let Some(sequence) = self.sequence.checked_add(1) else {
            self.snapshot = ManagementExposureSnapshot::default();
            self.next_request = Instant::now() + EXPOSURE_REFRESH_INTERVAL;
            return;
        };
        self.sequence = sequence;
        let command = Command::Request {
            service: MANAGEMENT_SERVICE.into(),
            payload: json!({"action":"upnp_exposure"}),
            timeout_ms: EXPOSURE_REQUEST_TIMEOUT.as_millis() as u64,
            operation_id: format!("nvhttp.upnp.exposure.{sequence}"),
            parent: None,
        };
        match runtime.send(command) {
            Ok(Response::Accepted { id }) => self.pending = Some(id),
            _ => {
                self.snapshot = ManagementExposureSnapshot::default();
                self.next_request = Instant::now() + EXPOSURE_REFRESH_INTERVAL;
            }
        }
    }

    pub(crate) fn snapshot(&self) -> ManagementExposureSnapshot {
        self.snapshot
    }

    fn cancel_pending(&mut self, runtime: &RuntimeClient, request: u64) {
        for command in [
            Command::Cancel { request },
            Command::ReleaseResult { request },
        ] {
            match runtime.send(command) {
                Ok(Response::Ok) => {}
                Ok(Response::Error { code, .. }) if code == "not_found" => {}
                _ => return,
            }
        }
        self.pending = None;
        self.cancelling = false;
        self.next_request = Instant::now() + EXPOSURE_REFRESH_INTERVAL;
    }
}

fn parse_management_exposure(payload: &Value) -> ManagementExposureSnapshot {
    if payload.get("ok").and_then(Value::as_bool) != Some(true)
        || payload.get("wan_enabled").and_then(Value::as_bool) != Some(true)
    {
        return ManagementExposureSnapshot::default();
    }
    let tls_address = payload
        .get("tls_address")
        .and_then(Value::as_str)
        .and_then(|address| address.parse().ok());
    ManagementExposureSnapshot {
        wan_enabled: tls_address.is_some(),
        tls_address,
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct DesiredMappings {
    enabled: bool,
    endpoints: Vec<MappingEndpoint>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UpnpRuntimeStatus {
    pub(crate) status: String,
    pub(crate) error: Option<String>,
}

impl Default for UpnpRuntimeStatus {
    fn default() -> Self {
        Self {
            status: "disabled".into(),
            error: None,
        }
    }
}

enum LifecycleCommand {
    Configure(DesiredMappings),
    Shutdown,
}

/// Owns the UPnP worker and guarantees an orderly best-effort cleanup on drop.
pub(crate) struct UpnpLifecycle {
    commands: Sender<LifecycleCommand>,
    statuses: Receiver<UpnpRuntimeStatus>,
    worker: Option<JoinHandle<()>>,
    last_desired: Option<DesiredMappings>,
    status: UpnpRuntimeStatus,
}

impl UpnpLifecycle {
    pub(crate) fn new() -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let (status_tx, status_rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("amaterasu-upnp".into())
            .spawn(move || run_worker(command_rx, status_tx))
            .ok();
        let status = if worker.is_some() {
            UpnpRuntimeStatus::default()
        } else {
            UpnpRuntimeStatus {
                status: "error".into(),
                error: Some("UPnP lifecycle worker could not be started".into()),
            }
        };
        Self {
            commands: command_tx,
            statuses: status_rx,
            worker,
            last_desired: None,
            status,
        }
    }

    pub(crate) fn update(
        &mut self,
        enabled: bool,
        http_addresses: &[SocketAddr],
        https_addresses: &[SocketAddr],
        rtsp_base_port: Option<u16>,
        rtsp_addresses: &[String],
        management: ManagementExposureSnapshot,
    ) {
        let desired = build_desired_mappings(
            enabled,
            http_addresses,
            https_addresses,
            rtsp_base_port,
            rtsp_addresses,
            management,
        );
        if self.last_desired.as_ref() != Some(&desired) {
            if self
                .commands
                .send(LifecycleCommand::Configure(desired.clone()))
                .is_ok()
            {
                self.last_desired = Some(desired);
            } else {
                self.status = UpnpRuntimeStatus {
                    status: "error".into(),
                    error: Some("UPnP lifecycle worker stopped unexpectedly".into()),
                };
            }
        }
        while let Ok(status) = self.statuses.try_recv() {
            self.status = status;
        }
    }

    pub(crate) fn status(&self) -> &UpnpRuntimeStatus {
        &self.status
    }

    pub(crate) fn shutdown(&mut self) {
        if self.worker.is_none() {
            return;
        }
        let _ = self.commands.send(LifecycleCommand::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for UpnpLifecycle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn build_desired_mappings(
    enabled: bool,
    http_addresses: &[SocketAddr],
    https_addresses: &[SocketAddr],
    rtsp_base_port: Option<u16>,
    rtsp_addresses: &[String],
    management: ManagementExposureSnapshot,
) -> DesiredMappings {
    if !enabled {
        return DesiredMappings::default();
    }

    let mut endpoints = Vec::new();
    if let Some(http_base_port) = http_addresses.first().map(SocketAddr::port)
        && let Ok(ports) = GameStreamPorts::new(http_base_port)
    {
        append_listener_endpoints(
            &mut endpoints,
            http_addresses,
            ports.http(),
            ports.http(),
            Protocol::Tcp,
        );
        append_listener_endpoints(
            &mut endpoints,
            https_addresses,
            ports.https(),
            ports.https(),
            Protocol::Tcp,
        );
    }

    if let Some(base_port) = rtsp_base_port
        && let Ok(ports) = GameStreamPorts::new(base_port)
    {
        let rtsp_addresses = parse_addresses(rtsp_addresses);
        append_listener_endpoints(
            &mut endpoints,
            &rtsp_addresses,
            ports.rtsp(),
            ports.rtsp(),
            Protocol::Tcp,
        );
        for port in [ports.video(), ports.audio(), ports.control()] {
            append_listener_endpoints(
                &mut endpoints,
                &rtsp_addresses,
                ports.rtsp(),
                port,
                Protocol::Udp,
            );
        }
    }

    if management.wan_enabled
        && let Some(address) = management.tls_address
    {
        endpoints.push(MappingEndpoint::new(
            address.ip(),
            address.port(),
            Protocol::Tcp,
            true,
        ));
    }

    DesiredMappings {
        enabled: true,
        endpoints,
    }
}

fn parse_addresses(addresses: &[String]) -> Vec<SocketAddr> {
    addresses
        .iter()
        .filter_map(|address| address.parse().ok())
        .collect()
}

fn append_listener_endpoints(
    endpoints: &mut Vec<MappingEndpoint>,
    addresses: &[SocketAddr],
    expected_listener_port: u16,
    mapped_port: u16,
    protocol: Protocol,
) {
    endpoints.extend(
        addresses
            .iter()
            .filter(|address| address.port() == expected_listener_port)
            .map(|address| MappingEndpoint::new(address.ip(), mapped_port, protocol, true)),
    );
}

fn run_worker(commands: Receiver<LifecycleCommand>, statuses: Sender<UpnpRuntimeStatus>) {
    let mut desired = DesiredMappings::default();
    let mut gateways: BTreeMap<String, GatewayOwner> = BTreeMap::new();
    let mut active_gateway: Option<String> = None;
    let mut next_discovery = Instant::now();
    let mut next_retry = Instant::now();

    loop {
        let now = Instant::now();
        let next_wakeup = if desired.enabled && !desired.endpoints.is_empty() {
            next_discovery.min(next_retry)
        } else {
            next_retry
        };
        let timeout = next_wakeup.saturating_duration_since(now);
        let mut shutdown = false;
        match commands.recv_timeout(timeout) {
            Ok(LifecycleCommand::Configure(next)) => {
                desired = next;
            }
            Ok(LifecycleCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                shutdown = true;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }

        if shutdown {
            shutdown_gateways(&mut gateways);
            break;
        }

        let now = Instant::now();
        let refresh_due = now >= next_discovery;
        let rediscover = active_gateway.is_none() || refresh_due;
        let status;
        if desired.enabled && !desired.endpoints.is_empty() {
            if !has_ipv4_candidate(&desired.endpoints) {
                cleanup_gateways(&mut gateways, &mut active_gateway);
                next_retry = Instant::now() + CLEANUP_RETRY_INTERVAL;
                status = UpnpRuntimeStatus {
                    status: "waiting_for_ipv4_listener".into(),
                    error: None,
                };
            } else {
                let mut discovery_error = None;
                if rediscover {
                    match discover_gateway() {
                        Ok(gateway) => {
                            let identity = gateway.identity();
                            let internal_address = gateway.internal_address();
                            if active_gateway.as_ref() != Some(&identity)
                                && let Some(previous) = active_gateway.as_ref()
                                && let Some(manager) = gateways.get_mut(previous)
                            {
                                let _ = manager.manager.shutdown();
                            }
                            match gateways.get_mut(&identity) {
                                Some(owner) => {
                                    owner.manager.replace_gateway_adapter(gateway);
                                    owner.internal_address = internal_address;
                                }
                                None => {
                                    gateways.insert(
                                        identity.clone(),
                                        GatewayOwner {
                                            manager: MappingManager::new(gateway),
                                            internal_address,
                                        },
                                    );
                                }
                            }
                            active_gateway = Some(identity);
                        }
                        Err(error) => discovery_error = Some(bounded_runtime_error(error)),
                    }
                    next_discovery = Instant::now() + RECOMMENDED_REFRESH_INTERVAL;
                }

                if let Some(identity) = active_gateway.as_ref()
                    && let Some(owner) = gateways.get_mut(identity)
                {
                    let gateway_endpoints =
                        materialize_gateway_endpoints(&desired.endpoints, owner.internal_address);
                    let mut error = discovery_error;
                    if refresh_due && let Err(refresh_error) = owner.manager.refresh_owned() {
                        error.get_or_insert_with(|| bounded_runtime_error(refresh_error));
                    }
                    if let Err(reconcile_error) =
                        owner.manager.reconcile_protocols(&gateway_endpoints)
                    {
                        error.get_or_insert_with(|| bounded_runtime_error(reconcile_error));
                    }
                    cleanup_inactive_gateways(&mut gateways, active_gateway.as_deref());
                    status = if let Some(error) = error {
                        next_retry = Instant::now() + CLEANUP_RETRY_INTERVAL;
                        UpnpRuntimeStatus {
                            status: "error".into(),
                            error: Some(error),
                        }
                    } else if gateway_endpoints.is_empty() {
                        next_retry = next_discovery;
                        UpnpRuntimeStatus {
                            status: "waiting_for_gateway_route".into(),
                            error: None,
                        }
                    } else {
                        next_retry = next_discovery;
                        UpnpRuntimeStatus {
                            status: "active".into(),
                            error: None,
                        }
                    };
                } else {
                    cleanup_gateways(&mut gateways, &mut active_gateway);
                    next_retry = Instant::now() + CLEANUP_RETRY_INTERVAL;
                    status = UpnpRuntimeStatus {
                        status: "error".into(),
                        error: Some(
                            discovery_error
                                .unwrap_or_else(|| "no UPnP gateway is available".into()),
                        ),
                    };
                }
            }
        } else {
            cleanup_gateways(&mut gateways, &mut active_gateway);
            next_retry = Instant::now() + CLEANUP_RETRY_INTERVAL;
            status = if desired.enabled {
                UpnpRuntimeStatus {
                    status: "waiting_for_listener".into(),
                    error: None,
                }
            } else {
                UpnpRuntimeStatus::default()
            };
        }
        let _ = statuses.send(status.clone());
    }
}

fn has_ipv4_candidate(endpoints: &[MappingEndpoint]) -> bool {
    endpoints.iter().any(|endpoint| {
        endpoint.active
            && matches!(endpoint.address, IpAddr::V4(address) if address.is_unspecified() || !address.is_loopback())
    })
}

fn bounded_runtime_error(error: impl std::fmt::Display) -> String {
    let rendered = error.to_string();
    let mut bounded = String::with_capacity(MAX_RUNTIME_ERROR_CHARS);
    let mut previous_was_space = true;
    let mut character_count = 0;

    for character in rendered.chars().take(MAX_RUNTIME_ERROR_CHARS * 4) {
        let normalized = if character.is_whitespace() || character.is_control() {
            ' '
        } else {
            character
        };
        if normalized == ' ' {
            if !previous_was_space {
                bounded.push(' ');
                character_count += 1;
                previous_was_space = true;
            }
        } else {
            bounded.push(normalized);
            character_count += 1;
            previous_was_space = false;
        }
        if character_count >= MAX_RUNTIME_ERROR_CHARS {
            break;
        }
    }

    if bounded.ends_with(' ') {
        bounded.pop();
    }
    if bounded.is_empty() {
        "UPnP gateway returned an empty error".into()
    } else {
        bounded
    }
}

fn materialize_gateway_endpoints(
    endpoints: &[MappingEndpoint],
    gateway_address: Ipv4Addr,
) -> Vec<MappingEndpoint> {
    endpoints
        .iter()
        .filter_map(|endpoint| {
            if !endpoint.active {
                return None;
            }
            let IpAddr::V4(address) = endpoint.address else {
                return None;
            };
            if address.is_unspecified() || address == gateway_address {
                Some(MappingEndpoint::new(
                    IpAddr::V4(gateway_address),
                    endpoint.port,
                    endpoint.protocol,
                    true,
                ))
            } else {
                None
            }
        })
        .collect()
}

struct GatewayOwner {
    manager: MappingManager<IgdGateway>,
    internal_address: Ipv4Addr,
}

fn cleanup_gateways(
    gateways: &mut BTreeMap<String, GatewayOwner>,
    active_gateway: &mut Option<String>,
) {
    for owner in gateways.values_mut() {
        let _ = owner.manager.shutdown();
    }
    gateways.retain(|_, owner| owner.manager.owned_mappings().next().is_some());
    *active_gateway = None;
}

fn cleanup_inactive_gateways(
    gateways: &mut BTreeMap<String, GatewayOwner>,
    active_gateway: Option<&str>,
) {
    let inactive: Vec<_> = gateways
        .keys()
        .filter(|identity| Some(identity.as_str()) != active_gateway)
        .cloned()
        .collect();
    for identity in inactive {
        let empty = if let Some(owner) = gateways.get_mut(&identity) {
            let _ = owner.manager.shutdown();
            owner.manager.owned_mappings().next().is_none()
        } else {
            false
        };
        if empty {
            gateways.remove(&identity);
        }
    }
}

fn shutdown_gateways(gateways: &mut BTreeMap<String, GatewayOwner>) {
    for _ in 0..3 {
        let mut retained = false;
        for owner in gateways.values_mut() {
            if owner.manager.shutdown().is_err() {
                retained = true;
            }
        }
        if !retained {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(value: &str) -> SocketAddr {
        value.parse().unwrap()
    }

    #[test]
    fn endpoint_plan_maps_only_live_gamestream_and_wan_management_ports() {
        let plan = build_desired_mappings(
            true,
            &[address("0.0.0.0:47989")],
            &[address("0.0.0.0:47984")],
            Some(47_989),
            &["0.0.0.0:48010".into()],
            ManagementExposureSnapshot {
                wan_enabled: true,
                tls_address: Some(address("0.0.0.0:49999")),
            },
        );
        let ports = plan
            .endpoints
            .iter()
            .map(|endpoint| (endpoint.port, endpoint.protocol))
            .collect::<Vec<_>>();
        assert_eq!(
            ports,
            [
                (47_989, Protocol::Tcp),
                (47_984, Protocol::Tcp),
                (48_010, Protocol::Tcp),
                (47_998, Protocol::Udp),
                (48_000, Protocol::Udp),
                (47_999, Protocol::Udp),
                (49_999, Protocol::Tcp),
            ]
        );
    }

    #[test]
    fn endpoint_plan_omits_stale_listeners_and_non_wan_management() {
        let plan = build_desired_mappings(
            true,
            &[address("0.0.0.0:47989")],
            &[address("0.0.0.0:47983")],
            Some(47_989),
            &["0.0.0.0:48011".into()],
            ManagementExposureSnapshot {
                wan_enabled: false,
                tls_address: Some(address("0.0.0.0:49999")),
            },
        );
        assert_eq!(
            plan.endpoints
                .iter()
                .map(|endpoint| endpoint.port)
                .collect::<Vec<_>>(),
            [47_989]
        );
    }

    #[test]
    fn only_wildcard_or_gateway_interface_listeners_are_materialized() {
        let gateway = Ipv4Addr::new(192, 168, 1, 10);
        let endpoints = [
            MappingEndpoint::new("0.0.0.0".parse().unwrap(), 47989, Protocol::Tcp, true),
            MappingEndpoint::new("192.168.1.10".parse().unwrap(), 47984, Protocol::Tcp, true),
            MappingEndpoint::new("192.168.1.11".parse().unwrap(), 48010, Protocol::Tcp, true),
            MappingEndpoint::new("127.0.0.1".parse().unwrap(), 47989, Protocol::Tcp, true),
            MappingEndpoint::new("::".parse().unwrap(), 47989, Protocol::Tcp, true),
        ];
        let mapped = materialize_gateway_endpoints(&endpoints, gateway);
        assert_eq!(mapped.len(), 2);
        assert!(
            mapped
                .iter()
                .all(|endpoint| endpoint.address == IpAddr::V4(gateway))
        );
        assert_eq!(mapped[0].port, 47_989);
        assert_eq!(mapped[1].port, 47_984);
    }

    #[test]
    fn management_exposure_requires_a_wan_gate_and_a_tls_socket() {
        assert_eq!(
            parse_management_exposure(&json!({
                "ok":true,"wan_enabled":true,"tls_address":"0.0.0.0:49999"
            })),
            ManagementExposureSnapshot {
                wan_enabled: true,
                tls_address: Some(address("0.0.0.0:49999")),
            }
        );
        for response in [
            json!({"ok":true,"wan_enabled":false,"tls_address":"0.0.0.0:49999"}),
            json!({"ok":true,"wan_enabled":true,"tls_address":null}),
            json!({"ok":false,"wan_enabled":true,"tls_address":"0.0.0.0:49999"}),
        ] {
            assert_eq!(
                parse_management_exposure(&response),
                ManagementExposureSnapshot::default()
            );
        }
    }

    #[test]
    fn runtime_errors_are_normalized_and_bounded_before_publication() {
        let error = bounded_runtime_error(format!(
            "  gateway\r\n error\t{}  ",
            "x".repeat(MAX_RUNTIME_ERROR_CHARS * 8)
        ));

        assert!(error.starts_with("gateway error x"));
        assert_eq!(error.chars().count(), MAX_RUNTIME_ERROR_CHARS);
        assert!(!error.chars().any(char::is_control));
        assert!(!error.contains("  "));
    }
}
