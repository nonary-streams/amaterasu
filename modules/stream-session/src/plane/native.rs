//! Native calls borrow copied headers only synchronously. Bindings are retained
//! until every pending call and callback has returned, then released by the SDK.
use super::*;
#[cfg(test)]
pub(crate) mod metadata_tests;
mod ordered_audio;
mod planned;
use amaterasu_gamestream::control::{RUMBLE_MESSAGE_TYPE, RumbleFeedback, encode_rumble_feedback};
use amaterasu_media::{CONTROL_RELIABLE, ControlMessage};
use amaterasu_module_sdk::{NativeBinding, abi::*};
use ordered_audio::OrderedBindings;
use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
};

fn profile_name(
    profile: &amaterasu_contracts::media_profile::VideoProfile,
) -> PlaneResult<[u8; 32]> {
    if !profile.is_valid() {
        return Err("invalid_video_profile");
    }
    let mut name = [0; 32];
    name[..profile.encoding_name.len()].copy_from_slice(profile.encoding_name.as_bytes());
    Ok(name)
}

/// Probe the selected downstream services before exposing an extension offer.
pub(super) fn probe_chain(
    runtime: &RuntimeClient,
    route: &crate::model::ExtensionRoute,
    transport_stream: u64,
    config: &crate::model::PrepareConfig,
) -> PlaneResult<VideoFramingLimits> {
    let _video = unsafe {
        if route.metadata_schema.is_some() {
            amaterasu_module_sdk::video_framing::bind_video_metadata(
                runtime,
                &route.video_unit_service,
            )
        } else {
            amaterasu_module_sdk::video_framing::bind_video(runtime, &route.video_unit_service)
        }
    }
    .map_err(|_| "native_binding_failed")?;
    let framer = unsafe {
        if route.metadata_schema.is_some() {
            amaterasu_module_sdk::video_framing::bind_with_metadata(runtime, &route.framer_service)
        } else {
            amaterasu_module_sdk::video_framing::bind_with_probe(runtime, &route.framer_service)
        }
    }
    .map_err(|_| "native_binding_failed")?;
    let transport = unsafe {
        if route.metadata_schema.is_some() {
            amaterasu_module_sdk::video_framing::bind_plan_transport_metadata(
                runtime,
                &route.plan_transport_service,
            )
        } else {
            amaterasu_module_sdk::video_framing::bind_plan_transport_v2(
                runtime,
                &route.plan_transport_service,
            )
        }
    }
    .map_err(|_| "native_binding_failed")?;
    let id = |format: amaterasu_contracts::media_profile::FormatId| EncoderFormatId {
        namespace_id: format.namespace_id,
        value: format.value,
        schema: format.schema,
    };
    let request = VideoPlanSelectionV2 {
        base: VideoPlanSelection {
            size: size_of::<VideoPlanSelectionV2>() as u32,
            version: 2,
            stream: 1,
            generation: 1,
            offer_id: u64::from(route.profile.wire_selector),
            source_id: route.source_id.max(1),
            config_generation: 1,
            codec: id(route.profile.codec),
            framing: id(route.profile.framing),
            clock_epoch: [1; 16],
            clock_origin_us: host_monotonic_micros().max(1),
            metadata_required: 0,
            reserved: 0,
        },
        payload_type: u32::from(route.profile.payload_type),
        encoding_name_len: route.profile.encoding_name.len() as u32,
        encoding_name: profile_name(&route.profile)?,
    };
    let api = unsafe { transport.table::<NegotiatedVideoPlanTransportApiV2>() }
        .map_err(|_| "native_layout_mismatch")?;
    let mut limits = VideoFramingLimits::default();
    // SAFETY: bindings and input/output descriptors remain live for each call.
    let metadata_schema = route.metadata_schema.map(|schema| FrameMetadataSchemaId {
        namespace_id: schema.namespace_id,
        value: schema.value,
        schema: schema.schema,
    });
    let status = if let Some(schema) = metadata_schema {
        let mut base = request;
        base.base.size = size_of::<VideoPlanSelectionV3>() as u32;
        base.base.version = 3;
        base.base.metadata_required = 1;
        let metadata_request = VideoPlanSelectionV3 {
            base,
            schema,
            carriage_owner: FRAME_METADATA_OWNER_FRAMER,
            reserved: 0,
        };
        let api = unsafe { transport.table::<NegotiatedVideoPlanTransportApiV2_1>() }
            .map_err(|_| "native_layout_mismatch")?;
        unsafe {
            (api.probe_metadata)(
                transport.context(),
                transport_stream,
                &metadata_request,
                &mut limits,
            )
        }
    } else {
        unsafe { (api.probe)(transport.context(), transport_stream, &request, &mut limits) }
    };
    if status != STATUS_OK {
        return Err("video_plan_probe_failed");
    }
    let expected_crypto = if config.encrypt_video {
        amaterasu_gamestream::crypto::VIDEO_PREFIX_BYTES as u32
    } else {
        0
    };
    if limits.size as usize != size_of::<VideoFramingLimits>()
        || limits.version != VIDEO_FRAMING_LIMITS_VERSION
        || limits.packet_size as usize != config.video.packet_size
        || limits.crypto_overhead != expected_crypto
        || limits.network_overhead != if config.peer_ip.is_ipv6() { 48 } else { 28 }
        || limits.min_fec_percentage != u32::from(config.video.fec_percentage)
        || limits.min_parity_shards != u32::from(config.video.min_parity_shards)
        || limits.max_fec_blocks != u32::from(config.video.max_fec_blocks)
        || limits.max_fec_percentage < limits.min_fec_percentage
        || limits.max_fec_percentage > 255
        || limits.fec_policy_flags != 0
        || limits.reserved != 0
        || limits.max_datagrams == 0
        || limits.max_datagrams > VIDEO_FRAME_MAX_DATAGRAMS
        || limits.max_queue_bytes == 0
        || limits.max_queue_bytes > 8 * 1024 * 1024
        || limits.max_wire_bytes == 0
        || limits.allowed_extensions & !(VIDEO_FRAME_RECORD_START | VIDEO_FRAME_CRITICAL_PREFIX)
            != 0
    {
        return Err("video_plan_policy_mismatch");
    }
    let api =
        unsafe { framer.table::<VideoFramingApiV2_1>() }.map_err(|_| "native_layout_mismatch")?;
    if unsafe {
        (api.probe)(
            framer.context(),
            &request.base.codec,
            &request.base.framing,
            &limits,
        )
    } != STATUS_OK
    {
        return Err("video_framer_probe_failed");
    }
    if let Some(schema) = metadata_schema {
        let api = unsafe { framer.table::<VideoFramingApiV2_2>() }
            .map_err(|_| "native_layout_mismatch")?;
        if unsafe {
            (api.probe_metadata)(
                framer.context(),
                &request.base.codec,
                &request.base.framing,
                &schema,
                &limits,
            )
        } != STATUS_OK
        {
            return Err("video_metadata_framer_probe_failed");
        }
    }
    Ok(limits)
}

