//! Provider-owned IPv4 gateway mapping and IPv6 pinhole lifecycles.
use crate::{
    Gateway, IgdGateway, MappingEndpoint, MappingManager, OperationDeadline,
    RECOMMENDED_REFRESH_INTERVAL, discover_gateway_with_deadline,
};
use amaterasu_module_sdk::network::Network;
use amaterasu_module_sdk::router_mapping::{DesiredMappings, Status as UpnpRuntimeStatus};
use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{
    Arc, Mutex,
    mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
const CLEANUP_RETRY_INTERVAL: Duration = Duration::from_secs(5);
const MAX_RUNTIME_ERROR_CHARS: usize = 256;
const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
#[derive(Clone, Default)]
pub(crate) struct UpnpStatusMailbox(Arc<Mutex<Option<UpnpRuntimeStatus>>>);

impl UpnpStatusMailbox {
    pub(crate) fn send(&self, status: UpnpRuntimeStatus) -> Result<(), ()> {
        *self.0.lock().map_err(|_| ())? = Some(status);
        Ok(())
    }

    pub(crate) fn try_recv(&self) -> Result<UpnpRuntimeStatus, ()> {
        self.0.lock().map_err(|_| ())?.take().ok_or(())
    }
}

#[derive(Debug)]
pub(crate) enum LifecycleCommand<T> {
    Configure(T),
    Shutdown,
}

struct LifecycleMailbox<T> {
    latest_configuration: Option<T>,
    shutdown: bool,
}

pub(crate) struct LifecycleSender<T> {
    mailbox: Arc<Mutex<LifecycleMailbox<T>>>,
    wakeup: SyncSender<()>,
}

pub(crate) struct LifecycleReceiver<T> {
    mailbox: Arc<Mutex<LifecycleMailbox<T>>>,
    wakeup: Receiver<()>,
}

/// Listener changes replace obsolete desired state instead of queuing more router
/// work. Shutdown has its own sticky flag and always preempts pending changes.
pub(crate) fn lifecycle_channel<T>() -> (LifecycleSender<T>, LifecycleReceiver<T>) {
    let mailbox = Arc::new(Mutex::new(LifecycleMailbox {
        latest_configuration: None,
        shutdown: false,
    }));
    let (wakeup_tx, wakeup_rx) = mpsc::sync_channel(1);
    (
        LifecycleSender {
            mailbox: mailbox.clone(),
            wakeup: wakeup_tx,
        },
        LifecycleReceiver {
            mailbox,
            wakeup: wakeup_rx,
        },
    )
}

impl<T> LifecycleSender<T> {
    pub(crate) fn send(&self, command: LifecycleCommand<T>) -> Result<(), ()> {
        let mut mailbox = self.mailbox.lock().map_err(|_| ())?;
        match command {
            LifecycleCommand::Configure(configuration) if !mailbox.shutdown => {
                mailbox.latest_configuration = Some(configuration);
            }
            LifecycleCommand::Configure(_) => return Err(()),
            LifecycleCommand::Shutdown => {
                mailbox.shutdown = true;
                mailbox.latest_configuration = None;
            }
        }
        match self.wakeup.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => Ok(()),
            Err(TrySendError::Disconnected(())) => Err(()),
        }
    }
}

impl<T> LifecycleReceiver<T> {
    pub(crate) fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> Result<LifecycleCommand<T>, RecvTimeoutError> {
        let now = Instant::now();
        let deadline = now.checked_add(timeout).unwrap_or(now);
        loop {
            {
                let mut mailbox = self
                    .mailbox
                    .lock()
                    .map_err(|_| RecvTimeoutError::Disconnected)?;
                if mailbox.shutdown {
                    return Ok(LifecycleCommand::Shutdown);
                }
                if let Some(configuration) = mailbox.latest_configuration.take() {
                    return Ok(LifecycleCommand::Configure(configuration));
                }
            }
            self.wakeup
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
        }
    }
}

/// A worker returns its exact remaining owners instead of losing them on error.
pub(crate) struct WorkerCompletion<T> {
    pub(crate) owners: T,
    pub(crate) error: Option<String>,
}

/// Polling never waits for live native work. Failed cleanup keeps both its
/// evidence and owners until the enclosing module is safely destroyed.
pub(crate) struct LifecycleWorker<T> {
    worker: Option<JoinHandle<WorkerCompletion<T>>>,
    completion: Option<WorkerCompletion<T>>,
    failure: Option<String>,
    never_started: bool,
}

