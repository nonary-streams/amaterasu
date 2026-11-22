//! Plans optional mappings from NVHTTP-owned listeners and management policy.
use amaterasu_module_sdk::router_mapping::{DesiredMappings, MappingEndpoint, Protocol};
use amaterasu_module_sdk::{Command, Response, RuntimeClient};
use amaterasu_network_policy::GameStreamPorts;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

const MANAGEMENT_SERVICE: &str = "amaterasu.management/1";
const EXPOSURE_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const EXPOSURE_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
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

    pub(crate) fn quiesce(&mut self, runtime: &RuntimeClient) -> bool {
        if let Some(request) = self.pending {
            self.cancelling = true;
            self.cancel_pending(runtime, request);
        }
        self.pending.is_none()
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

pub(crate) fn build_desired_mappings(
    enabled: bool,
    http_addresses: &[SocketAddr],
    https_addresses: &[SocketAddr],
    rtsp_base_port: Option<u16>,
    rtsp_addresses: &[String],
    management: ManagementExposureSnapshot,
    address_family_includes_ipv6: bool,
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
        ipv6_enabled: address_family_includes_ipv6,
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

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_module_sdk::abi::{self, AbiHeader, BorrowedBytes, HostApi, ReplySink};
    use amaterasu_module_sdk::router_mapping::{self, RouterMapping, RouterMappingApi};
    use std::{
        ffi::c_void,
        sync::{Arc, Mutex},
    };
    #[derive(Default)]
    struct ExposureEndpointState {
        commands: Vec<Command>,
        result: Value,
        release_failures_remaining: usize,
        router_mapping: Option<usize>,
    }
    unsafe extern "C" fn exposure_invoke(
        context: *mut c_void,
        input: BorrowedBytes,
        reply: ReplySink,
    ) -> abi::Status {
        // SAFETY: each test retains the Arc that owns this mutex for the runtime lifetime.
        let state = unsafe { &*context.cast::<Mutex<ExposureEndpointState>>() };
        let Ok(bytes) = (unsafe { input.read() }) else {
            return abi::STATUS_INVALID_ARGUMENT;
        };
        let Ok(command) = serde_json::from_slice::<Command>(bytes) else {
            return abi::STATUS_INVALID_ARGUMENT;
        };
        let mut state = state.lock().unwrap();
        state.commands.push(command.clone());
        let response = match command {
            Command::GetSettings {} => Response::Result { payload: json!({}) },
            Command::Register { .. }
            | Command::RegisterApi { .. }
            | Command::Log { .. }
            | Command::HostPackageBarrierRegister {} => Response::Ok,
            Command::Request { .. } => Response::Accepted { id: 41 },
            Command::TakeResult { request: 41 } => Response::Result {
                payload: state.result.clone(),
            },
            Command::ReleaseResult { request: 41 } if state.release_failures_remaining > 0 => {
                state.release_failures_remaining -= 1;
                Response::error("unavailable", "injected release failure")
            }
            Command::ReleaseResult { request: 41 } | Command::Cancel { request: 41 } => {
                Response::Ok
            }
            Command::NativeBindTyped {
                service, contract, ..
            } if service == "test.router/1" && contract == router_mapping::CONTRACT => {
                Response::NativeBound {
                    binding: 7,
                    address: state.router_mapping.unwrap(),
                }
            }
            Command::NativeRelease { binding: 7 } => Response::Ok,
            unexpected => panic!("unexpected management exposure command: {unexpected:?}"),
        };
        let output = serde_json::to_vec(&response).expect("test response");
        // SAFETY: the synchronous reply sink copies output bytes before returning.
        unsafe { reply.send(&output) };
        abi::STATUS_OK
    }

    fn exposure_runtime(state: Arc<Mutex<ExposureEndpointState>>) -> RuntimeClient {
        let api = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: Arc::as_ptr(&state).cast_mut().cast(),
            invoke: exposure_invoke,
        };
        // SAFETY: the caller retains the Arc containing the callback context.
        unsafe { RuntimeClient::from_endpoint(api) }
    }

    fn address(value: &str) -> SocketAddr {
        value.parse().unwrap()
    }

    #[test]
    fn listener_plan_reaches_the_selected_mapping_provider() {
        #[derive(Default)]
        struct Observed {
            desired: Option<DesiredMappings>,
            update_calls: u32,
            prepare_budget: Option<u64>,
            poll_calls: u32,
        }
        unsafe extern "C" fn request(
            context: *mut c_void,
            bytes: BorrowedBytes,
            reply: ReplySink,
        ) -> abi::Status {
            // SAFETY: test holds the context and request for this call.
            let observed = unsafe { &mut *context.cast::<Observed>() };
            let Ok(request) = unsafe { bytes.read() }.and_then(|bytes| {
                serde_json::from_slice(bytes).map_err(|_| abi::STATUS_INVALID_ARGUMENT)
            }) else {
                return abi::STATUS_INVALID_ARGUMENT;
            };
            use router_mapping::{Request, Response, Status};
            let status = || Status {
                status: "active".into(),
                error: None,
            };
            let response = match request {
                Request::Update { desired } => {
                    observed.update_calls += 1;
                    observed.desired = Some(desired);
                    Response::Status { status: status() }
                }
                Request::Status => Response::Status { status: status() },
                Request::PrepareShutdown { remaining_ms } => {
                    observed.prepare_budget = Some(remaining_ms);
                    Response::Accepted
                }
                Request::PollShutdown => {
                    observed.poll_calls += 1;
                    Response::Shutdown {
                        ready: observed.poll_calls > 1,
                        error: None,
                    }
                }
            };
            // SAFETY: synchronous SDK sink copies the response before returning.
            unsafe { reply.send(&serde_json::to_vec(&response).unwrap()) };
            abi::STATUS_OK
        }

        let mut observed = Observed::default();
        let api = RouterMappingApi {
            base: abi::NativeInterface {
                header: AbiHeader::of::<RouterMappingApi>(),
                context: (&mut observed as *mut Observed).cast(),
                release: None,
            },
            request: Some(request),
        };
        let state = Arc::new(Mutex::new(ExposureEndpointState {
            router_mapping: Some(&api as *const RouterMappingApi as usize),
            ..ExposureEndpointState::default()
        }));
        let runtime = exposure_runtime(state.clone());
        let desired = build_desired_mappings(
            true,
            &[address("0.0.0.0:47989")],
            &[],
            None,
            &[],
            ManagementExposureSnapshot::default(),
            false,
        );
        let mut provider = RouterMapping::bind(&runtime, "test.router/1").unwrap();
        assert_eq!(provider.update(&desired).unwrap().status, "active");
        assert_eq!(provider.update(&desired).unwrap().status, "active");
        provider.prepare_shutdown(314).unwrap();
        assert!(!provider.poll_shutdown().unwrap());
        assert!(provider.poll_shutdown().unwrap());
        drop(provider);
        assert_eq!(observed.desired, Some(desired));
        assert_eq!(observed.update_calls, 1);
        assert_eq!(observed.prepare_budget, Some(314));
        assert!(
            state
                .lock()
                .unwrap()
                .commands
                .iter()
                .any(|command| { matches!(command, Command::NativeRelease { binding: 7 }) })
        );
    }

    #[test]
    fn management_exposure_quiescence_keeps_retrying_the_same_pending_request() {
        let state = Arc::new(Mutex::new(ExposureEndpointState {
            release_failures_remaining: 1,
            ..ExposureEndpointState::default()
        }));
        let runtime = exposure_runtime(state.clone());
        let mut exposure = ManagementExposure::default();
        exposure.tick(&runtime);
        assert!(!exposure.quiesce(&runtime));
        assert_eq!(exposure.pending, Some(41));
        assert!(exposure.quiesce(&runtime));
        assert!(exposure.pending.is_none());
        let commands = &state.lock().unwrap().commands;
        assert_eq!(
            commands
                .iter()
                .filter(|command| matches!(command, Command::Request { .. }))
                .count(),
            1
        );
        assert_eq!(
            commands
                .iter()
                .filter(|command| matches!(command, Command::ReleaseResult { request: 41 }))
                .count(),
            2
        );
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
            false,
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
            false,
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
    fn endpoint_plan_keeps_ipv6_family_gate_separate_from_active_listeners() {
        let plan = build_desired_mappings(
            true,
            &[address("0.0.0.0:47989"), address("[::]:47989")],
            &[address("0.0.0.0:47984"), address("[::]:47984")],
            Some(47_989),
            &["0.0.0.0:48010".into(), "[::]:48010".into()],
            ManagementExposureSnapshot::default(),
            true,
        );

        assert!(plan.ipv6_enabled);
        assert!(
            plan.endpoints
                .iter()
                .any(|endpoint| endpoint.address.is_ipv6())
        );
        assert!(
            plan.endpoints
                .iter()
                .any(|endpoint| endpoint.address.is_ipv4())
        );
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
    fn management_exposure_releases_completed_runtime_results() {
        let state = Arc::new(Mutex::new(ExposureEndpointState {
            result: json!({
                "ok":true,
                "wan_enabled":true,
                "tls_address":"0.0.0.0:49999"
            }),
            ..ExposureEndpointState::default()
        }));
        let runtime = exposure_runtime(state.clone());
        let mut exposure = ManagementExposure::default();

        exposure.tick(&runtime);
        exposure.tick(&runtime);

        assert_eq!(
            exposure.snapshot(),
            ManagementExposureSnapshot {
                wan_enabled: true,
                tls_address: Some(address("0.0.0.0:49999")),
            }
        );
        let commands = &state.lock().unwrap().commands;
        assert_eq!(
            commands
                .iter()
                .filter(|command| matches!(command, Command::ReleaseResult { request: 41 }))
                .count(),
            1
        );
    }

    #[test]
    fn management_exposure_retries_cleanup_after_result_release_failure() {
        let state = Arc::new(Mutex::new(ExposureEndpointState {
            result: json!({
                "ok":true,
                "wan_enabled":true,
                "tls_address":"0.0.0.0:49999"
            }),
            release_failures_remaining: 1,
            ..ExposureEndpointState::default()
        }));
        let runtime = exposure_runtime(state.clone());
        let mut exposure = ManagementExposure::default();

        exposure.tick(&runtime);
        exposure.tick(&runtime);

        assert_eq!(exposure.snapshot(), ManagementExposureSnapshot::default());
        let commands = &state.lock().unwrap().commands;
        assert_eq!(
            commands
                .iter()
                .filter(|command| matches!(command, Command::ReleaseResult { request: 41 }))
                .count(),
            2
        );
        assert_eq!(
            commands
                .iter()
                .filter(|command| matches!(command, Command::Cancel { request: 41 }))
                .count(),
            1
        );
    }
}
