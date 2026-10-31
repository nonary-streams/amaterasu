use amaterasu_media::{
    AUDIO_CODEC_AC3, AUDIO_CODEC_EAC3, AUDIO_FRAMING_AC3_SYNCFRAME, AUDIO_FRAMING_EAC3_SYNCFRAME,
    NegotiatedAudioEncoderApi,
};
use libloading::Library;

#[test]
#[ignore = "requires separately built consumer and provider artifacts"]
fn unchanged_consumer_loads_independent_audio_provider_artifacts() {
    let consumer_path = std::env::var("AUDIO_FIXTURE_CONSUMER").unwrap();
    let ac3_path = std::env::var("AUDIO_FIXTURE_AC3").unwrap();
    let eac3_path = std::env::var("AUDIO_FIXTURE_EAC3").unwrap();
    // SAFETY: the paths name our separately built fixture libraries. Every symbol
    // has the declared C signature, and libraries stay loaded until after destroy.
    unsafe {
        let consumer = Library::new(consumer_path).unwrap();
        let run: libloading::Symbol<
            unsafe extern "C" fn(*const NegotiatedAudioEncoderApi, u32, u32, u8) -> i32,
        > = consumer.get(b"fixture_consume\0").unwrap();
        for (path, codec, framing, marker) in [
            (ac3_path, AUDIO_CODEC_AC3, AUDIO_FRAMING_AC3_SYNCFRAME, 0xa1),
            (
                eac3_path,
                AUDIO_CODEC_EAC3,
                AUDIO_FRAMING_EAC3_SYNCFRAME,
                0xe2,
            ),
        ] {
            let provider = Library::new(path).unwrap();
            let create: libloading::Symbol<
                unsafe extern "C" fn() -> *mut NegotiatedAudioEncoderApi,
            > = provider.get(b"fixture_create\0").unwrap();
            let destroy: libloading::Symbol<unsafe extern "C" fn(*mut NegotiatedAudioEncoderApi)> =
                provider.get(b"fixture_destroy\0").unwrap();
            let table = create();
            assert!(!table.is_null());
            assert_eq!(run(table, codec, framing, marker), 0);
            destroy(table);
        }
    }
}
