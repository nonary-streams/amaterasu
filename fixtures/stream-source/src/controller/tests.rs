use super::*;
use amaterasu_module_sdk::abi::STATUS_INVALID_ARGUMENT;
use std::ffi::c_void;

fn open(kind: Kind) -> Value {
    match kind {
        Kind::Video => {
            json!({"action": "open", "output": "synthetic-display", "packet_boundaries": true, "operation_id": "open"})
        }
        Kind::Audio => json!({"action": "open", "endpoint": "default", "operation_id": "open"}),
    }
}

#[test]
fn geometry_is_explicit_and_invalidation_stops_native_delivery() {
    let mut disabled = Controller::default();
    let (reply, _) = disabled
        .handle("owner", Kind::Video.service(), open(Kind::Video))
        .unwrap();
    assert!(reply.get("desktop_geometry").is_none());
    assert!(
        disabled
            .handle(
                "tester",
                crate::METRICS,
                json!({"action": "invalidate_geometry"})
            )
            .is_err()
    );

    let mut enabled = Controller::new(true);
    let (reply, created) = enabled
        .handle("owner", Kind::Video.service(), open(Kind::Video))
        .unwrap();
    let id = created.unwrap().1;
    assert_eq!(reply["desktop_geometry"]["output"]["left"], -1920);
    assert_eq!(reply["desktop_geometry"]["virtual_desktop"]["top"], -200);
    enabled
        .handle(
            "tester",
            crate::METRICS,
            json!({"action": "invalidate_geometry"}),
        )
        .unwrap();
    let mut output = Output::default();
    let sink = PacketSink {
        context: (&mut output as *mut Output).cast(),
        write: video_sink,
    };
    // SAFETY: the sink context remains live and initialized for this call.
    assert_eq!(
        unsafe { enabled.video("owner", id, sink) },
        MEDIA_BACKEND_ERROR
    );
    assert!(output.packets.is_empty());
    assert_eq!(
        enabled.metadata("owner", Kind::Video, id).unwrap()["state"],
        "failed"
    );
    enabled.close("owner", Kind::Video, id).unwrap();
    assert_eq!(enabled.metrics()["video"]["active"], 0);
}

#[derive(Default)]
struct Output {
    packets: Vec<(Vec<u8>, u32, u64, u64)>,
    busy: bool,
}

unsafe extern "C" fn video_sink(context: *mut c_void, packet: *const EncodedPacket) -> Status {
    // SAFETY: test provides initialized state; provider lends live packet bytes.
    let output = unsafe { &mut *context.cast::<Output>() };
    let packet = unsafe { &*packet };
    output.packets.push((
        unsafe { packet.bytes().unwrap() }.to_vec(),
        packet.flags,
        packet.sequence,
        packet.timestamp_us,
    ));
    if output.busy { MEDIA_BUSY } else { STATUS_OK }
}
unsafe extern "C" fn audio_sink(context: *mut c_void, packet: *const AudioFrame) -> Status {
    // SAFETY: initialized output and PCM allocation live for the callback.
    let output = unsafe { &mut *context.cast::<Output>() };
    let packet = unsafe { &*packet };
    output.packets.push((
        unsafe { packet.bytes().unwrap() }.to_vec(),
        packet.flags,
        packet.sequence,
        packet.timestamp_us,
    ));
    if output.busy { MEDIA_BUSY } else { STATUS_OK }
}

#[test]
fn operation_replay_is_owned_and_does_not_increment_opens() {
    let mut state = Controller::default();
    let (reply, created) = state
        .handle("alice", Kind::Video.service(), open(Kind::Video))
        .unwrap();
    let id = created.unwrap().1;
    assert_eq!(
        state
            .handle("alice", Kind::Video.service(), open(Kind::Video))
            .unwrap()
            .0,
        reply
    );
    assert_eq!(state.metrics()["video"]["opened"], 1);
    assert_eq!(state.close("bob", Kind::Video, id), Err("stream_not_found"));
    assert!(
        state
            .handle(
                "bob",
                Kind::Video.service(),
                json!({"action": "lookup", "operation_id": "open"})
            )
            .is_err()
    );
    state.close("alice", Kind::Video, id).unwrap();
    assert!(
        state
            .handle("alice", Kind::Video.service(), open(Kind::Video))
            .is_err()
    );
    assert_eq!(state.metrics()["video"]["active"], 0);
    assert_eq!(state.metrics()["video"]["closed"], 1);
}

