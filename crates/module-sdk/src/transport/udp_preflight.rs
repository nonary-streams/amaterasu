//! Broker contract for temporary UDP bind checks by the selected transport owners.
use crate::{Command, Response, RuntimeClient, network::Network};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, VecDeque},
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

const MAX_SLOTS_PER_FAMILY: usize = 32;
const MAX_ENDPOINTS: usize = 2 * MAX_SLOTS_PER_FAMILY;
pub const TRANSPORT_ACTION: &str = "probe_udp_binds";

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoints {
    pub video: SocketAddr,
    pub audio: SocketAddr,
    pub control: SocketAddr,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProbeRequest {
    /// The stream owner forwards media and control addresses to the same
    /// trusted service aliases it uses to open those transports.
    ProbeUdpEndpoints { endpoints: Vec<Endpoints> },
    /// The receiving transport checks its whole set using its normal UDP
    /// bind policy. Every temporary handle is retired before it replies.
    ProbeUdpBinds { addresses: Vec<SocketAddr> },
}

impl ProbeRequest {
    pub fn for_stream_owner(endpoints: Vec<Endpoints>) -> Value {
        json!(Self::ProbeUdpEndpoints { endpoints })
    }

    pub fn for_transport(addresses: Vec<SocketAddr>) -> Value {
        json!(Self::ProbeUdpBinds { addresses })
    }
}

pub fn validate_endpoints(endpoints: &[Endpoints], allow_remote: bool) -> Result<(), &'static str> {
    if endpoints.is_empty()
        || endpoints.len() > MAX_ENDPOINTS
        || endpoints
            .iter()
            .any(|set| set.video.ip() != set.audio.ip() || set.video.ip() != set.control.ip())
    {
        return Err("invalid_udp_binds");
    }
    validate_addresses(
        &endpoints
            .iter()
            .flat_map(|set| [set.video, set.audio, set.control])
            .collect::<Vec<_>>(),
        3,
        allow_remote,
    )
}

fn validate_addresses(
    addresses: &[SocketAddr],
    lanes: usize,
    allow_remote: bool,
) -> Result<(), &'static str> {
    if addresses.is_empty()
        || addresses.len() > MAX_ENDPOINTS * lanes.clamp(1, 3)
        || addresses.iter().any(|address| {
            let ip = address.ip();
            address.port() == 0
                || ip.is_multicast()
                || ip == std::net::Ipv4Addr::BROADCAST
                || matches!(ip, std::net::IpAddr::V6(ip) if ip.to_ipv4_mapped().is_some())
                || (!allow_remote && !ip.is_loopback())
        })
        || addresses.iter().collect::<BTreeSet<_>>().len() != addresses.len()
    {
        return Err("invalid_udp_binds");
    }
    Ok(())
}

/// Bind and verify the exact addresses, retaining the complete set until it has
/// been checked. The caller gives this operation a worker when running on the host tick.
pub fn probe_addresses(
    network: &Network,
    addresses: &[SocketAddr],
    cancelled: impl Fn() -> bool,
) -> Result<(), &'static str> {
    let mut sockets = Vec::with_capacity(addresses.len());
    for address in addresses {
        if cancelled() {
            return Err("udp_probe_cancelled");
        }
        let socket = network.bind_udp(*address).map_err(|_| "udp_bind_failed")?;
        if socket.local_addr().ok() != Some(*address) {
            return Err("udp_configuration_failed");
        }
        sockets.push(socket);
    }
    Ok(())
}

pub fn result_payload(result: Result<(), &'static str>) -> Value {
    match result {
        Ok(()) => json!({"ok":true,"udp_binds_preflighted":true}),
        Err(code) => json!({"ok":false,"code":code}),
    }
}

pub fn checked_result(payload: &Value) -> Result<(), &'static str> {
    match (
        payload["ok"].as_bool(),
        payload["udp_binds_preflighted"].as_bool(),
    ) {
        (Some(true), Some(true)) => Ok(()),
        (Some(false), _) => Err(match payload["code"].as_str() {
            Some("udp_bind_failed") => "udp_bind_failed",
            Some("udp_configuration_failed") => "udp_configuration_failed",
            Some("invalid_udp_binds") => "invalid_udp_binds",
            Some("udp_probe_timeout") => "udp_probe_timeout",
            _ => "udp_probe_unavailable",
        }),
        _ => Err("udp_probe_invalid_response"),
    }
}

/// One bounded worker per provider. No sockets survive the operation and
/// quiescence waits for any worker borrowing the selected Network binding.
#[derive(Default)]
pub struct ProbeWorker {
    job: Option<ProbeJob>,
    waiting_opens: VecDeque<Response>,
}

struct ProbeJob {
    request: u64,
    cancelled: Arc<AtomicBool>,
    addresses: Vec<SocketAddr>,
    worker: JoinHandle<Result<(), &'static str>>,
}

