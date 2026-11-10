use super::input::TouchPort;
use super::{Drive, rpc, workflow::Step};
use crate::plane::RumbleCommand;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Resource {
    Session,
    AudioEncoder,
    AudioProcessor,
    Media,
    Control,
    Video,
    AudioCapture,
}

impl Resource {
    pub const CLEANUP: [Self; 6] = [
        Self::AudioCapture,
        Self::Video,
        Self::Control,
        Self::Media,
        Self::AudioProcessor,
        Self::AudioEncoder,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::AudioEncoder => "audio-encode",
            Self::AudioProcessor => "audio-processor",
            Self::Media => "media-transport",
            Self::Control => "control-transport",
            Self::Video => "video",
            Self::AudioCapture => "audio",
        }
    }

    #[cfg(test)]
    pub fn service(self) -> String {
        format!("amaterasu.{}/1", self.name())
    }

    fn native_contract(self) -> Option<&'static str> {
        match self {
            Self::Session => None,
            Self::Video => Some("amaterasu.video/1"),
            Self::AudioCapture => Some("amaterasu.audio/1"),
            Self::AudioEncoder => Some("amaterasu.audio-encode/1"),
            Self::AudioProcessor => Some(amaterasu_module_sdk::audio::PROCESSOR_CONTRACT),
            Self::Media => Some("amaterasu.media-transport/1"),
            Self::Control => Some("amaterasu.control-transport/1"),
        }
    }
}

pub(super) struct Record {
    pub providers: crate::providers::ProviderServices,
    pub id: u64,
    pub history: super::history::SessionHistory,
    pub host_metrics: super::host_metrics_activity::HostMetricsActivity,
    pub session_effects: super::session_effects::SessionEffects,
    pub owner: String,
    pub config: PrepareConfig,
    pub pending_audio: Option<crate::model::AudioWire>,
    pub start: Option<StartConfig>,
    pub shared_capture: Option<super::shared_capture::Source>,
    pub shared_capture_attached: bool,
    pub lifecycle_observation: Option<Value>,
    pub lifecycle_retirement_receipt: Option<Value>,
    pub restart_lifecycle_operation: Option<String>,
    pub restart_lifecycle_target: Option<amaterasu_extension_api::LifecycleTarget>,
    pub restart_lifecycle_preparation: Option<super::journal::PreparationRecovery>,
    pub lifecycle_candidate_validated: bool,
    pub processing_plan_provider: Option<crate::model::ProcessingPlanProvider>,
    pub processing_frozen: Option<(String, Value)>,
    pub processing_plan_complete: bool,
    pub gpu_processing: Option<Value>,
    pub source_preparation: Option<crate::model::SourcePreparation>,
    pub source_hook_index: usize,
    pub source_scope: Option<super::shared_capture::SettingsScope>,
    pub source_hook_settings: Option<(String, Value)>,
    pub lifecycle_fenced: bool,
    pub lifecycle_retired: bool,
    pub lifecycle_retirement_proof: Option<amaterasu_extension_api::StreamRetirementProof>,
    pub lifecycle_delivery_permit: bool,
    pub lifecycle_control_permit: bool,
    pub lifecycle_validation_sequence: u64,
    pub lifecycle_invalid_reason: Option<&'static str>,
    pub lifecycle_input_token: Option<String>,
    pub lifecycle_input_epoch: Option<String>,
    pub lifecycle_input_geometry_revision: Option<u64>,
    pub lifecycle_input_sequence: u64,
    pub lifecycle_input_required: Option<bool>,
    pub state: State,
    pub outcome: State,
    pub resources: BTreeMap<Resource, u64>,
    pub attempted: BTreeSet<Resource>,
    pub uncertain: Option<rpc::Pending>,
    pub uncertain_step: Option<Step>,
    pub journal_uncertain: Option<UncertainJournalWrite>,
    /// At most one read-only observation can be in flight when shutdown begins.
    /// Its broker bookkeeping cannot keep native resources alive after teardown.
    pub shutdown_observations: Vec<rpc::Pending>,
    pub plane: Option<Box<dyn Drive>>,
    pub audio_recovery_needed: bool,
    pub prepared_until: u64,
    pub armed_accepted: bool,
    pub next_auth_poll: u64,
    pub control_authenticated: bool,
    pub control_disconnect_checked: bool,
    pub authenticated_disconnect_reason: Option<&'static str>,
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
    pub input_controller_supported: bool,
    pub input_mixed_batch_supported: bool,
    pub client_haptics_requested: bool,
    pub input_pen_touch_supported: bool,
    pub input_native_pen_touch: bool,
    pub touch_port: Option<TouchPort>,
    pub input_translator: InputTranslator,
    pub input_attempted: bool,
    pub input_lookup_required: bool,
    pub input_lease: Option<String>,
    pub virtual_display_attempted: bool,
    pub virtual_display_output: Option<String>,
    pub display_attempted: bool,
    pub display_lease: Option<String>,
    pub display_status_verified: bool,
    pub display_state: String,
    pub display_applied: bool,
    pub owns_display_restore: bool,
    pub display_operation_id: Option<String>,
    pub virtual_display_reference_owned: bool,
    pub virtual_display_status_verified: bool,
    pub virtual_display_remove_attempted: bool,
    pub virtual_display_state: String,
    pub virtual_display_create_error: Option<String>,
    pub virtual_display_device_path: Option<String>,
    pub virtual_display_next_poll_at: u64,
    pub input_request_started_at: u64,
    pub input_expires_at: u64,
    pub input_renew_at: u64,
    pub feedback_poll_at: u64,
    pub rumble_poll_at: u64,
    pub feedback_ack_through: Option<u64>,
    pub rumble_pending: BTreeMap<u16, (RumbleCommand, Option<u64>)>,
    pub rumble_active_until: BTreeMap<u16, u64>,
    pub rumble_shutdown_deadline: Option<u64>,
    pub rumble_stop_unconfirmed: bool,
    pub input_events: VecDeque<Value>,
    pub input_inflight: usize,
    pub input_timing: super::input_timing::InputTiming,
    pub input_events_sent: u64,
    pub input_batches: u64,
    pub packet_bytes: usize,
    pub audio_encoder_layout: Option<Value>,
    pub wire_audio_bytes: usize,
    pub control_local: Option<String>,
    pub video_local: Option<String>,
    pub audio_local: Option<String>,
}

