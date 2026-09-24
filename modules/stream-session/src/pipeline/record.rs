use super::{Drive, rpc};
use crate::{
    model::{PrepareConfig, StartConfig},
    plane::Handles,
};
use amaterasu_gamestream::control::InputTranslator;
use amaterasu_media::CaptureGeometry;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum State {
    Preparing,
    Prepared,
    Armed,
    Starting,
    Active,
    Stopping,
    Stopped,
    Failed,
    Cancelled,
    CleanupFailed,
}

impl State {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Stopped | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Resource {
    Session,
    AudioEncoder,
    Media,
    Control,
    Video,
    AudioCapture,
}

impl Resource {
    pub const CLEANUP: [Self; 5] = [
        Self::AudioCapture,
        Self::Video,
        Self::Control,
        Self::Media,
        Self::AudioEncoder,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::AudioEncoder => "audio-encode",
            Self::Media => "media-transport",
            Self::Control => "control-transport",
            Self::Video => "video",
            Self::AudioCapture => "audio",
        }
    }

    pub fn service(self) -> String {
        format!("amaterasu.{}/1", self.name())
    }
}

pub(super) struct Record {
    pub id: u64,
    pub owner: String,
    pub config: PrepareConfig,
    pub start: Option<StartConfig>,
    pub state: State,
    pub outcome: State,
    pub resources: BTreeMap<Resource, u64>,
    pub attempted: BTreeSet<Resource>,
    pub uncertain: Option<rpc::Pending>,
    pub plane: Option<Box<dyn Drive>>,
    pub prepared_until: u64,
    pub armed_accepted: bool,
    pub next_auth_poll: u64,
    pub control_authenticated: bool,
    pub media_ready: bool,
    pub peer_discovery: bool,
    pub video_peer_port: Option<u16>,
    pub audio_peer_port: Option<u16>,
    pub error: Option<&'static str>,
    pub cleanup_error: Option<&'static str>,
    pub audio_endpoint: Option<String>,
    pub force_idr: bool,
    pub control_messages: u64,
    pub last_control_sequence: Option<u64>,
    pub unsupported_controls: u64,
    pub rejected_controls: u64,
    pub input_ttl_ms: u64,
    pub capture_geometry: Option<CaptureGeometry>,
    pub input_absolute_pointer: bool,
    pub input_translator: InputTranslator,
    pub input_attempted: bool,
    pub input_lookup_required: bool,
    pub input_lease: Option<String>,
    pub input_request_started_at: u64,
    pub input_expires_at: u64,
    pub input_renew_at: u64,
    pub input_events: VecDeque<Value>,
    pub input_inflight: usize,
    pub input_events_sent: u64,
    pub input_batches: u64,
    pub packet_bytes: usize,
    pub wire_audio_bytes: usize,
    pub control_local: Option<String>,
    pub video_local: Option<String>,
    pub audio_local: Option<String>,
}

impl Record {
    pub fn new(id: u64, owner: String, config: PrepareConfig, input_ttl_ms: u64) -> Self {
        Self {
            id,
            owner,
            config,
            start: None,
            state: State::Preparing,
            outcome: State::Failed,
            resources: BTreeMap::new(),
            attempted: BTreeSet::new(),
            uncertain: None,
            plane: None,
            prepared_until: 0,
            armed_accepted: false,
            next_auth_poll: 0,
            control_authenticated: false,
            media_ready: false,
            peer_discovery: false,
            video_peer_port: None,
            audio_peer_port: None,
            error: None,
            cleanup_error: None,
            audio_endpoint: None,
            force_idr: false,
            control_messages: 0,
            last_control_sequence: None,
            unsupported_controls: 0,
            rejected_controls: 0,
            input_ttl_ms,
            capture_geometry: None,
            input_absolute_pointer: false,
            input_translator: InputTranslator::default(),
            input_attempted: false,
            input_lookup_required: false,
            input_lease: None,
            input_request_started_at: 0,
            input_expires_at: 0,
            input_renew_at: 0,
            input_events: VecDeque::new(),
            input_inflight: 0,
            input_events_sent: 0,
            input_batches: 0,
            packet_bytes: 0,
            wire_audio_bytes: 0,
            control_local: None,
            video_local: None,
            audio_local: None,
        }
    }

    pub fn operation(&self, resource: Resource) -> String {
        format!("ss-{:x}-{}", self.id, resource.name())
    }

    pub fn input_operation(&self) -> String {
        format!("ss-{:x}-input", self.id)
    }

