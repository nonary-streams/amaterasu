use super::lifecycle_issues::Cause;
use super::*;
use amaterasu_module_sdk::stream_lifecycle::StreamIssueOutbox;

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

impl Owner for Record {
    /// A ticket is open from admission until it is asked to close. From then
    /// on it only retires what it held.
    fn admission(&self) -> Admission {
        match self.state {
            State::Preparing | State::Ready | State::Arming | State::Playing => Admission::Open,
            State::Closing | State::Closed | State::Failed | State::CleanupFailed => {
                Admission::Ended
            }
        }
    }

    fn retirement(&self) -> Retirement {
        match self.state {
            State::Closed | State::Failed if self.cleanup_confirmed => Retirement::Confirmed,
            State::Closed | State::Failed => Retirement::Abandoned,
            State::CleanupFailed if self.cleanup_retry_count >= MAX_CLEANUP_RETRIES => {
                Retirement::Abandoned
            }
            State::CleanupFailed => Retirement::Unconfirmed,
            State::Preparing | State::Ready | State::Arming | State::Playing | State::Closing => {
                Retirement::Pending
            }
        }
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
    pub capture_timing: bool,
    pub adjustments: Vec<amaterasu_gamestream::sdp::StreamAdjustment>,
    pub negotiated_color: Option<amaterasu_gamestream::sdp::ColorMode>,
    pub negotiated_video: Option<crate::model::NegotiatedVideo>,
    pub extension_offer: Option<amaterasu_contracts::media_profile::VideoProfile>,
    pub extension_announce: Option<
        amaterasu_gamestream::sdp::Announce<amaterasu_gamestream::sdp::NegotiatedVideoFormat>,
    >,
    pub url: String,
    pub session_token: String,
    pub udp_discovery: Option<UdpDiscovery>,
    pub local: SocketAddr,
    /// Requested UDP endpoints remain reserved until downstream cleanup is
    /// confirmed. Port zero marks a provider-selected endpoint on this address.
    pub media_binds: Option<crate::model::MediaBinds>,
    pub expires: u64,
    pub next_status: u64,
    pub observation_stale_at: u64,
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
    /// When this ticket was first seen ended. A later ticket prefers the
    /// media ports of the ticket that ended longest ago.
    pub ended_at: Option<u64>,
    /// Failures this ticket recorded for the launch owner's lifecycle store.
    pub issues: StreamIssueOutbox,
}

impl Record {
    pub(super) fn accept_extension_announce(
        &mut self,
        announce: &amaterasu_gamestream::sdp::Announce<
            amaterasu_gamestream::sdp::NegotiatedVideoFormat,
        >,
    ) -> bool {
        self.extension_announce = None;
        let amaterasu_gamestream::sdp::NegotiatedVideoFormat::Extension(profile) =
            &announce.video.format
        else {
            return false;
        };
        if self.extension_offer.as_ref() != Some(profile)
            || !self.config.accepts_extension_announce(announce)
        {
            let expected = self.config.effective_profile();
            eprintln!(
                "amaterasu rtsp extension announce mismatch profile_match={} width={}/{} height={}/{} fps={}/{} bitrate={}/{} refs={}/{} packet_size={}/{} packet_ms={}/{} color={:?}",
                self.extension_offer.as_ref() == Some(profile),
                announce.video.width,
                expected.width,
                announce.video.height,
                expected.height,
                announce.video.fps,
                expected.fps,
                announce.video.bitrate_kbps,
                expected.bitrate_kbps,
                announce.video.max_reference_frames,
                expected.max_reference_frames,
                announce.packet_size,
                expected.packet_size,
                announce.audio.packet_duration_ms,
                expected.packet_ms,
                announce.video.color,
            );
            return false;
        }
        self.extension_announce = Some(announce.clone());
        true
    }
    pub(super) fn begin_arm(&mut self, now: u64) -> u64 {
        self.state = State::Arming;
        self.expires = now.saturating_add(u64::from(self.config.ping_timeout_ms));
        self.expires
    }

    pub(super) fn accept_announce(
        &mut self,
        announce: &amaterasu_gamestream::sdp::Announce,
    ) -> bool {
        let mut negotiated_video = self.config.negotiate_announce(announce);
        self.adjustments = if negotiated_video.is_some() {
            announce.adjustments.clone()
        } else {
            Vec::new()
        };
        if let (Some(video), Some(budget)) = (
            &mut negotiated_video,
            announce
                .video
                .configured_bitrate_kbps
                .filter(|value| *value > 0),
        ) {
            let profile = self.config.effective_profile();
            let after_fec = if profile.fec_percentage <= 80 {
                u64::from(budget) * u64::from(100 - profile.fec_percentage) / 100
            } else {
                u64::from(budget)
            };
            let audio = u64::from(profile.audio_bitrate)
                .div_ceil(1000)
                .min(after_fec / 5);
            let adjusted = after_fec
                .saturating_sub(audio)
                .saturating_sub(500.min(after_fec / 10))
                .clamp(100, u64::from(u32::MAX)) as u32;
            let effective = video.bitrate_kbps.min(adjusted);
            if effective != video.bitrate_kbps {
                self.adjustments
                    .push(amaterasu_gamestream::sdp::StreamAdjustment::new(
                        "bandwidth_adjusted",
                        video.bitrate_kbps,
                        effective,
                    ));
                video.bitrate_kbps = effective;
            }
        }
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
        self.capture_timing = accepted && announce.capture_timing;
        self.negotiated_color = accepted.then_some(announce.video.color);
        self.negotiated_video = negotiated_video;
        accepted
    }