#[test]
fn fragmented_video_is_finite_retries_exactly_and_eos_waits_for_drain() {
    let mut state = Controller::default();
    let id = state
        .handle("alice", Kind::Video.service(), open(Kind::Video))
        .unwrap()
        .1
        .unwrap()
        .1;
    let mut output = Output {
        busy: true,
        ..Output::default()
    };
    let sink = PacketSink {
        context: (&mut output as *mut Output).cast(),
        write: video_sink,
    };
    // SAFETY: sink state stays live and is borrowed only during each call.
    unsafe {
        assert_eq!(state.video("bob", id, sink), MEDIA_CLOSED);
        assert_eq!(state.video("alice", id, sink), MEDIA_BUSY);
        output.busy = false;
        assert_eq!(state.video("alice", id, sink), STATUS_OK);
        assert_eq!(output.packets[0], output.packets[1]);
        output.packets.remove(0);
        for _ in 1..16 {
            assert_eq!(state.video("alice", id, sink), STATUS_OK);
        }
        assert_eq!(state.video("alice", id, sink), MEDIA_PENDING);
        for sequence in 0..8usize {
            let first = &output.packets[sequence * 2];
            let last = &output.packets[sequence * 2 + 1];
            assert_eq!((first.1, last.1), (FLAG_KEYFRAME, FLAG_END_OF_ACCESS_UNIT));
            assert_eq!(
                (first.2, last.2, first.3),
                (sequence as u64, sequence as u64, sequence as u64 * 10000)
            );
            assert_eq!(
                [first.0.as_slice(), last.0.as_slice()].concat(),
                video_bytes(sequence as u64)
            );
        }
        state
            .handle(
                "alice",
                Kind::Video.service(),
                json!({"action": "drain", "stream": id}),
            )
            .unwrap();
        assert_eq!(state.video("alice", id, sink), STATUS_OK);
        assert_eq!(output.packets.last().unwrap().1, FLAG_END_OF_STREAM);
        assert_eq!(state.video("alice", id, sink), MEDIA_PENDING);
    }
    assert_eq!(state.metrics()["video"]["frames"], 8);
}

#[test]
fn pcm_rejection_retains_samples_and_format_failure_endpoint_is_explicit() {
    let mut state = Controller::default();
    let (reply, created) = state
        .handle("alice", Kind::Audio.service(), open(Kind::Audio))
        .unwrap();
    assert_eq!(reply["endpoint"], "synthetic-render");
    let id = created.unwrap().1;
    let mut output = Output {
        busy: true,
        ..Output::default()
    };
    let sink = AudioSink {
        context: (&mut output as *mut Output).cast(),
        write: audio_sink,
    };
    unsafe {
        assert_eq!(state.audio("alice", id, sink), MEDIA_BUSY);
        output.busy = false;
        assert_eq!(state.audio("alice", id, sink), STATUS_OK);
        assert_eq!(output.packets[0], output.packets[1]);
        for _ in 1..8 {
            assert_eq!(state.audio("alice", id, sink), STATUS_OK);
        }
        assert_eq!(state.audio("alice", id, sink), MEDIA_PENDING);
    }
    assert_eq!(state.metrics()["audio"]["frames"], 8);
    let (reply, _) = state
        .handle(
            "alice",
            Kind::Audio.service(),
            json!({"action": "open", "endpoint": "synthetic-unsupported-format"}),
        )
        .unwrap();
    assert_eq!(reply["format"]["sample_rate"], 44100);
    assert_eq!(reply["endpoint"], "synthetic-unsupported-format");
}

#[test]
fn native_binding_copies_caller_and_reentrant_read_returns_busy() {
    use crate::native::Factory;
    use amaterasu_module_sdk::abi::*;
    use std::sync::{Arc, Mutex};
    let mut state = Controller::default();
    let id = state
        .handle("alice", Kind::Video.service(), open(Kind::Video))
        .unwrap()
        .1
        .unwrap()
        .1;
    let shared = Arc::new(Mutex::new(state));
    let factory = Factory::new(shared.clone(), Kind::Video);
    let query = NativeInterfaceQuery {
        header: AbiHeader::of::<NativeInterfaceQuery>(),
        interface_major: 1,
        min_minor: 0,
        min_size: size_of::<VideoApi>() as u32,
    };
    let mut interface = std::ptr::null();
    let caller = String::from("alice");
    unsafe {
        assert_eq!(
            factory.api.bind.unwrap()(
                factory.api.context,
                BorrowedBytes::new(caller.as_bytes()),
                &query,
                &mut interface
            ),
            STATUS_OK
        );
        drop(caller);
        let api = &*interface.cast::<VideoApi>();
        let sink = PacketSink {
            context: std::ptr::null_mut(),
            write: reject,
        };
        let lock = shared.lock().unwrap();
        assert_eq!((api.receive)(api.base.context, id, sink), MEDIA_BUSY);
        drop(lock);
        assert_eq!(
            (api.receive)(api.base.context, id, sink),
            STATUS_INVALID_ARGUMENT
        );
        (api.base.release.unwrap())(api.base.context);
    }
    assert_eq!(Arc::strong_count(&shared), 2);
}
unsafe extern "C" fn reject(_: *mut c_void, _: *const EncodedPacket) -> Status {
    STATUS_INVALID_ARGUMENT
}
