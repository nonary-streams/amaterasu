use super::*;
use std::collections::VecDeque;

#[test]
fn packet_copy_accepts_negotiated_non_opus_audio_and_height_channels() {
    let samples = vec![0_u8; 12 * 441 * 4];
    let frame = AudioFrame {
        sample_rate: 44_100,
        channels: 12,
        format: amaterasu_media::AudioFormatId::standard(u64::from(PCM_F32)),
        sample_bytes: 4,
        packing: amaterasu_media::AUDIO_PACKING_INTERLEAVED,
        frames: 441,
        flags: 0,
        data: samples.as_ptr(),
        len: samples.len(),
        timestamp_us: 0,
        sequence: 1,
    };
    // SAFETY: both packet views borrow their backing allocations through copying.
    assert!(unsafe { PcmPacket::copy(&frame) }.is_ok());
    let bytes = [1_u8, 2, 3];
    let packet = AudioUnit {
        generation: 1,
        format: AudioFormatId {
            namespace: 42,
            value: 99,
            version: 1,
        },
        metadata_schema: AudioFormatId {
            namespace: 0,
            value: 0,
            version: 0,
        },
        flags: 0,
        duration_us: 10_000,
        payload: amaterasu_module_sdk::abi::BorrowedBytes::new(&bytes),
        metadata: amaterasu_module_sdk::abi::BorrowedBytes::new(&[]),
        timestamp_us: 0,
        sequence: 1,
    };
    assert!(unsafe { AudioPacket::copy_unit(&packet) }.is_ok());
}

#[test]
fn custom_audio_unit_and_metadata_survive_backpressure() {
    let format = AudioFormatId {
        namespace: 42,
        value: u64::from(u32::MAX) + 7,
        version: 1,
    };
    let schema = AudioFormatId {
        namespace: 42,
        value: 8,
        version: 1,
    };
    let mut bytes = vec![1, 2, 3];
    let mut metadata = vec![7, 8];
    let unit = AudioUnit {
        generation: 1,
        format,
        metadata_schema: schema,
        flags: 0,
        timestamp_us: 10,
        duration_us: 1234,
        sequence: 1,
        payload: amaterasu_module_sdk::abi::BorrowedBytes::new(&bytes),
        metadata: amaterasu_module_sdk::abi::BorrowedBytes::new(&metadata),
    };
    // SAFETY: both source allocations remain valid through the copy.
    let packet = unsafe { AudioPacket::copy_unit(&unit) }.unwrap();
    bytes.fill(0);
    metadata.fill(0);
    let mut backend = Fake {
        audio_unit_busy: true,
        ..Fake::default()
    };
    backend.audios.push_back(packet);
    let mut pump = Pump::new(backend);
    pump.tick(true).unwrap();
    assert!(pump.backend.audio_units.is_empty());
    pump.backend.audio_unit_busy = false;
    pump.tick(true).unwrap();
    assert_eq!(
        pump.backend.audio_units,
        [(format, vec![1, 2, 3], vec![7, 8])]
    );
}

#[test]
fn final_custom_audio_unit_keeps_payload_metadata_and_provider_flags() {
    let format = AudioFormatId {
        namespace: 42,
        value: 99,
        version: 1,
    };
    let bytes = [1, 2, 3];
    let metadata = [7, 8];
    let unit = AudioUnit {
        format,
        metadata_schema: format,
        generation: 1,
        flags: AUDIO_EXTENSIBLE_END | 0x80,
        timestamp_us: 10,
        duration_us: 10,
        sequence: 1,
        payload: amaterasu_module_sdk::abi::BorrowedBytes::new(&bytes),
        metadata: amaterasu_module_sdk::abi::BorrowedBytes::new(&metadata),
    };
    let mut backend = Fake::default();
    // SAFETY: the source buffers remain live through the synchronous copy.
    backend
        .audios
        .push_back(unsafe { AudioPacket::copy_unit(&unit).unwrap() });
    let mut pump = Pump::new(backend);
    let progress = pump.tick(true).unwrap();
    assert!(pump.audio_ended);
    assert_eq!(
        pump.backend.audio_units,
        [(format, bytes.to_vec(), metadata.to_vec())]
    );
    assert_eq!(progress.accepted_media_payload_bytes, 3);
}

#[test]
fn encoded_source_drains_and_forwards_without_polling_pcm() {
    let mut backend = Fake {
        audio_busy: true,
        ..Fake::default()
    };
    backend.audios.push_back(audio(false));
    backend.audios.push_back(audio(true));
    let mut pump = Pump::new(backend);
    pump.encoded_source = true;
    pump.stopping = true;
    pump.tick(true).unwrap();
    assert_eq!(pump.backend.encoder_drains, 1);
    assert_eq!(pump.backend.pcm_reads, 0);
    assert!(!pump.audio_ended);
    pump.backend.audio_busy = false;
    pump.tick(true).unwrap();
    assert_eq!(pump.backend.audio_sent.len(), 2);
    assert!(pump.audio_ended);
    assert_eq!(pump.backend.pcm_reads, 0);
}

