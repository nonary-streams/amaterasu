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
use sha2::{Digest, Sha256};
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
    client_id: Option<String>,
    certificate_id: Option<String>,
    client_fec_percentage: Option<u8>,
    client_stream_audio: Option<bool>,
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
            client_id: None,
            certificate_id: None,
            client_fec_percentage: None,
            client_stream_audio: None,
        })
    }

    pub(crate) fn with_paired_identity(
        mut self,
        client_id: String,
        certificate_id: String,
        fec_percentage: Option<u8>,
        stream_audio: Option<bool>,
    ) -> Result<Self, &'static str> {
        if !valid_client_id(&client_id)
            || !valid_fingerprint(&certificate_id)
            || !fingerprint(&self.certificate).eq_ignore_ascii_case(&certificate_id)
            || fec_percentage == Some(0)
        {
            return Err("invalid_paired_identity");
        }
        self.client_id = Some(client_id);
        self.certificate_id = Some(certificate_id.to_ascii_lowercase());
        self.client_fec_percentage = fec_percentage;
        self.client_stream_audio = stream_audio;
        Ok(self)
    }

    fn paired(&self) -> bool {
        self.client_id.is_some() && self.certificate_id.is_some()
    }

    #[cfg(test)]
    pub(crate) fn paired_client_id(&self) -> Option<&str> {
        self.client_id.as_deref()
    }
}

fn valid_client_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-:/".contains(&byte))
}

fn valid_fingerprint(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn fingerprint(certificate: &[u8]) -> String {
    Sha256::digest(certificate)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
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

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    #[default]
    Auto,
    Ddx,
    Wgc,
    Wgcc,
}

impl CaptureMode {
    pub fn resolve(self, bootstrap_provider: CaptureProvider) -> (CaptureProvider, bool) {
        match self {
            Self::Auto => (bootstrap_provider, false),
            Self::Ddx => (CaptureProvider::Dxgi, false),
            Self::Wgc => (CaptureProvider::Wgc, false),
            Self::Wgcc => (CaptureProvider::Wgc, true),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Ddx => "ddx",
            Self::Wgc => "wgc",
            Self::Wgcc => "wgcc",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
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
    configured_output_name: Option<String>,
    configured_capture: CaptureMode,
    output_settings_ready: bool,
    global_stream_audio: bool,
    global_video_max_batch_size_kb: u8,
    global_max_bitrate_kbps: Option<u32>,
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
            configured_output_name: None,
            configured_capture: CaptureMode::Auto,
            output_settings_ready: true,
            global_stream_audio: true,
            global_video_max_batch_size_kb: 64,
            global_max_bitrate_kbps: None,
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

    pub fn set_output_name_settings(&mut self, ready: bool, configured_output_name: Option<&str>) {
        self.output_settings_ready = ready;
        self.configured_output_name = configured_output_name.map(str::to_owned);
    }

    pub fn set_capture_mode(&mut self, configured_capture: CaptureMode) {
        self.configured_capture = configured_capture;
    }

    pub fn set_global_stream_audio(&mut self, stream_audio: bool) {
        self.global_stream_audio = stream_audio;
    }

    pub fn set_global_video_max_batch_size_kb(&mut self, value: u8) {
        self.global_video_max_batch_size_kb = match value {
            16 | 32 | 64 => value,
            _ => 64,
        };
    }

    pub fn set_global_max_bitrate_kbps(&mut self, value: Option<u32>) {
        self.global_max_bitrate_kbps = value;
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
        if !self.output_settings_ready {
            return Some(private_error(503, "stream settings are loading"));
        }
        if !peer.paired() {
            return Some(private_error(401, "authenticated paired TLS is required"));
        }
        if self
            .global_max_bitrate_kbps
            .is_some_and(|bitrate| bitrate < 100)
        {
            return Some(private_error(
                503,
                "configured maximum bitrate is below the supported 100 kbps encoder minimum",
            ));
        }
        if request.app_id == 0
            || request.app_id > i32::MAX as u32
            || !config.profile.accepts(&request)
        {
            return Some(private_error(400, "unsupported streaming request"));
        }
        let global_fec_percentage = config.profile.fec_percentage;
        let global_stream_audio = self.global_stream_audio;
        let global_video_max_batch_size_kb = self.global_video_max_batch_size_kb;
        let global_max_bitrate_kbps = self.global_max_bitrate_kbps;
        let capture_output = self
            .configured_output_name
            .as_deref()
            .filter(|output_name| !output_name.is_empty())
            .unwrap_or(&config.capture.output)
            .to_owned();
        let (capture_provider, constant_rate) = self
            .configured_capture
            .resolve(config.capture.capture_provider);

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
            global_fec_percentage,
            global_stream_audio,
            global_video_max_batch_size_kb,
            global_max_bitrate_kbps,
            capture_output,
            capture_provider,
            constant_rate,
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
