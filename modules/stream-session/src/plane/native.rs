//! Native calls borrow copied headers only synchronously. Bindings are retained
//! until every pending call and callback has returned, then released by the SDK.
use super::*;
use amaterasu_gamestream::control::{RUMBLE_MESSAGE_TYPE, RumbleFeedback, encode_rumble_feedback};
use amaterasu_media::{CONTROL_RELIABLE, ControlMessage};
use amaterasu_module_sdk::{NativeBinding, abi::*};
use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
};

pub(super) struct NativeBackend {
    video: NativeBinding,
    capture: Option<NativeBinding>,
    encoder: Option<NativeBinding>,
    media: NativeBinding,
    control: NativeBinding,
    handles: Handles,
    last_hdr: Option<HdrMetadata>,
}

impl NativeBackend {
    fn update_hdr(&mut self) -> PlaneResult<()> {
        if !self.handles.hdr {
            return Ok(());
        }
        // SAFETY: header is inspected before the additive video v1.2 tail.
        let prefix =
            unsafe { self.video.table::<VideoApi>() }.map_err(|_| "native_layout_mismatch")?;
        if prefix.base.header.minor < 2 {
            return Err("video_hdr_metadata_unsupported");
        }
        let api =
            unsafe { self.video.table::<VideoApiV1_2>() }.map_err(|_| "native_layout_mismatch")?;
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
        })
    }
}

impl Backend for NativeBackend {
    fn desktop_geometry(&mut self) -> PlaneResult<Option<CaptureGeometry>> {
        self.update_hdr()?;
        // SAFETY: the retained binding was validated for the v1.0 prefix. Only
        // inspect the optional v1.1 tail when both version and size advertise it.
        let prefix =
            unsafe { self.video.table::<VideoApi>() }.map_err(|_| "native_layout_mismatch")?;
        if prefix.base.header.minor < 1 {
            return Ok(None);
        }
        let api =
            unsafe { self.video.table::<VideoApiV1_1>() }.map_err(|_| "native_layout_mismatch")?;
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
        // Both bindings were obtained by their typed contract identities. Only
        // use the additive timing path when both ends advertise their tails.
        // SAFETY: retained bindings and their exact versioned tables outlive the call.
        let timed_video = unsafe { amaterasu_module_sdk::video::timed(&self.video) }
            .map_err(|_| "native_layout_mismatch")?;
        let timed_media = unsafe { amaterasu_module_sdk::transport::media_timed(&self.media) }
            .map_err(|_| "native_layout_mismatch")?;
        let status = if let (Some(api), Some(_)) = (timed_video, timed_media) {
            // SAFETY: provider lends the timed packet for this synchronous callback.
            unsafe {
                (api.receive_timed)(
                    self.video.context(),
                    self.handles.video,
                    TimedPacketSink {
                        context: (&mut output as *mut Delivery<VideoPacket>).cast(),
                        write: copy_timed_video,
                    },
                )
            }
        } else {
            // SAFETY: the retained binding was validated for the legacy prefix.
            let api =
                unsafe { self.video.table::<VideoApi>() }.map_err(|_| "native_layout_mismatch")?;
            unsafe {
                (api.receive)(
                    self.video.context(),
                    self.handles.video,
                    PacketSink {
                        context: (&mut output as *mut Delivery<VideoPacket>).cast(),
                        write: copy_video,
                    },
                )
            }
        };
        output.finish(status, DeliveryLane::Video)
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

    fn submit_video(
        &mut self,
        packet: &EncodedPacket,
        processing_started_host_us: u64,
    ) -> PlaneResult<bool> {
        // The packet allocation stays owned by the assembly through this call.
        // SAFETY: both bindings have validated identities and outlive this call.
        let timed_video = unsafe { amaterasu_module_sdk::video::timed(&self.video) }
            .map_err(|_| "native_layout_mismatch")?;
        let timed_media = unsafe { amaterasu_module_sdk::transport::media_timed(&self.media) }
            .map_err(|_| "native_layout_mismatch")?;
        let status = if let (Some(_), Some(api)) = (timed_video, timed_media) {
            let timed = TimedEncodedPacket {
                base: *packet,
                processing_started_host_us,
            };
            // SAFETY: packet, timing header and borrowed bytes remain live synchronously.
            unsafe {
                (api.submit_video_timed)(self.media.context(), self.handles.media_transport, &timed)
            }
        } else {
            // Legacy transport has no independent processing clock. The built-in
            // provider reports unknown; external providers own their semantics.
            let api = unsafe { self.media.table::<MediaTransportApi>() }
                .map_err(|_| "native_layout_mismatch")?;
            unsafe {
                (api.submit_video)(self.media.context(), self.handles.media_transport, packet)
            }
        };
        accepted(status, "native_video_submit_failed")
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

unsafe extern "C" fn copy_video(context: *mut c_void, packet: *const EncodedPacket) -> Status {
    // SAFETY: trusted provider lends initialized metadata and bytes during delivery.
    unsafe { deliver(context, packet, |packet| VideoPacket::copy(packet)) }
}
unsafe extern "C" fn copy_timed_video(
    context: *mut c_void,
    packet: *const TimedEncodedPacket,
) -> Status {
    // SAFETY: provider lends the timed header and bytes during this callback.
    unsafe { deliver(context, packet, |packet| VideoPacket::copy_timed(packet)) }
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