#[test]
fn processor_retains_output_when_encoder_is_busy_and_drains_before_encoder() {
    let mut backend = Fake {
        pcm_busy: true,
        ..Fake::default()
    };
    backend.pcms.push_back(pcm());
    let mut pump = Pump::new(backend);
    pump.processing = true;
    pump.tick(true).unwrap();
    assert_eq!(pump.backend.processor_submits, 1);
    assert!(pump.pending_processed.is_some());
    assert_eq!(pump.backend.pcm_sent, 0);
    pump.stopping = true;
    pump.tick(true).unwrap();
    assert_eq!(pump.backend.encoder_drains, 0);
    pump.backend.pcm_busy = false;
    pump.tick(true).unwrap();
    assert_eq!(pump.backend.pcm_sent, 1);
    assert!(pump.processor_ended);
    assert_eq!(pump.backend.encoder_drains, 1);
}

#[derive(Default)]
struct Fake {
    controls: VecDeque<ControlRecord>,
    videos: VecDeque<VideoPacket>,
    pcms: VecDeque<PcmPacket>,
    processed: VecDeque<PcmPacket>,
    processor_busy: bool,
    processor_draining: bool,
    processor_submits: usize,
    audios: VecDeque<AudioPacket>,
    video_reads: usize,
    pcm_reads: usize,
    audio_reads: usize,
    video_busy: bool,
    audio_busy: bool,
    audio_unit_busy: bool,
    audio_units: Vec<(AudioFormatId, Vec<u8>, Vec<u8>)>,
    pcm_busy: bool,
    control_failed: bool,
    feedback_busy: bool,
    encoder_failed: bool,
    video_failure_after_submit: bool,
    videos_sent: Vec<(u32, Vec<u8>)>,
    video_start_times: Vec<u64>,
    audio_sent: Vec<Vec<u8>>,
    audio_flags: Vec<u32>,
    feedback_sent: Vec<HostFeedbackMessage>,
    pcm_sent: usize,
    encoder_drains: usize,
    transport_drains: usize,
}

impl Backend for Fake {
    fn control(&mut self) -> PlaneResult<Option<ControlRecord>> {
        if self.control_failed {
            Err("control_failed")
        } else {
            Ok(self.controls.pop_front())
        }
    }
    fn send_feedback(&mut self, message: &HostFeedbackMessage) -> PlaneResult<bool> {
        if self.feedback_busy {
            return Ok(false);
        }
        self.feedback_sent.push(message.clone());
        Ok(true)
    }

    fn send_rumble(&mut self, _command: RumbleCommand) -> PlaneResult<bool> {
        Ok(true)
    }
    fn video(&mut self) -> PlaneResult<Option<VideoPacket>> {
        self.video_reads += 1;
        if self.video_failure_after_submit && !self.videos_sent.is_empty() {
            return Err("video_failed_after_admission");
        }
        Ok(self.videos.pop_front())
    }
    fn pcm(&mut self) -> PlaneResult<Option<PcmPacket>> {
        self.pcm_reads += 1;
        Ok(self.pcms.pop_front())
    }
    fn audio(&mut self) -> PlaneResult<Option<AudioPacket>> {
        self.audio_reads += 1;
        if let Some(packet) = self.audios.pop_front() {
            Ok(Some(packet))
        } else if self.encoder_failed {
            Err("audio_encoder_failed")
        } else {
            Ok(None)
        }
    }
    fn submit_video(
        &mut self,
        packet: &EncodedPacket,
        processing_started_host_us: u64,
    ) -> PlaneResult<bool> {
        if self.video_busy {
            return Ok(false);
        }
        self.video_start_times.push(processing_started_host_us);
        // SAFETY: pump retains its packet allocation through this synchronous call.
        self.videos_sent
            .push((packet.flags, unsafe { packet.bytes().unwrap() }.to_vec()));
        Ok(true)
    }
    fn submit_pcm(&mut self, _: &AudioFrame) -> PlaneResult<bool> {
        if self.pcm_busy {
            return Ok(false);
        }
        self.pcm_sent += 1;
        Ok(true)
    }
    fn submit_processor(&mut self, frame: &AudioFrame) -> PlaneResult<bool> {
        if self.processor_busy {
            return Ok(false);
        }
        self.processor_submits += 1;
        // SAFETY: the pump owns the input samples through this call.
        self.processed.push_back(unsafe { PcmPacket::copy(frame)? });
        Ok(true)
    }
    fn processor_output(&mut self) -> PlaneResult<ProcessorOutput> {
        if let Some(packet) = self.processed.pop_front() {
            Ok(ProcessorOutput::Frame(packet))
        } else if self.processor_draining {
            Ok(ProcessorOutput::End)
        } else {
            Ok(ProcessorOutput::Pending)
        }
    }
    fn drain_processor(&mut self) -> PlaneResult<bool> {
        self.processor_draining = true;
        Ok(true)
    }
    fn submit_audio(&mut self, unit: &AudioUnit) -> PlaneResult<bool> {
        if unit.format != AUDIO_FORMAT_OPUS {
            if self.audio_unit_busy {
                return Ok(false);
            }
            self.audio_units.push((
                unit.format,
                unsafe { unit.payload.read().unwrap() }.to_vec(),
                unsafe { unit.metadata.read().unwrap() }.to_vec(),
            ));
            return Ok(true);
        }
        if self.audio_busy {
            return Ok(false);
        }
        // SAFETY: pump retains initialized encoded bytes throughout submission.
        self.audio_sent
            .push(unsafe { unit.payload.read().unwrap() }.to_vec());
        self.audio_flags.push(unit.flags);
        Ok(true)
    }
    fn drain_encoder(&mut self) -> PlaneResult<bool> {
        self.encoder_drains += 1;
        Ok(true)
    }
    fn drain_transport(&mut self) -> PlaneResult<bool> {
        self.transport_drains += 1;
        Ok(true)
    }
}