    pub fn lease(&self, audio: bool) -> String {
        use sha2::{Digest, Sha256};
        let start = self.start.as_ref().expect("validated start workflow");
        let identity = if audio {
            self.audio_endpoint
                .as_ref()
                .expect("resolved audio endpoint")
                .as_bytes()
                .to_vec()
        } else {
            serde_json::to_vec(&(start.capture_provider, start.constant_rate, &start.output))
                .expect("bounded configuration")
        };
        let digest = Sha256::digest(identity);
        let mut resource = String::from(if audio {
            "stream-audio-"
        } else {
            "stream-output-"
        });
        for byte in digest {
            use std::fmt::Write;
            write!(&mut resource, "{byte:02x}").expect("string write");
        }
        resource
    }

    pub fn handles(&self) -> Result<Handles, &'static str> {
        let get = |resource| {
            self.resources
                .get(&resource)
                .copied()
                .ok_or("missing_component")
        };
        let audio_handle = |resource| self.config.stream_audio.then(|| get(resource)).transpose();
        Ok(Handles {
            video: get(Resource::Video)?,
            audio_capture: audio_handle(Resource::AudioCapture)?,
            audio_encoder: audio_handle(Resource::AudioEncoder)?,
            media_transport: get(Resource::Media)?,
            control_transport: get(Resource::Control)?,
            stream_audio: self.config.stream_audio,
        })
    }

    pub fn response(&self) -> Value {
        json!({
            "ok": true,
            "session": self.id,
            "state": self.state,
            "stream_audio": self.config.stream_audio,
            "packet_bytes": self.packet_bytes,
            "wire_audio_bytes": self.wire_audio_bytes,
            "control_local": self.control_local,
            "video_local": self.video_local,
            "audio_local": self.audio_local,
            "media_ready": self.media_ready,
            "peer_discovery": self.peer_discovery,
            "video_peer_port": self.video_peer_port,
            "audio_peer_port": self.audio_peer_port,
            "error": self.error,
            "cleanup_error": self.cleanup_error,
            "control_messages": self.control_messages,
            "last_control_sequence": self.last_control_sequence,
            "unsupported_controls": self.unsupported_controls,
            "rejected_controls": self.rejected_controls,
            "input_active": self.input_lease.is_some(),
            "absolute_input": self.input_lease.is_some()
                && self.capture_geometry.is_some()
                && self.input_absolute_pointer,
            "input_events_sent": self.input_events_sent,
            "input_batches": self.input_batches,
        })
    }

    pub fn open(&self, resource: Resource) -> Value {
        let config = &self.config;
        let mut payload = match resource {
            Resource::Session => return json!({"action":"create","key":self.operation(resource)}),
            Resource::AudioEncoder => json!({"action":"open","channels":2,
                "packet_ms":config.audio.packet_ms,"bitrate":config.audio.bitrate}),
            Resource::Media => {
                let audio_encrypted = config.stream_audio && config.encrypt_audio;
                let permit_legacy_audio_iv_wrap =
                    config.stream_audio && config.permit_legacy_audio_iv_wrap;
                let mut payload = json!({"action":"open","peer_ip":config.peer_ip,
                    "video_bind":config.video_bind,"audio_bind":config.audio_bind,"video":config.video,
                    "stream_audio":config.stream_audio,
                    "audio":{"packet_ms":config.audio.packet_ms,"channels":2,"wire_payload_bytes":self.wire_audio_bytes},
                    "encryption":if config.encrypt_video || audio_encrypted {
                        json!({"key_hex":config.key_hex,"key_id":config.key_id,"video":config.encrypt_video,
                            "audio":audio_encrypted,"permit_legacy_audio_iv_wrap":permit_legacy_audio_iv_wrap})
                    } else { Value::Null }});
                if let Some(discovery) = &config.discovery {
                    payload["discovery"] = json!(discovery);
                } else {
                    payload["video_port"] = json!(config.video_port.expect("validated fixed port"));
                    payload["audio_port"] = json!(config.audio_port.expect("validated fixed port"));
                }
                payload
            }
            Resource::Control => {
                json!({"action":"open","peer_ip":config.peer_ip,"local_bind":config.control_bind,
                "connect_data":config.connect_data,"key_hex":config.key_hex})
            }
            Resource::Video => {
                let start = self.start.as_ref().expect("validated start workflow");
                json!({"action":"open","capture_provider":start.capture_provider,"output":start.output,
                    "constant_rate":start.constant_rate,"encoder":start.encoder,
                    "configuration":start.configuration,"packet_boundaries":true})
            }
            Resource::AudioCapture => {
                json!({"action":"open","endpoint":self.start.as_ref().expect("validated start workflow").audio_endpoint})
            }
        };
        payload["operation_id"] = json!(self.operation(resource));
        payload
    }
}

pub(super) fn error(code: &str) -> Value {
    json!({"ok":false,"code":code})
}
