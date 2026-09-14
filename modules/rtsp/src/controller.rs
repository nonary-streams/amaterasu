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
    net::SocketAddr,
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
        let (transport, local) = if settings.enabled {
            let transport = Transport::bind(settings.bind, settings.allow_remote)?;
            let local = transport.local_addr()?;
            (Some(transport), Some(local))
        } else {
            (None, None)
        };
        Ok(Self {
            settings,
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