fn video(bytes: &[u8], flags: u32, sequence: u64) -> VideoPacket {
    let packet = EncodedPacket {
        codec: CODEC_H264,
        flags,
        data: bytes.as_ptr(),
        len: bytes.len(),
        timestamp_us: sequence * 1000,
        sequence,
    };
    // SAFETY: input slice remains initialized for the copy.
    unsafe { VideoPacket::copy(&packet).unwrap() }
}

fn audio(eos: bool) -> AudioPacket {
    let bytes = [0xf4, 1, 2];
    let packet = AudioUnit {
        generation: 1,
        format: AUDIO_FORMAT_OPUS,
        metadata_schema: AudioFormatId {
            namespace: 0,
            value: 0,
            version: 0,
        },
        flags: if eos { AUDIO_PACKET_END_OF_STREAM } else { 0 },
        duration_us: if eos { 0 } else { 10_000 },
        payload: amaterasu_module_sdk::abi::BorrowedBytes::new(if eos { &[] } else { &bytes }),
        metadata: amaterasu_module_sdk::abi::BorrowedBytes::new(&[]),
        timestamp_us: 0,
        sequence: 0,
    };
    // SAFETY: local encoded bytes are valid throughout the copy.
    unsafe { AudioPacket::copy_unit(&packet).unwrap() }
}

fn pcm() -> PcmPacket {
    let bytes = vec![0; 480 * 2 * 4];
    let frame = AudioFrame {
        sample_rate: 48_000,
        channels: 2,
        format: amaterasu_media::AudioFormatId::standard(u64::from(PCM_F32)),
        sample_bytes: 4,
        packing: amaterasu_media::AUDIO_PACKING_INTERLEAVED,
        frames: 480,
        flags: 0,
        data: bytes.as_ptr(),
        len: bytes.len(),
        timestamp_us: 0,
        sequence: 0,
    };
    // SAFETY: local samples stay live and initialized until copied.
    unsafe { PcmPacket::copy(&frame).unwrap() }
}

fn rumble(controller: u16, low_frequency: u16, high_frequency: u16) -> HostFeedback {
    HostFeedback::Rumble {
        controller,
        low_frequency,
        high_frequency,
    }
}

fn haptics_pcm(sequence: u32) -> HostFeedback {
    HostFeedback::HapticsPcm {
        controller: 0,
        sequence,
        frames: 240,
        samples_s16le: vec![0; 240 * 4],
    }
}

#[test]
fn control_bursts_drain_promptly_without_starving_video() {
    let mut backend = Fake::default();
    backend
        .controls
        .extend((0..65).map(|sequence| ControlRecord {
            message_type: 1,
            payload: Vec::new(),
            sequence,
        }));
    let mut pump = Pump::with_media(backend, true, 2, CODEC_H264);
    let first = pump.tick(true).unwrap();
    assert_eq!(first.controls.len(), 64);
    assert!(pump.backend.video_reads > 0);
    let second = pump.tick(true).unwrap();
    assert_eq!(second.controls.len(), 1);
    assert_eq!(second.controls[0].sequence, 64);
}

#[test]
fn a_video_tick_drains_multiple_queued_pcm_packets_without_losing_order() {
    let mut backend = Fake::default();
    backend.pcms.extend((0..25).map(|_| pcm()));
    let mut pump = Pump::with_media(backend, true, 2, CODEC_H264);

    pump.tick(true).unwrap();
    assert_eq!(pump.backend.pcm_sent, 25);
    assert_eq!(pump.backend.pcm_reads, 26); // The empty poll ends the batch.
    assert_eq!(pump.backend.audio_reads, 26);
}