/// Probe an encoder offer, ordered capture interface, and actual downstream
/// audio transport before exposing any codec marker to the client.
pub(super) fn probe_ordered_audio_chain(
    runtime: &RuntimeClient,
    route: &crate::model::AudioExtensionRoute,
    encoder_stream: u64,
    media_stream: u64,
    config: &crate::model::PrepareConfig,
    epoch: [u8; 16],
) -> PlaneResult<amaterasu_module_sdk::audio::OrderedAudioCandidate> {
    use amaterasu_module_sdk::audio::{self, AudioFormat};
    // SAFETY: trusted startup composition names the typed providers, and these
    // temporary bindings live through every synchronous probe below.
    let encoder = unsafe { audio::bind_ordered_encoder(runtime, &route.encoder_service) }
        .map_err(|_| "ordered_encoder_unavailable")?;
    let _capture = unsafe { audio::bind_ordered_capture(runtime, &route.capture_service) }
        .map_err(|_| "ordered_capture_unavailable")?;
    let transport = unsafe { audio::bind_ordered_transport(runtime, &route.transport_service) }
        .map_err(|_| "ordered_transport_unavailable")?;
    let (codec, framing) = match route.codec.as_str() {
        "AC3" => (AUDIO_CODEC_AC3, AUDIO_FRAMING_AC3_SYNCFRAME),
        "EAC3" => (AUDIO_CODEC_EAC3, AUDIO_FRAMING_EAC3_SYNCFRAME),
        _ => return Err("unsupported_audio_codec"),
    };
    let lanes = match (config.audio.channels, config.audio.channel_mask) {
        (2, 3) => [1, 2, 0, 0, 0, 0, 0, 0],
        (6, 63) => [1, 2, 4, 8, 16, 32, 0, 0],
        _ => return Err("unsupported_audio_layout"),
    };
    let layout = AudioChannelLayoutV2 {
        size: size_of::<AudioChannelLayoutV2>() as u32,
        version: AUDIO_ORDERED_VERSION,
        channels: config.audio.channels,
        reserved: 0,
        mask: u64::from(config.audio.channel_mask),
        lanes,
    };
    let format = AudioFormat {
        namespace: [0; 16],
        codec,
        framing,
        sample_rate: 48_000,
        channels: config.audio.channels,
        channel_mask: u64::from(config.audio.channel_mask),
        frame_samples: 1536,
        config: Vec::new(),
    };
    // SAFETY: retained bindings and table implement their typed native contracts.
    let api = unsafe { encoder.table::<OrderedAudioEncoderApiV2>() }
        .map_err(|_| "native_layout_mismatch")?;
    let candidate = unsafe { audio::probe_ordered(api, encoder_stream, &[(format, layout)]) }
        .map_err(|error| match error {
            audio::AudioNegotiationError::Provider(MEDIA_BUSY | MEDIA_PENDING) => {
                "ordered_probe_busy"
            }
            _ => "ordered_encoder_probe_failed",
        })?;
    let selection = candidate.transport_selection(epoch);
    let api = unsafe { transport.table::<OrderedAudioTransportApiV2>() }
        .map_err(|_| "native_layout_mismatch")?;
    // SAFETY: selection and nested config remain live for this synchronous probe.
    match unsafe { (api.probe)(transport.context(), media_stream, &selection) } {
        STATUS_OK => {}
        MEDIA_BUSY | MEDIA_PENDING => return Err("ordered_probe_busy"),
        _ => return Err("ordered_transport_probe_failed"),
    }
    Ok(candidate)
}

pub(super) struct NativeBackend {
    video: Option<NativeBinding>,
    video_unit: Option<NativeBinding>,
    media_unit: Option<NativeBinding>,
    framer: Option<NativeBinding>,
    plan_transport: Option<NativeBinding>,
    framing_limits: Option<VideoFramingLimits>,
    pending_video_metadata: Option<PendingVideoMetadata>,
    capture: Option<NativeBinding>,
    encoder: Option<NativeBinding>,
    media: NativeBinding,
    control: NativeBinding,
    handles: Handles,
    last_hdr: Option<HdrMetadata>,
    unit_transport_draining: bool,
    ordered_audio: Option<OrderedBindings>,
}

