use super::*;
use std::collections::VecDeque;

#[derive(Default)]
struct Fake {
    videos: VecDeque<VideoPacket>,
    pcms: VecDeque<PcmPacket>,
    audios: VecDeque<AudioPacket>,
    video_reads: usize,
    pcm_reads: usize,
    audio_reads: usize,
    video_busy: bool,
    audio_busy: bool,
    pcm_busy: bool,
    control_failed: bool,
    encoder_failed: bool,
    video_failure_after_submit: bool,
    videos_sent: Vec<(u32, Vec<u8>)>,
    audio_sent: Vec<Vec<u8>>,
    audio_flags: Vec<u32>,
    pcm_sent: usize,
    encoder_drains: usize,
    transport_drains: usize,
}

impl Backend for Fake {
    fn control(&mut self) -> PlaneResult<Option<ControlRecord>> {
        if self.control_failed {
            Err("control_failed")
        } else {
            Ok(None)
        }
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
    fn submit_video(&mut self, packet: &EncodedPacket) -> PlaneResult<bool> {
        if self.video_busy {
            return Ok(false);
        }
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
    fn submit_audio(&mut self, packet: &EncodedAudioPacket) -> PlaneResult<bool> {
        if self.audio_busy {
            return Ok(false);
        }
        // SAFETY: pump retains initialized encoded bytes throughout submission.
        self.audio_sent
            .push(unsafe { packet.bytes().unwrap() }.to_vec());
        self.audio_flags.push(packet.flags);
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
    let packet = EncodedAudioPacket {
        codec: CODEC_OPUS,
        flags: if eos { AUDIO_PACKET_END_OF_STREAM } else { 0 },
        sample_rate: 48_000,
        channels: 2,
        frames: if eos { 0 } else { 480 },
        data: bytes.as_ptr(),
        len: if eos { 0 } else { bytes.len() },
        timestamp_us: 0,
        sequence: 0,
    };
    // SAFETY: local encoded bytes are valid throughout the copy.
    unsafe { AudioPacket::copy(&packet).unwrap() }
}

fn pcm() -> PcmPacket {
    let bytes = vec![0; 480 * 2 * 4];
    let frame = AudioFrame {
        sample_rate: 48_000,
        channels: 2,
        format: PCM_F32,
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
            format: PCM_F32,
            frames: 480,
            flags: 0,
            data: pcm_bytes.as_ptr(),
            len: pcm_bytes.len(),
            timestamp_us: 0,
            sequence: 0,
        };
        let encoded_bytes = [0xf4, 1, 2];
        let encoded = EncodedAudioPacket {
            codec: CODEC_OPUS,
            flags: 0,
            sample_rate: 48_000,
            channels,
            frames: 480,
            data: encoded_bytes.as_ptr(),
            len: encoded_bytes.len(),
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
            .push_back(unsafe { AudioPacket::copy(&encoded).unwrap() });
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
fn assembler_fragment_count_and_pcm_format_are_explicitly_bounded() {
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
    frame.sample_rate = 44_100;
    assert_eq!(
        unsafe { PcmPacket::copy(&frame) }.err(),
        Some("unsupported_pcm_format")
    );
}