#[test]
fn pcm_drain_is_bounded_per_video_tick() {
    let mut backend = Fake::default();
    backend
        .pcms
        .extend((0..(PCM_READ_BUDGET + 1)).map(|_| pcm()));
    let mut pump = Pump::with_media(backend, true, 2, CODEC_H264);

    pump.tick(true).unwrap();
    assert_eq!(pump.backend.pcm_sent, PCM_READ_BUDGET);
    pump.tick(true).unwrap();
    assert_eq!(pump.backend.pcm_sent, PCM_READ_BUDGET + 1);
}

#[test]
fn surround_pcm_and_encoded_packets_cross_plane_and_drain() {
    for channels in [6, 8] {
        let pcm_bytes = vec![0; 480 * channels as usize * 4];
        let frame = AudioFrame {
            sample_rate: 48_000,
            channels,
            format: amaterasu_media::AudioFormatId::standard(u64::from(PCM_F32)),
            sample_bytes: 4,
            packing: amaterasu_media::AUDIO_PACKING_INTERLEAVED,
            frames: 480,
            flags: 0,
            data: pcm_bytes.as_ptr(),
            len: pcm_bytes.len(),
            timestamp_us: 0,
            sequence: 0,
        };
        let encoded_bytes = [0xf4, 1, 2];
        let encoded = AudioUnit {
            generation: 1,
            format: AUDIO_FORMAT_OPUS,
            metadata_schema: AudioFormatId {
                namespace: 0,
                value: 0,
                version: 0,
            },
            flags: 0,
            duration_us: 10_000,
            payload: amaterasu_module_sdk::abi::BorrowedBytes::new(&encoded_bytes),
            metadata: amaterasu_module_sdk::abi::BorrowedBytes::new(&[]),
            timestamp_us: 0,
            sequence: 0,
        };
        let mut backend = Fake::default();
        // SAFETY: both input allocations remain initialized throughout copying.
        backend
            .pcms
            .push_back(unsafe { PcmPacket::copy(&frame).unwrap() });
        backend
            .audios
            .push_back(unsafe { AudioPacket::copy_unit(&encoded).unwrap() });
        let mut pump = Pump::with_media(backend, true, channels, CODEC_H264);
        assert_eq!(pump.tick(true).unwrap().accepted_media_payload_bytes, 3);
        assert_eq!(pump.backend.audio_sent, [encoded_bytes]);
        assert_eq!(pump.backend.pcm_sent, 1);
        pump.stopping = true;
        pump.tick(true).unwrap();
        assert_eq!(pump.backend.encoder_drains, 1);
        assert_eq!(pump.backend.transport_drains, 0);
    }
}

#[test]
fn fragments_form_one_access_unit_without_waiting_for_a_later_frame() {
    let mut backend = Fake::default();
    backend.videos.extend([
        video(&[0, 0, 0, 1, 0x65], FLAG_KEYFRAME, 7),
        video(&[1, 2, 3], FLAG_END_OF_ACCESS_UNIT, 7),
    ]);
    let mut pump = Pump::new(backend);
    pump.tick(true).unwrap();
    assert_eq!(
        pump.backend.videos_sent,
        [(
            FLAG_KEYFRAME | FLAG_END_OF_ACCESS_UNIT,
            vec![0, 0, 0, 1, 0x65, 1, 2, 3]
        )]
    );
}

#[test]
fn feedback_busy_retains_the_same_event_until_transport_local_admission() {
    let backend = Fake {
        feedback_busy: true,
        ..Fake::default()
    };
    let mut pump = Pump::new(backend);
    assert_eq!(
        pump.enqueue_feedback(rumble(3, 10, 20)),
        Ok(FeedbackEnqueueOutcome::Queued)
    );
    let progress = pump.tick(false).unwrap();
    assert_eq!(progress.accepted_video_access_units, 0);
    assert!(pump.backend.feedback_sent.is_empty());
    assert_eq!(pump.pending_feedback.len(), 1);

    pump.backend.feedback_busy = false;
    let progress = pump.tick(false).unwrap();
    assert_eq!(progress.accepted_video_access_units, 0);
    assert_eq!(pump.pending_feedback.len(), 0);
    assert_eq!(pump.backend.feedback_sent.len(), 1);
    assert_eq!(pump.backend.feedback_sent[0].message_type, 0x010b);
}