struct PendingVideoMetadata {
    access_unit: u64,
    bytes: Vec<u8>,
    plan: Option<planned::OwnedPlan>,
    framed: bool,
    transport_admitted: bool,
}

impl NativeBackend {
    fn submit_metadata_unit(&mut self, unit: &NegotiatedVideoUnit) -> PlaneResult<bool> {
        let pending = self
            .pending_video_metadata
            .as_mut()
            .ok_or("video_metadata_missing")?;
        if pending.access_unit != unit.access_unit_id {
            return Err("video_metadata_mismatch");
        }
        let video = self
            .video_unit
            .as_ref()
            .ok_or("video_metadata_unavailable")?;
        let framer = self.framer.as_ref().ok_or("video_metadata_unavailable")?;
        let transport = self
            .plan_transport
            .as_ref()
            .ok_or("video_metadata_unavailable")?;
        let limits = self
            .framing_limits
            .as_ref()
            .ok_or("video_metadata_unavailable")?;
        let video_api = unsafe { video.table::<NegotiatedVideoApiV1_1>() }
            .map_err(|_| "native_layout_mismatch")?;
        if pending.plan.is_none() {
            let Some(plan) = planned::metadata_plan(framer, limits, unit, &pending.bytes)? else {
                return Ok(false);
            };
            pending.plan = Some(plan);
        }
        if !pending.framed {
            // SAFETY: the selected Video binding owns the live reserved unit.
            if !accepted(
                unsafe {
                    (video_api.framed_metadata)(video.context(), unit.stream, unit.access_unit_id)
                },
                "video_metadata_carriage_failed",
            )? {
                return Ok(false);
            }
            pending.framed = true;
        }
        if !pending.transport_admitted {
            // A fresh check precedes every attempt; BUSY retains the accepted plan.
            if !accepted(
                unsafe {
                    (video_api.check_metadata)(video.context(), unit.stream, unit.access_unit_id)
                },
                "video_metadata_stale",
            )? {
                return Ok(false);
            }
            let api = unsafe { transport.table::<NegotiatedVideoPlanTransportApiV2>() }
                .map_err(|_| "native_layout_mismatch")?;
            let plan = pending.plan.as_ref().ok_or("video_metadata_plan_missing")?;
            // SAFETY: unit and cached plan remain owned through this synchronous call.
            if !accepted(
                unsafe {
                    (api.submit)(
                        transport.context(),
                        self.handles.media_transport,
                        unit,
                        &plan.header(),
                    )
                },
                "native_video_plan_submit_failed",
            )? {
                return Ok(false);
            }
            pending.transport_admitted = true;
        }
        // Retire only after admission. If retirement is busy, do not submit again.
        if !accepted(
            unsafe {
                (video_api.retire_metadata)(video.context(), unit.stream, unit.access_unit_id)
            },
            "video_metadata_retirement_failed",
        )? {
            return Ok(false);
        }
        self.pending_video_metadata = None;
        Ok(true)
    }

    fn update_hdr(&mut self) -> PlaneResult<()> {
        if !self.handles.hdr {
            return Ok(());
        }
        // SAFETY: header is inspected before the additive video v1.2 tail.
        let video = self
            .video
            .as_ref()
            .ok_or("video_hdr_metadata_unsupported")?;
        let prefix = unsafe { video.table::<VideoApi>() }.map_err(|_| "native_layout_mismatch")?;
        if prefix.base.header.minor < 2 {
            return Err("video_hdr_metadata_unsupported");
        }
        let api = unsafe { video.table::<VideoApiV1_2>() }.map_err(|_| "native_layout_mismatch")?;
        let mut metadata = HdrMetadata::default();
        match unsafe { (api.hdr_metadata)(api.base.context, self.handles.video, &mut metadata) } {
            MEDIA_PENDING | MEDIA_BUSY => return Ok(()),
            STATUS_OK if metadata.enabled == 1 && metadata.reserved == 0 => {}
            _ => return Err("video_hdr_metadata_unavailable"),
        }
        if self.last_hdr == Some(metadata) {
            return Ok(());
        }
        let mut payload = vec![1u8];
        for field in metadata.fields {
            payload.extend_from_slice(&field.to_le_bytes());
        }
        payload.extend_from_slice(&0u16.to_le_bytes()); // optional max full-frame luminance
        let message = HostFeedbackMessage {
            message_type: 0x010e,
            channel: 0,
            delivery: HostFeedbackDelivery::Reliable,
            payload,
        };
        if self.send_feedback(&message)? {
            self.last_hdr = Some(metadata);
        }
        Ok(())
    }

    pub(super) fn new(
        runtime: &RuntimeClient,
        handles: Handles,
        providers: &crate::providers::ProviderServices,
    ) -> PlaneResult<Self> {
        let video = unsafe { amaterasu_module_sdk::video::bind(runtime, &providers.video) }
            .map_err(|_| "native_binding_failed")?;
        Self::with_video(runtime, handles, providers, Some(video), None, None)
    }

    /// Caller has already authenticated the client selection and prepared chain.
    #[allow(dead_code)]
    pub(super) fn new_negotiated(
        runtime: &RuntimeClient,
        handles: Handles,
        providers: &crate::providers::ProviderServices,
        video_service: &str,
        transport_service: &str,
    ) -> PlaneResult<Self> {
        if handles.hdr {
            return Err("negotiated_hdr_metadata_unsupported");
        }
        let video =
            unsafe { amaterasu_module_sdk::video_framing::bind_video(runtime, video_service) }
                .map_err(|_| "native_binding_failed")?;
        let transport = unsafe {
            amaterasu_module_sdk::video_framing::bind_transport(runtime, transport_service)
        }
        .map_err(|_| "native_binding_failed")?;
        Self::with_video(
            runtime,
            handles,
            providers,
            None,
            Some(video),
            Some(transport),
        )
    }

