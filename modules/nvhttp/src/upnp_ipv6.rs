//! IPv6 UPnP firewall pinholes for the live listener endpoints.

use amaterasu_module_sdk::network::Network;
use amaterasu_upnp::{
    IgdIpv6Firewall, Ipv6FirewallControl, OperationDeadline, PinholeEndpoint, PinholeManager,
    RECOMMENDED_REFRESH_INTERVAL, discover_ipv6_firewalls_with_deadline,
};
use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv6Addr};
use std::sync::mpsc::RecvTimeoutError;
use std::thread;
use std::time::{Duration, Instant};

use crate::upnp::{
    LifecycleCommand, LifecycleReceiver, LifecycleSender, LifecycleWorker, UpnpRuntimeStatus,
    UpnpStatusMailbox, WorkerCompletion, lifecycle_channel,
};

const CLEANUP_RETRY_INTERVAL: Duration = Duration::from_secs(5);
const MAX_RUNTIME_ERROR_CHARS: usize = 256;
const DEFAULT_SHUTDOWN_BUDGET: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct DesiredPinholes {
    enabled: bool,
    endpoints: Vec<amaterasu_upnp::MappingEndpoint>,
}

/// Owns the IPv6 pinhole worker and makes orderly cleanup part of module shutdown.
pub(crate) struct Ipv6PinholeLifecycle {
    commands: LifecycleSender<DesiredPinholes>,
    statuses: UpnpStatusMailbox,
    worker: LifecycleWorker<BTreeMap<String, Ipv6GatewayOwner<IgdIpv6Firewall>>>,
    deadline: OperationDeadline,
    last_desired: Option<DesiredPinholes>,
    status: UpnpRuntimeStatus,
}

impl Ipv6PinholeLifecycle {
    pub(crate) fn new(network: Option<Network>, deadline: OperationDeadline) -> Self {
        let (command_tx, command_rx) = lifecycle_channel();
        let statuses = UpnpStatusMailbox::default();
        let worker_statuses = statuses.clone();
        let worker_deadline = deadline.clone();
        let worker = thread::Builder::new()
            .name("amaterasu-upnp-ipv6".into())
            .spawn(move || run_worker(network, command_rx, worker_statuses, worker_deadline));
        let status = if worker.is_ok() {
            UpnpRuntimeStatus {
                status: "disabled".into(),
                error: None,
            }
        } else {
            UpnpRuntimeStatus {
                status: "error".into(),
                error: Some("IPv6 UPnP lifecycle worker could not be started".into()),
            }
        };
        Self {
            commands: command_tx,
            statuses,
            worker: LifecycleWorker::new(worker, "IPv6 UPnP lifecycle"),
            deadline,
            last_desired: None,
            status,
        }
    }