#[test]
fn queue_pressure_drops_pcm_but_preserves_critical_feedback() {
    let mut pump = Pump::new(Fake::default());
    for sequence in 100..116 {
        assert_eq!(
            pump.enqueue_feedback(haptics_pcm(sequence)),
            Ok(FeedbackEnqueueOutcome::Queued)
        );
    }
    assert_eq!(
        pump.enqueue_feedback(haptics_pcm(116)),
        Ok(FeedbackEnqueueOutcome::DroppedBestEffort)
    );

    for index in 0..45 {
        assert_eq!(
            pump.enqueue_feedback(HostFeedback::RgbLed {
                controller: 0,
                red: index,
                green: 2,
                blue: 3,
            }),
            Ok(FeedbackEnqueueOutcome::Queued)
        );
    }
    for critical_state in [
        HostFeedback::TriggerRumble {
            controller: 0,
            left: 0,
            right: 0,
        },
        HostFeedback::Motion {
            controller: 0,
            report_rate_hz: 0,
            motion_type: amaterasu_gamestream::control::ControllerMotionType::Accel,
        },
        HostFeedback::RgbLed {
            controller: 0,
            red: 0,
            green: 0,
            blue: 0,
        },
    ] {
        assert_eq!(
            pump.enqueue_feedback(critical_state),
            Ok(FeedbackEnqueueOutcome::Queued)
        );
    }
    assert_eq!(pump.pending_feedback.len(), MAX_PENDING_FEEDBACK_MESSAGES);
    assert_eq!(
        pump.enqueue_feedback(rumble(0, 0, 0)),
        Ok(FeedbackEnqueueOutcome::Queued)
    );
    assert_eq!(pump.pending_feedback.len(), MAX_PENDING_FEEDBACK_MESSAGES);

    while !pump.pending_feedback.is_empty() {
        pump.tick(false).unwrap();
    }
    assert_eq!(pump.backend.feedback_sent.len(), 64);
    let sent_pcm_sequences: Vec<u32> = pump
        .backend
        .feedback_sent
        .iter()
        .filter(|message| message.message_type == 0x5601)
        .map(|message| u32::from_le_bytes(message.payload[4..8].try_into().unwrap()))
        .collect();
    assert_eq!(sent_pcm_sequences, (101..116).collect::<Vec<_>>());
    let sent_types: Vec<u16> = pump
        .backend
        .feedback_sent
        .iter()
        .map(|message| message.message_type)
        .collect();
    for critical_type in [0x010b, 0x5500, 0x5501, 0x5502] {
        assert!(sent_types.contains(&critical_type));
    }
}

#[test]
fn stop_cancels_pending_feedback_and_rejects_new_events() {
    let mut pump = Pump::new(Fake::default());
    assert_eq!(
        pump.enqueue_feedback(rumble(1, 1, 1)),
        Ok(FeedbackEnqueueOutcome::Queued)
    );
    pump.stopping = true;
    pump.cancel_feedback();
    assert!(pump.pending_feedback.is_empty());
    assert_eq!(
        pump.enqueue_feedback(rumble(1, 0, 0)),
        Err(FeedbackEnqueueError::Stopped)
    );
    pump.tick(false).unwrap();
    assert!(pump.backend.feedback_sent.is_empty());
}

#[test]
fn accepted_media_counters_count_complete_units_once_and_ignore_busy_retries_and_eos() {
    let mut backend = Fake::default();
    backend.videos.extend([
        video(&[1, 2], FLAG_KEYFRAME, 7),
        video(&[3, 4, 5], FLAG_END_OF_ACCESS_UNIT, 7),
        video(&[6], FLAG_END_OF_ACCESS_UNIT, 9),
        video(&[], FLAG_END_OF_STREAM, 10),
    ]);
    let mut pump = Pump::new(backend);

    let progress = pump.tick(true).unwrap();
    assert_eq!(progress.accepted_video_access_units, 2);
    assert_eq!(progress.accepted_media_payload_bytes, 6);
    assert_eq!(progress.observed_video_sequence_gaps, 1);
}

#[test]
fn busy_video_admission_is_counted_once_only_after_acceptance() {
    let mut backend = Fake {
        video_busy: true,
        ..Fake::default()
    };
    backend
        .videos
        .push_back(video(&[1, 2, 3], FLAG_END_OF_ACCESS_UNIT, 1));
    let mut pump = Pump::new(backend);

    for _ in 0..2 {
        let progress = pump.tick(true).unwrap();
        assert_eq!(progress.accepted_video_access_units, 0);
        assert_eq!(progress.accepted_media_payload_bytes, 0);
    }

    pump.backend.video_busy = false;
    let accepted = pump.tick(true).unwrap();
    assert_eq!(accepted.accepted_video_access_units, 1);
    assert_eq!(accepted.accepted_media_payload_bytes, 3);
}

#[test]
fn later_backend_error_preserves_media_accepted_earlier_in_the_same_tick() {
    let mut backend = Fake {
        video_failure_after_submit: true,
        ..Fake::default()
    };
    backend
        .videos
        .push_back(video(&[1, 2, 3], FLAG_END_OF_ACCESS_UNIT, 1));
    let mut pump = Pump::new(backend);

    let failure = pump.tick(true).unwrap_err();

    assert_eq!(failure.code, "video_failed_after_admission");
    assert_eq!(failure.progress.accepted_video_access_units, 1);
    assert_eq!(failure.progress.accepted_media_payload_bytes, 3);
    assert_eq!(failure.progress.observed_video_sequence_gaps, 0);
}

