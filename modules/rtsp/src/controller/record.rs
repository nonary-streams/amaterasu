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
    pub revoke_after: Option<u64>,
    pub cleanup_confirmed: bool,
}

impl Record {
    pub fn operation(&self) -> String {
        format!("rtsp-{:016x}", self.id)
    }

    pub fn response(&self) -> Value {
        json!({"ok":true,"ticket":self.id,"state":self.state,"local":self.local,
            "rtsp_url":self.url,"session":self.session,"stream_state":self.stream_state,
            "failure":self.failure,"cleanup_error":self.cleanup_error})
    }

    pub fn close(&mut self, reason: Option<&'static str>) {
        if let Some(reason) = reason {
            self.failure.get_or_insert(reason);
        }
        self.state = State::Closing;
    }

    pub fn capabilities(&self) -> Capabilities {
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
            max_width: self.config.profile.width,
            max_height: self.config.profile.height,
            max_fps: self.config.profile.fps,
            max_bitrate_kbps: self.config.profile.bitrate_kbps,
            require_video_encryption: self.config.profile.encrypt_video,
            require_audio_encryption: self.config.profile.encrypt_audio,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Prepare,
    Find,
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
