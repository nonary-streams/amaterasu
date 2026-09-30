use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum State {
    Preparing,
    Ready,
    Arming,
    Playing,
    Closing,
    Closed,
    Failed,
    CleanupFailed,
}

impl State {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Closed | Self::Failed)
    }
}

pub(super) struct Record {
    pub id: u64,
    pub owner: String,
    pub config: Open,
    pub state: State,
    pub session: Option<u64>,
    pub negotiation: Option<Negotiation>,
    pub negotiated_encryption: Option<crate::model::EncryptionSnapshot>,
    pub negotiated_slices_per_frame: Option<i32>,
    pub client_feature_flags: Option<u32>,
    pub url: String,
    pub session_token: String,
    pub udp_discovery: Option<UdpDiscovery>,
    pub local: SocketAddr,
    pub expires: u64,
    pub next_status: u64,
    pub failure: Option<&'static str>,
    pub cleanup_error: Option<&'static str>,
    pub uncertain: Option<Call>,
    pub uncertain_phase: Option<Phase>,
    pub needs_lookup: bool,
    pub stream_state: Option<String>,
    /// True only after stream-session reports an applied, owned display lease.
    pub display_applied: bool,
    pub display_preflight_deadline: Option<u64>,
    /// Conservative Ready deadline measured when stream-session Prepare is sent.
    pub display_lease_ready_until: Option<u64>,
    pub display_prepare_timeout_ms: Option<u32>,
    pub revoke_after: Option<u64>,
    pub cleanup_confirmed: bool,
}

impl Record {
    pub(super) fn begin_arm(&mut self, now: u64) -> u64 {
        self.state = State::Arming;
        self.expires = now.saturating_add(u64::from(self.config.ping_timeout_ms));
        self.expires
    }

    pub(super) fn accept_announce(
        &mut self,
        announce: &amaterasu_gamestream::sdp::Announce,
    ) -> bool {
        let accepted = self.config.matches_announce(announce);
        self.negotiated_encryption = accepted.then_some(crate::model::EncryptionSnapshot {
            video: announce.encryption.video,
            audio: announce.encryption.audio,
        });
        self.negotiated_slices_per_frame = accepted.then_some(announce.video.slices_per_frame);
        self.client_feature_flags = accepted.then_some(announce.client_feature_flags);
        accepted
    }

    pub(super) fn authenticated_start_payload(&self, session: u64) -> Option<Value> {
        let encryption = self.negotiated_encryption?;
        let slices_per_frame = self.negotiated_slices_per_frame?;
        let client_feature_flags = self.client_feature_flags?;
        Some(self.config.arm_with_negotiation(
            session,
            encryption,
            slices_per_frame,
            client_feature_flags,
        ))
    }

    pub fn operation(&self) -> String {
        format!("rtsp-{:016x}", self.id)
    }

    pub fn response(&self) -> Value {
        json!({"ok":true,"ticket":self.id,"state":self.state,"local":self.local,
            "rtsp_url":self.url,"session":self.session,"stream_state":self.stream_state,
            "frame_limiter_settings":self.config.frame_limiter_settings,
            "encoder":self.config.capture.encoder,"sw_preset":self.config.sw_preset,"sw_tune":self.config.sw_tune,"min_threads":self.config.min_threads,"qp":self.config.qp,
            "slices_per_frame":self.negotiated_slices_per_frame,
            "nvenc_settings":self.config.nvenc_settings,
            "nvenc_spatial_aq":self.config.nvenc_spatial_aq,
            "frame_limiter_executable_path":self.config.frame_limiter_executable_path,
            "history_activity_id":self.config.history_activity_id,
            "display_request":self.config.display_request,
            "display_applied":self.display_applied,
            "display_policy_snapshot":self.config.display_policy_snapshot,
            "failure":self.failure,"cleanup_error":self.cleanup_error})
    }

    pub fn close(&mut self, reason: Option<&'static str>) {
        if let Some(reason) = reason {
            self.failure.get_or_insert(reason);
        }
        self.state = State::Closing;
    }

    pub fn capabilities(&self) -> Capabilities {
        let effective_profile = self.config.effective_profile();
        Capabilities {
            transport: amaterasu_gamestream::sdp::TransportSupport {
                stereo_audio: true,
                enet_control: true,
                encrypted_control: true,
                control_v2: true,
                encrypted_remote_input: false,
                encrypted_video: true,
                encrypted_audio: true,
                fec: true,
            },
            color: amaterasu_gamestream::sdp::ColorMode::Bt709Limited,
            max_width: effective_profile.width,
            max_height: effective_profile.height,
            max_fps: effective_profile.fps,
            max_bitrate_kbps: effective_profile.bitrate_kbps,
            require_video_encryption: effective_profile.encrypt_video,
            require_audio_encryption: effective_profile.encrypt_audio,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Prepare,
    Find,
    VerifyFrameLimiter,
    Arm,
    Stop,
    Status,
}

pub(super) struct Job {
    pub ticket: u64,
    pub phase: Phase,
    pub call: Call,
    pub caller: Option<u64>,
    pub network: Option<(u64, u64)>,
    pub deadline: u64,
}