impl<T> LifecycleWorker<T> {
    pub(crate) fn new(
        worker: std::io::Result<JoinHandle<WorkerCompletion<T>>>,
        start_error: &'static str,
    ) -> Self {
        match worker {
            Ok(worker) => Self {
                worker: Some(worker),
                completion: None,
                failure: None,
                never_started: false,
            },
            Err(error) => Self {
                worker: None,
                completion: None,
                failure: Some(format!("{start_error}: {error}")),
                never_started: true,
            },
        }
    }

    pub(crate) fn poll(&mut self) -> Result<bool, String> {
        if self.never_started {
            // A failed spawn drops the unstarted closure synchronously. Feature
            // health still reports the startup error, but no worker, callback,
            // mapping or pinhole exists that shutdown must retire.
            return Ok(true);
        }
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            return Ok(false);
        }
        self.finish();
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        match &self.completion {
            Some(WorkerCompletion {
                error: Some(error), ..
            }) => Err(error.clone()),
            Some(_) => Ok(true),
            None => Err("UPnP lifecycle worker has no retirement evidence".into()),
        }
    }

    pub(crate) fn finish(&mut self) {
        if let Some(worker) = self.worker.take() {
            match worker.join() {
                Ok(completion) => self.completion = Some(completion),
                Err(_) => {
                    self.failure = Some("UPnP lifecycle worker panicked during cleanup".into())
                }
            }
        }
    }

    pub(crate) fn completion(&self) -> Option<&WorkerCompletion<T>> {
        self.completion.as_ref()
    }
}

impl<T> Drop for LifecycleWorker<T> {
    fn drop(&mut self) {
        // Module destruction is a mandatory safety barrier: provider callbacks
        // and network bindings must not outlive the library that owns them.
        self.finish();
    }
}

/// Owns the UPnP workers and polls exact retirement before module readiness.
pub(crate) struct UpnpLifecycle {
    commands: LifecycleSender<DesiredMappings>,
    statuses: UpnpStatusMailbox,
    worker: LifecycleWorker<BTreeMap<String, GatewayOwner<IgdGateway>>>,
    deadline: OperationDeadline,
    ipv6_lifecycle: crate::upnp_ipv6::Ipv6PinholeLifecycle,
    last_desired: Option<DesiredMappings>,
    ipv4_status: UpnpRuntimeStatus,
    ipv6_status: UpnpRuntimeStatus,
    status: UpnpRuntimeStatus,
}

impl UpnpLifecycle {
    pub(crate) fn new(network: Option<Network>) -> Self {
        let deadline = OperationDeadline::default();
        let (command_tx, command_rx) = lifecycle_channel();
        let statuses = UpnpStatusMailbox::default();
        let worker_statuses = statuses.clone();
        let worker_network = network.clone();
        let worker_deadline = deadline.clone();
        let worker = thread::Builder::new()
            .name("amaterasu-upnp".into())
            .spawn(move || {
                run_worker(worker_network, command_rx, worker_statuses, worker_deadline)
            });
        let status = if worker.is_ok() {
            UpnpRuntimeStatus::default()
        } else {
            UpnpRuntimeStatus {
                status: "error".into(),
                error: Some("UPnP lifecycle worker could not be started".into()),
            }
        };
        let ipv6_lifecycle = crate::upnp_ipv6::Ipv6PinholeLifecycle::new(network, deadline.clone());
        let ipv6_status = ipv6_lifecycle.status().clone();
        Self {
            commands: command_tx,
            statuses,
            worker: LifecycleWorker::new(worker, "UPnP lifecycle worker could not be started"),
            deadline,
            ipv6_lifecycle,
            last_desired: None,
            ipv4_status: status.clone(),
            ipv6_status,
            status,
        }
    }

    pub(crate) fn update(&mut self, desired: DesiredMappings) {
        if self.last_desired.as_ref() != Some(&desired) {
            if self
                .commands
                .send(LifecycleCommand::Configure(desired.clone()))
                .is_ok()
            {
                self.last_desired = Some(desired.clone());
            } else {
                self.status = UpnpRuntimeStatus {
                    status: "error".into(),
                    error: Some("UPnP lifecycle worker stopped unexpectedly".into()),
                };
            }
        }
        while let Ok(status) = self.statuses.try_recv() {
            self.ipv4_status = status;
        }
        self.ipv6_lifecycle
            .update(desired.enabled, desired.ipv6_enabled, &desired.endpoints);
        self.ipv6_status = self.ipv6_lifecycle.status().clone();
        self.status = merge_runtime_status(&self.ipv4_status, &self.ipv6_status);
    }