    /// Binds the selected video, framer and plan transport, independently
    /// probes both downstream interfaces, then commits transport selection.
    pub(super) fn new_planned(
        runtime: &RuntimeClient,
        handles: Handles,
        providers: &crate::providers::ProviderServices,
        route: &NegotiatedRoute,
    ) -> PlaneResult<Self> {
        let selection = &route.selection;
        let clock_origin_us = route.clock_origin_us;
        if handles.hdr
            || !selection.valid()
            || selection.offer_id != u64::from(route.profile.wire_selector)
            || (
                selection.codec.namespace_id,
                selection.codec.value,
                selection.codec.schema,
            ) != (
                route.profile.codec.namespace_id,
                route.profile.codec.value,
                route.profile.codec.schema,
            )
            || (
                selection.framing.namespace_id,
                selection.framing.value,
                selection.framing.schema,
            ) != (
                route.profile.framing.namespace_id,
                route.profile.framing.value,
                route.profile.framing.schema,
            )
            || selection.stream != handles.video
            || clock_origin_us == 0
        {
            return Err("negotiated_video_selection_invalid");
        }
        let video = unsafe {
            if selection.metadata_required {
                amaterasu_module_sdk::video_framing::bind_video_metadata(
                    runtime,
                    &route.video_service,
                )
            } else {
                amaterasu_module_sdk::video_framing::bind_video(runtime, &route.video_service)
            }
        }
        .map_err(|_| "native_binding_failed")?;
        let framer = unsafe {
            if selection.metadata_required {
                amaterasu_module_sdk::video_framing::bind_with_metadata(
                    runtime,
                    &route.framer_service,
                )
            } else {
                amaterasu_module_sdk::video_framing::bind_with_probe(runtime, &route.framer_service)
            }
        }
        .map_err(|_| "native_binding_failed")?;
        let transport = unsafe {
            if selection.metadata_required {
                amaterasu_module_sdk::video_framing::bind_plan_transport_metadata(
                    runtime,
                    &route.transport_service,
                )
            } else {
                amaterasu_module_sdk::video_framing::bind_plan_transport_v2(
                    runtime,
                    &route.transport_service,
                )
            }
        }
        .map_err(|_| "native_binding_failed")?;
        let mut backend = Self::with_video(runtime, handles, providers, None, Some(video), None)?;
        let request = VideoPlanSelectionV2 {
            base: VideoPlanSelection {
                size: size_of::<VideoPlanSelectionV2>() as u32,
                version: 2,
                stream: selection.stream,
                generation: selection.generation,
                offer_id: selection.offer_id,
                source_id: selection.source_id,
                config_generation: selection.config_generation,
                codec: selection.codec,
                framing: selection.framing,
                clock_epoch: selection.clock_epoch,
                clock_origin_us,
                metadata_required: 0,
                reserved: 0,
            },
            payload_type: u32::from(route.profile.payload_type),
            encoding_name_len: route.profile.encoding_name.len() as u32,
            encoding_name: profile_name(&route.profile)?,
        };
        let api = unsafe { transport.table::<NegotiatedVideoPlanTransportApiV2>() }
            .map_err(|_| "native_layout_mismatch")?;
        let mut probe_limits = VideoFramingLimits::default();
        // SAFETY: bindings and stack descriptors remain valid during both calls.
        let metadata_request = selection.metadata_schema.map(|schema| {
            let mut base = request;
            base.base.size = size_of::<VideoPlanSelectionV3>() as u32;
            base.base.version = 3;
            base.base.metadata_required = 1;
            VideoPlanSelectionV3 {
                base,
                schema,
                carriage_owner: FRAME_METADATA_OWNER_FRAMER,
                reserved: 0,
            }
        });
        let probe_status = if let Some(request) = &metadata_request {
            let api = unsafe { transport.table::<NegotiatedVideoPlanTransportApiV2_1>() }
                .map_err(|_| "native_layout_mismatch")?;
            unsafe {
                (api.probe_metadata)(
                    transport.context(),
                    handles.media_transport,
                    request,
                    &mut probe_limits,
                )
            }
        } else {
            unsafe {
                (api.probe)(
                    transport.context(),
                    handles.media_transport,
                    &request,
                    &mut probe_limits,
                )
            }
        };
        if probe_status != STATUS_OK {
            return Err("video_plan_probe_failed");
        }
        if probe_limits != route.limits {
            return Err("video_plan_policy_changed");
        }
        let framer_api = unsafe { framer.table::<VideoFramingApiV2_1>() }
            .map_err(|_| "native_layout_mismatch")?;
        if unsafe {
            (framer_api.probe)(
                framer.context(),
                &selection.codec,
                &selection.framing,
                &probe_limits,
            )
        } != STATUS_OK
        {
            return Err("video_framer_probe_failed");
        }
        if let Some(schema) = selection.metadata_schema {
            let api = unsafe { framer.table::<VideoFramingApiV2_2>() }
                .map_err(|_| "native_layout_mismatch")?;
            if unsafe {
                (api.probe_metadata)(
                    framer.context(),
                    &selection.codec,
                    &selection.framing,
                    &schema,
                    &probe_limits,
                )
            } != STATUS_OK
            {
                return Err("video_metadata_framer_probe_failed");
            }
        }
        let mut selected_limits = VideoFramingLimits::default();
        let prepare_status = if let Some(request) = &metadata_request {
            let api = unsafe { transport.table::<NegotiatedVideoPlanTransportApiV2_1>() }
                .map_err(|_| "native_layout_mismatch")?;
            unsafe {
                (api.prepare_metadata)(
                    transport.context(),
                    handles.media_transport,
                    request,
                    &mut selected_limits,
                )
            }
        } else {
            unsafe {
                (api.prepare)(
                    transport.context(),
                    handles.media_transport,
                    &request,
                    &mut selected_limits,
                )
            }
        };
        if prepare_status != STATUS_OK {
            return Err("video_plan_prepare_failed");
        }
        if selected_limits != probe_limits {
            return Err("video_plan_policy_changed");
        }
        backend.framer = Some(framer);
        backend.plan_transport = Some(transport);
        backend.framing_limits = Some(selected_limits);
        Ok(backend)
    }

