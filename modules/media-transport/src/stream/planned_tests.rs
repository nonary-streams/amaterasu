use super::tests::{receivers, request};
use super::*;
use amaterasu_module_sdk::{Command, Response};
use amaterasu_module_sdk::{
    RuntimeClient,
    abi::*,
    encode::{self, EncoderProvider, OutputQueue, ProviderAdapter, SessionMode},
    video_framing::{self, FramingProvider, OwnedFramePlan},
};
use std::{ffi::c_void, mem::MaybeUninit};

unsafe extern "C" {
    fn amaterasu_runtime_v1() -> *const RuntimeApi;
}
const SERVICE: &str = "example.video-framing/1";
const ENCODER_SERVICE: &str = "example.encoder/1";

fn encoder_queue(marker: u8) -> OutputQueue {
    let queue = OutputQueue::new(SessionMode::Source, 4, 1024).unwrap();
    queue
        .push(encode::EncodedPacket {
            flags: ENCODER_CHUNK_ACCESS_UNIT_START
                | ENCODER_CHUNK_ACCESS_UNIT_END
                | ENCODER_CHUNK_KEYFRAME,
            session_id: 5,
            access_unit_id: 1,
            submission_id: 0,
            source_id: 6,
            config_generation: 8,
            timestamp_us: 0,
            codec: encode::FormatId {
                namespace_id: 42,
                value: 3,
                schema: 1,
            },
            framing: encode::FormatId {
                namespace_id: 42,
                value: 4,
                schema: 1,
            },
            bytes: vec![10, 20, marker],
        })
        .unwrap();
    queue
}
struct EncoderOne(OutputQueue);
struct EncoderTwo(OutputQueue);
impl Default for EncoderOne {
    fn default() -> Self {
        Self(encoder_queue(30))
    }
}
impl Default for EncoderTwo {
    fn default() -> Self {
        Self(encoder_queue(31))
    }
}
macro_rules! encoder_provider {
    ($kind:ty) => {
        impl EncoderProvider for $kind {
            fn probe(&self, _: encode::SessionConfig) -> Result<encode::ControlResponse, Status> {
                Err(MEDIA_UNSUPPORTED)
            }
            fn configure(&self, _: u64, _: encode::SessionConfig) -> Result<(), Status> {
                Ok(())
            }
            fn output(&self, session: u64) -> Result<OutputQueue, Status> {
                if session == 5 {
                    Ok(self.0.clone())
                } else {
                    Err(STATUS_INVALID_ARGUMENT)
                }
            }
            fn control(
                &self,
                _: u64,
                _: encode::ControlRequest,
            ) -> Result<encode::ControlResponse, Status> {
                Err(MEDIA_UNSUPPORTED)
            }
            fn drain(&self, _: u64) -> Result<(), Status> {
                Ok(())
            }
            fn destroy(&self, _: u64) {
                self.0.cancel();
            }
            fn release(&self) -> Result<(), Status> {
                self.0.cancel();
                Ok(())
            }
        }
    };
}
encoder_provider!(EncoderOne);
encoder_provider!(EncoderTwo);
unsafe extern "C" fn bind_encoder<P: EncoderProvider + Default>(
    _: *mut c_void,
    principal: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    if query.is_null() || output.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: runtime supplies live query/principal/output through this call.
    let query = unsafe { &*query };
    if unsafe { principal.read() }.ok() != Some(b"consumer")
        || !query.header.supports::<NativeInterfaceQuery>()
        || query.interface_major != 1
        || query.min_minor > 0
        || query.min_size as usize > size_of::<EncoderProviderApi>()
    {
        return STATUS_INVALID_ARGUMENT;
    }
    let table = ProviderAdapter::new(P::default()).into_binding();
    // SAFETY: runtime takes ownership of the complete bound table.
    unsafe {
        *output = table.cast();
    }
    STATUS_OK
}