    pub(crate) fn status(&self) -> &UpnpRuntimeStatus {
        &self.status
    }

    pub(crate) fn prepare_shutdown(&self, deadline: Instant) {
        self.deadline.restrict(deadline);
    }

    fn request_shutdown(&self) {
        self.deadline
            .restrict(Instant::now() + DEFAULT_SHUTDOWN_TIMEOUT);
        let _ = self.commands.send(LifecycleCommand::Shutdown);
        self.ipv6_lifecycle.request_shutdown();
    }

    pub(crate) fn poll_shutdown(&mut self) -> Result<bool, String> {
        self.request_shutdown();
        // Poll both families even when one fails, so one stalled router does
        // not prevent the independent worker from retiring its resources.
        let ipv4 = self.worker.poll().and_then(|ready| {
            let retained = self.worker.completion().map_or(0, |completion| {
                completion
                    .owners
                    .values()
                    .map(|owner| owner.manager.owned_mappings().count())
                    .sum::<usize>()
            });
            if ready && retained > 0 {
                Err(format!(
                    "{retained} owned IPv4 mapping(s) remain pending cleanup"
                ))
            } else {
                Ok(ready)
            }
        });
        let ipv6 = self.ipv6_lifecycle.poll_shutdown();
        self.refresh_status();
        match (ipv4, ipv6) {
            (Ok(ipv4), Ok(ipv6)) => Ok(ipv4 && ipv6),
            (Err(error), Ok(_)) | (Ok(_), Err(error)) => Err(error),
            (Err(ipv4), Err(ipv6)) => Err(bounded_runtime_error(format!("{ipv4}; {ipv6}"))),
        }
    }

    pub(crate) fn refresh_status(&mut self) {
        while let Ok(status) = self.statuses.try_recv() {
            self.ipv4_status = status;
        }
        self.ipv6_lifecycle.refresh_status();
        self.ipv6_status = self.ipv6_lifecycle.status().clone();
        self.status = merge_runtime_status(&self.ipv4_status, &self.ipv6_status);
    }

    pub(crate) fn shutdown(&mut self) {
        self.request_shutdown();
        self.worker.finish();
        self.ipv6_lifecycle.shutdown();
        self.refresh_status();
    }
}

impl Drop for UpnpLifecycle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn merge_runtime_status(ipv4: &UpnpRuntimeStatus, ipv6: &UpnpRuntimeStatus) -> UpnpRuntimeStatus {
    let error = combine_runtime_errors(ipv4.error.iter().chain(ipv6.error.iter()).cloned());
    if ipv4.status == "active" || ipv6.status == "active" {
        return UpnpRuntimeStatus {
            status: "active".into(),
            error,
        };
    }
    if let Some(error) = error {
        return UpnpRuntimeStatus {
            status: "error".into(),
            error: Some(error),
        };
    }
    let status = if ipv4.status == "waiting_for_gateway_route"
        || ipv6.status == "waiting_for_gateway_route"
    {
        "waiting_for_gateway_route"
    } else if ipv4.status == "waiting_for_ipv4_listener"
        || ipv6.status == "waiting_for_ipv4_listener"
    {
        "waiting_for_ipv4_listener"
    } else if ipv4.status == "waiting_for_listener" || ipv6.status == "waiting_for_listener" {
        "waiting_for_listener"
    } else {
        "disabled"
    };
    UpnpRuntimeStatus {
        status: status.into(),
        error: None,
    }
}

