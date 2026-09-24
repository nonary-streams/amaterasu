mod actions;
mod network;
#[cfg(test)]
mod ownership_tests;
mod record;
#[cfg(test)]
mod tests;
mod workflow;

use crate::{
    model::{Open, Operation, Settings, valid_name},
    rpc::{self, Call, Outcome},
    transport::{Event, Transport},
};
use amaterasu_gamestream::rtsp::{
    Request,
    negotiation::{
        Action, Capabilities, Negotiation, Step as NegotiationStep, TrustedLaunch, UdpDiscovery,
    },
    serialize_response,
};
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};
use record::{Job, Phase, Record, State};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    net::{IpAddr, SocketAddr, UdpSocket},
};

const PREPARE_BUDGET_MS: u64 = 5_000;
const CLEANUP_BUDGET_MS: u64 = 15_000;
const STATUS_INTERVAL_MS: u64 = 250;
const MAX_RECORDS: usize = 128;
const MAX_LIVE_TICKETS: usize = 2;
const MAX_PENDING_NETWORK: usize = 16;

struct PendingNetwork {
    connection: u64,
    ticket: u64,
    request: Request,
}

pub struct Controller {
    settings: Settings,
    network_base_port: Option<u16>,
    listener_base_port: Option<u16>,
    active_network_base_port: Option<u16>,
    network_error: Option<&'static str>,
    network_retry_at: u64,
    transport: Option<Transport>,
    local: Option<SocketAddr>,
    records: BTreeMap<u64, Record>,
    pending_network: VecDeque<PendingNetwork>,
    job: Option<Job>,
    sequence: u64,
    status_after: u64,
}

impl Controller {
    pub fn new(settings: Settings) -> Result<Self, &'static str> {
        settings.validate()?;
        let network_base_port = settings.default_base_port();
        let (transport, local) = if settings.enabled {
            let transport = Transport::bind(settings.resolved_bind()?, settings.allow_remote)?;
            let local = transport.local_addr()?;
            (Some(transport), Some(local))
        } else {
            (None, None)
        };
        Ok(Self {
            settings,
            network_base_port,
            listener_base_port: network_base_port,
            active_network_base_port: None,
            network_error: None,
            network_retry_at: 0,
            transport,
            local,
            records: BTreeMap::new(),
            pending_network: VecDeque::new(),
            job: None,
            sequence: 0,
            status_after: 0,
        })
    }

    pub fn tick(&mut self, runtime: &RuntimeClient, now: u64) -> ModuleResult<()> {
        self.poll_transport(now);
        self.advance_job(runtime, now);
        if self.job.is_none() {
            self.process_pending_network(now);
        }
        if self.job.is_none() {
            self.schedule_background(now);
        }
        if self.job.is_none() {
            self.poll_caller(runtime, now)?;
        }
        self.reconcile_network_port(now);
        Ok(())
    }

    /// Module unload cannot drive the stream-session provider to terminal state.
    /// It only makes bounded settlement attempts; provider unload is the backstop.
    pub fn shutdown(&mut self, runtime: &RuntimeClient) {
        if let Some(job) = self.job.as_mut() {
            job.call.shutdown(runtime);
        }
        for record in self.records.values_mut() {
            if let Some(call) = record.uncertain.as_mut() {
                call.shutdown(runtime);
            }
        }
    }

    fn reply_status(&mut self, connection: u64, cseq: u32, status: u16) -> bool {
        serialize_response(status, cseq, &[], &[])
            .ok()
            .is_some_and(|reply| self.reply_bytes(connection, &reply))
    }

    fn reply_bytes(&mut self, connection: u64, reply: &[u8]) -> bool {
        self.transport
            .as_mut()
            .is_some_and(|transport| transport.reply(connection, reply).is_ok())
    }

    fn revoke_ticket(&mut self, ticket: u64) {
        if let Some(transport) = self.transport.as_mut() {
            let _ = transport.remove_ticket(ticket);
        }
        self.pending_network
            .retain(|pending| pending.ticket != ticket);
    }

    pub(super) fn configure_network_port(
        &mut self,
        port: u16,
        now: u64,
    ) -> Result<Value, &'static str> {
        crate::model::validate_base_port(port)?;
        self.network_base_port = Some(port);
        self.network_error = None;
        let applied = self.try_apply_network_port();
        if let Err(error) = applied {
            self.network_error = Some(error);
            self.network_retry_at = now.saturating_add(1_000);
        } else if applied == Ok(true) {
            self.active_network_base_port = Some(port);
            self.network_retry_at = 0;
        }
        Ok(self.network_status(applied.is_ok_and(|applied| applied)))
    }

    pub(super) fn media_binds_for_open(
        &self,
        local_address: IpAddr,
    ) -> Option<crate::model::MediaBinds> {
        self.active_network_base_port
            .or(self.listener_base_port)
            .and_then(|port| self.settings.media_binds_on(port, local_address).ok())
    }

    pub(super) fn advertised_local_for_peer(
        &self,
        peer: IpAddr,
    ) -> Result<SocketAddr, &'static str> {
        let listener = self.local.ok_or("listener_unavailable")?;
        if listener.ip().is_ipv4() != peer.is_ipv4() {
            return Err("peer_family_mismatch");
        }
        if !listener.ip().is_unspecified() {
            return Ok(listener);
        }

        // UDP connect performs route selection without sending a datagram. The
        // resulting local endpoint is advertised and reused for this stream's
        // per-session UDP listeners instead of returning an unusable wildcard.
        let route = UdpSocket::bind(SocketAddr::new(listener.ip(), 0))
            .map_err(|_| "route_selection_failed")?;
        route
            .connect(SocketAddr::new(peer, 9))
            .map_err(|_| "route_selection_failed")?;
        let source = route.local_addr().map_err(|_| "route_selection_failed")?;
        if source.ip().is_unspecified() {
            return Err("route_selection_failed");
        }
        Ok(SocketAddr::new(source.ip(), listener.port()))
    }

    fn reconcile_network_port(&mut self, now: u64) {
        if self.network_base_port.is_none() || now < self.network_retry_at {
            return;
        }
        match self.try_apply_network_port() {
            Ok(true) => {
                self.active_network_base_port = self.network_base_port;
                self.network_error = None;
                self.network_retry_at = 0;
            }
            Ok(false) => {}
            Err(error) => {
                self.network_error = Some(error);
                self.network_retry_at = now.saturating_add(1_000);
            }
        }
    }

    fn try_apply_network_port(&mut self) -> Result<bool, &'static str> {
        let Some(base_port) = self.network_base_port else {
            return Ok(false);
        };
        let bind = self.settings.rtsp_bind_for_base(base_port)?;
        if self.active_network_base_port == Some(base_port) && self.local == Some(bind) {
            return Ok(true);
        }
        if !self.settings.enabled
            || self.records.values().any(|record| !record.state.terminal())
            || self
                .transport
                .as_ref()
                .is_some_and(|transport| transport.connection_count() != 0)
        {
            return Ok(false);
        }

        self.settings.probe_media_binds(base_port)?;
        if self.local == Some(bind) {
            self.listener_base_port = Some(base_port);
            self.active_network_base_port = Some(base_port);
            self.network_error = None;
            return Ok(true);
        }
        let replacement = Transport::bind(bind, self.settings.allow_remote)?;
        let local = replacement.local_addr()?;
        self.transport = Some(replacement);
        self.local = Some(local);
        self.settings.bind = local;
        self.listener_base_port = Some(base_port);
        self.active_network_base_port = Some(base_port);
        self.network_error = None;
        Ok(true)
    }

    fn network_status(&self, applied: bool) -> Value {
        serde_json::json!({
            "ok": true,
            "desired_port": self.network_base_port,
            "rtsp_port": self.local.map(|address| address.port()),
            "active_base_port": self.active_network_base_port,
            "active": applied,
            "pending": !applied,
            "udp_binds_preflighted": applied,
            "error": self.network_error,
        })
    }
}

