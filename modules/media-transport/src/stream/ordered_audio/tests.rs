use super::*;
use crate::stream::tests::{receivers, request};

pub(crate) fn selection(codec: u32, framing: u32, bytes: u32) -> AudioTransportSelectionV2 {
    AudioTransportSelectionV2 {
        size: size_of::<AudioTransportSelectionV2>() as u32,
        version: AUDIO_ORDERED_VERSION,
        stream: 5,
        generation: 7,
        offer_id: 9,
        clock_epoch: [1; 16],
        format: AudioFormatDescriptorV2 {
            size: size_of::<AudioFormatDescriptorV2>() as u32,
            version: AUDIO_ORDERED_VERSION,
            format: AudioFormatDescriptor {
                size: size_of::<AudioFormatDescriptor>() as u32,
                version: AUDIO_FORMAT_VERSION,
                namespace: [0; 16],
                codec,
                framing,
                sample_rate: 48_000,
                channels: 2,
                channel_mask: 3,
                frame_samples: 1536,
                reserved: 0,
                config: BorrowedBytes::new(&[]),
            },
            layout: AudioChannelLayoutV2 {
                size: size_of::<AudioChannelLayoutV2>() as u32,
                version: AUDIO_ORDERED_VERSION,
                channels: 2,
                reserved: 0,
                mask: 3,
                lanes: [1, 2, 0, 0, 0, 0, 0, 0],
            },
            fixed_packet_bytes: bytes,
            reserved: 0,
        },
    }
}

pub(crate) fn packet(
    data: &[u8],
    sequence: u64,
    timestamp_us: u64,
    flags: u32,
) -> NegotiatedAudioPacket {
    NegotiatedAudioPacket {
        stream: 5,
        generation: 7,
        offer_id: 9,
        flags,
        frames: 1536,
        timestamp_us,
        sequence,
        data: BorrowedBytes::new(data),
    }
}

fn receive(socket: &std::net::UdpSocket) -> Vec<u8> {
    let mut bytes = vec![0; 8192];
    let count = socket.recv(&mut bytes).unwrap();
    bytes.truncate(count);
    bytes
}

#[test]
fn selected_formats_send_real_udp_and_preserve_source_time_at_discontinuity() {
    for (codec, framing) in [
        (AUDIO_CODEC_AC3, AUDIO_FRAMING_AC3_SYNCFRAME),
        (AUDIO_CODEC_EAC3, AUDIO_FRAMING_EAC3_SYNCFRAME),
    ] {
        for encrypted in [false, true] {
            let (video, audio) = receivers();
            let mut stream = Stream::open("owner", &request(&video, &audio, encrypted)).unwrap();
            let selected = selection(codec, framing, 2048);
            let mut variable = selected;
            variable.format.fixed_packet_bytes = 0;
            assert!(stream.probe_ordered_audio(&variable).is_err());
            assert!(stream.probe_ordered_audio(&selected).is_ok());
            assert_eq!(stream.audio.next_sequence(), 0);
            stream.prepare_ordered_audio(&selected).unwrap();
            let bytes = vec![0x55; 2048];
            let first = packet(&bytes, 10, 1_000_000, 0);
            let mut stale = first;
            stale.generation += 1;
            // SAFETY: packet and bytes stay live through each synchronous submission.
            assert_eq!(
                unsafe { stream.submit_ordered_audio(&stale) },
                STATUS_INVALID_ARGUMENT
            );
            assert_eq!(unsafe { stream.submit_ordered_audio(&first) }, STATUS_OK);
            stream.delivery.tick();
            let output = receive(&audio);
            assert_eq!(&output[2..4], &0u16.to_be_bytes());
            assert_eq!(&output[4..8], &0u32.to_be_bytes());
            assert_eq!(output.len(), 12 + if encrypted { 2064 } else { 2048 });
            if !encrypted {
                assert_eq!(&output[12..], &bytes);
            }
            let wrong_time = packet(&bytes, 11, 1_031_000, 0);
            assert_eq!(
                unsafe { stream.submit_ordered_audio(&wrong_time) },
                STATUS_INVALID_ARGUMENT
            );
            let second = packet(&bytes, 11, 1_032_000, AUDIO_PACKET_DISCONTINUITY);
            assert_eq!(unsafe { stream.submit_ordered_audio(&second) }, STATUS_OK);
            assert_eq!(stream.discarded_audio_fec_shards, 1);
            stream.delivery.tick();
            let output = receive(&audio);
            assert_eq!(&output[2..4], &4u16.to_be_bytes());
            assert_eq!(&output[4..8], &32u32.to_be_bytes());
            for index in 0..3 {
                let next = packet(&bytes, 12 + index, 1_064_000 + index * 32_000, 0);
                assert_eq!(unsafe { stream.submit_ordered_audio(&next) }, STATUS_OK);
            }
            stream.delivery.tick();
            for index in 0..3 {
                let output = receive(&audio);
                assert_eq!(&output[2..4], &(5u16 + index).to_be_bytes());
                assert_eq!(
                    &output[4..8],
                    &(64u32 + u32::from(index) * 32).to_be_bytes()
                );
            }
            for parity in 0..2 {
                let output = receive(&audio);
                assert_eq!(output[1], 127);
                assert_eq!(output[12], parity);
                assert_eq!(&output[14..16], &4u16.to_be_bytes());
                assert_eq!(&output[16..20], &32u32.to_be_bytes());
            }
        }
    }
}

#[test]
fn busy_does_not_consume_the_selected_packet_or_clock() {
    let (video, audio) = receivers();
    let mut stream = Stream::open("owner", &request(&video, &audio, false)).unwrap();
    stream
        .prepare_ordered_audio(&selection(AUDIO_CODEC_AC3, AUDIO_FRAMING_AC3_SYNCFRAME, 4))
        .unwrap();
    stream
        .delivery
        .push_video(1, vec![vec![0; 8]; MAX_DATAGRAMS]);
    let bytes = [1, 2, 3, 4];
    let packet = packet(&bytes, 1, 400_000, 0);
    // SAFETY: owned bytes and header remain live through retries.
    assert_eq!(unsafe { stream.submit_ordered_audio(&packet) }, MEDIA_BUSY);
    assert_eq!(stream.audio.next_sequence(), 0);
    assert!(stream.ordered_audio.unwrap().previous_us.is_none());
    for _ in 0..1024 {
        if stream.delivery.queued() == 0 {
            break;
        }
        stream.delivery.tick();
    }
    assert_eq!(stream.delivery.queued(), 0);
    assert_eq!(unsafe { stream.submit_ordered_audio(&packet) }, STATUS_OK);
    stream.delivery.tick();
    let output = receive(&audio);
    assert_eq!(&output[2..4], &0u16.to_be_bytes());
    assert_eq!(&output[4..8], &0u32.to_be_bytes());
    assert_eq!(&output[12..], &bytes);
}