fn run_worker(
    network: Option<Network>,
    commands: LifecycleReceiver<DesiredMappings>,
    statuses: UpnpStatusMailbox,
    deadline: OperationDeadline,
) -> WorkerCompletion<BTreeMap<String, GatewayOwner<IgdGateway>>> {
    let mut desired = DesiredMappings::default();
    let mut gateways: BTreeMap<String, GatewayOwner<IgdGateway>> = BTreeMap::new();
    let mut active_gateway: Option<String> = None;
    let mut next_discovery = Instant::now();
    let mut next_retry = Instant::now();
    let mut configuration_changed = true;
    let mut last_discovery_error = None;

    loop {
        let now = Instant::now();
        let has_candidate = desired.enabled && has_ipv4_candidate(&desired.endpoints);
        let next_wakeup = next_worker_wakeup(has_candidate, next_discovery, next_retry);
        let timeout = next_wakeup.saturating_duration_since(now);
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
            deadline.restrict(Instant::now() + DEFAULT_SHUTDOWN_TIMEOUT);
            let error = shutdown_gateways(&mut gateways, &deadline);
            if let Some(error) = &error {
                let _ = statuses.send(UpnpRuntimeStatus {
                    status: "error".into(),
                    error: Some(error.clone()),
                });
            } else {
                let _ = statuses.send(UpnpRuntimeStatus::default());
            }
            return WorkerCompletion {
                owners: gateways,
                error,
            };
        }

        let now = Instant::now();
        let refresh_due = now >= next_discovery;
        let has_candidate = desired.enabled && has_ipv4_candidate(&desired.endpoints);
        let rediscover = should_rediscover(
            has_candidate,
            configuration_changed,
            last_discovery_error.is_some(),
            now,
            next_discovery,
            next_retry,
        );
        let status;
        if desired.enabled && !desired.endpoints.is_empty() {
            if !has_ipv4_candidate(&desired.endpoints) {
                let cleanup_error = cleanup_gateways(&mut gateways, &mut active_gateway);
                next_retry = Instant::now() + CLEANUP_RETRY_INTERVAL;
                status = cleanup_status_or(
                    cleanup_error,
                    UpnpRuntimeStatus {
                        status: "waiting_for_ipv4_listener".into(),
                        error: None,
                    },
                );
            } else {
                let mut discovery_error = last_discovery_error.clone();
                if rediscover {
                    configuration_changed = false;
                    match network
                        .as_ref()
                        .ok_or_else(|| {
                            crate::GatewayError::new("UPnP network provider unavailable")
                        })
                        .and_then(|network| {
                            discover_gateway_with_deadline(network, deadline.clone())
                        }) {
                        Ok(gateway) => {
                            last_discovery_error = None;
                            discovery_error = None;
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
                        Err(error) => {
                            let error = bounded_runtime_error(error);
                            last_discovery_error = Some(error.clone());
                            discovery_error = Some(error);
                        }
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
                    let cleanup_error =
                        cleanup_inactive_gateways(&mut gateways, active_gateway.as_deref());
                    if let Some(cleanup_error) = cleanup_error {
                        error.get_or_insert(cleanup_error);
                    }
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
                    let cleanup_error = cleanup_gateways(&mut gateways, &mut active_gateway);
                    next_retry = next_gateway_retry(
                        Instant::now(),
                        next_discovery,
                        discovery_error.is_some(),
                        cleanup_error.is_some(),
                    );
                    status = UpnpRuntimeStatus {
                        status: "error".into(),
                        error: Some(
                            combine_runtime_errors(
                                discovery_error
                                    .into_iter()
                                    .chain(cleanup_error)
                                    .chain(std::iter::once("no UPnP gateway is available".into())),
                            )
                            .expect("the no-gateway message is always present"),
                        ),
                    };
                }
            }
        } else {
            let cleanup_error = cleanup_gateways(&mut gateways, &mut active_gateway);
            next_retry = Instant::now() + CLEANUP_RETRY_INTERVAL;
            let fallback = if desired.enabled {
                UpnpRuntimeStatus {
                    status: "waiting_for_listener".into(),
                    error: None,
                }
            } else {
                UpnpRuntimeStatus::default()
            };
            status = cleanup_status_or(cleanup_error, fallback);
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

fn should_rediscover(
    has_candidate: bool,
    configuration_changed: bool,
    discovery_failed: bool,
    now: Instant,
    next_discovery: Instant,
    next_retry: Instant,
) -> bool {
    has_candidate
        && (configuration_changed
            || now >= next_discovery
            || (discovery_failed && now >= next_retry))
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

fn next_gateway_retry(
    now: Instant,
    next_discovery: Instant,
    discovery_failed: bool,
    cleanup_failed: bool,
) -> Instant {
    if discovery_failed || cleanup_failed {
        now + CLEANUP_RETRY_INTERVAL
    } else {
        next_discovery
    }
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

struct GatewayOwner<G: Gateway> {
    manager: MappingManager<G>,
    internal_address: Ipv4Addr,
}

fn cleanup_gateways<G: Gateway>(
    gateways: &mut BTreeMap<String, GatewayOwner<G>>,
    active_gateway: &mut Option<String>,
) -> Option<String> {
    let mut errors = Vec::new();
    for owner in gateways.values_mut() {
        if let Err(error) = owner.manager.shutdown() {
            errors.push(bounded_runtime_error(error));
        }
    }
    gateways.retain(|_, owner| owner.manager.owned_mappings().next().is_some());
    let retained_mappings = gateways
        .values()
        .map(|owner| owner.manager.owned_mappings().count())
        .sum::<usize>();
    if retained_mappings > 0 {
        errors.push(format!(
            "{retained_mappings} owned mapping(s) remain pending cleanup"
        ));
    }
    *active_gateway = None;
    combine_runtime_errors(errors)
}

fn cleanup_inactive_gateways<G: Gateway>(
    gateways: &mut BTreeMap<String, GatewayOwner<G>>,
    active_gateway: Option<&str>,
) -> Option<String> {
    let mut errors = Vec::new();
    let inactive: Vec<_> = gateways
        .keys()
        .filter(|identity| Some(identity.as_str()) != active_gateway)
        .cloned()
        .collect();
    for identity in inactive {
        let empty = if let Some(owner) = gateways.get_mut(&identity) {
            if let Err(error) = owner.manager.shutdown() {
                errors.push(bounded_runtime_error(error));
            }
            owner.manager.owned_mappings().next().is_none()
        } else {
            false
        };
        if empty {
            gateways.remove(&identity);
        }
    }
    let retained_mappings = gateways
        .iter()
        .filter(|(identity, _)| Some(identity.as_str()) != active_gateway)
        .map(|(_, owner)| owner.manager.owned_mappings().count())
        .sum::<usize>();
    if retained_mappings > 0 {
        errors.push(format!(
            "{retained_mappings} owned mapping(s) remain pending cleanup"
        ));
    }
    combine_runtime_errors(errors)
}

fn shutdown_gateways<G: Gateway>(
    gateways: &mut BTreeMap<String, GatewayOwner<G>>,
    deadline: &OperationDeadline,
) -> Option<String> {
    deadline.restrict(Instant::now() + DEFAULT_SHUTDOWN_TIMEOUT);
    if gateways
        .values()
        .all(|owner| owner.manager.owned_mappings().next().is_none())
    {
        return None;
    }
    let mut last_errors = Vec::new();
    let mut attempts = 0;
    for _ in 0..3 {
        if deadline.is_expired() {
            last_errors.push("UPnP shutdown deadline exceeded".into());
            break;
        }
        attempts += 1;
        let mut attempt_errors = Vec::new();
        for owner in gateways.values_mut() {
            if deadline.is_expired() {
                attempt_errors.push("UPnP shutdown deadline exceeded".into());
                break;
            }
            if let Err(error) = owner
                .manager
                .shutdown_until(deadline.deadline().expect("shutdown deadline"))
            {
                attempt_errors.push(bounded_runtime_error(error));
            }
        }
        let retained_mappings = gateways
            .values()
            .map(|owner| owner.manager.owned_mappings().count())
            .sum::<usize>();
        if retained_mappings == 0 {
            return None;
        }
        last_errors = attempt_errors;
    }
    let retained_mappings = gateways
        .values()
        .map(|owner| owner.manager.owned_mappings().count())
        .sum::<usize>();
    let last_error = last_errors
        .first()
        .map(String::as_str)
        .unwrap_or("gateway cleanup did not complete");
    let attempts = if attempts == 3 {
        "three shutdown attempts".to_owned()
    } else {
        format!("{attempts} shutdown attempt(s) before the deadline expired")
    };
    Some(bounded_runtime_error(format!(
        "Could not remove {retained_mappings} owned mapping(s) after {attempts}; leases may remain active for up to 3600 seconds. Last error: {last_error}"
    )))
}

fn combine_runtime_errors(errors: impl IntoIterator<Item = String>) -> Option<String> {
    let errors: Vec<_> = errors.into_iter().collect();
    (!errors.is_empty()).then(|| bounded_runtime_error(errors.join("; ")))
}

fn cleanup_status_or(error: Option<String>, fallback: UpnpRuntimeStatus) -> UpnpRuntimeStatus {
    error.map_or(fallback, |error| UpnpRuntimeStatus {
        status: "error".into(),
        error: Some(error),
    })
}

#[cfg(test)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GatewayError, Mapping, MappingKey, Protocol, RefreshOutcome, RemovalOutcome};
    #[derive(Default)]
    struct FakeCleanupGatewayState {
        mappings: BTreeMap<MappingKey, Mapping>,
        remove_failures_remaining: usize,
        remove_calls: usize,
        tighten_deadline_on_remove: Option<OperationDeadline>,
    }

    #[derive(Clone)]
    struct FakeCleanupGateway {
        state: Arc<Mutex<FakeCleanupGatewayState>>,
    }

    impl Gateway for FakeCleanupGateway {
        fn find_mappings(&mut self, key: MappingKey) -> Result<Vec<Mapping>, GatewayError> {
            Ok(self
                .state
                .lock()
                .unwrap()
                .mappings
                .get(&key)
                .cloned()
                .into_iter()
                .collect())
        }

        fn create_if_absent(&mut self, mapping: &Mapping) -> Result<(), GatewayError> {
            let mut state = self.state.lock().unwrap();
            if state.mappings.contains_key(&mapping.key()) {
                return Err(GatewayError::new("mapping already exists"));
            }
            state.mappings.insert(mapping.key(), mapping.clone());
            Ok(())
        }

        fn remove_if_matches(&mut self, mapping: &Mapping) -> Result<RemovalOutcome, GatewayError> {
            let mut state = self.state.lock().unwrap();
            state.remove_calls += 1;
            if let Some(deadline) = &state.tighten_deadline_on_remove {
                deadline.restrict(Instant::now());
            }
            if state.remove_failures_remaining > 0 {
                state.remove_failures_remaining -= 1;
                return Err(GatewayError::new("temporary gateway cleanup failure"));
            }
            let Some(existing) = state.mappings.get(&mapping.key()) else {
                return Ok(RemovalOutcome::Missing);
            };
            if !existing.has_same_owned_attributes(mapping) {
                return Ok(RemovalOutcome::Changed);
            }
            state.mappings.remove(&mapping.key());
            Ok(RemovalOutcome::Removed)
        }

        fn remove_if_matches_until(
            &mut self,
            mapping: &Mapping,
            deadline: Instant,
        ) -> Result<RemovalOutcome, GatewayError> {
            if Instant::now() >= deadline {
                return Err(GatewayError::new("mock cleanup deadline exceeded"));
            }
            self.remove_if_matches(mapping)
        }

        fn refresh_if_matches(
            &mut self,
            mapping: &Mapping,
        ) -> Result<RefreshOutcome, GatewayError> {
            let state = self.state.lock().unwrap();
            let Some(existing) = state.mappings.get(&mapping.key()) else {
                return Ok(RefreshOutcome::Missing);
            };
            if existing.has_same_owned_attributes(mapping) {
                Ok(RefreshOutcome::Refreshed)
            } else {
                Ok(RefreshOutcome::Changed)
            }
        }
    }

    fn owned_fake_gateway(
        remove_failures_remaining: usize,
    ) -> (
        GatewayOwner<FakeCleanupGateway>,
        Arc<Mutex<FakeCleanupGatewayState>>,
    ) {
        let state = Arc::new(Mutex::new(FakeCleanupGatewayState::default()));
        let gateway = FakeCleanupGateway {
            state: state.clone(),
        };
        let mut manager = MappingManager::new(gateway);
        manager
            .reconcile_protocols(&[MappingEndpoint::new(
                IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10)),
                47_989,
                Protocol::Tcp,
                true,
            )])
            .unwrap();
        state.lock().unwrap().remove_failures_remaining = remove_failures_remaining;
        (
            GatewayOwner {
                manager,
                internal_address: Ipv4Addr::new(192, 168, 1, 10),
            },
            state,
        )
    }

    #[test]
    fn lifecycle_commands_coalesce_pending_listener_configurations() {
        let (commands, worker) = lifecycle_channel();
        for revision in 0..10_000 {
            commands
                .send(LifecycleCommand::Configure(revision))
                .unwrap();
        }
        assert!(matches!(
            worker.recv_timeout(Duration::ZERO),
            Ok(LifecycleCommand::Configure(9_999))
        ));
        assert!(matches!(
            worker.recv_timeout(Duration::ZERO),
            Err(RecvTimeoutError::Timeout)
        ));
    }

    #[test]
    fn lifecycle_shutdown_preempts_pending_listener_configurations() {
        let (commands, worker) = lifecycle_channel();
        for revision in 0..10_000 {
            commands
                .send(LifecycleCommand::Configure(revision))
                .unwrap();
        }
        commands.send(LifecycleCommand::Shutdown).unwrap();
        assert!(matches!(
            worker.recv_timeout(Duration::ZERO),
            Ok(LifecycleCommand::Shutdown)
        ));
        assert!(commands.send(LifecycleCommand::Configure(10_000)).is_err());
    }

    #[test]
    fn lifecycle_status_retains_only_the_latest_worker_snapshot() {
        let statuses = UpnpStatusMailbox::default();
        for revision in 0..10_000 {
            statuses
                .send(UpnpRuntimeStatus {
                    status: "error".into(),
                    error: Some(format!("revision {revision}")),
                })
                .unwrap();
        }
        assert_eq!(
            statuses.try_recv().unwrap().error.as_deref(),
            Some("revision 9999")
        );
        assert!(statuses.try_recv().is_err());
    }

    #[test]
    fn lifecycle_command_sender_detects_a_retired_worker() {
        let (commands, worker) = lifecycle_channel();
        drop(worker);
        assert!(commands.send(LifecycleCommand::Configure(1)).is_err());
    }

    #[test]
    fn worker_cleanup_failure_retains_its_exact_returned_owners() {
        let (release, held) = mpsc::channel();
        let worker = thread::spawn(move || {
            held.recv_timeout(Duration::from_secs(2)).unwrap();
            WorkerCompletion {
                owners: vec!["gateway:TCP:47989".to_owned()],
                error: Some("router did not confirm mapping removal".into()),
            }
        });
        let mut worker = LifecycleWorker::new(Ok(worker), "test worker");
        let pending = worker.poll();
        release.send(()).unwrap();
        assert_eq!(pending, Ok(false));
        worker.finish();
        assert_eq!(
            worker.poll(),
            Err("router did not confirm mapping removal".into())
        );
        assert_eq!(worker.completion().unwrap().owners, ["gateway:TCP:47989"]);
        assert_eq!(
            worker.poll(),
            Err("router did not confirm mapping removal".into())
        );
        assert_eq!(worker.completion().unwrap().owners, ["gateway:TCP:47989"]);
    }

    #[test]
    fn unstarted_worker_is_retired_while_panicked_worker_remains_failed() {
        let mut unstarted = LifecycleWorker::<Vec<String>>::new(
            Err(std::io::Error::other("injected thread resource exhaustion")),
            "UPnP worker failed to start",
        );
        assert_eq!(unstarted.poll(), Ok(true));
        assert!(unstarted.completion().is_none());

        let handle = thread::spawn(|| -> WorkerCompletion<Vec<String>> {
            panic!("injected worker panic after start")
        });
        let mut panicked = LifecycleWorker::new(Ok(handle), "test worker");
        panicked.finish();
        assert!(panicked.poll().unwrap_err().contains("panicked"));
    }

    #[test]
    fn shutdown_preparation_cannot_renew_the_shared_budget() {
        let mut lifecycle = UpnpLifecycle::new(None);
        let deadline = Instant::now();
        lifecycle.prepare_shutdown(deadline);
        lifecycle.prepare_shutdown(deadline + Duration::from_secs(30));
        lifecycle.request_shutdown();
        assert_eq!(lifecycle.deadline.deadline(), Some(deadline));
        lifecycle.shutdown();
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

    #[test]
    fn cleanup_retries_do_not_accelerate_gateway_rediscovery() {
        let now = Instant::now();
        let next_discovery = now + RECOMMENDED_REFRESH_INTERVAL;

        assert!(!should_rediscover(
            true,
            false,
            false,
            now + CLEANUP_RETRY_INTERVAL,
            next_discovery,
            now + CLEANUP_RETRY_INTERVAL,
        ));
        assert!(should_rediscover(
            true,
            true,
            false,
            now + CLEANUP_RETRY_INTERVAL,
            next_discovery,
            now + CLEANUP_RETRY_INTERVAL,
        ));
        assert!(should_rediscover(
            true,
            false,
            false,
            next_discovery,
            next_discovery,
            now + CLEANUP_RETRY_INTERVAL,
        ));
        assert!(!should_rediscover(
            false,
            true,
            true,
            next_discovery,
            next_discovery,
            now + CLEANUP_RETRY_INTERVAL,
        ));
    }

    #[test]
    fn failed_gateway_discovery_retries_without_waiting_for_refresh() {
        let now = Instant::now();
        let next_discovery = now + RECOMMENDED_REFRESH_INTERVAL;
        let next_retry = next_gateway_retry(now, next_discovery, true, false);
        assert_eq!(next_retry, now + CLEANUP_RETRY_INTERVAL);
        assert!(!should_rediscover(
            true,
            false,
            true,
            now,
            next_discovery,
            next_retry
        ));
        assert!(should_rediscover(
            true,
            false,
            true,
            next_retry,
            next_discovery,
            next_retry,
        ));
        assert!(!should_rediscover(
            false,
            false,
            true,
            next_retry,
            next_discovery,
            next_retry,
        ));
    }

    #[test]
    fn cleanup_failure_is_reported_and_retried_while_mapping_remains_owned() {
        let (owner, state) = owned_fake_gateway(1);
        let mut gateways = BTreeMap::from([("gateway".into(), owner)]);
        let mut active_gateway = Some("gateway".into());
        let waiting = UpnpRuntimeStatus {
            status: "waiting_for_ipv4_listener".into(),
            error: None,
        };

        let first_status = cleanup_status_or(
            cleanup_gateways(&mut gateways, &mut active_gateway),
            waiting.clone(),
        );
        assert_eq!(first_status.status, "error");
        assert!(
            first_status
                .error
                .as_deref()
                .is_some_and(|error| error.contains("mapping cleanup failure"))
        );
        assert_eq!(active_gateway, None);
        assert_eq!(state.lock().unwrap().mappings.len(), 1);
        assert_eq!(gateways["gateway"].manager.owned_mappings().count(), 1);

        let retry_status = cleanup_status_or(
            cleanup_gateways(&mut gateways, &mut active_gateway),
            waiting.clone(),
        );
        assert_eq!(retry_status, waiting);
        assert!(gateways.is_empty());
        assert!(state.lock().unwrap().mappings.is_empty());
    }

    #[test]
    fn shutdown_cleanup_failure_reports_retained_mapping_lease_expiry() {
        let (owner, state) = owned_fake_gateway(3);
        let mut gateways = BTreeMap::from([("gateway".into(), owner)]);

        let deadline = OperationDeadline::default();
        deadline.restrict(Instant::now() + DEFAULT_SHUTDOWN_TIMEOUT);
        let error = shutdown_gateways(&mut gateways, &deadline).unwrap();

        assert!(error.contains("three shutdown attempts"));
        assert!(error.contains("3600 seconds"));
        assert!(error.contains("mapping cleanup failure"));
        assert_eq!(state.lock().unwrap().mappings.len(), 1);
        assert_eq!(gateways["gateway"].manager.owned_mappings().count(), 1);
    }

    #[test]
    fn shortened_shutdown_budget_stops_later_gateways_and_keeps_exact_mappings() {
        let deadline = OperationDeadline::default();
        deadline.restrict(Instant::now() + DEFAULT_SHUTDOWN_TIMEOUT);
        let (first, first_state) = owned_fake_gateway(usize::MAX);
        let (second, second_state) = owned_fake_gateway(usize::MAX);
        first_state.lock().unwrap().tighten_deadline_on_remove = Some(deadline.clone());
        let mut gateways = BTreeMap::from([("a".into(), first), ("b".into(), second)]);

        assert!(shutdown_gateways(&mut gateways, &deadline).is_some());
        assert_eq!(first_state.lock().unwrap().remove_calls, 1);
        assert_eq!(second_state.lock().unwrap().remove_calls, 0);
        assert_eq!(gateways["a"].manager.owned_mappings().count(), 1);
        assert_eq!(gateways["b"].manager.owned_mappings().count(), 1);
    }

    #[test]
    fn loopback_listener_does_not_spin_on_an_overdue_discovery_deadline() {
        let now = Instant::now();
        let next_retry = now + CLEANUP_RETRY_INTERVAL;
        let overdue_discovery = now - Duration::from_secs(1);
        let endpoints = [MappingEndpoint::new(
            "127.0.0.1".parse().unwrap(),
            47_989,
            Protocol::Tcp,
            true,
        )];

        assert_eq!(
            next_worker_wakeup(
                has_ipv4_candidate(&endpoints),
                overdue_discovery,
                next_retry,
            ),
            next_retry
        );
        assert_eq!(
            next_worker_wakeup(true, overdue_discovery, next_retry),
            overdue_discovery
        );
    }
}