#[test]
fn audio_payload_bytes_are_counted_after_admission_but_audio_eos_is_not() {
    let mut backend = Fake::default();
    backend.audios.extend([audio(false), audio(true)]);
    let mut pump = Pump::new(backend);

    let progress = pump.tick(true).unwrap();

    assert_eq!(progress.accepted_video_access_units, 0);
    assert_eq!(progress.accepted_media_payload_bytes, 3);
    assert_eq!(progress.observed_video_sequence_gaps, 0);
}

#[test]
fn encoder_recovery_preserves_admitted_packets_and_waiting_pcm() {
    let mut backend = Fake {
        encoder_failed: true,
        ..Fake::default()
    };
    backend.audios.extend([audio(false), audio(false)]);
    backend.pcms.push_back(pcm());
    let mut pump = Pump::new(backend);

    let failure = pump.tick(true).unwrap_err();
    assert_eq!(failure.code, "audio_encoder_failed");
    assert_eq!(failure.progress.accepted_media_payload_bytes, 6);
    assert_eq!(pump.backend.audio_sent.len(), 2);
    assert_eq!(pump.backend.pcm_reads, 0);
    assert!(pump.tick(true).is_ok());
    assert_eq!(pump.backend.audio_reads, 3);
    assert_eq!(pump.backend.pcm_reads, 0);

    pump.backend.encoder_failed = false;
    pump.audio_recovery_needed = false;
    pump.tick(true).unwrap();
    assert_eq!(
        pump.backend.audio_sent,
        [vec![0xf4, 1, 2], vec![0xf4, 1, 2]]
    );
    assert_eq!(pump.backend.pcm_sent, 1);
}

#[test]
fn blocked_video_retains_exact_output_and_stops_upstream_reads() {
    let mut backend = Fake {
        video_busy: true,
        ..Fake::default()
    };
    backend.videos.extend([
        video(&[1, 2], FLAG_END_OF_ACCESS_UNIT, 1),
        video(&[3, 4], FLAG_END_OF_ACCESS_UNIT, 2),
    ]);
    let mut pump = Pump::new(backend);
    pump.tick(true).unwrap();
    assert_eq!(pump.backend.video_reads, 1);
    pump.tick(true).unwrap();
    assert_eq!(pump.backend.video_reads, 1);
    pump.backend.video_busy = false;
    pump.tick(true).unwrap();
    assert_eq!(
        pump.backend
            .videos_sent
            .iter()
            .map(|(_, bytes)| bytes.clone())
            .collect::<Vec<_>>(),
        [vec![1, 2], vec![3, 4]]
    );
}

#[test]
fn blocked_audio_output_stops_encoder_reads_and_pcm_capture() {
    let mut backend = Fake {
        audio_busy: true,
        ..Fake::default()
    };
    backend.audios.push_back(audio(false));
    backend.pcms.push_back(pcm());
    let mut pump = Pump::new(backend);
    pump.tick(true).unwrap();
    pump.tick(true).unwrap();
    assert_eq!(pump.backend.audio_reads, 1);
    assert_eq!(pump.backend.pcm_reads, 0);
    pump.backend.audio_busy = false;
    pump.tick(true).unwrap();
    assert_eq!(pump.backend.audio_sent, [vec![0xf4, 1, 2]]);
    assert_eq!(pump.backend.pcm_sent, 1);
}

#[test]
fn disabled_audio_retries_only_eos_without_polling_capture_or_encoder() {
    let mut backend = Fake {
        audio_busy: true,
        ..Fake::default()
    };
    backend.videos.push_back(video(&[], FLAG_END_OF_STREAM, 0));
    backend.audios.push_back(audio(false));
    backend.pcms.push_back(pcm());
    let mut pump = Pump::with_media(backend, false, 2, CODEC_H264);

    pump.tick(true).unwrap();
    assert!(pump.video_ended);
    assert!(!pump.audio_ended);
    assert_eq!(pump.backend.audio_reads, 0);
    assert_eq!(pump.backend.pcm_reads, 0);
    assert!(pump.backend.audio_sent.is_empty());
    assert_eq!(pump.backend.transport_drains, 0);

    pump.backend.audio_busy = false;
    pump.tick(true).unwrap();
    assert!(pump.audio_ended);
    assert_eq!(pump.backend.audio_reads, 0);
    assert_eq!(pump.backend.pcm_reads, 0);
    assert_eq!(pump.backend.pcm_sent, 0);
    assert_eq!(pump.backend.encoder_drains, 0);
    assert_eq!(pump.backend.audio_sent, [Vec::<u8>::new()]);
    assert_eq!(pump.backend.audio_flags, [AUDIO_PACKET_END_OF_STREAM]);
    assert_eq!(pump.backend.transport_drains, 1);
    assert!(pump.transport_draining);
}