    pub(crate) fn update(
        &mut self,
        enabled: bool,
        address_family_includes_ipv6: bool,
        endpoints: &[amaterasu_upnp::MappingEndpoint],
    ) {
        let desired = DesiredPinholes {
            enabled: enabled && address_family_includes_ipv6,
            endpoints: endpoints.to_vec(),
        };
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
                    error: Some("IPv6 UPnP lifecycle worker stopped unexpectedly".into()),
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

    pub(crate) fn request_shutdown(&self) {
        let _ = self.commands.send(LifecycleCommand::Shutdown);
    }

    pub(crate) fn poll_shutdown(&mut self) -> Result<bool, String> {
        let outcome = self.worker.poll().and_then(|ready| {
            if !ready {
                return Ok(false);
            }
            let retained = self
                .worker
                .completion()
                .map_or(0, |completion| owned_pinhole_count(&completion.owners));
            if retained > 0 {
                Err(format!(
                    "{retained} IPv6 pinhole(s) remain pending cleanup after worker retirement"
                ))
            } else {
                Ok(true)
            }
        });
        while let Ok(status) = self.statuses.try_recv() {
            self.status = status;
        }
        if let Err(error) = &outcome {
            self.status = UpnpRuntimeStatus {
                status: "error".into(),
                error: Some(error.clone()),
            };
        }
        outcome
    }

    pub(crate) fn shutdown(&mut self) {
        install_default_shutdown_deadline(&self.deadline);
        self.request_shutdown();
        // Module destruction is the final safety barrier. A live worker retains
        // network-provider callbacks and must finish before its code can unload.
        self.worker.finish();
    }
}

impl Drop for Ipv6PinholeLifecycle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn install_default_shutdown_deadline(deadline: &OperationDeadline) {
    if deadline.deadline().is_none() {
        deadline.restrict(Instant::now() + DEFAULT_SHUTDOWN_BUDGET);
    }
}

fn run_worker(
    network: Option<Network>,
    commands: LifecycleReceiver<DesiredPinholes>,
    statuses: UpnpStatusMailbox,
    deadline: OperationDeadline,
) -> WorkerCompletion<BTreeMap<String, Ipv6GatewayOwner<IgdIpv6Firewall>>> {
    let mut desired = DesiredPinholes::default();
    let mut gateways: BTreeMap<String, Ipv6GatewayOwner<IgdIpv6Firewall>> = BTreeMap::new();
    let mut active_gateways = BTreeSet::new();
    let mut next_discovery = Instant::now();
    let mut next_retry = Instant::now();
    let mut configuration_changed = true;
    let mut last_discovery_error = None;

    loop {
        let has_candidate = desired.enabled && has_ipv6_candidate(&desired.endpoints);
        let wakeup = next_worker_wakeup(has_candidate, next_discovery, next_retry);
        let timeout = wakeup.saturating_duration_since(Instant::now());
        let mut shutdown = false;
        match commands.recv_timeout(timeout) {
            Ok(LifecycleCommand::Configure(next)) => {
                configuration_changed |= next != desired;
                desired = next;
            }
            Ok(LifecycleCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                shutdown = true;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }

        if shutdown {
            let error = shutdown_gateways(&mut gateways, &deadline);
            let status = error.clone().map_or_else(
                || UpnpRuntimeStatus {
                    status: "disabled".into(),
                    error: None,
                },
                |error| UpnpRuntimeStatus {
                    status: "error".into(),
                    error: Some(error),
                },
            );
            let _ = statuses.send(status);
            return WorkerCompletion {
                owners: gateways,
                error,
            };
        }

        let now = Instant::now();
        let has_candidate = desired.enabled && has_ipv6_candidate(&desired.endpoints);
        let rediscover =
            should_rediscover(has_candidate, configuration_changed, now, next_discovery)
                || (has_candidate && last_discovery_error.is_some() && now >= next_retry);
        let status = if desired.enabled {
            if !has_candidate {
                active_gateways.clear();
                let cleanup_error = cleanup_gateways(&mut gateways);
                next_retry = Instant::now() + CLEANUP_RETRY_INTERVAL;
                cleanup_status_or(
                    cleanup_error,
                    UpnpRuntimeStatus {
                        status: "waiting_for_listener".into(),
                        error: None,
                    },
                )
            } else {
                let mut error = last_discovery_error.clone();
                if rediscover {
                    configuration_changed = false;
                    match network
                        .as_ref()
                        .ok_or_else(|| {
                            amaterasu_upnp::GatewayError::new(
                                "IPv6 UPnP network provider unavailable",
                            )
                        })
                        .and_then(|network| {
                            discover_ipv6_firewalls_with_deadline(network, deadline.clone())
                        }) {
                        Ok(discovered) => {
                            if !discovered.is_empty() {
                                last_discovery_error = None;
                                error = None;
                                let identities: BTreeSet<_> = discovered
                                    .iter()
                                    .map(|gateway| gateway.identity().to_owned())
                                    .collect();
                                active_gateways = identities;
                                for gateway in discovered {
                                    let identity = gateway.identity().to_owned();
                                    let internal_addresses = gateway.internal_addresses().to_vec();
                                    match gateways.get_mut(&identity) {
                                        Some(owner) => {
                                            owner.manager.replace_control_adapter(gateway);
                                            owner.internal_addresses = internal_addresses;
                                        }
                                        None => {
                                            gateways.insert(
                                                identity,
                                                Ipv6GatewayOwner {
                                                    manager: PinholeManager::new(gateway),
                                                    internal_addresses,
                                                },
                                            );
                                        }
                                    }
                                }
                            } else if active_gateways.is_empty() {
                                let message =
                                    "no IPv6 IGD with WANIPv6FirewallControl responded".to_owned();
                                last_discovery_error = Some(message.clone());
                                error = Some(message);
                            }
                        }
                        Err(discovery_error) => {
                            let message = bounded_runtime_error(discovery_error);
                            last_discovery_error = Some(message.clone());
                            error = Some(message);
                        }
                    }
                    next_discovery = Instant::now() + RECOMMENDED_REFRESH_INTERVAL;
                }

                if let Some(cleanup_error) =
                    cleanup_inactive_gateways(&mut gateways, &active_gateways)
                {
                    error.get_or_insert(cleanup_error);
                }
                let mut active_pinhole_count = 0usize;
                let mut pinhole_policy_denied = false;
                for identity in &active_gateways {
                    let Some(owner) = gateways.get_mut(identity) else {
                        continue;
                    };
                    let endpoints = materialize_pinhole_endpoints(
                        &desired.endpoints,
                        &owner.internal_addresses,
                    );
                    match owner.manager.reconcile(&endpoints) {
                        Ok(report) => {
                            active_pinhole_count += owner.manager.owned_pinholes().count();
                            if !report.allowed {
                                pinhole_policy_denied = true;
                                continue;
                            }
                        }
                        Err(reconcile_error) => {
                            error.get_or_insert_with(|| bounded_runtime_error(reconcile_error));
                            active_pinhole_count += owner.manager.owned_pinholes().count();
                        }
                    }
                }
                next_retry = if error.is_some() {
                    Instant::now() + CLEANUP_RETRY_INTERVAL
                } else {
                    next_discovery
                };
                if active_pinhole_count > 0 {
                    UpnpRuntimeStatus {
                        status: if error.is_some() {
                            "error".into()
                        } else {
                            "active".into()
                        },
                        error,
                    }
                } else if let Some(error) = error {
                    UpnpRuntimeStatus {
                        status: "error".into(),
                        error: Some(error),
                    }
                } else {
                    inactive_pinhole_status(pinhole_policy_denied)
                }
            }
        } else {
            active_gateways.clear();
            let cleanup_error = cleanup_gateways(&mut gateways);
            next_retry = Instant::now() + CLEANUP_RETRY_INTERVAL;
            cleanup_status_or(
                cleanup_error,
                UpnpRuntimeStatus {
                    status: "disabled".into(),
                    error: None,
                },
            )
        };
        let _ = statuses.send(status);
    }
}

struct Ipv6GatewayOwner<C: Ipv6FirewallControl> {
    manager: PinholeManager<C>,
    internal_addresses: Vec<Ipv6Addr>,
}

fn has_ipv6_candidate(endpoints: &[amaterasu_upnp::MappingEndpoint]) -> bool {
    endpoints.iter().any(|endpoint| {
        endpoint.active
            && matches!(endpoint.address, IpAddr::V6(address) if address.is_unspecified() || routable_listener_address(address))
    })
}

fn routable_listener_address(address: Ipv6Addr) -> bool {
    !address.is_loopback()
        && !address.is_multicast()
        && !address.is_unicast_link_local()
        && !address.is_unspecified()
}

fn materialize_pinhole_endpoints(
    endpoints: &[amaterasu_upnp::MappingEndpoint],
    internal_addresses: &[Ipv6Addr],
) -> Vec<PinholeEndpoint> {
    let Some(preferred_internal_address) = internal_addresses.first().copied() else {
        return Vec::new();
    };
    let mut materialized = BTreeSet::new();
    for endpoint in endpoints {
        if !endpoint.active {
            continue;
        }
        let IpAddr::V6(address) = endpoint.address else {
            continue;
        };
        let internal_client = if address.is_unspecified() {
            preferred_internal_address
        } else if internal_addresses.contains(&address) {
            address
        } else {
            continue;
        };
        if routable_listener_address(internal_client) && endpoint.port != 0 {
            materialized.insert(PinholeEndpoint::new(
                internal_client,
                endpoint.port,
                endpoint.protocol,
            ));
        }
    }
    materialized.into_iter().collect()
}

fn should_rediscover(
    has_candidate: bool,
    configuration_changed: bool,
    now: Instant,
    next_discovery: Instant,
) -> bool {
    has_candidate && (configuration_changed || now >= next_discovery)
}

fn next_worker_wakeup(
    has_candidate: bool,
    next_discovery: Instant,
    next_retry: Instant,
) -> Instant {
    if has_candidate {
        next_discovery.min(next_retry)
    } else {
        next_retry
    }
}

fn cleanup_gateways<C: Ipv6FirewallControl>(
    gateways: &mut BTreeMap<String, Ipv6GatewayOwner<C>>,
) -> Option<String> {
    cleanup_gateways_until(gateways, None)
}

fn cleanup_gateways_until<C: Ipv6FirewallControl>(
    gateways: &mut BTreeMap<String, Ipv6GatewayOwner<C>>,
    deadline: Option<&OperationDeadline>,
) -> Option<String> {
    let mut errors = Vec::new();
    for owner in gateways.values_mut() {
        if owner.manager.owned_pinholes().next().is_none() {
            continue;
        }
        let operation_deadline = deadline.and_then(OperationDeadline::deadline);
        if operation_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            errors.push("IPv6 UPnP cleanup deadline expired".into());
            break;
        }
        let cleanup = match operation_deadline {
            Some(deadline) => owner.manager.shutdown_until(deadline),
            None => owner.manager.shutdown(),
        };
        if let Err(error) = cleanup {
            errors.push(bounded_runtime_error(error));
        }
    }
    gateways.retain(|_, owner| owner.manager.owned_pinholes().next().is_some());
    let retained = owned_pinhole_count(gateways);
    if retained > 0 {
        errors.push(format!("{retained} IPv6 pinhole(s) remain pending cleanup"));
    }
    combine_runtime_errors(errors)
}

fn cleanup_inactive_gateways<C: Ipv6FirewallControl>(
    gateways: &mut BTreeMap<String, Ipv6GatewayOwner<C>>,
    active: &BTreeSet<String>,
) -> Option<String> {
    let inactive: Vec<_> = gateways
        .keys()
        .filter(|identity| !active.contains(*identity))
        .cloned()
        .collect();
    let mut errors = Vec::new();
    for identity in inactive {
        if let Some(owner) = gateways.get_mut(&identity) {
            if let Err(error) = owner.manager.shutdown() {
                errors.push(bounded_runtime_error(error));
            }
            if owner.manager.owned_pinholes().next().is_none() {
                gateways.remove(&identity);
            }
        }
    }
    combine_runtime_errors(errors)
}

fn shutdown_gateways<C: Ipv6FirewallControl>(
    gateways: &mut BTreeMap<String, Ipv6GatewayOwner<C>>,
    deadline: &OperationDeadline,
) -> Option<String> {
    install_default_shutdown_deadline(deadline);
    let mut last_error = cleanup_gateways_until(gateways, Some(deadline))?;
    for _ in 1..3 {
        if deadline.is_expired() {
            break;
        }
        last_error = cleanup_gateways_until(gateways, Some(deadline))?;
    }
    let retained = owned_pinhole_count(gateways);
    Some(bounded_runtime_error(format!(
        "Could not remove {retained} owned IPv6 pinhole(s) within the shutdown deadline and at most three shutdown attempts; leases may remain active for up to 3600 seconds. Last error: {}",
        last_error
    )))
}

fn owned_pinhole_count<C: Ipv6FirewallControl>(
    gateways: &BTreeMap<String, Ipv6GatewayOwner<C>>,
) -> usize {
    gateways
        .values()
        .map(|owner| owner.manager.owned_pinholes().count())
        .sum()
}

fn cleanup_status_or(error: Option<String>, fallback: UpnpRuntimeStatus) -> UpnpRuntimeStatus {
    error.map_or(fallback, |error| UpnpRuntimeStatus {
        status: "error".into(),
        error: Some(error),
    })
}

fn inactive_pinhole_status(policy_denied: bool) -> UpnpRuntimeStatus {
    if policy_denied {
        UpnpRuntimeStatus {
            status: "error".into(),
            error: Some("IPv6 gateway policy disallows inbound pinholes".into()),
        }
    } else {
        UpnpRuntimeStatus {
            status: "waiting_for_gateway_route".into(),
            error: None,
        }
    }
}

fn combine_runtime_errors(errors: impl IntoIterator<Item = String>) -> Option<String> {
    let errors: Vec<_> = errors.into_iter().collect();
    (!errors.is_empty()).then(|| bounded_runtime_error(errors.join("; ")))
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
        "IPv6 UPnP gateway returned an empty error".into()
    } else {
        bounded
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_upnp::{
        FirewallStatus, GatewayError, Ipv6FirewallControl, MappingEndpoint, PinholeDeleteOutcome,
        PinholeRefreshOutcome, Protocol,
    };
    use std::collections::BTreeSet;
    use std::sync::{Arc, Condvar, Mutex, mpsc};

    #[derive(Clone, Default)]
    struct FakeFirewall(Arc<Mutex<FakeFirewallState>>);

    struct FakeFirewallState {
        next_id: u16,
        owned_ids: BTreeSet<u16>,
        delete_failures: usize,
        delete_attempts: usize,
        pinhole_allowed: bool,
        cleanup_gate: Option<Arc<CleanupGate>>,
        expire_after_delete: Option<OperationDeadline>,
    }

    impl Default for FakeFirewallState {
        fn default() -> Self {
            Self {
                next_id: 0,
                owned_ids: BTreeSet::new(),
                delete_failures: 0,
                delete_attempts: 0,
                pinhole_allowed: true,
                cleanup_gate: None,
                expire_after_delete: None,
            }
        }
    }

    impl FakeFirewall {
        fn fail_deletes(&self, count: usize) {
            self.0.lock().unwrap().delete_failures = count;
        }

        fn allow_pinholes(&self, allowed: bool) {
            self.0.lock().unwrap().pinhole_allowed = allowed;
        }

        fn delete_attempts(&self) -> usize {
            self.0.lock().unwrap().delete_attempts
        }

        fn pinhole_count(&self) -> usize {
            self.0.lock().unwrap().owned_ids.len()
        }
    }

    impl Ipv6FirewallControl for FakeFirewall {
        fn firewall_status(&mut self) -> Result<FirewallStatus, GatewayError> {
            Ok(FirewallStatus {
                firewall_enabled: true,
                pinhole_allowed: self.0.lock().unwrap().pinhole_allowed,
            })
        }

        fn add_pinhole(
            &mut self,
            _endpoint: PinholeEndpoint,
            _lease_duration_seconds: u32,
        ) -> Result<u16, GatewayError> {
            let mut state = self.0.lock().unwrap();
            state.next_id = state.next_id.saturating_add(1).max(1);
            let unique_id = state.next_id;
            state.owned_ids.insert(unique_id);
            Ok(unique_id)
        }

        fn refresh_pinhole(
            &mut self,
            unique_id: u16,
            _lease_duration_seconds: u32,
        ) -> Result<PinholeRefreshOutcome, GatewayError> {
            Ok(if self.0.lock().unwrap().owned_ids.contains(&unique_id) {
                PinholeRefreshOutcome::Refreshed
            } else {
                PinholeRefreshOutcome::Missing
            })
        }

        fn delete_pinhole(&mut self, unique_id: u16) -> Result<PinholeDeleteOutcome, GatewayError> {
            let mut state = self.0.lock().unwrap();
            state.delete_attempts += 1;
            if state.delete_failures > 0 {
                state.delete_failures -= 1;
                return Err(GatewayError::new("injected pinhole cleanup failure"));
            }
            let outcome = if state.owned_ids.remove(&unique_id) {
                PinholeDeleteOutcome::Deleted
            } else {
                PinholeDeleteOutcome::Missing
            };
            if let Some(deadline) = state.expire_after_delete.take() {
                deadline.restrict(Instant::now());
            }
            Ok(outcome)
        }

        fn delete_pinhole_until(
            &mut self,
            unique_id: u16,
            deadline: Instant,
        ) -> Result<PinholeDeleteOutcome, GatewayError> {
            let gate = self.0.lock().unwrap().cleanup_gate.clone();
            if let Some(gate) = gate {
                let _ = gate.entered.send(());
                let released = gate.released.lock().unwrap();
                let (released, _) = gate
                    .changed
                    .wait_timeout_while(
                        released,
                        deadline.saturating_duration_since(Instant::now()),
                        |released| !*released,
                    )
                    .unwrap();
                if !*released {
                    return Err(GatewayError::new("mock IPv6 cleanup deadline expired"));
                }
            }
            if Instant::now() >= deadline {
                return Err(GatewayError::new("mock IPv6 cleanup deadline expired"));
            }
            self.delete_pinhole(unique_id)
        }
    }

    struct CleanupGate {
        entered: mpsc::Sender<()>,
        released: Mutex<bool>,
        changed: Condvar,
    }

    fn owned_gateway(control: FakeFirewall) -> Ipv6GatewayOwner<FakeFirewall> {
        let endpoint = PinholeEndpoint::new("2001:db8::10".parse().unwrap(), 47989, Protocol::Tcp);
        let mut manager = PinholeManager::new(control);
        manager.reconcile(&[endpoint]).unwrap();
        Ipv6GatewayOwner {
            manager,
            internal_addresses: vec![endpoint.internal_client],
        }
    }

    fn gate_cleanup(control: &FakeFirewall) -> (Arc<CleanupGate>, mpsc::Receiver<()>) {
        let (entered, receiver) = mpsc::channel();
        let gate = Arc::new(CleanupGate {
            entered,
            released: Mutex::new(false),
            changed: Condvar::new(),
        });
        control.0.lock().unwrap().cleanup_gate = Some(gate.clone());
        (gate, receiver)
    }

    #[test]
    fn slow_pinhole_cleanup_is_polled_without_joining_a_live_worker() {
        let control = FakeFirewall::default();
        let owner = owned_gateway(control.clone());
        let (gate, entered) = gate_cleanup(&control);
        let deadline = OperationDeadline::default();
        let worker = thread::spawn(move || {
            deadline.restrict(Instant::now() + Duration::from_secs(2));
            let mut gateways = BTreeMap::from([("gateway".to_owned(), owner)]);
            let error = shutdown_gateways(&mut gateways, &deadline);
            WorkerCompletion {
                owners: gateways,
                error,
            }
        });
        let mut retirement = LifecycleWorker::new(Ok(worker), "test IPv6 worker startup failed");
        entered.recv_timeout(Duration::from_secs(1)).unwrap();

        let poll_started = Instant::now();
        assert_eq!(retirement.poll(), Ok(false));
        assert!(poll_started.elapsed() < Duration::from_millis(100));
        assert_eq!(control.pinhole_count(), 1);
        assert!(retirement.completion().is_none());

        *gate.released.lock().unwrap() = true;
        gate.changed.notify_all();
        let wait_deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if retirement.poll().unwrap() {
                break;
            }
            assert!(
                Instant::now() < wait_deadline,
                "cleanup worker failed to retire"
            );
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(control.pinhole_count(), 0);
        assert!(retirement.completion().unwrap().owners.is_empty());
    }

    #[test]
    fn shutdown_deadline_is_shared_across_gateways_and_retry_attempts() {
        let slow_control = FakeFirewall::default();
        let untouched_control = FakeFirewall::default();
        let first = owned_gateway(slow_control.clone());
        let second = owned_gateway(untouched_control.clone());
        let (_gate, entered) = gate_cleanup(&slow_control);
        let deadline = OperationDeadline::default();
        let worker = thread::spawn(move || {
            deadline.restrict(Instant::now() + Duration::from_millis(40));
            let mut gateways = BTreeMap::from([
                ("first-gateway".to_owned(), first),
                ("second-gateway".to_owned(), second),
            ]);
            let error = shutdown_gateways(&mut gateways, &deadline);
            WorkerCompletion {
                owners: gateways,
                error,
            }
        });
        let mut retirement = LifecycleWorker::new(Ok(worker), "test IPv6 worker startup failed");
        entered.recv_timeout(Duration::from_secs(1)).unwrap();
        retirement.finish();

        assert!(retirement.poll().unwrap_err().contains("shutdown deadline"));
        assert_eq!(
            owned_pinhole_count(&retirement.completion().unwrap().owners),
            2
        );
        assert_eq!(slow_control.pinhole_count(), 1);
        assert_eq!(untouched_control.pinhole_count(), 1);
        assert_eq!(untouched_control.delete_attempts(), 0);
        assert!(entered.try_recv().is_err(), "expired cleanup was retried");
        // Polling an error preserves the exact managers for subsequent diagnosis.
        assert!(retirement.poll().is_err());
        assert_eq!(
            owned_pinhole_count(&retirement.completion().unwrap().owners),
            2
        );
    }

    #[test]
    fn expired_shutdown_budget_keeps_owners_without_starting_router_operations() {
        let control = FakeFirewall::default();
        let mut gateways = BTreeMap::from([("gateway".to_owned(), owned_gateway(control.clone()))]);
        let deadline = OperationDeadline::default();
        let expired = Instant::now();
        deadline.restrict(expired);

        assert!(shutdown_gateways(&mut gateways, &deadline).is_some());
        assert_eq!(deadline.deadline(), Some(expired));
        assert_eq!(control.delete_attempts(), 0);
        assert_eq!(owned_pinhole_count(&gateways), 1);
    }

    #[test]
    fn expired_shutdown_budget_accepts_gateways_without_owned_pinholes() {
        let control = FakeFirewall::default();
        control.allow_pinholes(false);
        let mut gateways = BTreeMap::from([("gateway".to_owned(), owned_gateway(control.clone()))]);
        let deadline = OperationDeadline::default();
        deadline.restrict(Instant::now());

        assert_eq!(shutdown_gateways(&mut gateways, &deadline), None);
        assert!(gateways.is_empty());
        assert_eq!(control.pinhole_count(), 0);
        assert_eq!(control.delete_attempts(), 0);
    }

    #[test]
    fn last_pinhole_removed_at_deadline_does_not_make_later_empty_gateways_fail() {
        let control = FakeFirewall::default();
        let owner = owned_gateway(control.clone());
        let empty_control = FakeFirewall::default();
        let empty_owner = Ipv6GatewayOwner {
            manager: PinholeManager::new(empty_control.clone()),
            internal_addresses: Vec::new(),
        };
        let deadline = OperationDeadline::default();
        deadline.restrict(Instant::now() + Duration::from_secs(2));
        control.0.lock().unwrap().expire_after_delete = Some(deadline.clone());
        let mut gateways = BTreeMap::from([
            ("first-owned-gateway".to_owned(), owner),
            ("last-empty-gateway".to_owned(), empty_owner),
        ]);

        assert_eq!(shutdown_gateways(&mut gateways, &deadline), None);
        assert!(deadline.is_expired());
        assert!(gateways.is_empty());
        assert_eq!(control.pinhole_count(), 0);
        assert_eq!(control.delete_attempts(), 1);
        assert_eq!(empty_control.delete_attempts(), 0);
    }

    #[test]
    fn shutdown_surfaces_and_retries_persistent_pinhole_cleanup_failures() {
        let control = FakeFirewall::default();
        let mut manager = PinholeManager::new(control.clone());
        let endpoint = PinholeEndpoint::new("2001:db8::10".parse().unwrap(), 47989, Protocol::Tcp);
        manager.reconcile(&[endpoint]).unwrap();
        let mut gateways = BTreeMap::from([(
            "gateway".to_owned(),
            Ipv6GatewayOwner {
                manager,
                internal_addresses: vec![endpoint.internal_client],
            },
        )]);
        control.fail_deletes(3);

        let error = shutdown_gateways(&mut gateways, &OperationDeadline::default()).unwrap();

        assert!(error.contains("three shutdown attempts"));
        assert_eq!(control.delete_attempts(), 3);
        assert_eq!(control.pinhole_count(), 1);
        assert_eq!(gateways["gateway"].manager.owned_pinholes().count(), 1);

        assert_eq!(
            shutdown_gateways(&mut gateways, &OperationDeadline::default()),
            None
        );
        assert_eq!(control.delete_attempts(), 4);
        assert_eq!(control.pinhole_count(), 0);
        assert!(gateways.is_empty());
    }

    #[test]
    fn inactive_gateway_cleanup_failure_remains_owned_for_retry() {
        let control = FakeFirewall::default();
        let mut manager = PinholeManager::new(control.clone());
        let endpoint = PinholeEndpoint::new("2001:db8::10".parse().unwrap(), 47989, Protocol::Tcp);
        manager.reconcile(&[endpoint]).unwrap();
        let mut gateways = BTreeMap::from([(
            "stale-gateway".to_owned(),
            Ipv6GatewayOwner {
                manager,
                internal_addresses: vec![endpoint.internal_client],
            },
        )]);
        control.fail_deletes(1);

        let first_error = cleanup_inactive_gateways(&mut gateways, &BTreeSet::new());

        assert!(first_error.is_some());
        assert_eq!(control.pinhole_count(), 1);
        assert_eq!(
            gateways["stale-gateway"].manager.owned_pinholes().count(),
            1
        );

        assert_eq!(
            cleanup_inactive_gateways(&mut gateways, &BTreeSet::new()),
            None
        );
        assert_eq!(control.pinhole_count(), 0);
        assert!(gateways.is_empty());
    }

    #[test]
    fn pinholes_follow_only_active_ipv6_listeners_on_discovered_interface() {
        let local_address: Ipv6Addr = "2001:db8::10".parse().unwrap();
        let plan = [
            MappingEndpoint::new("::".parse().unwrap(), 47989, Protocol::Tcp, true),
            MappingEndpoint::new(IpAddr::V6(local_address), 47998, Protocol::Udp, true),
            MappingEndpoint::new("2001:db8::11".parse().unwrap(), 48000, Protocol::Tcp, true),
            MappingEndpoint::new("::1".parse().unwrap(), 48001, Protocol::Tcp, true),
            MappingEndpoint::new(IpAddr::V6(local_address), 48002, Protocol::Tcp, false),
            MappingEndpoint::new("0.0.0.0".parse().unwrap(), 48003, Protocol::Tcp, true),
        ];

        let materialized = materialize_pinhole_endpoints(&plan, &[local_address]);

        assert_eq!(
            materialized,
            vec![
                PinholeEndpoint::new(local_address, 47989, Protocol::Tcp),
                PinholeEndpoint::new(local_address, 47998, Protocol::Udp),
            ]
        );
    }

    #[test]
    fn wildcard_or_remote_listener_never_becomes_an_unspecified_pinhole_client() {
        let plan = [MappingEndpoint::new(
            "::".parse().unwrap(),
            47989,
            Protocol::Tcp,
            true,
        )];
        assert!(materialize_pinhole_endpoints(&plan, &[]).is_empty());
        assert!(!has_ipv6_candidate(&[
            MappingEndpoint::new("::1".parse().unwrap(), 47989, Protocol::Tcp, true),
            MappingEndpoint::new("fe80::1".parse().unwrap(), 47989, Protocol::Tcp, true),
        ]));
    }

    #[test]
    fn explicit_listeners_on_secondary_interface_addresses_are_preserved() {
        let primary: Ipv6Addr = "2001:db8::10".parse().unwrap();
        let secondary: Ipv6Addr = "2001:db8::11".parse().unwrap();
        let plan = [
            MappingEndpoint::new(IpAddr::V6(secondary), 47998, Protocol::Udp, true),
            MappingEndpoint::new(IpAddr::V6(secondary), 47999, Protocol::Udp, false),
        ];

        assert_eq!(
            materialize_pinhole_endpoints(&plan, &[primary, secondary]),
            vec![PinholeEndpoint::new(secondary, 47998, Protocol::Udp)]
        );
    }

    #[test]
    fn rediscovery_and_refresh_have_independent_deadlines_without_busy_polling() {
        let now = Instant::now();
        let next_discovery = now + RECOMMENDED_REFRESH_INTERVAL;
        let next_retry = now + CLEANUP_RETRY_INTERVAL;
        assert_eq!(
            next_worker_wakeup(true, next_discovery, next_retry),
            next_retry
        );
        assert!(!should_rediscover(true, false, now, next_discovery));
        assert!(should_rediscover(true, true, now, next_discovery));
    }

    #[test]
    fn pinhole_policy_denial_is_visible_when_no_gateway_pinholes_are_active() {
        let control = FakeFirewall::default();
        control.allow_pinholes(false);
        let mut manager = PinholeManager::new(control);
        let report = manager.reconcile(&[]).unwrap();
        assert!(!report.allowed);
        assert_eq!(manager.owned_pinholes().count(), 0);
        let status = inactive_pinhole_status(!report.allowed);

        assert_eq!(status.status, "error");
        assert_eq!(
            status.error.as_deref(),
            Some("IPv6 gateway policy disallows inbound pinholes")
        );
        assert_eq!(
            inactive_pinhole_status(false).status,
            "waiting_for_gateway_route"
        );
    }
}
