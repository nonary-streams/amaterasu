//! Ordered audio bindings and synchronous ownership across provider calls.
use super::*;

pub(super) struct OrderedBindings {
    encoder: NativeBinding,
    capture: NativeBinding,
    transport: NativeBinding,
    candidate: amaterasu_module_sdk::audio::OrderedAudioCandidate,
    input: Option<AudioInputSelectionV2>,
    synthetic_eos: bool,
    clock_epoch: [u8; 16],
    prepared: bool,
}

impl NativeBackend {
    pub(crate) fn enable_ordered_audio(
        &mut self,
        runtime: &RuntimeClient,
        route: &OrderedAudioRoute,
    ) -> PlaneResult<()> {
        use amaterasu_module_sdk::audio;
        if !self.handles.stream_audio
            || self.handles.audio_capture.is_none()
            || self.ordered_audio.is_some()
        {
            return Err("ordered_audio_selection_invalid");
        }
        // SAFETY: trusted composition selects these exact typed families and
        // the runtime and providers outlive the retained bindings.
        let encoder = unsafe { audio::bind_ordered_encoder(runtime, &route.encoder_service) }
            .map_err(|_| "ordered_encoder_unavailable")?;
        let capture = unsafe { audio::bind_ordered_capture(runtime, &route.capture_service) }
            .map_err(|_| "ordered_capture_unavailable")?;
        let transport = unsafe { audio::bind_ordered_transport(runtime, &route.transport_service) }
            .map_err(|_| "ordered_transport_unavailable")?;
        self.ordered_audio = Some(OrderedBindings {
            encoder,
            capture,
            transport,
            candidate: route.candidate.clone(),
            input: None,
            synthetic_eos: false,
            clock_epoch: route.clock_epoch,
            prepared: false,
        });
        Ok(())
    }

    pub(super) fn prepare_ordered_audio_impl(&mut self) -> PlaneResult<bool> {
        use amaterasu_module_sdk::audio::{self, AudioNegotiationError};
        let state = self
            .ordered_audio
            .as_mut()
            .ok_or("ordered_audio_unsupported")?;
        if state.prepared {
            return Ok(true);
        }
        // SAFETY: this binding was obtained for the ordered encoder family.
        let api = unsafe { state.encoder.table::<OrderedAudioEncoderApiV2>() }
            .map_err(|_| "native_layout_mismatch")?;
        let format = state.candidate.format();
        // SAFETY: the exact bound table remains live and enumeration is synchronous.
        let current = match unsafe {
            audio::probe_ordered(
                api,
                state.candidate.stream(),
                &[(format.format.clone(), format.layout)],
            )
        } {
            Ok(candidate) => candidate,
            Err(AudioNegotiationError::Provider(MEDIA_BUSY | MEDIA_PENDING)) => return Ok(false),
            Err(_) => return Err("ordered_encoder_probe_failed"),
        };
        if current != state.candidate {
            return Err("ordered_audio_offer_stale");
        }
        let selection = current.transport_selection(state.clock_epoch);
        // SAFETY: the live binding was obtained for the ordered transport family.
        let api = unsafe { state.transport.table::<OrderedAudioTransportApiV2>() }
            .map_err(|_| "native_layout_mismatch")?;
        // SAFETY: selected offer and nested bytes outlive each call.
        if !accepted(
            unsafe {
                (api.probe)(
                    state.transport.context(),
                    self.handles.media_transport,
                    &selection,
                )
            },
            "ordered_transport_probe_failed",
        )? {
            return Ok(false);
        }
        if !accepted(
            unsafe {
                (api.prepare)(
                    state.transport.context(),
                    self.handles.media_transport,
                    &selection,
                )
            },
            "ordered_transport_prepare_failed",
        )? {
            return Ok(false);
        }
        state.candidate = current;
        state.prepared = true;
        Ok(true)
    }

