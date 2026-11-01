use super::*;
use amaterasu_abi::{AbiHeader, NativeInterface};
use amaterasu_media::{
    AUDIO_CODEC_AC3, AUDIO_FRAMING_AC3_SYNCFRAME, AudioFrame, MEDIA_PENDING,
    NegotiatedAudioPacketSink,
};

struct Provider {
    generation: u64,
    configuration: [u8; 1],
    configure_calls: u32,
    selected: bool,
}

unsafe extern "C" fn enumerate(
    context: *mut c_void,
    _: u64,
    sink: AudioFormatOfferSink,
    generation: *mut u64,
) -> Status {
    // SAFETY: the test table keeps this provider and the output pointer live.
    let provider = unsafe { &*context.cast::<Provider>() };
    let mut format = sample_format();
    format.config = provider.configuration.to_vec();
    let offer = AudioFormatOffer {
        id: 3,
        format: format.descriptor(),
    };
    // SAFETY: the callback borrows the offer and its bytes synchronously.
    unsafe {
        *generation = provider.generation;
        (sink.write)(sink.context, &offer)
    }
}

unsafe extern "C" fn select(context: *mut c_void, stream: u64, generation: u64, id: u64) -> Status {
    // SAFETY: the table retains the provider throughout this test.
    let provider = unsafe { &mut *context.cast::<Provider>() };
    provider.configure_calls += 1;
    if stream != 7 || generation != provider.generation || id != 3 {
        return MEDIA_UNSUPPORTED;
    }
    provider.selected = true;
    STATUS_OK
}

unsafe extern "C" fn submit(_: *mut c_void, _: u64, _: *const AudioFrame) -> Status {
    MEDIA_PENDING
}

unsafe extern "C" fn receive(_: *mut c_void, _: u64, _: NegotiatedAudioPacketSink) -> Status {
    MEDIA_PENDING
}

unsafe extern "C" fn drain(_: *mut c_void, _: u64) -> Status {
    STATUS_OK
}

fn sample_format() -> AudioFormat {
    AudioFormat {
        namespace: [0; 16],
        codec: AUDIO_CODEC_AC3,
        framing: AUDIO_FRAMING_AC3_SYNCFRAME,
        sample_rate: 48_000,
        channels: 2,
        channel_mask: 3,
        frame_samples: 1536,
        config: vec![17],
    }
}

#[test]
fn probing_copies_offer_without_selection_and_stale_commit_fails() {
    let mut provider = Provider {
        generation: 2,
        configuration: [17],
        configure_calls: 0,
        selected: false,
    };
    let api = NegotiatedAudioEncoderApi {
        base: NativeInterface {
            header: AbiHeader {
                size: size_of::<NegotiatedAudioEncoderApi>() as u32,
                major: 1,
                minor: 0,
            },
            context: (&mut provider as *mut Provider).cast(),
            release: None,
        },
        enumerate,
        configure: select,
        submit,
        receive,
        drain,
    };
    // SAFETY: provider and table stay live and are used synchronously.
    let candidate = unsafe { probe(&api, 7, &[sample_format()]) }.unwrap();
    assert_eq!(provider.configure_calls, 0);
    assert!(!provider.selected);
    provider.configuration = [42];
    assert_eq!(candidate.format().config, [17]);
    provider.generation = 3;
    // SAFETY: the same binding and stream are still live.
    assert_eq!(
        unsafe { configure(&api, candidate) },
        Err(AudioNegotiationError::Stale)
    );
    assert!(!provider.selected);
    assert_eq!(provider.configure_calls, 1);

    let mut preferred = sample_format();
    preferred.config = vec![42];
    // SAFETY: provider and table stay live and are used synchronously.
    let candidate = unsafe { probe(&api, 7, &[preferred]) }.unwrap();
    // SAFETY: the candidate is selected on the same binding and live stream.
    assert!(unsafe { configure(&api, candidate) }.is_ok());
    assert!(provider.selected);
}
