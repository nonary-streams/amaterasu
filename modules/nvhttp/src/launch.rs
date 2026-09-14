//! Caller-owned Moonlight launch orchestration.
//!
//! The wire layer authenticates the TLS peer and parses Moonlight fields. This
//! controller owns the resulting application and RTSP operations until the
//! successful HTTP reply has been re-authorized and completely written.
mod call;
mod record;
mod workflow;

#[cfg(test)]
mod tests;

use crate::wire::{Reply, xml};
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use record::{Record, Stage, State};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    net::{IpAddr, Ipv4Addr},
    time::{Duration, Instant},
};
use workflow::Job;

pub(crate) type Transport = ModuleResult<Response>;
pub(crate) type Send<'a> = dyn FnMut(Command) -> Transport + 'a;

const MAX_RECORDS: usize = 32;
const INITIAL_DEADLINE: Duration = Duration::from_secs(40);
const CLEANUP_DEADLINE: Duration = Duration::from_secs(20);
const MONITOR_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone, PartialEq, Eq)]
pub struct LaunchRequest {
    pub app_id: u32,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub remote_input_key: [u8; 16],
    pub key_id: u32,
}

#[derive(Clone, PartialEq, Eq)]
pub struct PeerIdentity {
    remote: IpAddr,
    certificate: Vec<u8>,
}