    pub(super) fn authenticated_start_payload(&self, session: u64) -> Option<Value> {
        if let Some(announce) = &self.extension_announce {
            let amaterasu_gamestream::sdp::NegotiatedVideoFormat::Extension(profile) =
                &announce.video.format
            else {
                return None;
            };
            let mut payload = self.config.arm(session);
            if announce.capture_timing {
                payload["configuration"]["capture_timing"] = json!(true);
            }
            payload["configuration"]["encrypt_video"] = json!(announce.encryption.video);
            payload["configuration"]["encrypt_audio"] = json!(announce.encryption.audio);
            payload["configuration"]["client_feature_flags"] = json!(announce.client_feature_flags);
            payload["configuration"]["configuration"] = self.config.extension_configuration({
                let format = profile.media_format.unwrap_or_default();
                (
                    format.bit_depth,
                    format.dynamic_range == amaterasu_module_sdk::DynamicRange::Hdr,
                )
            });
            payload["configuration"]["configuration"]["bitrate"] =
                json!(announce.video.bitrate_kbps.checked_mul(1_000)?);
            payload["configuration"]["configuration"]["extension_profile"] = json!(profile);
            payload["configuration"]["configuration"]["color_mode"] = json!(announce.video.color);
            return Some(payload);
        }
        let encryption = self.negotiated_encryption?;
        let color = self.negotiated_color?;
        let video = self.negotiated_video?;
        let bitrate = video.bitrate_kbps.checked_mul(1_000)?;
        let mut payload = self.config.arm_with_negotiation(
            session,
            encryption,
            self.negotiated_slices_per_frame?,
            self.negotiated_video_stream_format?,
            self.client_feature_flags? & amaterasu_gamestream::sdp::KNOWN_CLIENT_FEATURE_FLAGS,
        );
        if self.capture_timing {
            payload["configuration"]["capture_timing"] = json!(true);
        }
        payload["configuration"]["configuration"]["width"] = serde_json::json!(video.width);
        payload["configuration"]["configuration"]["height"] = serde_json::json!(video.height);
        payload["configuration"]["configuration"]["fps"] = serde_json::json!(video.fps);
        if let Some(fps_milli) = video.fps_milli {
            payload["configuration"]["configuration"]["fps_milli"] = json!(fps_milli);
        }
        payload["configuration"]["configuration"]["bitrate"] = serde_json::json!(bitrate);
        payload["configuration"]["configuration"]["max_reference_frames"] =
            serde_json::json!(video.max_reference_frames);
        if !video.codec.is_default() {
            payload["configuration"]["configuration"]["codec"] = serde_json::json!(video.codec);
        }
        payload["configuration"]["configuration"]["color_mode"] = json!(color);
        if !self.adjustments.is_empty() {
            payload["configuration"]["stream_adjustments"] = json!(self.adjustments);
        }
        Some(payload)
    }

    /// Lifecycle admission needs capture geometry and transfer, independently of
    /// the selected encoder's codec identity. Keep opaque profiles opaque.
    pub(super) fn negotiated_video_mode(&self) -> Option<amaterasu_extension_api::VideoMode> {
        if let Some(announce) = &self.extension_announce {
            if !matches!(
                announce.video.format,
                amaterasu_gamestream::sdp::NegotiatedVideoFormat::Extension(_)
            ) {
                return None;
            }
            return Some(amaterasu_extension_api::VideoMode {
                width: announce.video.width,
                height: announce.video.height,
                refresh_millihz: announce.video.fps * 1000,
                hdr: self.config.profile.hdr,
            });
        }
        let video = self.negotiated_video?;
        let format = self.negotiated_video_stream_format?;
        Some(amaterasu_extension_api::VideoMode {
            width: video.width,
            height: video.height,
            refresh_millihz: video.fps * 1000,
            hdr: format.dynamic_range == amaterasu_module_sdk::DynamicRange::Hdr,
        })
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
        let mut warnings = serde_json::to_value(&self.adjustments).unwrap_or_else(|_| json!([]));
        let ignored = self.client_feature_flags.unwrap_or(0)
            & !amaterasu_gamestream::sdp::KNOWN_CLIENT_FEATURE_FLAGS;
        if ignored != 0 {
            warnings.as_array_mut().expect("warning array").push(json!({
                "code":"unsupported_optional_client_features",
                "message":"Some optional client features are unavailable; streaming can continue.",
                "ignored_feature_flags":ignored
            }));
        }
        response["warnings"] = warnings;
        if let Some(context) = &self.config.source_capture_context {
            response["source_capture_context"] = json!(context);
        }
        response
    }

    /// Close this ticket. The first reason given is its failure, reported
    /// with the dependency the reason itself names, if any.
    #[track_caller]
    pub fn close(&mut self, reason: Option<&'static str>) {
        let cause = reason.and_then(Cause::of_close);
        self.close_with_cause(reason, cause);
    }

    /// Close this ticket; `cause` is the dependency whose failure ended it.
    #[track_caller]
    pub fn close_with_cause(&mut self, reason: Option<&'static str>, cause: Option<Cause<'_>>) {
        if let Some(reason) = reason {
            if self.state == State::Playing {
                eprintln!("amaterasu rtsp closing playing stream reason={reason}");
            }
            if self.failure.is_none() {
                self.failure = Some(reason);
                self.report_failure(reason, cause);
            }
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
    Observe,
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