    fn with_video(
        runtime: &RuntimeClient,
        handles: Handles,
        providers: &crate::providers::ProviderServices,
        video: Option<NativeBinding>,
        video_unit: Option<NativeBinding>,
        media_unit: Option<NativeBinding>,
    ) -> PlaneResult<Self> {
        let (capture, encoder) = match (
            handles.stream_audio,
            handles.audio_capture,
            handles.audio_encoder,
        ) {
            (true, Some(_), Some(_)) => (
                Some(
                    unsafe {
                        amaterasu_module_sdk::audio::bind_capture(runtime, &providers.audio_capture)
                    }
                    .map_err(|_| "native_binding_failed")?,
                ),
                Some(
                    unsafe {
                        amaterasu_module_sdk::audio::bind_encoder(runtime, &providers.audio_encoder)
                    }
                    .map_err(|_| "native_binding_failed")?,
                ),
            ),
            (false, None, None) => (None, None),
            _ => return Err("invalid_audio_handles"),
        };
        Ok(Self {
            video,
            video_unit,
            media_unit,
            framer: None,
            plan_transport: None,
            framing_limits: None,
            pending_video_metadata: None,
            capture,
            encoder,
            media: unsafe {
                amaterasu_module_sdk::transport::bind_media(runtime, &providers.media_transport)
            }
            .map_err(|_| "native_binding_failed")?,
            control: unsafe {
                amaterasu_module_sdk::transport::bind_control(runtime, &providers.control_transport)
            }
            .map_err(|_| "native_binding_failed")?,
            handles,
            last_hdr: None,
            unit_transport_draining: false,
            ordered_audio: None,
        })
    }
}

impl Backend for NativeBackend {
    fn prepare_ordered_audio(&mut self) -> PlaneResult<bool> {
        self.prepare_ordered_audio_impl()
    }

    fn ordered_pcm(&mut self) -> PlaneResult<Option<OrderedPcmPacket>> {
        self.ordered_pcm_impl()
    }

    fn ordered_audio(&mut self) -> PlaneResult<Option<OrderedAudioPacket>> {
        self.ordered_audio_impl()
    }

    fn submit_ordered_pcm(&mut self, frame: &OrderedAudioFrameV2) -> PlaneResult<bool> {
        self.submit_ordered_pcm_impl(frame)
    }

    fn submit_ordered_audio(&mut self, packet: &NegotiatedAudioPacket) -> PlaneResult<bool> {
        self.submit_ordered_audio_impl(packet)
    }

    fn drain_ordered_encoder(&mut self) -> PlaneResult<bool> {
        self.drain_ordered_encoder_impl()
    }
    fn desktop_geometry(&mut self) -> PlaneResult<Option<CaptureGeometry>> {
        self.update_hdr()?;
        // SAFETY: the retained binding was validated for the v1.0 prefix. Only
        // inspect the optional v1.1 tail when both version and size advertise it.
        let Some(video) = &self.video else {
            return Ok(None);
        };
        let prefix = unsafe { video.table::<VideoApi>() }.map_err(|_| "native_layout_mismatch")?;
        if prefix.base.header.minor < 1 {
            return Ok(None);
        }
        let api = unsafe { video.table::<VideoApiV1_1>() }.map_err(|_| "native_layout_mismatch")?;
        let mut geometry = std::mem::MaybeUninit::uninit();
        // SAFETY: STATUS_OK initializes this output slot before returning.
        match unsafe { (api.geometry)(api.base.context, self.handles.video, geometry.as_mut_ptr()) }
        {
            STATUS_OK => {
                let geometry = unsafe { geometry.assume_init() };
                geometry
                    .validate()
                    .map_err(|_| "invalid_capture_geometry")?;
                Ok(Some(geometry))
            }
            MEDIA_BUSY | MEDIA_PENDING | MEDIA_UNSUPPORTED | MEDIA_CLOSED => Ok(None),
            _ => Err("video_geometry_failed"),
        }
    }

    fn send_rumble(&mut self, command: RumbleCommand) -> PlaneResult<bool> {
        let payload = encode_rumble_feedback(RumbleFeedback {
            slot: command.slot,
            low_frequency: command.low_frequency,
            high_frequency: command.high_frequency,
        })
        .map_err(|_| "invalid_rumble_feedback")?;
        let message = ControlMessage {
            message_type: u32::from(RUMBLE_MESSAGE_TYPE),
            channel: 0,
            delivery: CONTROL_RELIABLE,
            reserved: 0,
            data: payload.as_ptr(),
            len: payload.len(),
            sequence: 0,
        };
        // SAFETY: the authenticated control provider copies this bounded stack
        // payload synchronously and owns encryption/sequence assignment.
        let api = unsafe { self.control.table::<ControlTransportApi>() }
            .map_err(|_| "native_layout_mismatch")?;
        let status = unsafe {
            (api.send)(
                self.control.context(),
                self.handles.control_transport,
                &message,
            )
        };
        accepted(status, "native_rumble_failed")
    }

