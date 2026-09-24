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
    fn video(&mut self) -> PlaneResult<Option<VideoPacket>> {
        self.video_reads += 1;
        Ok(self.videos.pop_front())
    }
    fn pcm(&mut self) -> PlaneResult<Option<PcmPacket>> {
        self.pcm_reads += 1;
        Ok(self.pcms.pop_front())
    }
    fn audio(&mut self) -> PlaneResult<Option<AudioPacket>> {
        self.audio_reads += 1;
        Ok(self.audios.pop_front())
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
    let mut pump = Pump::with_audio(backend, false);

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

#[test]
fn missing_access_unit_boundary_is_a_sticky_failure_never_a_guessed_frame() {
    for next in [
        video(&[2], FLAG_END_OF_ACCESS_UNIT, 2),
        video(&[], FLAG_END_OF_STREAM, 0),
    ] {
        let mut backend = Fake::default();
        backend.videos.extend([video(&[1], 0, 1), next]);
        let mut pump = Pump::new(backend);
        assert_eq!(pump.tick(true).err(), Some("video_access_unit_incomplete"));
        let reads = pump.backend.video_reads;
        assert_eq!(pump.tick(true).err(), Some("video_access_unit_incomplete"));
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
    assert_eq!(pump.tick(true).err(), Some("control_failed"));
    assert_eq!(
        pump.backend.video_reads + pump.backend.audio_reads + pump.backend.pcm_reads,
        0
    );
}

#[test]
fn assembler_fragment_count_and_pcm_format_are_explicitly_bounded() {
    let mut assembly = VideoAssembly::default();
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