fn response_session(payload: &Value) -> Option<u64> {
    payload["session"].as_u64().filter(|session| *session != 0)
}

fn local_port(payload: &Value, field: &str) -> Result<u16, &'static str> {
    let port = payload[field]
        .as_str()
        .ok_or("invalid_prepare_response")?
        .parse::<SocketAddr>()
        .map_err(|_| "invalid_prepare_response")?
        .port();
    (port != 0)
        .then_some(port)
        .ok_or("invalid_prepare_response")
}

fn clean_terminal(record: &Record, payload: &Value) -> bool {
    payload["ok"] == true
        && response_session(payload) == record.session
        && matches!(
            stream_state(payload),
            Some("stopped" | "failed" | "cancelled")
        )
        && payload["cleanup_error"].is_null()
}

fn stream_state(payload: &Value) -> Option<&'static str> {
    match payload["state"].as_str()? {
        "preparing" => Some("preparing"),
        "prepared" => Some("prepared"),
        "armed" => Some("armed"),
        "starting" => Some("starting"),
        "active" => Some("active"),
        "stopping" => Some("stopping"),
        "stopped" => Some("stopped"),
        "failed" => Some("failed"),
        "cancelled" => Some("cancelled"),
        "cleanup_failed" => Some("cleanup_failed"),
        _ => None,
    }
}

fn missing(payload: &Value) -> bool {
    matches!(
        payload["code"].as_str(),
        Some("session_not_found" | "not_found")
    )
}

fn finish_record(record: &mut Record) {
    record.state = if record.failure.is_some() {
        State::Failed
    } else {
        State::Closed
    };
    record.cleanup_error = None;
}

fn complete_caller(record: &Record, caller: Option<u64>, runtime: &RuntimeClient) {
    if let Some(request) = caller.filter(|request| rpc::active(runtime, *request)) {
        rpc::complete(runtime, request, record.response());
    }
}

fn error(code: &str) -> Value {
    json!({"ok":false,"code":code})
}