    fn control(&mut self) -> PlaneResult<Option<ControlRecord>> {
        let mut output = Delivery::default();
        // SAFETY: the owned binding and stack sink remain live for one callback.
        let api = unsafe { self.control.table::<ControlTransportApi>() }
            .map_err(|_| "native_layout_mismatch")?;
        let status = unsafe {
            (api.receive)(
                self.control.context(),
                self.handles.control_transport,
                ControlMessageSink {
                    context: (&mut output as *mut Delivery<ControlRecord>).cast(),
                    write: Some(copy_control),
                },
            )
        };
        output.finish(status, DeliveryLane::Control)
    }

    fn feedback_stream(&self) -> Option<u64> {
        Some(self.handles.control_transport)
    }

    fn send_feedback(&mut self, message: &HostFeedbackMessage) -> PlaneResult<bool> {
        // SAFETY: the owned binding and message allocation remain live through
        // the synchronous provider call. The transport copies accepted bytes.
        let api = unsafe { self.control.table::<ControlTransportApi>() }
            .map_err(|_| "native_layout_mismatch")?;
        let control_message = ControlMessage {
            message_type: u32::from(message.message_type),
            channel: u32::from(message.channel),
            delivery: match message.delivery {
                HostFeedbackDelivery::Reliable => CONTROL_RELIABLE,
                HostFeedbackDelivery::SequencedUnreliable => CONTROL_UNRELIABLE,
            },
            reserved: 0,
            data: message.payload.as_ptr(),
            len: message.payload.len(),
            sequence: 0,
        };
        accepted(
            unsafe {
                (api.send)(
                    self.control.context(),
                    self.handles.control_transport,
                    &control_message,
                )
            },
            "native_feedback_failed",
        )
    }

    fn video(&mut self) -> PlaneResult<Option<VideoPacket>> {
        let mut output = Delivery::default();
        // SAFETY: binding and output are retained throughout synchronous delivery.
        let video = self.video.as_ref().ok_or("legacy_video_unsupported")?;
        let api = unsafe { video.table::<VideoApi>() }.map_err(|_| "native_layout_mismatch")?;
        let status = unsafe {
            (api.receive)(
                video.context(),
                self.handles.video,
                PacketSink {
                    context: (&mut output as *mut Delivery<VideoPacket>).cast(),
                    write: copy_video,
                },
            )
        };
        output.finish(status, DeliveryLane::Video)
    }

    fn video_unit(
        &mut self,
        selection: &VideoUnitSelection,
    ) -> PlaneResult<Option<VideoUnitPacket>> {
        let binding = self
            .video_unit
            .as_ref()
            .ok_or("negotiated_video_unsupported")?;
        let mut output = UnitDelivery {
            delivery: Delivery::default(),
            selection: *selection,
        };
        // SAFETY: binding and stack output remain live throughout the callback.
        let status = if selection.metadata_required {
            let api = unsafe { binding.table::<NegotiatedVideoApiV1_1>() }
                .map_err(|_| "native_layout_mismatch")?;
            unsafe {
                (api.receive_metadata)(
                    binding.context(),
                    self.handles.video,
                    NegotiatedVideoMetadataUnitSinkV1 {
                        context: (&mut output as *mut UnitDelivery).cast(),
                        write: copy_metadata_video_unit,
                    },
                )
            }
        } else {
            let api = unsafe { binding.table::<NegotiatedVideoApi>() }
                .map_err(|_| "native_layout_mismatch")?;
            unsafe {
                (api.receive)(
                    binding.context(),
                    self.handles.video,
                    NegotiatedVideoUnitSink {
                        context: (&mut output as *mut UnitDelivery).cast(),
                        write: copy_video_unit,
                    },
                )
            }
        };
        let packet = output.delivery.finish(status, DeliveryLane::Video)?;
        if let Some(packet) = &packet
            && let Some(metadata) = packet.side_data()
        {
            if self.pending_video_metadata.is_some() {
                return Err("video_metadata_already_pending");
            }
            self.pending_video_metadata = Some(PendingVideoMetadata {
                access_unit: packet.header().access_unit_id,
                bytes: metadata.to_vec(),
                plan: None,
                framed: false,
                transport_admitted: false,
            });
        }
        Ok(packet)
    }

    fn submit_video_unit(&mut self, unit: &NegotiatedVideoUnit) -> PlaneResult<bool> {
        if self.pending_video_metadata.is_some() {
            return self.submit_metadata_unit(unit);
        }
        if let (Some(framer), Some(transport), Some(limits)) =
            (&self.framer, &self.plan_transport, &self.framing_limits)
        {
            return planned::submit(
                framer,
                transport,
                limits,
                self.handles.media_transport,
                unit,
            );
        }
        let binding = self
            .media_unit
            .as_ref()
            .ok_or("negotiated_video_unsupported")?;
        let api = unsafe { binding.table::<NegotiatedVideoTransportApi>() }
            .map_err(|_| "native_layout_mismatch")?;
        // SAFETY: pump retains unit and bytes until synchronous submit returns.
        accepted(
            unsafe { (api.submit)(binding.context(), self.handles.media_transport, unit) },
            "native_video_unit_submit_failed",
        )
    }

