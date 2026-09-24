use super::{CaptureProvider, LaunchRequest, PeerIdentity};
use crate::launch::{CLEANUP_DEADLINE, MONITOR_INTERVAL};
use std::{collections::VecDeque, time::Instant};

use super::{call::Call, workflow::Phase};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Starting,
    AwaitingDelivery,
    Ready,
    Stopping,
    Stopped,
    Failed,
    CleanupFailed,
}

impl State {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Stopped | Self::Failed)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    LaunchApplication,
    OpenRtsp,
    AwaitingDelivery,
    Live,
    Cleanup,
    VerifyRtspCleanup,
    VerifyApplicationCleanup,
    Terminal,
}

pub struct Record {
    pub id: u64,
    pub peer: PeerIdentity,
    pub request: LaunchRequest,
    pub device_name: String,
    pub application_operation: String,
    pub rtsp_operation: String,
    pub connect_data: u32,
    pub global_fec_percentage: u8,
    pub global_stream_audio: bool,
    pub global_audio_sink_capture_only: bool,
    pub global_configured_audio_sink: Option<String>,
    pub global_configured_virtual_sink: Option<String>,
    pub resolved_virtual_sink: Option<String>,
    pub capture_sink_without_routing: bool,
    /// Sparse client-over-application timeout override captured for this launch.
    /// Input resolves `None` against its authoritative global policy at acquire.
    pub scoped_back_button_timeout: Option<i32>,
    pub global_ping_timeout_ms: u32,
    pub global_video_max_batch_size_kb: u8,
    pub global_max_bitrate_kbps: Option<u32>,
    pub global_minimum_fps_target: f64,
    pub global_wgc_pacing_smoothing: bool,
    pub application_stream_audio: Option<bool>,
    pub effective_stream_audio: bool,
    pub capture_output: String,
    pub audio_endpoint: String,
    pub bootstrap_audio_endpoint: String,
    pub capture_provider: CaptureProvider,
    pub constant_rate: bool,
    pub application_fec_percentage: Option<u8>,
    pub effective_fec_percentage: u8,
    pub application_requested: bool,
    pub application_launch: Option<u64>,
    pub rtsp_requested: bool,
    pub rtsp_ticket: Option<u64>,
    pub rtsp_url: Option<String>,
    pub rtsp_cleanup_blocked: bool,
    pub application_cleanup_blocked: bool,
    pub state: State,
    pub stage: Stage,
    pub adopted: bool,
    pub revoked: bool,
    pub launch_connection: Option<u64>,
    pub cancel_connection: Option<u64>,
    pub deadline: Instant,
    pub next_monitor: Instant,
    pub monitor_application_next: bool,
    pub failure: Option<&'static str>,
    pub cleanup_error: Option<&'static str>,
    pub uncertain: VecDeque<(Phase, Call)>,
    pub retrying_uncertainty: bool,
    pub rtsp_reconcile: Option<Phase>,
    pub application_reconcile: Option<Phase>,
}

impl Record {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: u64,
        peer: PeerIdentity,
        request: LaunchRequest,
        device_name: String,
        application_operation: String,
        rtsp_operation: String,
        connect_data: u32,
        global_fec_percentage: u8,
        global_stream_audio: bool,
        global_audio_sink_capture_only: bool,
        global_configured_audio_sink: Option<String>,
        global_configured_virtual_sink: Option<String>,
        global_ping_timeout_ms: u32,
        global_video_max_batch_size_kb: u8,
        global_max_bitrate_kbps: Option<u32>,
        global_minimum_fps_target: f64,
        global_wgc_pacing_smoothing: bool,
        capture_output: String,
        audio_endpoint: String,
        bootstrap_audio_endpoint: String,
        capture_provider: CaptureProvider,
        constant_rate: bool,
        connection: u64,
        deadline: Instant,
    ) -> Self {
        Self {
            id,
            peer,
            request,
            device_name,
            application_operation,
            rtsp_operation,
            connect_data,
            global_fec_percentage,
            global_stream_audio,
            global_audio_sink_capture_only,
            global_configured_audio_sink,
            global_configured_virtual_sink,
            resolved_virtual_sink: None,
            capture_sink_without_routing: false,
            scoped_back_button_timeout: None,
            global_ping_timeout_ms,
            global_video_max_batch_size_kb,
            global_max_bitrate_kbps,
            global_minimum_fps_target,
            global_wgc_pacing_smoothing,
            application_stream_audio: None,
            effective_stream_audio: global_stream_audio,
            capture_output,
            audio_endpoint,
            bootstrap_audio_endpoint,
            capture_provider,
            constant_rate,
            application_fec_percentage: None,
            effective_fec_percentage: global_fec_percentage,
            application_requested: false,
            application_launch: None,
            rtsp_requested: false,
            rtsp_ticket: None,
            rtsp_url: None,
            rtsp_cleanup_blocked: false,
            application_cleanup_blocked: false,
            state: State::Starting,
            stage: Stage::LaunchApplication,
            adopted: false,
            revoked: false,
            launch_connection: Some(connection),
            cancel_connection: None,
            deadline,
            next_monitor: deadline,
            monitor_application_next: true,
            failure: None,
            cleanup_error: None,
            uncertain: VecDeque::new(),
            retrying_uncertainty: false,
            rtsp_reconcile: None,
            application_reconcile: None,
        }
    }

    pub fn operation_prefix(&self) -> &str {
        &self.rtsp_operation
    }

    pub fn request_cleanup(&mut self, failure: Option<&'static str>, deadline: Instant) {
        if let Some(failure) = failure {
            self.failure.get_or_insert(failure);
        }
        let already_stopping = self.state == State::Stopping;
        self.state = State::Stopping;
        self.stage = Stage::Cleanup;
        if !already_stopping {
            self.deadline = deadline;
        }
        self.cleanup_error = None;
    }

    pub fn fail_cleanup(&mut self, error: &'static str) {
        self.state = State::CleanupFailed;
        self.cleanup_error = Some(error);
    }

    pub fn finish_cleanup(&mut self) {
        debug_assert!(self.uncertain.is_empty());
        self.state = if self.failure.is_some() {
            State::Failed
        } else {
            State::Stopped
        };
        self.stage = Stage::Terminal;
        self.cleanup_error = None;
    }

    pub fn monitored(&mut self, now: Instant) {
        self.next_monitor = now + MONITOR_INTERVAL;
        self.monitor_application_next = !self.monitor_application_next;
    }

    pub fn retry_cleanup(&mut self, now: Instant) -> Option<(Phase, Call)> {
        self.state = State::Stopping;
        self.stage = Stage::Cleanup;
        self.cleanup_error = None;
        self.rtsp_cleanup_blocked = false;
        self.application_cleanup_blocked = false;
        self.deadline = now + CLEANUP_DEADLINE;
        self.retrying_uncertainty = true;
        self.uncertain.pop_front().map(|(phase, mut call)| {
            call.retry_settlement();
            (phase, call)
        })
    }
}