    pub(super) fn ordered_pcm_impl(&mut self) -> PlaneResult<Option<OrderedPcmPacket>> {
        let state = self
            .ordered_audio
            .as_ref()
            .ok_or("ordered_audio_unsupported")?;
        let capture_stream = self.handles.audio_capture.ok_or("audio_disabled")?;
        // SAFETY: the live binding has the ordered capture family identity.
        let api = unsafe { state.capture.table::<OrderedAudioCaptureApiV2>() }
            .map_err(|_| "native_layout_mismatch")?;
        let mut output = Delivery::<OrderedPcmPacket>::default();
        // SAFETY: provider borrows the output sink during this call.
        let status = unsafe {
            (api.read_audio)(
                state.capture.context(),
                capture_stream,
                OrderedAudioSinkV2 {
                    context: (&mut output as *mut Delivery<OrderedPcmPacket>).cast(),
                    write: copy_ordered_pcm,
                },
            )
        };
        let packet = output.finish(status, DeliveryLane::Pcm)?;
        if packet
            .as_ref()
            .is_some_and(|packet| packet.header().stream != capture_stream)
        {
            return Err("ordered_capture_stream_mismatch");
        }
        Ok(packet)
    }

    pub(super) fn ordered_audio_impl(&mut self) -> PlaneResult<Option<OrderedAudioPacket>> {
        let state = self
            .ordered_audio
            .as_mut()
            .ok_or("ordered_audio_unsupported")?;
        if state.synthetic_eos {
            state.synthetic_eos = false;
            let packet = NegotiatedAudioPacket {
                stream: state.candidate.stream(),
                generation: state.candidate.generation(),
                offer_id: state.candidate.offer_id(),
                flags: AUDIO_PACKET_END_OF_STREAM,
                frames: 0,
                timestamp_us: 0,
                sequence: 0,
                data: BorrowedBytes::new(&[]),
            };
            // SAFETY: EOS has no borrowed data; candidate is live.
            return unsafe { OrderedAudioPacket::copy(&packet, &state.candidate) }.map(Some);
        }
        if state.input.is_none() {
            return Ok(None);
        }
        // SAFETY: the live binding has the ordered encoder family identity.
        let api = unsafe { state.encoder.table::<OrderedAudioEncoderApiV2>() }
            .map_err(|_| "native_layout_mismatch")?;
        let mut output = OrderedAudioDelivery {
            delivery: Delivery::default(),
            candidate: state.candidate.clone(),
        };
        // SAFETY: callback output and encoder binding live throughout this call.
        let status = unsafe {
            (api.receive)(
                state.encoder.context(),
                state.candidate.stream(),
                NegotiatedAudioPacketSink {
                    context: (&mut output as *mut OrderedAudioDelivery).cast(),
                    write: copy_ordered_audio,
                },
            )
        };
        output.delivery.finish(status, DeliveryLane::Audio)
    }

    pub(super) fn submit_ordered_pcm_impl(
        &mut self,
        frame: &OrderedAudioFrameV2,
    ) -> PlaneResult<bool> {
        use amaterasu_module_sdk::audio::{self, AudioNegotiationError};
        let state = self
            .ordered_audio
            .as_mut()
            .ok_or("ordered_audio_unsupported")?;
        if frame.stream != self.handles.audio_capture.ok_or("audio_disabled")? {
            return Err("ordered_capture_stream_mismatch");
        }
        if state.input.is_none() {
            let input = AudioInputSelectionV2 {
                size: size_of::<AudioInputSelectionV2>() as u32,
                version: AUDIO_ORDERED_VERSION,
                stream: frame.stream,
                generation: frame.generation,
                layout: frame.layout,
            };
            // SAFETY: the live binding has the ordered encoder family identity.
            let api = unsafe { state.encoder.table::<OrderedAudioEncoderApiV2>() }
                .map_err(|_| "native_layout_mismatch")?;
            // SAFETY: input and bound table are live through this synchronous configuration.
            match unsafe { audio::configure_ordered(api, &state.candidate, &input) } {
                Ok(()) => state.input = Some(input),
                Err(AudioNegotiationError::Provider(MEDIA_BUSY | MEDIA_PENDING)) => {
                    return Ok(false);
                }
                Err(_) => return Err("ordered_encoder_configuration_failed"),
            }
        }
        state
            .input
            .as_ref()
            .expect("configured input")
            .validate_frame(frame)
            .map_err(|_| "ordered_capture_layout_changed")?;
        // SAFETY: the live binding has the ordered encoder family identity.
        let api = unsafe { state.encoder.table::<OrderedAudioEncoderApiV2>() }
            .map_err(|_| "native_layout_mismatch")?;
        // SAFETY: plane retains the owned PCM until submission succeeds.
        accepted(
            unsafe { (api.submit)(state.encoder.context(), state.candidate.stream(), frame) },
            "ordered_encoder_submit_failed",
        )
    }