    fn pcm(&mut self) -> PlaneResult<Option<PcmPacket>> {
        let mut output: Delivery<PcmPacket> = Delivery::default();
        // SAFETY: callback copies initialized PCM before the provider borrow ends.
        let capture = self.capture.as_ref().ok_or("audio_disabled")?;
        let stream = self.handles.audio_capture.ok_or("audio_disabled")?;
        let api =
            unsafe { capture.table::<AudioCaptureApi>() }.map_err(|_| "native_layout_mismatch")?;
        let status = unsafe {
            (api.read_audio)(
                capture.context(),
                stream,
                AudioSink {
                    context: (&mut output as *mut Delivery<PcmPacket>).cast(),
                    write: copy_pcm,
                },
            )
        };
        let packet = output.finish(status, DeliveryLane::Pcm)?;
        if packet
            .as_ref()
            .is_some_and(|packet| packet.header().channels != self.handles.audio_channels)
        {
            return Err("unsupported_audio_channels");
        }
        Ok(packet)
    }

    fn audio(&mut self) -> PlaneResult<Option<AudioPacket>> {
        let mut output: Delivery<AudioPacket> = Delivery::default();
        // SAFETY: stack output and binding outlive the synchronous encoder call.
        let encoder = self.encoder.as_ref().ok_or("audio_disabled")?;
        let stream = self.handles.audio_encoder.ok_or("audio_disabled")?;
        let api =
            unsafe { encoder.table::<AudioEncoderApi>() }.map_err(|_| "native_layout_mismatch")?;
        let status = unsafe {
            (api.receive)(
                encoder.context(),
                stream,
                AudioPacketSink {
                    context: (&mut output as *mut Delivery<AudioPacket>).cast(),
                    write: copy_audio,
                },
            )
        };
        if status == MEDIA_BACKEND_ERROR {
            Err("audio_encoder_failed")
        } else {
            let packet = output.finish(status, DeliveryLane::Audio)?;
            if packet
                .as_ref()
                .is_some_and(|packet| packet.header().channels != self.handles.audio_channels)
            {
                return Err("unsupported_audio_channels");
            }
            Ok(packet)
        }
    }

    fn submit_video(&mut self, packet: &EncodedPacket) -> PlaneResult<bool> {
        // SAFETY: packet points into an owned assembly retained until return.
        let api = unsafe { self.media.table::<MediaTransportApi>() }
            .map_err(|_| "native_layout_mismatch")?;
        accepted(
            unsafe {
                (api.submit_video)(self.media.context(), self.handles.media_transport, packet)
            },
            "native_video_submit_failed",
        )
    }

    fn submit_pcm(&mut self, frame: &AudioFrame) -> PlaneResult<bool> {
        // SAFETY: frame points into the plane's retained initialized PCM allocation.
        let encoder = self.encoder.as_ref().ok_or("audio_disabled")?;
        let stream = self.handles.audio_encoder.ok_or("audio_disabled")?;
        let api =
            unsafe { encoder.table::<AudioEncoderApi>() }.map_err(|_| "native_layout_mismatch")?;
        encoder_accepted(unsafe { (api.submit)(encoder.context(), stream, frame) })
    }

    fn submit_audio(&mut self, packet: &EncodedAudioPacket) -> PlaneResult<bool> {
        // SAFETY: packet allocation remains owned by the plane until return.
        let api = unsafe { self.media.table::<MediaTransportApi>() }
            .map_err(|_| "native_layout_mismatch")?;
        accepted(
            unsafe {
                (api.submit_audio)(self.media.context(), self.handles.media_transport, packet)
            },
            "native_audio_submit_failed",
        )
    }

    fn drain_encoder(&mut self) -> PlaneResult<bool> {
        // SAFETY: binding and owned stream ID are retained by the pipeline.
        let encoder = self.encoder.as_ref().ok_or("audio_disabled")?;
        let stream = self.handles.audio_encoder.ok_or("audio_disabled")?;
        let api =
            unsafe { encoder.table::<AudioEncoderApi>() }.map_err(|_| "native_layout_mismatch")?;
        encoder_accepted(unsafe { (api.drain)(encoder.context(), stream) })
    }

    fn drain_transport(&mut self) -> PlaneResult<bool> {
        if let Some(binding) = &self.plan_transport
            && !self.unit_transport_draining
        {
            let api = unsafe { binding.table::<NegotiatedVideoPlanTransportApiV2>() }
                .map_err(|_| "native_layout_mismatch")?;
            if !accepted(
                unsafe { (api.drain)(binding.context(), self.handles.media_transport) },
                "native_video_plan_drain_failed",
            )? {
                return Ok(false);
            }
            self.unit_transport_draining = true;
        }
        if let Some(binding) = &self.media_unit
            && !self.unit_transport_draining
        {
            let api = unsafe { binding.table::<NegotiatedVideoTransportApi>() }
                .map_err(|_| "native_layout_mismatch")?;
            // SAFETY: binding is retained and no borrowed allocation escapes.
            if !accepted(
                unsafe { (api.drain)(binding.context(), self.handles.media_transport) },
                "native_video_unit_drain_failed",
            )? {
                return Ok(false);
            }
            self.unit_transport_draining = true;
        }
        // SAFETY: no callback or borrowed allocation escapes this call.
        let api = unsafe { self.media.table::<MediaTransportApi>() }
            .map_err(|_| "native_layout_mismatch")?;
        accepted(
            unsafe { (api.drain)(self.media.context(), self.handles.media_transport) },
            "native_transport_drain_failed",
        )
    }
}

fn accepted(status: Status, failure: &'static str) -> PlaneResult<bool> {
    match status {
        STATUS_OK => Ok(true),
        MEDIA_BUSY | MEDIA_PENDING => Ok(false),
        MEDIA_CLOSED => Err("native_stream_closed"),
        _ => {
            eprintln!("amaterasu native operation failed operation={failure} status={status}");
            Err(failure)
        }
    }
}