impl PeerIdentity {
    pub fn new(remote: IpAddr, certificate: Vec<u8>) -> Result<Self, &'static str> {
        if invalid_peer(remote)
            || certificate.is_empty()
            || certificate.len() > amaterasu_identity::MAX_CERTIFICATE_BYTES
        {
            return Err("invalid_peer_identity");
        }
        Ok(Self {
            remote,
            certificate,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StreamingConfig {
    pub capture: CaptureConfig,
    pub profile: StreamProfile,
}

impl StreamingConfig {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.capture.validate()?;
        self.profile.validate()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureConfig {
    pub capture_provider: CaptureProvider,
    pub output: String,
    pub encoder: u32,
    pub audio_endpoint: String,
}

impl CaptureConfig {
    fn validate(&self) -> Result<(), &'static str> {
        if !valid_label(&self.output, 512) || !valid_label(&self.audio_endpoint, 4096) {
            return Err("invalid_streaming_capture");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureProvider {
    Dxgi,
    Wgc,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StreamProfile {
    pub max_width: u32,
    pub max_height: u32,
    pub max_fps: u32,
    pub bitrate_kbps: u32,
    pub max_reference_frames: u32,
    pub packet_size: usize,
    pub fec_percentage: u8,
    pub min_parity_shards: u8,
    pub max_fec_blocks: u8,
    pub packet_ms: u32,
    pub audio_bitrate: u32,
    pub encrypt_video: bool,
    pub encrypt_audio: bool,
    pub permit_legacy_audio_iv_wrap: bool,
}

impl StreamProfile {
    fn validate(&self) -> Result<(), &'static str> {
        if !(16..=3840).contains(&self.max_width)
            || !(16..=2160).contains(&self.max_height)
            || !self.max_width.is_multiple_of(2)
            || !self.max_height.is_multiple_of(2)
            || !(1..=120).contains(&self.max_fps)
            || !(100..=100_000).contains(&self.bitrate_kbps)
            || self.max_reference_frames > 1
            || !matches!(self.packet_ms, 5 | 10)
            || !(6_000..=256_000).contains(&self.audio_bitrate)
        {
            return Err("invalid_streaming_profile");
        }
        amaterasu_gamestream::video::VideoPacketizer::new(
            amaterasu_gamestream::video::VideoConfig {
                packet_size: self.packet_size,
                fec_percentage: self.fec_percentage,
                min_parity_shards: self.min_parity_shards,
                max_fec_blocks: self.max_fec_blocks,
                encrypted: false,
            },
        )
        .map_err(|_| "invalid_streaming_profile")?;
        Ok(())
    }

    fn accepts(&self, request: &LaunchRequest) -> bool {
        (16..=self.max_width).contains(&request.width)
            && (16..=self.max_height).contains(&request.height)
            && request.width.is_multiple_of(2)
            && request.height.is_multiple_of(2)
            && (1..=self.max_fps).contains(&request.fps)
    }
}

pub struct Controller {
    config: Option<StreamingConfig>,
    records: BTreeMap<u64, Record>,
    job: Option<Job>,
    nonce: [u8; 16],
    next_id: u64,
    sequence: u64,
}

impl Controller {
    pub fn new(config: Option<StreamingConfig>) -> Result<Self, &'static str> {
        if let Some(config) = &config {
            config.validate()?;
        }
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|_| "random_unavailable")?;
        Ok(Self {
            config,
            records: BTreeMap::new(),
            job: None,
            nonce,
            next_id: 0,
            sequence: 0,
        })
    }

    pub fn supported(&self) -> bool {
        self.config.is_some()
    }

    pub fn busy(&self) -> bool {
        self.records.values().any(|record| !record.state.terminal())
    }

    pub fn launch(
        &mut self,
        connection: u64,
        request: LaunchRequest,
        peer: PeerIdentity,
        now: Instant,
    ) -> Option<Reply> {
        let Some(config) = &self.config else {
            return Some(private_error(503, "streaming is disabled"));
        };
        if request.app_id == 0
            || request.app_id > i32::MAX as u32
            || !config.profile.accepts(&request)
        {
            return Some(private_error(400, "unsupported streaming request"));
        }

        if let Some(record) = self
            .records
            .values_mut()
            .find(|record| record.request.remote_input_key == request.remote_input_key)
        {
            if record.peer == peer && record.request == request {
                if matches!(record.state, State::Ready) {
                    record.launch_connection = Some(connection);
                    return Some(success_reply(record));
                }
                if !record.state.terminal() {
                    return Some(private_error(409, "launch is already in progress"));
                }
                return Some(private_error(409, "launch key was already used"));
            }
            return Some(private_error(
                409,
                "launch key conflicts with an existing request",
            ));
        }
        if self.records.len() >= MAX_RECORDS
            || self.records.values().any(|record| !record.state.terminal())
        {
            return Some(private_error(503, "another stream is active"));
        }

        let id = match self.allocate_id() {
            Ok(id) => id,
            Err(code) => return Some(private_error(503, code)),
        };
        let connect_data = match random_nonzero_u32() {
            Ok(value) => value,
            Err(code) => return Some(private_error(503, code)),
        };
        let record = Record::new(
            id,
            peer,
            request,
            operation_id(&self.nonce, id, "application"),
            operation_id(&self.nonce, id, "rtsp"),
            connect_data,
            connection,
            now + INITIAL_DEADLINE,
        );
        self.records.insert(id, record);
        None
    }

    pub fn cancel(&mut self, connection: u64, peer: &PeerIdentity, now: Instant) -> Option<Reply> {
        let Some(record) = self
            .records
            .values_mut()
            .find(|record| record.peer.certificate == peer.certificate && !record.state.terminal())
        else {
            if self.records.values().any(|record| !record.state.terminal()) {
                return Some(private_error(403, "stream is owned by another client"));
            }
            return Some(cancel_reply());
        };
        if record.state == State::Stopping || record.cancel_connection.is_some() {
            return Some(private_error(409, "stream cleanup is already in progress"));
        }
        record.cancel_connection = Some(connection);
        let retry = if record.state == State::CleanupFailed {
            record.retry_cleanup(now)
        } else {
            record.request_cleanup(None, now + CLEANUP_DEADLINE);
            None
        };
        if let Some((phase, call)) = retry {
            self.job = Some(Job {
                record: record.id,
                phase,
                call,
                deadline: record.deadline,
            });
        }
        if let Some(job) = self.job.as_mut().filter(|job| job.record == record.id) {
            job.call.cancel();
        }
        None
    }

    pub fn current_game(&self, peer: &PeerIdentity) -> u32 {
        self.records
            .values()
            .find(|record| {
                record.peer.certificate == peer.certificate && record.state == State::Ready
            })
            .map_or(0, |record| record.request.app_id)
    }

    pub fn poll(&mut self, now: Instant, send: &mut Send<'_>) -> Vec<(u64, Reply)> {
        let mut replies = Vec::new();
        if let Some(mut job) = self.job.take() {
            self.advance(&mut job, now, send, &mut replies);
        }
        if self.job.is_none() {
            self.schedule(now);
        }
        replies
    }

    pub fn reply_delivered(&mut self, connection: u64, now: Instant) {
        if let Some(record) = self
            .records
            .values_mut()
            .find(|record| record.launch_connection == Some(connection))
        {
            record.launch_connection = None;
            if record.state == State::AwaitingDelivery {
                record.adopted = true;
                record.state = State::Ready;
                record.stage = Stage::Live;
                record.next_monitor = now;
            }
        }
        if let Some(record) = self
            .records
            .values_mut()
            .find(|record| record.cancel_connection == Some(connection))
        {
            record.cancel_connection = None;
        }
    }

    pub fn connection_closed(&mut self, connection: u64, now: Instant) {
        let mut affected = None;
        for record in self.records.values_mut() {
            if record.launch_connection == Some(connection) {
                record.launch_connection = None;
                if !record.adopted {
                    record.request_cleanup(
                        Some("launch_response_unavailable"),
                        now + CLEANUP_DEADLINE,
                    );
                    affected = Some(record.id);
                }
            }
            if record.cancel_connection == Some(connection) {
                record.cancel_connection = None;
            }
        }
        if let Some(job) = self.job.as_mut().filter(|job| Some(job.record) == affected) {
            job.call.cancel();
        }
    }

    pub fn revoke_untrusted(
        &mut self,
        now: Instant,
        mut trusted: impl FnMut(&[u8]) -> bool,
    ) -> Vec<u64> {
        let mut close = Vec::new();
        let mut affected = Vec::new();
        for record in self.records.values_mut() {
            if !record.state.terminal() && !record.revoked && !trusted(&record.peer.certificate) {
                record.revoked = true;
                close.extend(record.launch_connection.take());
                close.extend(record.cancel_connection.take());
                if !matches!(record.state, State::Stopping | State::CleanupFailed) {
                    record.request_cleanup(Some("client_revoked"), now + CLEANUP_DEADLINE);
                    affected.push(record.id);
                }
            }
        }
        if let Some(job) = self
            .job
            .as_mut()
            .filter(|job| affected.contains(&job.record))
        {
            job.call.cancel();
        }
        close
    }

    pub fn shutdown(&mut self, send: &mut Send<'_>) {
        if let Some(job) = self.job.as_mut() {
            job.call.shutdown(send);
        }
        for record in self.records.values_mut() {
            for (_, call) in &mut record.uncertain {
                call.shutdown(send);
            }
        }
        // Synchronous module unload cannot prove downstream terminal state.
        // Provider shutdown is the backstop for already adopted resources.
    }

    fn allocate_id(&mut self) -> Result<u64, &'static str> {
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or("launch capacity exhausted")?;
        Ok(self.next_id)
    }

    fn next_operation(&mut self, record: &Record, phase: &str) -> String {
        self.sequence = self.sequence.saturating_add(1);
        format!("{}.{}.{}", record.operation_prefix(), phase, self.sequence)
    }
}

fn private_error(status: u16, message: &str) -> Reply {
    let mut reply = xml::error(status, message);
    reply.requires_paired_tls = true;
    reply
}

fn success_reply(record: &Record) -> Reply {
    let mut reply = xml::response(
        200,
        "OK",
        &[
            (
                "sessionUrl0",
                record.rtsp_url.as_deref().unwrap_or_default(),
            ),
            ("gamesession", "1"),
        ],
    );
    reply.requires_paired_tls = true;
    reply
}

fn cancel_reply() -> Reply {
    let mut reply = xml::response(200, "OK", &[("gamesession", "0")]);
    reply.requires_paired_tls = true;
    reply
}

fn valid_label(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
}

fn invalid_peer(address: IpAddr) -> bool {
    address.is_unspecified()
        || address.is_multicast()
        || address == IpAddr::V4(Ipv4Addr::BROADCAST)
        || matches!(address, IpAddr::V6(value) if value.to_ipv4_mapped().is_some())
}

fn random_nonzero_u32() -> Result<u32, &'static str> {
    for _ in 0..8 {
        let mut bytes = [0; 4];
        getrandom::fill(&mut bytes).map_err(|_| "random_unavailable")?;
        let value = u32::from_ne_bytes(bytes);
        if value != 0 {
            return Ok(value);
        }
    }
    Err("random_unavailable")
}

fn operation_id(nonce: &[u8; 16], id: u64, purpose: &str) -> String {
    format!("nvhttp-{}-{id:016x}-{purpose}", hex(nonce))
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        write!(&mut output, "{byte:02x}").expect("hex string write");
    }
    output
}

fn key_hex(key: &[u8; 16]) -> String {
    hex(key)
}

fn app_id(value: u32) -> String {
    format!("app-{value:016x}")
}
