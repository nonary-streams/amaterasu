use super::*;
use amaterasu_gamestream::video::{VideoConfig, VideoPacketizer};
use amaterasu_module_sdk::media_abi::*;
use amaterasu_module_sdk::video_framing::{self, FramingProvider, OwnedFramePlan};

const SERVICE: &str = "example.video-framing/1";
struct First;
struct Second;
fn make_plan(principal: &str, bytes: &[u8], marker: u8) -> Result<OwnedFramePlan, Status> {
    if principal != "consumer" {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    let mut output = bytes.to_vec();
    output.push(marker);
    Ok(OwnedFramePlan {
        framed_bytes: output,
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
impl FramingProvider for First {
    fn plan(
        &self,
        principal: &str,
        _: &NegotiatedVideoUnit,
        bytes: &[u8],
        _: &VideoFramingLimits,
    ) -> Result<OwnedFramePlan, Status> {
        make_plan(principal, bytes, 1)
    }
}
impl FramingProvider for Second {
    fn plan(
        &self,
        principal: &str,
        _: &NegotiatedVideoUnit,
        bytes: &[u8],
        _: &VideoFramingLimits,
    ) -> Result<OwnedFramePlan, Status> {
        make_plan(principal, bytes, 2)
    }
}
struct Output {
    busy: bool,
    seen: Vec<u8>,
    calls: usize,
    packetizer: VideoPacketizer,
    limits: VideoFramingLimits,
    datagrams: Vec<Vec<u8>>,
}
unsafe extern "C" fn receive(context: *mut c_void, plan: *const VideoFramePlan) -> Status {
    // SAFETY: test caller owns output and provider holds plan bytes for this callback.
    let (state, plan) = unsafe { (&mut *context.cast::<Output>(), &*plan) };
    state.calls += 1;
    if state.busy {
        return MEDIA_BUSY;
    }
    // SAFETY: provider owns the immutable plan through this synchronous callback.
    let bytes = unsafe { plan.framed_bytes.read() }.unwrap();
    state.seen.extend_from_slice(bytes);
    // SAFETY: provider retains plan bytes through this synchronous callback.
    match unsafe {
        state
            .packetizer
            .packetize_external_plan(plan, &state.limits, true, 1, 90_000, 0)
    } {
        Ok(datagrams) => {
            state.datagrams = datagrams;
            STATUS_OK
        }
        Err(_) => STATUS_INVALID_ARGUMENT,
    }
}
fn exercise<P: FramingProvider>(provider_impl: P, marker: u8) {
    let config = RuntimeConfig {
        principals: [
            (
                "provider".into(),
                Grants {
                    native_provide: [SERVICE.into()].into(),
                    ..Default::default()
                },
            ),
            (
                "consumer".into(),
                Grants {
                    native_use: [SERVICE.into()].into(),
                    ..Default::default()
                },
            ),
            ("intruder".into(), Grants::default()),
        ]
        .into(),
        ..Default::default()
    };
    let bytes = serde_json::to_vec(&config).unwrap();
    let mut runtime = std::ptr::null_mut();
    assert_eq!(
        unsafe { create_runtime(BorrowedBytes::new(&bytes), &mut runtime) },
        STATUS_OK
    );
    {
        let provider = unsafe { RuntimeClient::from_endpoint(endpoint(runtime, "provider")) };
        let consumer = unsafe { RuntimeClient::from_endpoint(endpoint(runtime, "consumer")) };
        let intruder = unsafe { RuntimeClient::from_endpoint(endpoint(runtime, "intruder")) };
        let factory = video_framing::Factory::new(provider_impl);
        unsafe { video_framing::register(&provider, SERVICE, &factory) }.unwrap();
        assert!(unsafe { video_framing::bind(&intruder, SERVICE) }.is_err());
        let binding = unsafe { video_framing::bind(&consumer, SERVICE) }.unwrap();
        let api = unsafe { binding.table::<VideoFramingApi>() }.unwrap();
        let bytes = [10, 20, 30];
        let id = EncoderFormatId {
            namespace_id: 42,
            value: 7,
            schema: 1,
        };
        let unit = NegotiatedVideoUnit {
            size: size_of::<NegotiatedVideoUnit>() as u32,
            version: VIDEO_UNIT_VERSION,
            stream: 5,
            generation: 1,
            offer_id: 1,
            codec: id,
            framing: id,
            access_unit_id: 1,
            submission_id: 1,
            source_id: 2,
            config_generation: 1,
            timestamp_us: 0,
            clock: FrameClockMetadata {
                version: 1,
                size: size_of::<FrameClockMetadata>() as u32,
                domain: FRAME_CLOCK_STREAM_RELATIVE,
                reserved: 0,
                epoch_id: [1; 16],
            },
            flags: VIDEO_UNIT_KEYFRAME,
            reserved: 0,
            data: BorrowedBytes::new(&bytes),
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
        let mut output = Output {
            busy: true,
            seen: Vec::new(),
            calls: 0,
            packetizer: VideoPacketizer::new(VideoConfig {
                packet_size: 1024,
                fec_percentage: 0,
                min_parity_shards: 0,
                max_fec_blocks: 1,
                encrypted: false,
            })
            .unwrap(),
            limits,
            datagrams: Vec::new(),
        };
        let sink = VideoFramePlanSink {
            context: (&mut output as *mut Output).cast(),
            write: receive,
        };
        assert_eq!(
            unsafe { (api.plan)(api.base.context, 5, &unit, &limits, sink) },
            MEDIA_BUSY
        );
        assert!(output.seen.is_empty());
        output.busy = false;
        assert_eq!(
            unsafe { (api.plan)(api.base.context, 5, &unit, &limits, sink) },
            STATUS_OK
        );
        assert_eq!(output.seen, [10, 20, 30, marker]);
        assert_eq!(output.datagrams.len(), 1);
        assert_eq!(&output.datagrams[0][2..4], &[0, 0]);
        assert_eq!(&output.datagrams[0][40..44], &[10, 20, 30, marker]);
        assert_eq!(output.calls, 2);
        assert_eq!(
            unsafe { (api.plan)(api.base.context, 9, &unit, &limits, sink) },
            STATUS_INVALID_ARGUMENT
        );
        drop(binding);
    }
    unsafe { destroy_runtime(runtime) };
}
#[test]
fn runtime_selects_two_framers_with_retry_and_grants() {
    exercise(First, 1);
    exercise(Second, 2);
}