impl ProbeWorker {
    pub fn overlaps_open(&self, addresses: impl IntoIterator<Item = SocketAddr>) -> bool {
        self.job.as_ref().is_some_and(|job| {
            addresses.into_iter().any(|requested| {
                job.addresses.iter().any(|checked| {
                    requested.port() == checked.port()
                        && requested.is_ipv4() == checked.is_ipv4()
                        && (requested.ip() == checked.ip()
                            || requested.ip().is_unspecified()
                            || checked.ip().is_unspecified())
                })
            })
        })
    }

    pub fn has_probe(&self) -> bool {
        self.job.is_some()
    }

    /// A valid overlapping Open takes priority over a configuration probe.
    /// Keep its broker request intact until only the temporary handles retire.
    pub fn defer_open(&mut self, runtime: &RuntimeClient, open: Response) {
        let Response::Request { id, .. } = &open else {
            return;
        };
        if !matches!(
            runtime.send(Command::CheckRequest { request: *id }),
            Ok(Response::Ok)
        ) {
            return;
        }
        if self.waiting_opens.len() >= MAX_SLOTS_PER_FAMILY {
            let _ = runtime.send(Command::Complete {
                request: *id,
                payload: result_payload(Err("udp_probe_busy")),
            });
            return;
        }
        if let Some(job) = &self.job {
            job.cancelled.store(true, Ordering::Release);
        }
        self.waiting_opens.push_back(open);
    }

    /// Run other operations and disjoint Opens while a probe is outstanding.
    pub fn poll_request(&mut self, runtime: &RuntimeClient) -> crate::ModuleResult<Response> {
        if self.job.is_none()
            && let Some(open) = self.waiting_opens.pop_front()
        {
            return Ok(open);
        }
        runtime.send(Command::Poll {})
    }

    pub fn dispatch(
        &mut self,
        runtime: &RuntimeClient,
        request: u64,
        payload: Value,
        lanes: usize,
        bind_policy: impl FnOnce() -> Result<(Network, bool), &'static str>,
    ) {
        let result = (|| {
            let ProbeRequest::ProbeUdpBinds { addresses } =
                serde_json::from_value(payload).map_err(|_| "invalid_udp_binds")?
            else {
                return Err("invalid_udp_binds");
            };
            let (network, allow_remote) = bind_policy()?;
            self.start(runtime, network, request, addresses, lanes, allow_remote)
        })();
        if result.is_err() {
            let _ = runtime.send(Command::Complete {
                request,
                payload: result_payload(result),
            });
        }
    }

    fn start(
        &mut self,
        runtime: &RuntimeClient,
        network: Network,
        request: u64,
        addresses: Vec<SocketAddr>,
        lanes: usize,
        allow_remote: bool,
    ) -> Result<(), &'static str> {
        validate_addresses(&addresses, lanes, allow_remote)?;
        if self.job.is_some() {
            return Err("udp_probe_unavailable");
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancellation = cancelled.clone();
        let notifier = runtime.work_notifier();
        let probed_addresses = addresses.clone();
        let worker = thread::Builder::new()
            .name("amaterasu-udp-bind-probe".into())
            .spawn(move || {
                let result = probe_addresses(&network, &addresses, || {
                    cancellation.load(Ordering::Acquire)
                });
                notifier.notify();
                result
            })
            .map_err(|_| "udp_probe_unavailable")?;
        self.job = Some(ProbeJob {
            request,
            cancelled,
            addresses: probed_addresses,
            worker,
        });
        Ok(())
    }

    pub fn tick(&mut self, runtime: &RuntimeClient) {
        let Some(job) = &self.job else { return };
        if !matches!(
            runtime.send(Command::CheckRequest {
                request: job.request
            }),
            Ok(Response::Ok)
        ) {
            job.cancelled.store(true, Ordering::Release);
        }
        if job.worker.is_finished() {
            let job = self.job.take().expect("finished UDP probe");
            let result = job.worker.join().unwrap_or(Err("udp_probe_unavailable"));
            let result = if job.cancelled.load(Ordering::Acquire) {
                Err("udp_probe_cancelled")
            } else {
                result
            };
            let _ = runtime.send(Command::Complete {
                request: job.request,
                payload: result_payload(result),
            });
        }
    }

    pub fn quiesce(&mut self, runtime: &RuntimeClient) -> bool {
        for open in self.waiting_opens.drain(..) {
            if let Response::Request { id, .. } = open {
                let _ = runtime.send(Command::Complete {
                    request: id,
                    payload: result_payload(Err("shutting_down")),
                });
            }
        }
        if let Some(job) = &self.job {
            job.cancelled.store(true, Ordering::Release);
        }
        self.tick(runtime);
        self.job.is_none()
    }
}

impl Drop for ProbeWorker {
    fn drop(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancelled.store(true, Ordering::Release);
            let _ = job.worker.join();
        }
    }
}
