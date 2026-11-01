use super::*;
use crate::{AUDIO_CODEC_AC3, AUDIO_FORMAT_VERSION, AUDIO_FRAMING_AC3_SYNCFRAME, PCM_F32};
use amaterasu_abi::BorrowedBytes;

fn layout() -> AudioChannelLayoutV2 {
    AudioChannelLayoutV2 {
        size: size_of::<AudioChannelLayoutV2>() as u32,
        version: AUDIO_ORDERED_VERSION,
        channels: 6,
        reserved: 0,
        mask: 0x3f,
        lanes: [1, 2, 4, 8, 16, 32, 0, 0],
    }
}

#[test]
fn lane_order_is_explicit_and_a_permutation_of_mask() {
    let reference = layout();
    assert_eq!(reference.validate(), Ok(()));
    let mut reordered = reference;
    reordered.lanes.swap(0, 1);
    assert_eq!(reordered.validate(), Ok(()));
    assert_ne!(reference, reordered);
    for lanes in [
        [1, 1, 4, 8, 16, 32, 0, 0],
        [1, 2, 4, 8, 16, 64, 0, 0],
        [1, 2, 4, 8, 16, 0, 0, 0],
        [1, 2, 4, 8, 16, 32, 64, 0],
    ] {
        assert!(
            AudioChannelLayoutV2 { lanes, ..reference }
                .validate()
                .is_err()
        );
    }
}

#[test]
fn fixed_size_offer_and_frame_are_bound_and_validated() {
    let samples = vec![0u8; 6 * 4 * 480];
    let frame = OrderedAudioFrameV2 {
        size: size_of::<OrderedAudioFrameV2>() as u32,
        version: AUDIO_ORDERED_VERSION,
        stream: 5,
        generation: 7,
        frame: AudioFrame {
            sample_rate: 48_000,
            channels: 6,
            format: PCM_F32,
            frames: 480,
            flags: 0,
            data: samples.as_ptr(),
            len: samples.len(),
            timestamp_us: 0,
            sequence: 1,
        },
        layout: layout(),
    };
    assert!(frame.validate().is_ok());
    assert!(
        OrderedAudioFrameV2 {
            generation: 0,
            ..frame
        }
        .validate()
        .is_err()
    );
    let selection = AudioInputSelectionV2 {
        size: size_of::<AudioInputSelectionV2>() as u32,
        version: AUDIO_ORDERED_VERSION,
        stream: 5,
        generation: 7,
        layout: layout(),
    };
    assert!(selection.validate_frame(&frame).is_ok());
    assert!(
        selection
            .validate_frame(&OrderedAudioFrameV2 {
                generation: 8,
                ..frame
            })
            .is_err()
    );
    let mut different_order = frame.layout;
    different_order.lanes.swap(0, 1);
    assert!(
        selection
            .validate_frame(&OrderedAudioFrameV2 {
                layout: different_order,
                ..frame
            })
            .is_err()
    );
    let format = AudioFormatDescriptorV2 {
        size: size_of::<AudioFormatDescriptorV2>() as u32,
        version: AUDIO_ORDERED_VERSION,
        format: AudioFormatDescriptor {
            size: size_of::<AudioFormatDescriptor>() as u32,
            version: AUDIO_FORMAT_VERSION,
            namespace: [0; 16],
            codec: AUDIO_CODEC_AC3,
            framing: AUDIO_FRAMING_AC3_SYNCFRAME,
            sample_rate: 48_000,
            channels: 6,
            channel_mask: 0x3f,
            frame_samples: 1536,
            reserved: 0,
            config: BorrowedBytes::new(&[]),
        },
        layout: layout(),
        fixed_packet_bytes: 4,
        reserved: 0,
    };
    let bytes = [1, 2, 3, 4];
    let packet = NegotiatedAudioPacket {
        stream: 9,
        generation: 10,
        offer_id: 11,
        flags: 0,
        frames: 1536,
        timestamp_us: 0,
        sequence: 1,
        data: BorrowedBytes::new(&bytes),
    };
    assert!(format.validate_packet(&packet, 9, 10, 11).is_ok());
    assert!(format.validate_packet(&packet, 9, 12, 11).is_err());
    assert!(
        AudioFormatDescriptorV2 {
            fixed_packet_bytes: 3,
            ..format
        }
        .validate_packet(&packet, 9, 10, 11)
        .is_err()
    );
    assert!(
        AudioFormatDescriptorV2 {
            fixed_packet_bytes: 0,
            ..format
        }
        .validate_packet(&packet, 9, 10, 11)
        .is_ok()
    );
}

#[cfg(target_pointer_width = "64")]
#[test]
fn ordered_audio_types_match_the_c_header() {
    assert_eq!(size_of::<AudioChannelLayoutV2>(), 88);
    assert_eq!(size_of::<OrderedAudioFrameV2>(), 168);
    assert_eq!(size_of::<AudioInputSelectionV2>(), 112);
    assert_eq!(size_of::<AudioFormatDescriptorV2>(), 176);
    assert_eq!(size_of::<OrderedAudioCaptureApiV2>(), 40);
    assert_eq!(size_of::<OrderedAudioEncoderApiV2>(), 72);
    assert_eq!(size_of::<AudioTransportSelectionV2>(), 224);
    assert_eq!(size_of::<OrderedAudioTransportApiV2>(), 56);
}