fn encoder_accepted(status: Status) -> PlaneResult<bool> {
    if status == MEDIA_BACKEND_ERROR {
        Err("audio_encoder_failed")
    } else {
        accepted(status, "native_audio_encoder_failed")
    }
}

struct Delivery<T> {
    result: Option<PlaneResult<T>>,
}

#[derive(Clone, Copy)]
enum DeliveryLane {
    Control,
    Video,
    Pcm,
    Audio,
}

impl DeliveryLane {
    fn name(self) -> &'static str {
        match self {
            Self::Control => "control",
            Self::Video => "video",
            Self::Pcm => "pcm",
            Self::Audio => "audio",
        }
    }

    fn failure(self) -> &'static str {
        match self {
            Self::Control => "native_delivery_failed_control",
            Self::Video => "native_delivery_failed_video",
            Self::Pcm => "native_delivery_failed_pcm",
            Self::Audio => "native_delivery_failed_audio",
        }
    }
}

impl<T> Default for Delivery<T> {
    fn default() -> Self {
        Self { result: None }
    }
}

impl<T> Delivery<T> {
    fn finish(self, status: Status, lane: DeliveryLane) -> PlaneResult<Option<T>> {
        let callback = self.result.is_some();
        match (status, self.result) {
            (STATUS_OK, Some(result)) => result.map(Some),
            (MEDIA_BUSY | MEDIA_PENDING, None) => Ok(None),
            (_, Some(Err(error))) => Err(error),
            (MEDIA_CLOSED, _) => Err(if matches!(lane, DeliveryLane::Control) {
                "native_control_closed"
            } else {
                "native_stream_closed"
            }),
            _ => {
                // Only fixed lane names, numeric ABI status, and callback presence
                // are logged. No borrowed media, control message, or peer data escapes.
                eprintln!(
                    "amaterasu native delivery failed lane={} status={} callback={}",
                    lane.name(),
                    status,
                    callback
                );
                Err(lane.failure())
            }
        }
    }
}

unsafe fn deliver<T, P>(
    context: *mut c_void,
    input: *const P,
    copy: impl FnOnce(&P) -> PlaneResult<T>,
) -> Status {
    if context.is_null() || input.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: callbacks receive exclusive stack delivery state and a live header.
    let output = unsafe { &mut *context.cast::<Delivery<T>>() };
    if output.result.is_some() {
        output.result = Some(Err("native_multiple_callbacks"));
        return STATUS_INVALID_ARGUMENT;
    }
    let result = catch_unwind(AssertUnwindSafe(|| copy(unsafe { &*input })))
        .unwrap_or(Err("native_callback_failed"));
    let status = if result.is_ok() {
        STATUS_OK
    } else {
        STATUS_INVALID_ARGUMENT
    };
    output.result = Some(result);
    status
}

struct UnitDelivery {
    delivery: Delivery<VideoUnitPacket>,
    selection: VideoUnitSelection,
}
unsafe extern "C" fn copy_video_unit(
    context: *mut c_void,
    unit: *const NegotiatedVideoUnit,
) -> Status {
    if context.is_null()
        || unit.is_null()
        || !(unit as usize).is_multiple_of(align_of::<NegotiatedVideoUnit>())
    {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: provider keeps the unit and stack context live through callback.
    let output = unsafe { &mut *context.cast::<UnitDelivery>() };
    let selection = output.selection;
    // SAFETY: provider guarantees initialized unit bytes for this callback.
    unsafe {
        deliver(
            (&mut output.delivery as *mut Delivery<VideoUnitPacket>).cast(),
            unit,
            |value| VideoUnitPacket::copy(value, &selection),
        )
    }
}

unsafe extern "C" fn copy_metadata_video_unit(
    context: *mut c_void,
    unit: *const NegotiatedVideoMetadataUnitV1,
) -> Status {
    if context.is_null()
        || unit.is_null()
        || !(unit as usize).is_multiple_of(align_of::<NegotiatedVideoMetadataUnitV1>())
    {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: provider lends the wrapper and buffers through this synchronous callback.
    let output = unsafe { &mut *context.cast::<UnitDelivery>() };
    let selection = output.selection;
    unsafe {
        deliver(
            (&mut output.delivery as *mut Delivery<VideoUnitPacket>).cast(),
            unit,
            |value| VideoUnitPacket::copy_metadata(value, &selection),
        )
    }
}

unsafe extern "C" fn copy_video(context: *mut c_void, packet: *const EncodedPacket) -> Status {
    // SAFETY: trusted provider lends initialized metadata and bytes during delivery.
    unsafe { deliver(context, packet, |packet| VideoPacket::copy(packet)) }
}
unsafe extern "C" fn copy_pcm(context: *mut c_void, frame: *const AudioFrame) -> Status {
    // SAFETY: trusted provider keeps frame and samples live until callback returns.
    unsafe { deliver(context, frame, |frame| PcmPacket::copy(frame)) }
}

unsafe extern "C" fn copy_audio(context: *mut c_void, packet: *const EncodedAudioPacket) -> Status {
    // SAFETY: trusted provider keeps packet bytes initialized and immutable.
    unsafe { deliver(context, packet, |packet| AudioPacket::copy(packet)) }
}
unsafe extern "C" fn copy_control(context: *mut c_void, message: *const ControlMessage) -> Status {
    // SAFETY: provider lends authenticated plaintext for this callback only.
    unsafe {
        deliver(context, message, |message| {
            let bytes = message.bytes().map_err(|_| "invalid_control_message")?;
            Ok(ControlRecord {
                message_type: message.message_type as u16,
                payload: bytes.to_vec(),
                sequence: message.sequence,
            })
        })
    }
}

#[cfg(test)]
mod tests;