    pub(super) fn submit_ordered_audio_impl(
        &mut self,
        packet: &NegotiatedAudioPacket,
    ) -> PlaneResult<bool> {
        let state = self
            .ordered_audio
            .as_ref()
            .ok_or("ordered_audio_unsupported")?;
        // SAFETY: the live binding has the ordered transport family identity.
        let api = unsafe { state.transport.table::<OrderedAudioTransportApiV2>() }
            .map_err(|_| "native_layout_mismatch")?;
        // SAFETY: pump retains packet and immutable bytes during synchronous admission.
        accepted(
            unsafe {
                (api.submit)(
                    state.transport.context(),
                    self.handles.media_transport,
                    packet,
                )
            },
            "ordered_transport_submit_failed",
        )
    }

    pub(super) fn drain_ordered_encoder_impl(&mut self) -> PlaneResult<bool> {
        let state = self
            .ordered_audio
            .as_mut()
            .ok_or("ordered_audio_unsupported")?;
        if state.input.is_none() {
            state.synthetic_eos = true;
            return Ok(true);
        }
        // SAFETY: the live binding has the ordered encoder family identity.
        let api = unsafe { state.encoder.table::<OrderedAudioEncoderApiV2>() }
            .map_err(|_| "native_layout_mismatch")?;
        // SAFETY: encoder binding and stream remain live until drain completes.
        accepted(
            unsafe { (api.drain)(state.encoder.context(), state.candidate.stream()) },
            "ordered_encoder_drain_failed",
        )
    }
}

unsafe extern "C" fn copy_ordered_pcm(
    context: *mut c_void,
    frame: *const OrderedAudioFrameV2,
) -> Status {
    if frame.is_null() || !(frame as usize).is_multiple_of(align_of::<OrderedAudioFrameV2>()) {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: provider lends frame and samples through this callback.
    unsafe { deliver(context, frame, |frame| OrderedPcmPacket::copy(frame)) }
}

struct OrderedAudioDelivery {
    delivery: Delivery<OrderedAudioPacket>,
    candidate: amaterasu_module_sdk::audio::OrderedAudioCandidate,
}

unsafe extern "C" fn copy_ordered_audio(
    context: *mut c_void,
    packet: *const NegotiatedAudioPacket,
) -> Status {
    if context.is_null()
        || packet.is_null()
        || !(packet as usize).is_multiple_of(align_of::<NegotiatedAudioPacket>())
    {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: caller supplies a live stack context and a callback-scoped packet.
    let output = unsafe { &mut *context.cast::<OrderedAudioDelivery>() };
    let candidate = &output.candidate;
    // SAFETY: provider lends immutable initialized packet bytes for this call.
    unsafe {
        deliver(
            (&mut output.delivery as *mut Delivery<OrderedAudioPacket>).cast(),
            packet,
            |packet| OrderedAudioPacket::copy(packet, candidate),
        )
    }
}
