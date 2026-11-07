use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
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
    pub negotiated_video_stream_format: Option<amaterasu_module_sdk::VideoStreamFormat>,
    pub client_feature_flags: Option<u32>,
    pub negotiated_color: Option<amaterasu_gamestream::sdp::ColorMode>,
    pub negotiated_video: Option<crate::model::NegotiatedVideo>,
    pub url: String,
    pub session_token: String,
    pub udp_discovery: Option<UdpDiscovery>,
    pub local: SocketAddr,
    /// Requested UDP endpoints remain reserved until downstream cleanup is
    /// confirmed. Port zero marks a provider-selected endpoint on this address.
    pub media_binds: Option<crate::model::MediaBinds>,
    pub expires: u64,
    pub next_status: u64,
    pub failure: Option<&'static str>,
    pub cleanup_error: Option<&'static str>,
    pub cleanup_retry_count: u8,
    pub cleanup_retry_at: Option<u64>,
    pub uncertain: Option<Call>,
    pub uncertain_phase: Option<Phase>,
    pub needs_lookup: bool,
    pub stream_state: Option<String>,
    pub lifecycle_target: Option<Value>,
    pub lifecycle_candidate: Option<Value>,
    /// Original producer identity frozen by the coordinator's successful
    /// admission. A subscriber may not substitute another generation on release.
    pub lifecycle_source_target: Option<amaterasu_extension_api::LifecycleTarget>,
    /// Present only after an exact shared-retained terminal receipt. This is
    /// consumer retirement, not evidence that the producer resources released.
    pub lifecycle_retained_source: Option<amaterasu_extension_api::LifecycleTarget>,
    pub lifecycle_prepare_outstanding: bool,
    pub lifecycle_input_required: Option<bool>,
    pub authenticated_disconnect_reason: Option<&'static str>,
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
        let negotiated_video = self.config.negotiate_announce(announce);
        let accepted = negotiated_video.is_some();
        if !accepted {
            let expected = self.config.effective_profile();
            eprintln!(
                "amaterasu rtsp announce prepared-profile mismatch width={} height={} fps={} bitrate={} packet_size={} packet_ms={} refs={} parity={} color={:?} video_encryption={} audio_encryption={} expected_width={} expected_height={} expected_fps={} expected_bitrate={} expected_packet_size={} expected_packet_ms={} expected_refs={} expected_parity={}",
                announce.video.width,
                announce.video.height,
                announce.video.fps,
                announce.video.bitrate_kbps,
                announce.packet_size,
                announce.audio.packet_duration_ms,
                announce.video.max_reference_frames,
                announce.fec.minimum_packets,
                announce.video.color,
                announce.encryption.video,
                announce.encryption.audio,
                expected.width,
                expected.height,
                expected.fps,
                expected.bitrate_kbps,
                expected.packet_size,
                expected.packet_ms,
                expected.max_reference_frames,
                expected.min_parity_shards,
            );
        }
        self.negotiated_encryption = accepted.then_some(crate::model::EncryptionSnapshot {
            video: announce.encryption.video,
            audio: announce.encryption.audio,
        });
        self.negotiated_slices_per_frame = accepted.then_some(announce.video.slices_per_frame);
        // GameStream dynamicRangeMode selects ten-bit encoding; the trusted
        // launch profile independently decides whether those samples are HDR.
        let mut format = announce.video.format;
        if format.bit_depth == 10 && !self.config.effective_profile().hdr {
            format.dynamic_range = amaterasu_module_sdk::DynamicRange::Sdr;
        }
        self.negotiated_video_stream_format = accepted.then_some(format);
        self.client_feature_flags = accepted.then_some(announce.client_feature_flags);
        self.negotiated_color = accepted.then_some(announce.video.color);
        self.negotiated_video = negotiated_video;
        accepted
    }

    pub(super) fn authenticated_start_payload(&self, session: u64) -> Option<Value> {
        let encryption = self.negotiated_encryption?;
        let color = self.negotiated_color?;
        let video = self.negotiated_video?;
        let bitrate = video.bitrate_kbps.checked_mul(1_000)?;
        let mut payload = self.config.arm_with_negotiation(
            session,
            encryption,
            self.negotiated_slices_per_frame?,
            self.negotiated_video_stream_format?,
            self.client_feature_flags?,
        );
        payload["configuration"]["configuration"]["width"] = serde_json::json!(video.width);
        payload["configuration"]["configuration"]["height"] = serde_json::json!(video.height);
        payload["configuration"]["configuration"]["fps"] = serde_json::json!(video.fps);
        payload["configuration"]["configuration"]["bitrate"] = serde_json::json!(bitrate);
        payload["configuration"]["configuration"]["max_reference_frames"] =
            serde_json::json!(video.max_reference_frames);
        if !video.codec.is_default() {
            payload["configuration"]["configuration"]["codec"] = serde_json::json!(video.codec);
        }
        payload["configuration"]["configuration"]["color_mode"] = serde_json::json!(match color {
            amaterasu_gamestream::sdp::ColorMode::Bt601Limited => "bt601_limited",
            amaterasu_gamestream::sdp::ColorMode::Bt601Full => "bt601_full",
            amaterasu_gamestream::sdp::ColorMode::Bt709Limited => "bt709_limited",
            amaterasu_gamestream::sdp::ColorMode::Bt709Full => "bt709_full",
        });
        Some(payload)
    }

    /// Preserve the same validated binding for startup and cleanup-only adoption.
    /// Cleanup never dispatches this payload as a start operation.
    pub(super) fn authenticated_lifecycle_start_payload(&self) -> Option<Value> {
        let target = self.lifecycle_target.as_ref()?;
        let mut payload = self.authenticated_start_payload(self.session?)?;
        payload["configuration"]["lifecycle_target"] = target.clone();
        payload["configuration"]["lifecycle_candidate"] = self.lifecycle_candidate.clone()?;
        if self.config.lifecycle_intent.as_deref() == Some("join") {
            payload["configuration"]["lifecycle_shared_source"] =
                json!(self.lifecycle_source_target.as_ref()?);
        }
        payload["configuration"]["lifecycle_capture_mode"] =
            json!(self.config.lifecycle_capture_mode);
        Some(payload)
    }

    pub(super) fn stop_payload(&self) -> Value {
        let mut payload = json!({"action":"stop","session":self.session});
        if let Some(start) = self.authenticated_lifecycle_start_payload() {
            payload["lifecycle_configuration"] = start["configuration"].clone();
        }
        payload
    }

    pub fn operation(&self) -> String {
        format!("rtsp-{:016x}", self.id)
    }

    pub fn response(&self) -> Value {
        let lifecycle_cleanup_state = self.config.lifecycle_session_target.as_ref().map(|_| {
            if self.lifecycle_prepare_outstanding {
                "prepare_unresolved"
            } else if self.lifecycle_target.is_none() {
                "not_admitted"
            } else if self.cleanup_confirmed && self.lifecycle_retained_source.is_some() {
                "shared_retained"
            } else if self.cleanup_confirmed {
                "retired"
            } else {
                "retirement_pending"
            }
        });
        let mut response = json!({"ok":true,"ticket":self.id,"state":self.state,"local":self.local,
            "rtsp_url":self.url,"session":self.session,"stream_state":self.stream_state,
            "lifecycle_session_target":self.config.lifecycle_session_target,
            "lifecycle_capture_mode":self.config.lifecycle_capture_mode,
            "lifecycle_settings_revision":self.config.lifecycle_settings_revision,
            "lifecycle_intent":self.config.lifecycle_intent,
            "lifecycle_target":self.lifecycle_target,
            "lifecycle_retired":self.lifecycle_target.is_some() && self.cleanup_confirmed && self.lifecycle_retained_source.is_none(),
            "lifecycle_consumer_retired":self.lifecycle_target.is_some() && self.cleanup_confirmed,
            "lifecycle_resources_released":self.lifecycle_target.is_some() && self.cleanup_confirmed && self.lifecycle_retained_source.is_none(),
            "lifecycle_shared_retained":self.cleanup_confirmed && self.lifecycle_retained_source.is_some(),
            "lifecycle_shared_source":self.lifecycle_retained_source,
            "lifecycle_cleanup_state":lifecycle_cleanup_state,
            "authenticated_disconnect_reason":self.authenticated_disconnect_reason,
            "encoder":self.config.capture.encoder,"sw_preset":self.config.sw_preset,"sw_tune":self.config.sw_tune,"min_threads":self.config.min_threads,"qp":self.config.qp,
            "slices_per_frame":self.negotiated_slices_per_frame,
            "nvenc_settings":self.config.nvenc_settings,
            "nvenc_spatial_aq":self.config.nvenc_spatial_aq,
            "history_activity_id":self.config.history_activity_id,
            "display_policy_snapshot":self.config.display_policy_snapshot,
            "display_request":self.config.display_request,
            "display_applied":self.display_applied,
            "failure":self.failure,"cleanup_error":self.cleanup_error});
        if let Some(context) = &self.config.source_capture_context {
            response["source_capture_context"] = json!(context);
        }
        response
    }

    pub fn close(&mut self, reason: Option<&'static str>) {
        if let Some(reason) = reason {
            self.failure.get_or_insert(reason);
        }
        self.state = State::Closing;
    }

    pub fn capabilities(&self) -> Capabilities {
        let effective_profile = self.config.effective_profile();
        let bounds = self.config.negotiation_bounds;
        let offered = effective_profile.offered_codecs();
        Capabilities {
            transport: amaterasu_gamestream::sdp::TransportSupport {
                hevc: offered.contains(&amaterasu_media::VideoCodec::Hevc),
                av1: offered.contains(&amaterasu_media::VideoCodec::Av1),
                stereo_audio: true,
                enet_control: true,
                encrypted_control: true,
                control_v2: true,
                encrypted_remote_input: false,
                encrypted_video: true,
                encrypted_audio: true,
                fec: true,
            },
            supported_colors: if cfg!(target_os = "linux") {
                vec![
                    amaterasu_gamestream::sdp::ColorMode::Bt601Limited,
                    amaterasu_gamestream::sdp::ColorMode::Bt601Full,
                    amaterasu_gamestream::sdp::ColorMode::Bt709Limited,
                    amaterasu_gamestream::sdp::ColorMode::Bt709Full,
                ]
            } else {
                vec![amaterasu_gamestream::sdp::ColorMode::Bt709Limited]
            },
            max_width: bounds.map_or(effective_profile.width, |bounds| bounds.max_width),
            max_height: bounds.map_or(effective_profile.height, |bounds| bounds.max_height),
            max_fps: bounds.map_or(effective_profile.fps, |bounds| bounds.max_fps),
            max_bitrate_kbps: bounds.map_or(effective_profile.bitrate_kbps, |bounds| {
                bounds.max_bitrate_kbps
            }),
            require_video_encryption: effective_profile.encrypt_video,
            require_audio_encryption: effective_profile.encrypt_audio,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Prepare,
    Find,
    LifecyclePrepare,
    LifecycleLookup,
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