struct FramerOne;
struct FramerTwo;
fn frame(principal: &str, bytes: &[u8], marker: u8) -> Result<OwnedFramePlan, Status> {
    if principal != "consumer" {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    let mut framed = bytes.to_vec();
    framed.push(marker);
    Ok(OwnedFramePlan {
        framed_bytes: framed,
        record_starts: vec![],
        critical_prefix_shards: 0,
        blocks: vec![VideoFecBlock {
            first_data_shard: 0,
            data_shards: 1,
            parity_shards: 0,
            percentage: 0,
        }],
    })
}
impl FramingProvider for FramerOne {
    fn plan(
        &self,
        principal: &str,
        _: &NegotiatedVideoUnit,
        bytes: &[u8],
        _: &VideoFramingLimits,
    ) -> Result<OwnedFramePlan, Status> {
        frame(principal, bytes, 1)
    }
}
impl FramingProvider for FramerTwo {
    fn plan(
        &self,
        principal: &str,
        _: &NegotiatedVideoUnit,
        bytes: &[u8],
        _: &VideoFramingLimits,
    ) -> Result<OwnedFramePlan, Status> {
        frame(principal, bytes, 2)
    }
}
struct Output<'a> {
    stream: &'a mut Stream,
    unit: &'a NegotiatedVideoUnit,
}
unsafe extern "C" fn submit(context: *mut c_void, plan: *const VideoFramePlan) -> Status {
    // SAFETY: test keeps stream/unit live; the framer holds plan through callback.
    let state = unsafe { &mut *context.cast::<Output<'_>>() };
    unsafe { state.stream.submit_planned_video(state.unit, &*plan) }
}
struct EncoderBridge<'a> {
    stream: &'a mut Stream,
    framer: &'a VideoFramingApi,
    limits: VideoFramingLimits,
}
unsafe extern "C" fn encode_output(
    context: *mut c_void,
    chunk: *const EncoderOutputChunk,
) -> Status {
    if context.is_null() || chunk.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: encoder supplies a live chunk and callback context until return.
    let (state, chunk) = unsafe { (&mut *context.cast::<EncoderBridge<'_>>(), &*chunk) };
    if chunk.flags
        != ENCODER_CHUNK_ACCESS_UNIT_START | ENCODER_CHUNK_ACCESS_UNIT_END | ENCODER_CHUNK_KEYFRAME
    {
        return STATUS_INVALID_ARGUMENT;
    }
    let unit = NegotiatedVideoUnit {
        size: size_of::<NegotiatedVideoUnit>() as u32,
        version: 1,
        stream: chunk.session_id,
        generation: 7,
        offer_id: 9,
        codec: chunk.codec,
        framing: chunk.framing,
        access_unit_id: chunk.access_unit_id,
        submission_id: chunk.submission_id,
        source_id: chunk.source_id,
        config_generation: chunk.config_generation,
        timestamp_us: chunk.timestamp_us,
        clock: FrameClockMetadata {
            version: 1,
            size: size_of::<FrameClockMetadata>() as u32,
            domain: FRAME_CLOCK_STREAM_RELATIVE,
            reserved: 0,
            epoch_id: [1; 16],
        },
        flags: VIDEO_UNIT_KEYFRAME,
        reserved: 0,
        data: chunk.bytes,
    };
    let mut output = Output {
        stream: state.stream,
        unit: &unit,
    };
    let sink = VideoFramePlanSink {
        context: (&mut output as *mut Output<'_>).cast(),
        write: submit,
    };
    // SAFETY: both bound provider and unit/plan callback state remain live.
    unsafe { (state.framer.plan)(state.framer.base.context, 5, &unit, &state.limits, sink) }
}

fn exercise<E: EncoderProvider + Default, P: FramingProvider>(
    framer: P,
    marker: u8,
    encoded_marker: u8,
) {
    // Referencing the crate ensures the runtime API symbol is linked.
    std::hint::black_box(size_of::<amaterasu_runtime::Broker>());
    let policy = serde_json::json!({
        "principals": {
            "provider": {"native_provide":[SERVICE, ENCODER_SERVICE]},
            "consumer": {"native_use":[SERVICE, ENCODER_SERVICE]}
        },
        "service_selections": {"native": {SERVICE:{"provider":"provider","contract":video_framing::FRAMING_CONTRACT}, ENCODER_SERVICE:{"provider":"provider","contract":amaterasu_module_sdk::media::native_contracts::ENCODER_PROVIDER}}}
    });
    let policy = serde_json::to_vec(&policy).unwrap();
    // SAFETY: static API table and runtime handle remain live until explicit destroy.
    let runtime_api = unsafe { &*amaterasu_runtime_v1() };
    let mut runtime = std::ptr::null_mut();
    assert_eq!(
        unsafe { (runtime_api.create)(BorrowedBytes::new(&policy), &mut runtime) },
        STATUS_OK
    );
    let endpoint = |name: &str| {
        let mut output = MaybeUninit::uninit();
        assert_eq!(
            unsafe {
                (runtime_api.endpoint)(
                    runtime,
                    BorrowedBytes::new(name.as_bytes()),
                    output.as_mut_ptr(),
                )
            },
            STATUS_OK
        );
        unsafe { RuntimeClient::from_endpoint(output.assume_init()) }
    };
    {
        let provider = endpoint("provider");
        let consumer = endpoint("consumer");
        let encoder_factory = NativeServiceFactory {
            header: AbiHeader::of::<NativeServiceFactory>(),
            context: std::ptr::null_mut(),
            bind: Some(bind_encoder::<E>),
        };
        unsafe {
            provider.register_native_typed(
                ENCODER_SERVICE,
                amaterasu_module_sdk::media::native_contracts::ENCODER_PROVIDER,
                &encoder_factory,
            )
        }
        .unwrap();
        let encoder_binding = unsafe {
            consumer.bind_native_typed(
                ENCODER_SERVICE,
                amaterasu_module_sdk::media::native_contracts::ENCODER_PROVIDER,
                1,
                0,
                size_of::<EncoderProviderApi>() as u32,
            )
        }
        .unwrap();
        let encoder = unsafe { encoder_binding.table::<EncoderProviderApi>() }.unwrap();
        let factory = video_framing::Factory::new(framer);
        unsafe { video_framing::register(&provider, SERVICE, &factory) }.unwrap();
        let binding = unsafe { video_framing::bind(&consumer, SERVICE) }.unwrap();
        let api = unsafe { binding.table::<VideoFramingApi>() }.unwrap();
        let (video, audio) = receivers();
        let mut configuration = request(&video, &audio, false);
        configuration.video.fec_percentage = 0;
        configuration.video.min_parity_shards = 0;
        let mut stream = Stream::open("owner", &configuration).unwrap();
        let codec = EncoderFormatId {
            namespace_id: 42,
            value: 3,
            schema: 1,
        };
        let framing = EncoderFormatId {
            namespace_id: 42,
            value: 4,
            schema: 1,
        };
        let limits = VideoFramingLimits {
            size: size_of::<VideoFramingLimits>() as u32,
            version: 1,
            packet_size: 1024,
            crypto_overhead: 0,
            network_overhead: 28,
            allowed_extensions: 0,
            max_datagrams: 8,
            max_queue_bytes: 8192,
            min_parity_shards: 0,
            max_fec_percentage: 0,
            max_wire_bytes: 8192,
        };
        stream
            .configure_planned_video(PlannedVideoSelection {
                stream: 5,
                generation: 7,
                offer_id: 9,
                codec,
                framing,
                source_id: 6,
                config_generation: 8,
                clock_epoch: [1; 16],
                clock_origin_us: host_monotonic_micros().max(1),
                limits,
            })
            .unwrap();
        let mut bridge = EncoderBridge {
            stream: &mut stream,
            framer: api,
            limits,
        };
        let sink = EncoderOutputSink {
            context: (&mut bridge as *mut EncoderBridge<'_>).cast(),
            write: encode_output,
        };
        assert_eq!(
            unsafe { (encoder.receive)(encoder.base.context, 5, 1024, sink) },
            STATUS_OK
        );
        bridge.stream.delivery.tick();
        let mut datagram = [0u8; 2048];
        let length = video.recv(&mut datagram).unwrap();
        assert!(length >= 44);
        assert_eq!(&datagram[2..4], &[0, 0]);
        assert_eq!(&datagram[40..44], &[10, 20, encoded_marker, marker]);
        assert!(matches!(
            consumer.send(Command::GetSettings {}),
            Ok(Response::Result { .. })
        ));
        drop(binding);
        drop(encoder_binding);
    }
    unsafe { (runtime_api.destroy)(runtime) };
}
#[test]
fn selected_runtime_framers_feed_the_same_transport_over_loopback() {
    exercise::<EncoderOne, _>(FramerOne, 1, 30);
    exercise::<EncoderTwo, _>(FramerTwo, 2, 31);
}