pub(super) struct UncertainJournalWrite {
    pub pending: rpc::Pending,
    pub document: Option<super::journal::Document>,
    pub validated: bool,
    pub next_poll: u64,
}

impl Record {
    pub fn required_native_contract(&self, resource: Resource) -> Option<&'static str> {
        if resource == Resource::AudioCapture && self.config.audio.encoded_source() {
            return Some(amaterasu_module_sdk::audio::ENCODED_SOURCE_CONTRACT);
        }
        resource
            .native_contract()
            .filter(|contract| self.service(resource) != *contract)
    }

    pub fn validate_native_contract(
        &self,
        resource: Resource,
        payload: &Value,
    ) -> Result<(), &'static str> {
        if let Some(contract) = self.required_native_contract(resource)
            && payload["native_contract"].as_str() != Some(contract)
        {
            return Err("native_contract_unconfirmed");
        }
        Ok(())
    }

    pub fn service(&self, resource: Resource) -> String {
        match resource {
            Resource::Session => &self.providers.session,
            Resource::Video => &self.providers.video,
            Resource::AudioCapture => &self.providers.audio_capture,
            Resource::AudioEncoder => &self.providers.audio_encoder,
            Resource::AudioProcessor => self
                .providers
                .audio_processor
                .as_ref()
                .expect("processor selected before allocation"),
            Resource::Media => &self.providers.media_transport,
            Resource::Control => &self.providers.control_transport,
        }
        .clone()
    }

    pub fn new(id: u64, owner: String, config: PrepareConfig, input_ttl_ms: u64) -> Self {
        let virtual_display_state = "not_requested".to_owned();
        Self {
            providers: Default::default(),
            id,
            history: super::history::SessionHistory::new(id, config.history_activity_id.clone()),
            host_metrics: super::host_metrics_activity::HostMetricsActivity::new(id),
            session_effects: super::session_effects::SessionEffects::default(),
            owner,
            config,
            pending_audio: None,
            start: None,
            shared_capture: None,
            shared_capture_attached: false,
            lifecycle_observation: None,
            lifecycle_retirement_receipt: None,
            restart_lifecycle_target: None,
            restart_lifecycle_preparation: None,
            restart_lifecycle_operation: None,
            lifecycle_candidate_validated: false,
            processing_plan_provider: None,
            processing_frozen: None,
            processing_plan_complete: false,
            gpu_processing: None,
            source_preparation: None,
            source_hook_index: 0,
            source_scope: None,
            source_hook_settings: None,
            lifecycle_fenced: false,
            lifecycle_retired: false,
            lifecycle_retirement_proof: None,
            lifecycle_delivery_permit: false,
            lifecycle_control_permit: false,
            lifecycle_validation_sequence: 0,
            lifecycle_invalid_reason: None,
            lifecycle_input_token: None,
            lifecycle_input_epoch: None,
            lifecycle_input_geometry_revision: None,
            lifecycle_input_sequence: 1,
            lifecycle_input_required: None,
            state: State::Preparing,
            outcome: State::Failed,
            resources: BTreeMap::new(),
            attempted: BTreeSet::new(),
            uncertain: None,
            uncertain_step: None,
            journal_uncertain: None,
            shutdown_observations: Vec::new(),
            plane: None,
            audio_recovery_needed: false,
            prepared_until: 0,
            armed_accepted: false,
            next_auth_poll: 0,
            control_authenticated: false,
            control_disconnect_checked: false,
            authenticated_disconnect_reason: None,
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
            input_controller_supported: false,
            input_mixed_batch_supported: false,
            client_haptics_requested: false,
            input_pen_touch_supported: false,
            input_native_pen_touch: false,
            touch_port: None,
            input_translator: InputTranslator::default(),
            input_attempted: false,
            input_lookup_required: false,
            input_lease: None,
            virtual_display_attempted: false,
            virtual_display_output: None,
            display_attempted: false,
            display_lease: None,
            display_status_verified: false,
            display_state: "not_requested".into(),
            display_applied: false,
            owns_display_restore: false,
            display_operation_id: None,
            virtual_display_reference_owned: false,
            virtual_display_status_verified: false,
            virtual_display_remove_attempted: false,
            virtual_display_state,
            virtual_display_create_error: None,
            virtual_display_device_path: None,
            virtual_display_next_poll_at: 0,
            input_request_started_at: 0,
            input_expires_at: 0,
            input_renew_at: 0,
            feedback_poll_at: 0,
            rumble_poll_at: 0,
            feedback_ack_through: None,
            rumble_pending: BTreeMap::new(),
            rumble_active_until: BTreeMap::new(),
            rumble_shutdown_deadline: None,
            rumble_stop_unconfirmed: false,
            input_events: VecDeque::new(),
            input_inflight: 0,
            input_timing: super::input_timing::InputTiming::default(),
            input_events_sent: 0,
            input_batches: 0,
            packet_bytes: 0,
            audio_encoder_layout: None,
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

    pub fn display_operation(&self) -> String {
        self.display_operation_id
            .clone()
            .unwrap_or_else(|| format!("ss-{:x}-display", self.id))
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
        } else if let Some(source_id) = start.source_id {
            serde_json::to_vec(&(&start.encoder_provider, source_id))
                .expect("bounded source identity")
        } else {
            serde_json::to_vec(&(&start.capture_provider, start.constant_rate, &start.output))
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
            hdr: self
                .start
                .as_ref()
                .ok_or("missing_start_configuration")?
                .configuration
                .video_stream_format
                .dynamic_range
                == amaterasu_module_sdk::DynamicRange::Hdr,
            audio_capture: audio_handle(Resource::AudioCapture)?,
            audio_encoder: if self.config.audio.encoded_source() {
                None
            } else {
                audio_handle(Resource::AudioEncoder)?
            },
            audio_processor: if self
                .config
                .audio
                .custom
                .as_ref()
                .is_some_and(|custom| custom.processing.is_some())
            {
                audio_handle(Resource::AudioProcessor)?
            } else {
                None
            },
            media_transport: get(Resource::Media)?,
            control_transport: get(Resource::Control)?,
            stream_audio: self.config.stream_audio,
            audio_channels: self
                .config
                .audio
                .custom
                .as_ref()
                .and_then(|custom| custom.pcm.as_ref())
                .map_or(self.config.audio.channels, |pcm| pcm.speakers.len() as u32),
            audio_sample_rate: self.config.audio.custom.as_ref().map_or(48_000, |custom| {
                custom
                    .pcm
                    .as_ref()
                    .map_or(custom.sample_rate, |pcm| pcm.sample_rate)
            }),
            audio_format: self.config.audio.custom.as_ref().map(|custom| custom.codec),
            audio_generation: self
                .config
                .audio
                .custom
                .as_ref()
                .map_or(1, |custom| custom.stream.generation),
            audio_max_unit_bytes: self
                .config
                .audio
                .custom
                .as_ref()
                .and_then(|custom| {
                    self.config
                        .audio
                        .client_offers
                        .iter()
                        .find(|offer| offer.supports(custom))
                })
                .map_or(amaterasu_media::AUDIO_EXTENSIBLE_MAX_PAYLOAD, |offer| {
                    offer.max_unit_bytes as usize
                }),
            audio_max_metadata_bytes: self
                .config
                .audio
                .custom
                .as_ref()
                .and_then(|custom| {
                    self.config
                        .audio
                        .client_offers
                        .iter()
                        .find(|offer| offer.supports(custom))
                })
                .map_or(amaterasu_media::AUDIO_EXTENSIBLE_MAX_METADATA, |offer| {
                    offer.max_metadata_bytes as usize
                }),
            audio_pcm: self
                .config
                .audio
                .custom
                .as_ref()
                .and_then(|custom| custom.pcm.as_ref())
                .map(|pcm| (pcm.sample_format, pcm.sample_bytes, pcm.packing)),
            encoded_audio_source: self.config.audio.encoded_source(),
            processed_audio: self
                .config
                .audio
                .custom
                .as_ref()
                .and_then(|custom| custom.processing.as_ref())
                .map(|processing| {
                    let pcm = &processing.output;
                    (
                        pcm.sample_rate,
                        pcm.speakers.len() as u32,
                        pcm.sample_format,
                        pcm.sample_bytes,
                        pcm.packing,
                    )
                }),
            video_codec: self
                .start
                .as_ref()
                .ok_or("missing_start_configuration")?
                .configuration
                .codec
                .abi_id(),
        })
    }

    pub fn lifecycle_consumers_retired(&self) -> bool {
        self.lifecycle_retired
            || self
                .lifecycle_retirement_proof
                .as_ref()
                .is_some_and(|proof| proof.consumer_retired)
    }

    pub fn response(&self) -> Value {
        let mut response = json!({
            "ok": true,
            "session": self.id,
            "lifecycle_session_target": self.config.lifecycle_session_target,
            "lifecycle_capture_mode": self.config.lifecycle_capture_mode,
            "lifecycle_input_required": self.lifecycle_input_required,
            "lifecycle_target": self.start.as_ref().and_then(|start| start.lifecycle_target.as_ref()),
            "lifecycle_retired": self.lifecycle_retired,
            "lifecycle_consumer_retired": self.lifecycle_consumers_retired(),
            "lifecycle_resources_released": self.lifecycle_retired,
            "lifecycle_shared_retained": self.lifecycle_retirement_proof.as_ref().is_some_and(|proof| proof.shared_retained),
            "lifecycle_shared_source": self.lifecycle_retirement_proof.as_ref().and_then(|proof| proof.shared_source.as_ref()),
            "authenticated_disconnect_reason": self.authenticated_disconnect_reason,
            "history_activity_id": self.config.history_activity_id,
            "rtsp_operation_id": self.config.rtsp_operation_id,
            "state": self.state,
            "encoder_provider": self.start.as_ref().and_then(|start| start.encoder_provider.as_deref()),
            "source_id": self.start.as_ref().and_then(|start| start.source_id),
            "client_feature_flags": self.start.as_ref().and_then(|start| start.client_feature_flags),
            "haptics_pcm_negotiated": self.start.as_ref().is_some_and(|start| {
                start.client_feature_flags.is_some_and(|flags| {
                    flags & amaterasu_gamestream::sdp::CLIENT_FEATURE_HAPTICS_PCM != 0
                })
            }),
            "client_feedback_active": self.input_lease.is_some()
                && self.state == State::Active,
            "stream_audio": self.config.stream_audio,
            "audio": self.config.audio,
            "display_request": self.config.display_request.as_ref().map(|request| {
                let mut value = json!({"configuration": request.configuration,
                    "expected_topology_revision":request.expected_topology_revision});
                if let Some(policy) = &request.mode_policy { value["mode_policy"] = policy.clone(); }
                if let Some(policies) = &request.mode_policies { value["mode_policies"] = json!(policies); }
                value
            }),
            "display_prepare_timeout_ms": self.config.display_prepare_timeout_ms,
            "display_state": self.display_state,
            "display_applied": self.display_applied && self.display_state == "applied",
            "virtual_display_ready": self.virtual_display_reference_owned
                && self.virtual_display_status_verified
                && self.virtual_display_state == "ready",
            "packet_bytes": self.packet_bytes,
            "audio_encoder_layout": self.audio_encoder_layout,
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
            "remote_rumble_stop_unconfirmed": self.rumble_stop_unconfirmed,
            "absolute_input": self.input_lease.is_some()
                && self.capture_geometry.is_some()
                && self.input_absolute_pointer,
            "controller_input": self.input_lease.is_some()
                && self.input_controller_supported,
            "haptics_feedback_available": self.input_lease.is_some()
                && self.input_controller_supported,
            "haptics_feedback_enabled": self.client_haptics_requested
                && self.input_lease.is_some()
                && self.input_controller_supported,
            "input_events_sent": self.input_events_sent,
            "input_batches": self.input_batches,
        });
        if let Some(source) = &self.config.borrowed_capture {
            response["borrowed_capture"] = json!(source);
        }
        if let Some(context) = &self.config.source_capture_context {
            response["source_capture_context"] = json!(context);
        }
        if let Some(reason) = self.authenticated_disconnect_reason
            && self.control_authenticated
        {
            response["authenticated_disconnect"] = json!({"version":1,"session":self.id,
                "operation_id":self.config.operation_id,"control_authenticated":true,"reason":reason});
        }
        response
    }

    pub fn open(&self, resource: Resource) -> Value {
        let config = &self.config;
        let mut payload = match resource {
            Resource::Session => return json!({"action":"create","key":self.operation(resource)}),
            Resource::AudioEncoder => {
                let mut payload = json!({"action":"open","channels":config.audio.channels,
                    "packet_ms":config.audio.packet_ms,"bitrate":config.audio.bitrate});
                if config.audio.custom.is_some() {
                    payload["audio_profile"] = config.audio.custom_profile();
                }
                payload
            }
            Resource::AudioProcessor => {
                json!({"action":"open", "audio_profile":config.audio.custom_profile()})
            }
            Resource::Media => {
                let audio_encrypted = config.stream_audio && config.encrypt_audio;
                let permit_legacy_audio_iv_wrap =
                    config.stream_audio && config.permit_legacy_audio_iv_wrap;
                let mut payload = json!({"action":"open","peer_ip":config.peer_ip,
                    "video_bind":config.video_bind,"audio_bind":config.audio_bind,"video":config.video,
                    "ping_timeout_ms":config.ping_timeout_ms,
                    "stream_audio":config.stream_audio,
                    "audio":{"packet_ms":config.audio.packet_ms,"channels":config.audio.channels,"wire_payload_bytes":self.wire_audio_bytes},
                    "encryption":if config.encrypt_video || audio_encrypted {
                        json!({"key_hex":config.key_hex,"key_id":config.key_id,"video":config.encrypt_video,
                            "audio":audio_encrypted,"permit_legacy_audio_iv_wrap":permit_legacy_audio_iv_wrap})
                    } else { Value::Null }});
                if config.audio.custom.is_some() {
                    payload["audio_profile"] = config.audio.custom_profile();
                }
                if let Some(policy) = &config.video_encryption_policy {
                    payload["video_encryption_policy"] = json!({
                        "lan_mode": policy.lan_mode,
                        "wan_mode": policy.wan_mode,
                    });
                }
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
                "connect_data":config.connect_data,"key_hex":config.key_hex,
                "ping_timeout_ms":config.ping_timeout_ms})
            }
            Resource::Video => {
                let start = self.start.as_ref().expect("validated start workflow");
                let mut payload = json!({"action":"open", "encoder":start.encoder,
                    "configuration":start.configuration,"packet_boundaries":true});
                if let Some(processing) = &self.gpu_processing {
                    payload["configuration"]["gpu_processing"] = processing.clone();
                }
                if let Some(alias) = &start.encoder_provider {
                    payload["encoder_provider"] = json!(alias);
                }
                if let Some(options) = &start.encoder_options {
                    payload["encoder_options"] = options.clone();
                }
                if let Some(context) = &start.source_capture_context {
                    payload["source_capture_context"] = json!(context);
                }
                if let Some(source_id) = start.source_id {
                    payload["source_id"] = json!(source_id);
                } else {
                    payload["capture_provider"] = json!(start.capture_provider);
                    payload["output"] = json!(
                        start
                            .lifecycle_candidate
                            .as_ref()
                            .and_then(|binding| binding.selector["output"].as_str())
                            .or(self.virtual_display_output.as_deref())
                            .unwrap_or(&start.output)
                    );
                    payload["constant_rate"] = json!(start.constant_rate);
                    payload["minimum_fps_target"] = json!(start.minimum_fps_target);
                    payload["wgc_pacing_smoothing"] = json!(start.wgc_pacing_smoothing);
                }
                payload
            }
            Resource::AudioCapture => {
                let start = self.start.as_ref().expect("validated start workflow");
                let mut payload = json!({"action":"open","endpoint":start.audio_endpoint,
                    "capture_sink_without_routing":start.capture_sink_without_routing,
                    "virtual_sink":start.virtual_sink,
                    "channels":start.virtual_sink.as_ref().and(start.channels),
                    "host_audio_enabled":start.host_audio_enabled});
                if let Some(custom) = &self.config.audio.custom {
                    if let Some(pcm) = &custom.pcm {
                        payload["pcm_format"] =
                            serde_json::to_value(pcm).expect("PCM format serializes");
                    } else {
                        payload["audio_source"] = json!("encoded");
                    }
                } else if self.config.stream_audio {
                    payload["capture_format"] = json!({
                        "sample_rate": 48_000,
                        "channels": self.config.audio.channels,
                        "sample_format": "f32"
                    });
                }
                if config.audio.custom.is_some() {
                    payload["audio_profile"] = config.audio.custom_profile();
                }
                if self.config.stream_audio && start.continuous_audio == Some(true) {
                    payload["continuous_audio"] = json!({"packet_ms":config.audio.packet_ms});
                }
                payload
            }
        };
        payload["operation_id"] = json!(self.operation(resource));
        if let Some(contract) = self.required_native_contract(resource) {
            payload["required_native_contract"] = json!(contract);
        }
        if matches!(
            resource,
            Resource::AudioCapture
                | Resource::AudioEncoder
                | Resource::AudioProcessor
                | Resource::Media
        ) {
            let request: amaterasu_module_sdk::audio::AudioControlRequest =
                serde_json::from_value(payload)
                    .expect("audio open matches the shared SDK contract");
            request.payload().expect("audio open serializes")
        } else {
            payload
        }
    }
}

pub(super) fn error(code: &str) -> Value {
    json!({"ok":false,"code":code})
}