#[test]
fn stop_flushes_accepted_pcm_then_both_eos_before_transport_drain() {
    let mut backend = Fake {
        pcm_busy: true,
        ..Fake::default()
    };
    backend.pcms.push_back(pcm());
    let mut pump = Pump::new(backend);
    pump.tick(true).unwrap();
    pump.stopping = true;
    pump.tick(true).unwrap();
    assert_eq!(pump.backend.pcm_reads, 1);
    assert_eq!(pump.backend.encoder_drains, 0);
    pump.backend.pcm_busy = false;
    pump.tick(true).unwrap();
    assert_eq!(pump.backend.pcm_sent, 1);
    assert_eq!(pump.backend.encoder_drains, 1);
    assert_eq!(pump.backend.transport_drains, 0);
    pump.backend
        .videos
        .push_back(video(&[], FLAG_END_OF_STREAM, 0));
    pump.backend.audios.push_back(audio(true));
    pump.tick(true).unwrap();
    assert!(pump.transport_draining);
    pump.tick(true).unwrap();
    assert_eq!(pump.backend.encoder_drains, 1);
    assert_eq!(pump.backend.transport_drains, 1);
    assert_eq!(pump.backend.pcm_reads, 1);
}

fn av1_video(bytes: &[u8], flags: u32, sequence: u64) -> VideoPacket {
    let packet = EncodedPacket {
        codec: CODEC_AV1,
        flags,
        data: bytes.as_ptr(),
        len: bytes.len(),
        timestamp_us: sequence * 1000,
        sequence,
    };
    // SAFETY: input slice remains initialized for the copy.
    unsafe { VideoPacket::copy(&packet).unwrap() }
}

#[test]
fn av1_fragments_assemble_into_one_temporal_unit_with_the_keyframe_flag() {
    let mut backend = Fake::default();
    backend.videos.extend([
        av1_video(&[0x12, 0x00], FLAG_KEYFRAME, 1),
        av1_video(&[0x0a, 0x01, 0xaa], 0, 1),
        av1_video(&[0x32, 0x01, 0xbb], FLAG_END_OF_ACCESS_UNIT, 1),
    ]);
    let mut pump = Pump::with_media(backend, true, 2, CODEC_AV1);
    let progress = pump.tick(true).unwrap();
    assert_eq!(progress.accepted_video_access_units, 1);
    let (flags, bytes) = &pump.backend.videos_sent[0];
    assert_eq!(*flags, FLAG_KEYFRAME | FLAG_END_OF_ACCESS_UNIT);
    assert_eq!(bytes, &[0x12, 0x00, 0x0a, 0x01, 0xaa, 0x32, 0x01, 0xbb]);
}

#[test]
fn packets_of_another_codec_than_negotiated_fail_before_submission() {
    let mut backend = Fake::default();
    backend
        .videos
        .push_back(video(&[0, 0, 1, 0x65], FLAG_END_OF_ACCESS_UNIT, 1));
    let mut pump = Pump::with_media(backend, true, 2, CODEC_AV1);
    assert_eq!(
        pump.tick(true).err().map(|failure| failure.code),
        Some("video_codec_mismatch")
    );
    assert!(pump.backend.videos_sent.is_empty());
}

#[test]
fn missing_access_unit_boundary_is_a_sticky_failure_never_a_guessed_frame() {
    for next in [
        video(&[2], FLAG_END_OF_ACCESS_UNIT, 2),
        video(&[], FLAG_END_OF_STREAM, 0),
    ] {
        let mut backend = Fake::default();
        backend.videos.extend([video(&[1], 0, 1), next]);
        let mut pump = Pump::new(backend);
        assert_eq!(
            pump.tick(true).err().map(|failure| failure.code),
            Some("video_access_unit_incomplete")
        );
        let reads = pump.backend.video_reads;
        assert_eq!(
            pump.tick(true).err().map(|failure| failure.code),
            Some("video_access_unit_incomplete")
        );
        assert_eq!(pump.backend.video_reads, reads);
        assert!(pump.backend.videos_sent.is_empty());
    }
}

#[test]
fn control_failure_prevents_all_media_polling() {
    let mut pump = Pump::new(Fake {
        control_failed: true,
        ..Fake::default()
    });
    assert_eq!(
        pump.tick(true).err().map(|failure| failure.code),
        Some("control_failed")
    );
    assert_eq!(
        pump.backend.video_reads + pump.backend.audio_reads + pump.backend.pcm_reads,
        0
    );
}

#[test]
fn assembler_fragment_count_and_pcm_shape_are_explicitly_bounded() {
    let mut assembly = VideoAssembly::new(CODEC_H264);
    for _ in 0..1024 {
        assembly.push(video(&[1], 0, 1)).unwrap();
    }
    assert_eq!(
        assembly.push(video(&[1], FLAG_END_OF_ACCESS_UNIT, 1)),
        Err("video_access_unit_limit")
    );
    let original = pcm();
    let mut frame = original.header();
    frame.len -= 1;
    assert_eq!(
        unsafe { PcmPacket::copy(&frame) }.err(),
        Some("invalid_pcm_frame")
    );
}

#[test]
fn timed_fragments_must_agree_on_processing_start_and_survive_busy() {
    let bytes = [1, 2, 3];
    let make = |flags, started| {
        let packet = TimedEncodedPacket {
            base: EncodedPacket {
                codec: CODEC_H264,
                flags,
                data: bytes.as_ptr(),
                len: bytes.len(),
                timestamp_us: 7000,
                sequence: 7,
            },
            processing_started_host_us: started,
        };
        // SAFETY: bytes remain live until copy returns.
        unsafe { VideoPacket::copy_timed(&packet).unwrap() }
    };
    let mut assembly = VideoAssembly::new(CODEC_H264);
    assembly.push(make(0, 6500)).unwrap();
    assert_eq!(
        assembly.push(make(FLAG_END_OF_ACCESS_UNIT, 6501)),
        Err("video_access_unit_incomplete")
    );
    assembly.push(make(FLAG_END_OF_ACCESS_UNIT, 6500)).unwrap();
    assert_eq!(assembly.processing_started_host_us(), 6500);
    assert_eq!(assembly.header().unwrap().timestamp_us, 7000);
    let mut pump = Pump::new(Fake {
        video_busy: true,
        ..Fake::default()
    });
    pump.video = assembly;
    pump.tick(true).unwrap();
    assert!(pump.backend.video_start_times.is_empty());
    assert_eq!(pump.video.processing_started_host_us(), 6500);
    pump.backend.video_busy = false;
    pump.tick(true).unwrap();
    assert_eq!(pump.backend.video_start_times, [6500]);
    let mut assembly = pump.video;
    assembly.clear();
    assert_eq!(assembly.processing_started_host_us(), 0);
}

#[test]
fn contributed_feedback_obeys_shared_queue_busy_and_invalidation_gate() {
    let mut pump = Pump::new(Fake {
        feedback_busy: true,
        ..Fake::default()
    });
    let message = HostFeedbackMessage {
        message_type: 0x6001,
        channel: 0,
        delivery: HostFeedbackDelivery::Reliable,
        payload: vec![7],
    };
    assert_eq!(
        pump.enqueue_message(PendingFeedback {
            message: message.clone(),
            contribution: true
        }),
        Ok(FeedbackEnqueueOutcome::Queued)
    );
    assert_eq!(
        pump.enqueue_feedback(rumble(0, 3, 4)),
        Ok(FeedbackEnqueueOutcome::Queued)
    );
    pump.tick(false).unwrap();
    assert!(pump.backend.feedback_sent.is_empty());
    pump.pump_feedback(true).unwrap();
    assert!(pump.backend.feedback_sent.is_empty());
    pump.backend.feedback_busy = false;
    pump.pump_feedback(false).unwrap();
    assert!(pump.backend.feedback_sent.is_empty());
    pump.pump_feedback(true).unwrap();
    assert_eq!(pump.backend.feedback_sent[0], message);
    assert_eq!(pump.backend.feedback_sent[1].message_type, 0x010b);
}

#[test]
fn contribution_queue_is_bounded_and_stop_discards_unsent_messages() {
    let mut pump = Pump::new(Fake {
        feedback_busy: true,
        ..Fake::default()
    });
    for index in 0..MAX_PENDING_FEEDBACK_MESSAGES {
        let message = HostFeedbackMessage {
            message_type: 0x6001,
            channel: 0,
            delivery: HostFeedbackDelivery::Reliable,
            payload: vec![index as u8],
        };
        assert_eq!(
            pump.enqueue_message(PendingFeedback {
                message,
                contribution: true
            }),
            Ok(FeedbackEnqueueOutcome::Queued)
        );
    }
    let message = HostFeedbackMessage {
        message_type: 0x6001,
        channel: 0,
        delivery: HostFeedbackDelivery::Reliable,
        payload: vec![1],
    };
    assert_eq!(
        pump.enqueue_message(PendingFeedback {
            message,
            contribution: true
        }),
        Err(FeedbackEnqueueError::QueueFull)
    );
    pump.stopping = true;
    pump.cancel_feedback();
    pump.backend.feedback_busy = false;
    pump.pump_feedback(true).unwrap();
    assert!(pump.backend.feedback_sent.is_empty());
}
