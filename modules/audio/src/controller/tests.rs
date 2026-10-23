use super::*;
use crate::{
    native::Factory,
    worker::{Backend, Opened},
};
use amaterasu_media::*;
use amaterasu_module_sdk::{Command, ManagedModule, Response, RuntimeClient, abi::*};
use std::{
    collections::VecDeque,
    ffi::c_void,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
type OpenCall = (String, Option<String>, Option<u32>, bool);

#[derive(Default)]
struct Observed {
    opens: Mutex<Vec<OpenCall>>,
    capture_formats: Mutex<Vec<Option<PcmCaptureFormat>>>,
    host_audio_settings: Mutex<Vec<bool>>,
    drops: AtomicUsize,
    health: Mutex<Health>,
}

struct FakeBackend {
    observed: Arc<Observed>,
}

impl Backend for FakeBackend {
    fn enumerate(&self) -> AudioResult<Vec<Endpoint>> {
        Ok(vec![Endpoint {
            id: "render.explicit".into(),
            name: "Selected endpoint".into(),
            description: None,
            adapter_name: None,
            default_multimedia: false,
        }])
    }

    fn open(
        &self,
        endpoint: &str,
        virtual_sink: Option<&str>,
        channels: Option<u32>,
        capture_format: Option<PcmCaptureFormat>,
        host_audio_enabled: bool,
        capture_sink_without_routing: bool,
    ) -> AudioResult<Opened> {
        self.observed
            .host_audio_settings
            .lock()
            .unwrap()
            .push(host_audio_enabled);
        self.observed.opens.lock().unwrap().push((
            endpoint.into(),
            virtual_sink.map(str::to_owned),
            channels,
            capture_sink_without_routing,
        ));
        self.observed
            .capture_formats
            .lock()
            .unwrap()
            .push(capture_format);
        let selected = match virtual_sink {
            Some("Studio Virtual") if matches!(channels, Some(2 | 6 | 8)) => "render.virtual",
            None if endpoint == "render.explicit" => "render.explicit",
            None if endpoint == "default" || endpoint.is_empty() => "render.default",
            _ => return Err("endpoint_not_found"),
        };
        Ok(Opened {
            endpoint: selected.into(),
            format: Format::new(
                48_000,
                capture_format.map_or_else(|| channels.unwrap_or(2), |format| format.channels),
                PCM_F32,
            )
            .unwrap(),
            source: Box::new(FakeSource {
                packets: VecDeque::from([Packet {
                    frames: 1,
                    flags: AUDIO_SILENT | AUDIO_DISCONTINUITY,
                    bytes: vec![0; 8],
                    timestamp_us: 100,
                    sequence: 7,
                }]),
                observed: self.observed.clone(),
            }),
        })
    }
}

struct FakeSource {
    packets: VecDeque<Packet>,
    observed: Arc<Observed>,
}

impl Source for FakeSource {
    fn read(&mut self) -> AudioResult<Option<Packet>> {
        if let Some(packet) = self.packets.pop_front() {
            return Ok(Some(packet));
        }
        if let Some(error) = self.health().failure {
            return Err(error);
        }
        Ok(None)
    }

    fn health(&self) -> Health {
        self.observed.health.lock().unwrap().clone()
    }
}

impl Drop for FakeSource {
    fn drop(&mut self) {
        self.observed.drops.fetch_add(1, Ordering::SeqCst);
    }
}

fn controller(enabled: bool) -> (Controller, Arc<Observed>) {
    let observed = Arc::new(Observed::default());
    let mut controller = Controller::new(enabled);
    controller.backend = Box::new(FakeBackend {
        observed: observed.clone(),
    });
    (controller, observed)
}

fn open() -> Operation {
    Operation::Open {
        endpoint: "render.explicit".into(),
        virtual_sink: None,
        channels: None,
        capture_format: None,
        host_audio_enabled: true,
        operation_id: Some("one".into()),
        capture_sink_without_routing: false,
    }
}

#[test]
fn explicit_capture_format_reaches_backend_and_reports_requested_stereo() {
    let (mut controller, observed) = controller(true);
    let operation: Operation = serde_json::from_value(json!({
        "action":"open",
        "endpoint":"render.explicit",
        "host_audio_enabled":true,
        "capture_format":{"sample_rate":48000,"channels":2,"sample_format":"f32"}
    }))
    .unwrap();

    let opened = controller.handle("alice", operation).unwrap();

    assert_eq!(
        observed.capture_formats.lock().unwrap().as_slice(),
        &[Some(PcmCaptureFormat {
            sample_rate: 48_000,
            channels: 2,
            sample_format: PcmSampleFormat::F32,
        })]
    );
    assert_eq!(
        opened.payload["format"],
        json!({"sample_rate":48000,"channels":2,"channel_mask":3,"format":"f32"})
    );
}

#[test]
fn unsupported_requested_capture_format_fails_before_backend_open() {
    for requested in [
        json!({"sample_rate":44100,"channels":2,"sample_format":"f32"}),
        json!({"sample_rate":48000,"channels":2,"sample_format":"i16"}),
        json!({"sample_rate":48000,"channels":4,"sample_format":"f32"}),
    ] {
        let (mut controller, observed) = controller(true);
        let operation: Operation = serde_json::from_value(json!({
            "action":"open",
            "endpoint":"render.explicit",
            "host_audio_enabled":true,
            "capture_format":requested
        }))
        .unwrap();

        assert_eq!(
            controller.handle("alice", operation).err(),
            Some("unsupported_capture_format")
        );
        assert!(observed.capture_formats.lock().unwrap().is_empty());
    }
}

fn continuous_open(packet_ms: u32, operation_id: &str) -> Operation {
    serde_json::from_value(json!({
        "action":"open",
        "endpoint":"render.explicit",
        "host_audio_enabled":true,
        "operation_id":operation_id,
        "continuous_audio":{"packet_ms":packet_ms}
    }))
    .unwrap()
}

#[test]
fn selected_endpoint_open_is_caller_scoped_idempotent_with_stable_endpoint() {
    let (mut controller, observed) = controller(true);
    let opened = controller.handle("alice", open()).unwrap();
    assert_eq!(opened.created, Some(1));
    assert_eq!(opened.payload["endpoint"], "render.explicit");
    assert_eq!(opened.payload["format"]["channel_mask"], 3);
    let repeated = controller.handle("alice", open()).unwrap();
    assert!(repeated.created.is_none());
    assert_eq!(opened.payload, repeated.payload);
    assert_eq!(
        *observed.opens.lock().unwrap(),
        [("render.explicit".into(), None, None, false)]
    );
    assert_eq!(
        controller
            .handle(
                "alice",
                Operation::Lookup {
                    operation_id: "one".into()
                }
            )
            .unwrap()
            .payload,
        opened.payload
    );
    assert!(
        controller
            .handle(
                "bob",
                Operation::Lookup {
                    operation_id: "one".into()
                }
            )
            .is_err()
    );
    assert!(controller.close("bob", 1).is_err());
    assert_eq!(controller.status("bob", None)["streams"], json!([]));
    assert!(
        controller
            .handle(
                "alice",
                Operation::Open {
                    endpoint: "default".into(),
                    virtual_sink: None,
                    channels: None,
                    capture_format: None,
                    host_audio_enabled: true,
                    operation_id: Some("one".into()),
                    capture_sink_without_routing: false
                }
            )
            .is_err()
    );
    controller.close("alice", 1).unwrap();
    assert_eq!(observed.drops.load(Ordering::SeqCst), 1);
    assert_eq!(
        controller.handle("alice", open()).err(),
        Some("stream_closed")
    );
    assert_eq!(
        *observed.opens.lock().unwrap(),
        [("render.explicit".into(), None, None, false)]
    );
}

#[test]
fn capture_only_routing_snapshot_is_forwarded_and_part_of_open_idempotency() {
    let (mut controller, observed) = controller(true);
    let operation = Operation::Open {
        endpoint: "render.explicit".into(),
        virtual_sink: None,
        channels: None,
        capture_format: None,
        host_audio_enabled: true,
        operation_id: Some("capture-only".into()),
        capture_sink_without_routing: true,
    };

    let opened = controller.handle("alice", operation).unwrap();
    assert_eq!(opened.payload["capture_sink_without_routing"], true);
    assert_eq!(
        controller
            .handle("alice", Operation::Status { stream: Some(1) })
            .unwrap()
            .payload["streams"][0]["capture_sink_without_routing"],
        true
    );
    assert_eq!(
        *observed.opens.lock().unwrap(),
        [("render.explicit".into(), None, None, true)]
    );
    assert_eq!(
        controller
            .handle(
                "alice",
                Operation::Open {
                    endpoint: "render.explicit".into(),
                    virtual_sink: None,
                    channels: None,
                    capture_format: None,
                    host_audio_enabled: true,
                    operation_id: Some("capture-only".into()),
                    capture_sink_without_routing: false,
                }
            )
            .err(),
        Some("operation_conflict")
    );
    assert_eq!(
        *observed.opens.lock().unwrap(),
        [("render.explicit".into(), None, None, true)]
    );
}

#[test]
fn omitted_open_policy_defaults_to_host_audio_off_and_routing_enabled() {
    let operation: Operation = serde_json::from_value(json!({
        "action":"open",
        "endpoint":"render.explicit",
        "operation_id":"legacy"
    }))
    .unwrap();
    assert!(matches!(
        operation,
        Operation::Open {
            capture_sink_without_routing: false,
            host_audio_enabled: false,
            ..
        }
    ));
}

#[test]
fn missing_or_null_continuous_audio_keeps_pending_on_idle_behavior() {
    let requests = [
        json!({
            "action":"open",
            "endpoint":"render.explicit",
            "operation_id":"missing-continuous"
        }),
        json!({
            "action":"open",
            "endpoint":"render.explicit",
            "operation_id":"null-continuous",
            "continuous_audio":null
        }),
    ];

    for request in requests {
        let operation: Operation = serde_json::from_value(request).unwrap();
        assert!(matches!(&operation, Operation::Open { .. }));
        let (mut controller, _) = controller(true);
        controller.handle("alice", operation).unwrap();
        let mut sink = Sink {
            reject: false,
            packets: Vec::new(),
        };
        let audio_sink = AudioSink {
            context: (&mut sink as *mut Sink).cast(),
            write,
        };

        // SAFETY: the sink callback context remains live for both synchronous reads.
        assert_eq!(
            unsafe { controller.read("alice", 1, audio_sink) },
            STATUS_OK
        );
        assert_eq!(
            unsafe { controller.read("alice", 1, audio_sink) },
            MEDIA_PENDING
        );
        assert_eq!(sink.packets.len(), 1);
        assert_eq!(sink.packets[0].1, 1);
    }
}

#[test]
fn continuous_idle_emits_silent_fixed_frames_with_advancing_clock() {
    let (mut controller, _) = controller(true);
    controller
        .handle("alice", continuous_open(5, "continuous-idle"))
        .unwrap();

    let mut sink = Sink {
        reject: false,
        packets: Vec::new(),
    };
    let audio_sink = AudioSink {
        context: (&mut sink as *mut Sink).cast(),
        write,
    };
    // SAFETY: the sink callback context remains live for both synchronous reads.
    assert_eq!(
        unsafe { controller.read("alice", 1, audio_sink) },
        MEDIA_PENDING
    );
    assert!(sink.packets.is_empty());
    controller
        .streams
        .get_mut(&1)
        .unwrap()
        .continuous
        .as_mut()
        .unwrap()
        .deadline = Instant::now() - Duration::from_millis(1);
    assert_eq!(
        unsafe { controller.read("alice", 1, audio_sink) },
        STATUS_OK
    );
    let first = sink.packets[0].clone();
    assert_eq!(first.0 & AUDIO_SILENT, AUDIO_SILENT);
    assert_eq!(first.0 & AUDIO_TIMESTAMP_ESTIMATED, 0);
    assert_eq!(first.1, 240);
    assert_eq!(first.3, 0);
    assert_eq!(first.4, vec![0; 240 * 2 * 4]);

    controller
        .streams
        .get_mut(&1)
        .unwrap()
        .continuous
        .as_mut()
        .unwrap()
        .deadline = Instant::now() - Duration::from_millis(1);
    assert_eq!(
        unsafe { controller.read("alice", 1, audio_sink) },
        STATUS_OK
    );
    let second = &sink.packets[1];
    assert_eq!(second.0 & AUDIO_SILENT, AUDIO_SILENT);
    assert_ne!(second.0 & AUDIO_TIMESTAMP_ESTIMATED, 0);
    assert_eq!(second.1, 240);
    assert_eq!(second.3, 1);
    assert!(second.2 > first.2);
    assert_eq!(second.4, vec![0; 240 * 2 * 4]);
}

#[test]
fn continuous_packetizer_pads_partial_pcm_only_when_idle_and_preserves_it() {
    let format = Format::new(48_000, 2, PCM_F32).unwrap();
    let mut packetizer = ContinuousPacketizer::new(format, 240).unwrap();
    let captured = [0, 0, 128, 63, 0, 0, 0, 64];
    let ready = packetizer
        .push(Packet {
            frames: 1,
            flags: AUDIO_DISCONTINUITY | AUDIO_TIMESTAMP_ESTIMATED,
            bytes: captured.to_vec(),
            timestamp_us: 2_000,
            sequence: 9,
        })
        .unwrap();
    assert!(ready.is_empty());

    let padded = packetizer.idle(5_000).unwrap();
    assert_eq!(padded.frames, 240);
    assert_eq!(&padded.bytes[..captured.len()], &captured);
    assert!(padded.bytes[captured.len()..].iter().all(|byte| *byte == 0));
    assert_eq!(padded.flags & AUDIO_SILENT, 0);
    assert_ne!(padded.flags & AUDIO_DISCONTINUITY, 0);
    assert_ne!(padded.flags & AUDIO_TIMESTAMP_ESTIMATED, 0);
    assert_eq!((padded.timestamp_us, padded.sequence), (2_000, 0));
}

#[test]
fn continuous_packetizer_keeps_device_recovery_pcm_out_of_the_previous_frame() {
    let format = Format::new(48_000, 2, PCM_F32).unwrap();
    let mut packetizer = ContinuousPacketizer::new(format, 240).unwrap();
    let before_loss = [0, 0, 128, 63, 0, 0, 0, 64];
    let after_recovery = [0, 0, 64, 64, 0, 0, 128, 64];
    assert!(
        packetizer
            .push(Packet {
                frames: 1,
                flags: 0,
                bytes: before_loss.to_vec(),
                timestamp_us: 1_000,
                sequence: 0,
            })
            .unwrap()
            .is_empty()
    );

    let previous = packetizer
        .push(Packet {
            frames: 1,
            flags: AUDIO_DISCONTINUITY,
            bytes: after_recovery.to_vec(),
            timestamp_us: 2_000,
            sequence: 1,
        })
        .unwrap();
    assert_eq!(previous.len(), 1);
    assert_eq!(&previous[0].bytes[..before_loss.len()], &before_loss);
    assert!(
        previous[0].bytes[before_loss.len()..]
            .iter()
            .all(|byte| *byte == 0)
    );
    assert_eq!(previous[0].flags & AUDIO_DISCONTINUITY, 0);
    assert_eq!(previous[0].flags & AUDIO_TIMESTAMP_ESTIMATED, 0);

    let recovered = packetizer.idle(10_000).unwrap();
    assert_eq!(&recovered.bytes[..after_recovery.len()], &after_recovery);
    assert!(
        recovered.bytes[after_recovery.len()..]
            .iter()
            .all(|byte| *byte == 0)
    );
    assert_ne!(recovered.flags & AUDIO_DISCONTINUITY, 0);
    assert_eq!(recovered.flags & AUDIO_TIMESTAMP_ESTIMATED, 0);
    assert_eq!((previous[0].sequence, recovered.sequence), (0, 1));
    assert!(recovered.timestamp_us > previous[0].timestamp_us);
}

#[test]
fn continuous_partial_pcm_survives_terminal_failure_and_sink_retry_until_close() {
    let (mut controller, observed) = controller(true);
    let stream_id = controller
        .handle("alice", continuous_open(5, "partial-failure"))
        .unwrap()
        .created
        .unwrap();
    let mut sink = Sink {
        reject: true,
        packets: Vec::new(),
    };
    let callback = AudioSink {
        context: (&mut sink as *mut Sink).cast(),
        write,
    };

    // The first captured frame is smaller than the configured 240-frame packet.
    // SAFETY: the sink callback context remains live through every read.
    assert_eq!(
        unsafe { controller.read("alice", stream_id, callback) },
        MEDIA_PENDING
    );
    observed.health.lock().unwrap().failure = Some("audio_capture_failed");
    assert_eq!(
        controller.status("alice", Some(stream_id))["streams"][0]["state"],
        "failed"
    );
    // SAFETY: the callback may reject this packet and the controller retains it.
    assert_eq!(
        unsafe { controller.read("alice", stream_id, callback) },
        MEDIA_BUSY
    );
    assert!(sink.packets.is_empty());
    sink.reject = false;
    // SAFETY: the retained packet remains owned by the controller during callback.
    assert_eq!(
        unsafe { controller.read("alice", stream_id, callback) },
        STATUS_OK
    );
    assert_eq!(sink.packets.len(), 1);
    assert_eq!(sink.packets[0].1, 240);
    assert_eq!((sink.packets[0].2, sink.packets[0].3), (100, 0));
    assert_ne!(sink.packets[0].0 & AUDIO_DISCONTINUITY, 0);
    assert_eq!(sink.packets[0].0 & AUDIO_TIMESTAMP_ESTIMATED, 0);
    assert_eq!(sink.packets[0].4, vec![0; 240 * 2 * 4]);
    // SAFETY: the terminal source failure becomes visible after retained PCM.
    assert_eq!(
        unsafe { controller.read("alice", stream_id, callback) },
        MEDIA_BACKEND_ERROR
    );
    controller.close("alice", stream_id).unwrap();
    assert_eq!(observed.drops.load(Ordering::SeqCst), 1);
    // SAFETY: a closed stream owns no pending packet or source.
    assert_eq!(
        unsafe { controller.read("alice", stream_id, callback) },
        MEDIA_CLOSED
    );
}

#[test]
fn continuous_packet_frame_count_truncates_fractional_samples() {
    let format = Format::new(44_100, 2, PCM_F32).unwrap();
    let continuous = ContinuousStream::new(format, ContinuousAudio { packet_ms: 5 }).unwrap();

    assert_eq!(continuous.packetizer.frames_per_packet, 220);
}

#[test]
fn continuous_packetizer_splits_large_capture_into_fixed_monotonic_frames() {
    let format = Format::new(48_000, 2, PCM_F32).unwrap();
    let mut packetizer = ContinuousPacketizer::new(format, 240).unwrap();
    let captured: Vec<u8> = (0..format.packet_bytes(480).unwrap())
        .map(|index| (index % 251 + 1) as u8)
        .collect();
    let frames = packetizer
        .push(Packet {
            frames: 480,
            flags: AUDIO_DISCONTINUITY,
            bytes: captured.clone(),
            timestamp_us: 1_000,
            sequence: 7,
        })
        .unwrap();

    assert_eq!(frames.len(), 2);
    assert!(frames.iter().all(|frame| frame.frames == 240));
    assert_eq!(frames[0].bytes, captured[..1_920]);
    assert_eq!(frames[1].bytes, captured[1_920..]);
    assert_ne!(frames[0].flags & AUDIO_DISCONTINUITY, 0);
    assert_eq!(frames[1].flags & AUDIO_DISCONTINUITY, 0);
    assert_eq!((frames[0].sequence, frames[1].sequence), (0, 1));
    assert_eq!(
        (frames[0].timestamp_us, frames[1].timestamp_us),
        (1_000, 6_000)
    );
}

#[test]
fn continuous_packet_size_is_part_of_retry_identity() {
    let (mut controller, observed) = controller(true);
    let opened = controller
        .handle("alice", continuous_open(5, "continuous-retry"))
        .unwrap();
    let retried = controller
        .handle("alice", continuous_open(5, "continuous-retry"))
        .unwrap();
    assert_eq!(opened.created, Some(1));
    assert_eq!(retried.created, None);
    assert_eq!(opened.payload, retried.payload);
    assert_eq!(opened.payload["continuous_audio"]["packet_ms"], 5);
    assert_eq!(
        controller
            .handle("alice", continuous_open(10, "continuous-retry"))
            .err(),
        Some("operation_conflict")
    );
    assert_eq!(
        controller
            .handle("alice", continuous_open(20, "invalid-continuous"))
            .err(),
        Some("invalid_continuous_audio_packet_size")
    );
    assert_eq!(observed.opens.lock().unwrap().len(), 1);
}

#[test]
fn continuous_stream_emission_stops_after_close_and_shutdown() {
    let (mut controller, observed) = controller(true);
    controller
        .handle("alice", continuous_open(5, "continuous-close"))
        .unwrap();
    controller.close("alice", 1).unwrap();

    let mut sink = Sink {
        reject: false,
        packets: Vec::new(),
    };
    let audio_sink = AudioSink {
        context: (&mut sink as *mut Sink).cast(),
        write,
    };
    // SAFETY: the sink callback context remains live for the synchronous read.
    assert_eq!(
        unsafe { controller.read("alice", 1, audio_sink) },
        MEDIA_CLOSED
    );

    controller
        .handle("alice", continuous_open(5, "continuous-shutdown"))
        .unwrap();
    controller.shutdown();
    // SAFETY: the sink callback context remains live for the synchronous read.
    assert_eq!(
        unsafe { controller.read("alice", 2, audio_sink) },
        MEDIA_CLOSED
    );
    assert!(sink.packets.is_empty());
    assert_eq!(observed.drops.load(Ordering::SeqCst), 2);
}

#[test]
fn empty_ordinary_sink_uses_default_endpoint_and_host_audio_policy_is_idempotent() {
    let (mut controller, observed) = controller(true);
    let request = Operation::Open {
        endpoint: String::new(),
        virtual_sink: None,
        channels: None,
        capture_format: None,
        host_audio_enabled: false,
        operation_id: Some("automatic-sink".into()),
        capture_sink_without_routing: false,
    };

    let opened = controller.handle("alice", request).unwrap();
    assert_eq!(opened.payload["endpoint"], "render.default");
    assert_eq!(opened.payload["requested_endpoint"], "");
    assert_eq!(opened.payload["host_audio_enabled"], false);
    assert_eq!(
        observed.host_audio_settings.lock().unwrap().as_slice(),
        [false]
    );

    assert_eq!(
        controller
            .handle(
                "alice",
                Operation::Open {
                    endpoint: String::new(),
                    virtual_sink: None,
                    channels: None,
                    capture_format: None,
                    host_audio_enabled: true,
                    operation_id: Some("automatic-sink".into()),
                    capture_sink_without_routing: false,
                }
            )
            .err(),
        Some("operation_conflict")
    );
    assert_eq!(
        observed.host_audio_settings.lock().unwrap().as_slice(),
        [false]
    );
}

#[test]
fn status_tracks_followed_default_while_open_and_lookup_keep_initial_endpoint() {
    let (mut controller, observed) = controller(true);
    let open = Operation::Open {
        endpoint: String::new(),
        virtual_sink: None,
        channels: None,
        capture_format: None,
        host_audio_enabled: false,
        operation_id: Some("follow-default".into()),
        capture_sink_without_routing: false,
    };
    let opened = controller.handle("alice", open).unwrap();
    assert_eq!(opened.payload["endpoint"], "render.default");
    assert_eq!(opened.payload["requested_endpoint"], "");

    assert_eq!(
        controller.status("alice", Some(1))["streams"][0]["endpoint"],
        "render.default"
    );
    observed.health.lock().unwrap().active_endpoint = Some("render.console-new".into());

    let status = controller.status("alice", Some(1));
    assert_eq!(status["streams"][0]["endpoint"], "render.console-new");
    assert_eq!(status["streams"][0]["requested_endpoint"], "");

    let repeated_open = controller
        .handle(
            "alice",
            Operation::Open {
                endpoint: String::new(),
                virtual_sink: None,
                channels: None,
                capture_format: None,
                host_audio_enabled: false,
                operation_id: Some("follow-default".into()),
                capture_sink_without_routing: false,
            },
        )
        .unwrap();
    let lookup = controller
        .handle(
            "alice",
            Operation::Lookup {
                operation_id: "follow-default".into(),
            },
        )
        .unwrap();

    assert_eq!(repeated_open.payload["endpoint"], "render.default");
    assert_eq!(lookup.payload["endpoint"], "render.default");
    assert_eq!(repeated_open.payload["requested_endpoint"], "");
    assert_eq!(lookup.payload["requested_endpoint"], "");
}

#[test]
fn enabled_host_audio_policy_is_forwarded_and_reported_in_status() {
    let (mut controller, observed) = controller(true);
    let opened = controller
        .handle(
            "alice",
            Operation::Open {
                endpoint: "render.explicit".into(),
                virtual_sink: None,
                channels: None,
                capture_format: None,
                host_audio_enabled: true,
                operation_id: None,
                capture_sink_without_routing: false,
            },
        )
        .unwrap();

    assert_eq!(opened.payload["host_audio_enabled"], true);
    assert_eq!(
        controller.status("alice", Some(1))["streams"][0]["host_audio_enabled"],
        true
    );
    assert_eq!(
        observed.host_audio_settings.lock().unwrap().as_slice(),
        [true]
    );
}

#[test]
fn status_reports_recovering_until_capture_produces_a_recovered_packet() {
    let (mut controller, observed) = controller(true);
    controller.handle("alice", open()).unwrap();
    observed.health.lock().unwrap().recovering = true;

    assert_eq!(
        controller.status("alice", Some(1))["streams"][0]["state"],
        "recovering"
    );
    assert_eq!(controller.status("bob", Some(1))["streams"], json!([]));
}

#[test]
fn unavailable_selected_endpoint_fails_without_falling_back_to_the_default() {
    let (mut controller, observed) = controller(true);
    let result = controller.handle(
        "alice",
        Operation::Open {
            endpoint: "render.missing".into(),
            virtual_sink: None,
            channels: None,
            capture_format: None,
            host_audio_enabled: true,
            operation_id: Some("missing-endpoint".into()),
            capture_sink_without_routing: false,
        },
    );

    assert_eq!(result.err(), Some("endpoint_not_found"));
    assert_eq!(
        *observed.opens.lock().unwrap(),
        [("render.missing".into(), None, None, false)]
    );
}

#[test]
fn virtual_sink_selection_precedes_regular_endpoint_and_binds_layout_to_retry() {
    let (mut controller, observed) = controller(true);
    let request = Operation::Open {
        endpoint: String::new(),
        virtual_sink: Some("Studio Virtual".into()),
        channels: Some(6),
        capture_format: None,
        host_audio_enabled: true,
        operation_id: Some("virtual-audio".into()),
        capture_sink_without_routing: false,
    };

    let opened = controller.handle("alice", request).unwrap();
    assert_eq!(opened.payload["endpoint"], "render.virtual");
    assert_eq!(opened.payload["requested_endpoint"], "");
    assert_eq!(opened.payload["virtual_sink"], "Studio Virtual");
    assert_eq!(opened.payload["channels"], 6);
    assert_eq!(opened.payload["format"]["channels"], 6);

    let retry = controller
        .handle(
            "alice",
            Operation::Open {
                endpoint: String::new(),
                virtual_sink: Some("Studio Virtual".into()),
                channels: Some(6),
                capture_format: None,
                host_audio_enabled: true,
                operation_id: Some("virtual-audio".into()),
                capture_sink_without_routing: false,
            },
        )
        .unwrap();
    assert_eq!(opened.payload, retry.payload);
    assert_eq!(
        controller
            .handle(
                "alice",
                Operation::Open {
                    endpoint: String::new(),
                    virtual_sink: Some("Studio Virtual".into()),
                    channels: Some(8),
                    capture_format: None,
                    host_audio_enabled: true,
                    operation_id: Some("virtual-audio".into()),
                    capture_sink_without_routing: false,
                },
            )
            .err(),
        Some("operation_conflict")
    );
    assert_eq!(
        *observed.opens.lock().unwrap(),
        [(String::new(), Some("Studio Virtual".into()), Some(6), false)]
    );
}

#[test]
fn unavailable_virtual_sink_does_not_fall_back_to_requested_regular_endpoint() {
    let (mut controller, observed) = controller(true);
    let result = controller.handle(
        "alice",
        Operation::Open {
            endpoint: "render.explicit".into(),
            virtual_sink: Some("Missing Virtual".into()),
            channels: Some(2),
            capture_format: None,
            host_audio_enabled: true,
            operation_id: Some("unavailable-virtual".into()),
            capture_sink_without_routing: false,
        },
    );

    assert_eq!(result.err(), Some("endpoint_not_found"));
    assert_eq!(
        *observed.opens.lock().unwrap(),
        [(
            "render.explicit".into(),
            Some("Missing Virtual".into()),
            Some(2),
            false
        )]
    );
}

#[test]
fn virtual_sink_rejects_capture_only_routing_policy() {
    let (mut controller, observed) = controller(true);
    let result = controller.handle(
        "alice",
        Operation::Open {
            endpoint: String::new(),
            virtual_sink: Some(String::new()),
            channels: Some(2),
            capture_format: None,
            host_audio_enabled: true,
            operation_id: None,
            capture_sink_without_routing: true,
        },
    );

    assert_eq!(result.err(), Some("invalid_open_policy"));
    assert!(observed.opens.lock().unwrap().is_empty());
}

#[test]
fn virtual_sink_requires_supported_channel_layout_and_regular_open_rejects_layout() {
    let (mut controller, observed) = controller(true);
    let unsupported_virtual = controller.handle(
        "alice",
        Operation::Open {
            endpoint: String::new(),
            virtual_sink: Some(String::new()),
            channels: Some(4),
            capture_format: None,
            host_audio_enabled: true,
            operation_id: None,
            capture_sink_without_routing: false,
        },
    );
    assert_eq!(
        unsupported_virtual.err(),
        Some("unsupported_channel_layout")
    );

    let regular_with_layout = controller.handle(
        "alice",
        Operation::Open {
            endpoint: "render.explicit".into(),
            virtual_sink: None,
            channels: Some(6),
            capture_format: None,
            host_audio_enabled: true,
            operation_id: None,
            capture_sink_without_routing: false,
        },
    );
    assert_eq!(regular_with_layout.err(), Some("invalid_channel_layout"));
    assert!(observed.opens.lock().unwrap().is_empty());
}

#[test]
fn disabled_controls_do_not_enumerate_or_open_and_limits_are_validated() {
    let (mut controller, observed) = controller(false);
    assert_eq!(
        controller
            .handle("alice", Operation::Enumerate)
            .unwrap()
            .payload,
        json!({"ok":true,"enabled":false,"endpoints":[]})
    );
    assert_eq!(controller.handle("alice", open()).err(), Some("disabled"));
    assert!(observed.opens.lock().unwrap().is_empty());
    assert!(validate_endpoint("invalid\0endpoint").is_err());
    assert!(validate_operation("two words").is_err());
    assert!(
        serde_json::from_value::<Operation>(
            json!({"action":"open","endpoint":"default","owner":"bob"})
        )
        .is_err()
    );
}

struct Sink {
    reject: bool,
    packets: Vec<(u32, u32, u64, u64, Vec<u8>)>,
}

unsafe extern "C" fn write(context: *mut c_void, frame: *const AudioFrame) -> Status {
    // SAFETY: test owns context and callback borrows a provider-owned live frame.
    let sink = unsafe { &mut *context.cast::<Sink>() };
    if sink.reject {
        return MEDIA_BUSY;
    }
    let frame = unsafe { &*frame };
    sink.packets.push((
        frame.flags,
        frame.frames,
        frame.timestamp_us,
        frame.sequence,
        unsafe { frame.bytes().unwrap() }.to_vec(),
    ));
    STATUS_OK
}

struct FailingSource;

impl Source for FailingSource {
    fn read(&mut self) -> AudioResult<Option<Packet>> {
        Err("audio_source_failed")
    }

    fn health(&self) -> Health {
        Health {
            recovering: true,
            ..Health::default()
        }
    }
}

#[test]
fn read_failure_overrides_recovery_status_until_close_and_reacquire() {
    let (mut controller, observed) = controller(true);
    let opened = controller.handle("alice", open()).unwrap();
    let stream_id = opened.created.unwrap();
    controller.streams.get_mut(&stream_id).unwrap().source = Box::new(FailingSource);
    assert_eq!(
        controller.status("alice", Some(stream_id))["streams"][0]["state"],
        "recovering"
    );

    let mut sink = Sink {
        reject: false,
        packets: Vec::new(),
    };
    let callback = AudioSink {
        context: (&mut sink as *mut Sink).cast(),
        write,
    };
    // SAFETY: callback context remains live through the synchronous read.
    assert_eq!(
        unsafe { controller.read("alice", stream_id, callback) },
        MEDIA_BACKEND_ERROR
    );
    let status = controller.status("alice", Some(stream_id));
    assert_eq!(status["streams"][0]["state"], "failed");
    assert_eq!(status["streams"][0]["failure"], "audio_source_failed");
    // SAFETY: callback context remains live through the synchronous read.
    assert_eq!(
        unsafe { controller.read("alice", stream_id, callback) },
        MEDIA_BACKEND_ERROR
    );

    controller.close("alice", stream_id).unwrap();
    assert_eq!(controller.status("alice", None)["streams"], json!([]));
    let reopened = controller
        .handle(
            "alice",
            Operation::Open {
                endpoint: "render.explicit".into(),
                virtual_sink: None,
                channels: None,
                capture_format: None,
                host_audio_enabled: true,
                operation_id: Some("reacquired".into()),
                capture_sink_without_routing: false,
            },
        )
        .unwrap();
    assert_ne!(reopened.created, Some(stream_id));
    assert_eq!(
        controller.status("alice", reopened.created)["streams"][0]["state"],
        "capturing"
    );
    assert_eq!(observed.opens.lock().unwrap().len(), 2);
}

#[test]
fn rejected_packet_can_be_delivered_after_capture_fails_then_stream_is_released() {
    let (mut controller, observed) = controller(true);
    let stream_id = controller.handle("alice", open()).unwrap().created.unwrap();
    let mut sink = Sink {
        reject: true,
        packets: Vec::new(),
    };
    let callback = AudioSink {
        context: (&mut sink as *mut Sink).cast(),
        write,
    };

    // SAFETY: callback context and the retained provider packet outlive each read.
    assert_eq!(
        unsafe { controller.read("alice", stream_id, callback) },
        MEDIA_BUSY
    );
    observed.health.lock().unwrap().failure = Some("device_lost");
    assert_eq!(
        controller.status("alice", Some(stream_id))["streams"][0]["state"],
        "failed"
    );
    sink.reject = false;
    // SAFETY: callback context remains live through the synchronous read.
    assert_eq!(
        unsafe { controller.read("alice", stream_id, callback) },
        STATUS_OK
    );
    assert_eq!(
        sink.packets,
        [(AUDIO_SILENT | AUDIO_DISCONTINUITY, 1, 100, 7, vec![0; 8])]
    );
    // The accepted packet is delivered once; the failed capture cannot supply another.
    // SAFETY: callback context remains live through the synchronous read.
    assert_eq!(
        unsafe { controller.read("alice", stream_id, callback) },
        MEDIA_BACKEND_ERROR
    );
    controller.close("alice", stream_id).unwrap();
    assert_eq!(observed.drops.load(Ordering::SeqCst), 1);
    // SAFETY: callback context remains live through the synchronous read.
    assert_eq!(
        unsafe { controller.read("alice", stream_id, callback) },
        MEDIA_CLOSED
    );
}

#[test]
fn queued_packet_reaches_sink_before_terminal_capture_failure() {
    let (mut controller, observed) = controller(true);
    let stream_id = controller.handle("alice", open()).unwrap().created.unwrap();
    observed.health.lock().unwrap().failure = Some("audio_capture_failed");
    let mut sink = Sink {
        reject: true,
        packets: Vec::new(),
    };
    let callback = AudioSink {
        context: (&mut sink as *mut Sink).cast(),
        write,
    };

    // SAFETY: callback context remains live through each synchronous read.
    assert_eq!(
        unsafe { controller.read("alice", stream_id, callback) },
        MEDIA_BUSY
    );
    sink.reject = false;
    // SAFETY: callback context remains live through each synchronous read.
    assert_eq!(
        unsafe { controller.read("alice", stream_id, callback) },
        STATUS_OK
    );
    assert_eq!(sink.packets[0].3, 7);
    // SAFETY: callback context remains live through each synchronous read.
    assert_eq!(
        unsafe { controller.read("alice", stream_id, callback) },
        MEDIA_BACKEND_ERROR
    );
    controller.close("alice", stream_id).unwrap();
    assert_eq!(observed.drops.load(Ordering::SeqCst), 1);
}

unsafe fn bind(factory: &Factory, owner: &str) -> *const AudioCaptureApi {
    let query = NativeInterfaceQuery {
        header: AbiHeader::of::<NativeInterfaceQuery>(),
        interface_major: 1,
        min_minor: 0,
        min_size: size_of::<AudioCaptureApi>() as u32,
    };
    let mut output = std::ptr::null();
    // SAFETY: factory, principal and query outlive call; one binding is transferred.
    assert_eq!(
        unsafe {
            factory.api.bind.unwrap()(
                factory.api.context,
                BorrowedBytes::new(owner.as_bytes()),
                &query,
                &mut output,
            )
        },
        STATUS_OK
    );
    output.cast()
}

#[test]
fn native_owner_isolation_sink_retry_pending_and_close_are_real_callbacks() {
    let (mut controller, observed) = controller(true);
    controller.handle("alice", open()).unwrap();
    let shared = Arc::new(Mutex::new(controller));
    let factory = Factory::new(shared.clone());
    let mut sink = Sink {
        reject: true,
        packets: Vec::new(),
    };
    let callback = AudioSink {
        context: (&mut sink as *mut Sink).cast(),
        write,
    };
    // SAFETY: bindings and borrowed frame callbacks are live through calls, and
    // each binding is released once before dropping its provider factory.
    unsafe {
        let alice = &*bind(&factory, "alice");
        let bob = &*bind(&factory, "bob");
        assert_eq!(
            (bob.read_audio)(bob.base.context, 1, callback),
            MEDIA_CLOSED
        );
        assert_eq!(
            (alice.read_audio)(alice.base.context, 1, callback),
            MEDIA_BUSY
        );
        assert!(sink.packets.is_empty());
        sink.reject = false;
        assert_eq!(
            (alice.read_audio)(alice.base.context, 1, callback),
            STATUS_OK
        );
        assert_eq!(
            sink.packets,
            [(AUDIO_SILENT | AUDIO_DISCONTINUITY, 1, 100, 7, vec![0; 8])]
        );
        assert_eq!(
            (alice.read_audio)(alice.base.context, 1, callback),
            MEDIA_PENDING
        );
        shared.lock().unwrap().shutdown();
        assert_eq!(
            (alice.read_audio)(alice.base.context, 1, callback),
            MEDIA_CLOSED
        );
        alice.base.release.unwrap()(alice.base.context);
        bob.base.release.unwrap()(bob.base.context);
    }
    assert_eq!(observed.drops.load(Ordering::SeqCst), 1);
}

struct Runtime {
    request: Option<Response>,
    complete: Option<serde_json::Value>,
    reject: bool,
    accept_request: bool,
}

unsafe extern "C" fn invoke(
    context: *mut c_void,
    bytes: BorrowedBytes,
    reply: ReplySink,
) -> Status {
    // SAFETY: endpoint state and serialized request bytes live for the call.
    let runtime = unsafe { &mut *context.cast::<Runtime>() };
    let command: Command = serde_json::from_slice(unsafe { bytes.read().unwrap() }).unwrap();
    let response = match command {
        Command::Poll {} => runtime.request.take().unwrap_or(Response::Pending),
        Command::CheckRequest { .. } => {
            if runtime.accept_request {
                Response::Ok
            } else {
                Response::error("request_ended", "cancelled")
            }
        }
        Command::Complete { payload, .. } => {
            runtime.complete = Some(payload);
            if runtime.reject {
                Response::error("request_ended", "cancelled")
            } else {
                Response::Ok
            }
        }
        _ => panic!("unexpected test command"),
    };
    unsafe {
        reply.send(&serde_json::to_vec(&response).unwrap());
    }
    STATUS_OK
}

#[test]
fn cancelled_open_completion_rolls_back_only_newly_created_streams() {
    let (controller, observed) = controller(true);
    let shared = Arc::new(Mutex::new(controller));
    let mut runtime = Runtime {
        request: None,
        complete: None,
        reject: true,
        accept_request: true,
    };
    let endpoint = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (&mut runtime as *mut Runtime).cast(),
        invoke,
    };
    // SAFETY: runtime state outlives client/module and synchronous endpoint calls.
    let client = unsafe { RuntimeClient::from_endpoint(endpoint) };
    let mut module = crate::AudioModule {
        runtime: client,
        controller: shared.clone(),
        _factory: Factory::new(shared.clone()),
        platform_started: false,
    };
    let request = |payload| Response::Request {
        id: 1,
        caller: "alice".into(),
        initiator: "alice".into(),
        service: crate::SERVICE.into(),
        payload,
        remaining_ms: 1000,
    };
    runtime.request = Some(request(
        json!({"action":"open","endpoint":"render.explicit","operation_id":"one","continuous_audio":{"packet_ms":5}}),
    ));
    module.tick().unwrap();
    assert!(shared.lock().unwrap().streams.is_empty());
    assert!(shared.lock().unwrap().operations.is_empty());
    assert_eq!(observed.drops.load(Ordering::SeqCst), 1);
    runtime.accept_request = false;
    runtime.complete = None;
    runtime.request = Some(request(json!({
        "action":"open","endpoint":"render.explicit","operation_id":"before-dispatch"
    })));
    module.tick().unwrap();
    assert!(runtime.complete.is_none());
    assert!(shared.lock().unwrap().operations.is_empty());
    assert_eq!(observed.opens.lock().unwrap().len(), 1);
    runtime.accept_request = true;
    runtime.reject = false;
    runtime.request = Some(request(
        json!({"action":"open","endpoint":"render.explicit","operation_id":"one","continuous_audio":{"packet_ms":5}}),
    ));
    module.tick().unwrap();
    assert_eq!(runtime.complete.as_ref().unwrap()["ok"], true);
    assert_eq!(shared.lock().unwrap().operations.len(), 1);
    runtime.reject = true;
    runtime.request = Some(request(json!({"action":"lookup","operation_id":"one"})));
    module.tick().unwrap();
    assert_eq!(shared.lock().unwrap().streams.len(), 1);
    assert_eq!(observed.drops.load(Ordering::SeqCst), 1);
}

#[test]
fn unpublished_opens_release_operation_capacity_and_preserve_other_owners() {
    let (mut controller, observed) = controller(true);
    let bob = controller.handle("bob", open()).unwrap().created.unwrap();
    for index in 0..128 {
        let operation_id = format!("cancel-{index}");
        let opened = controller
            .handle(
                "alice",
                Operation::Open {
                    endpoint: "render.explicit".into(),
                    virtual_sink: None,
                    channels: None,
                    capture_format: None,
                    host_audio_enabled: true,
                    operation_id: Some(operation_id.clone()),
                    capture_sink_without_routing: false,
                },
            )
            .unwrap();
        let stream = opened.created.unwrap();
        controller.rollback_unpublished("bob", stream);
        assert_eq!(controller.streams.len(), 2);
        controller.rollback_unpublished("alice", stream);
        assert_eq!(controller.streams.len(), 1);
        assert!(
            !controller
                .operations
                .contains_key(&("alice".into(), operation_id))
        );
    }
    assert_eq!(controller.operations.len(), 1);
    assert_eq!(observed.drops.load(Ordering::SeqCst), 128);
    assert!(controller.lookup("bob", "one").is_ok());

    let stream = controller.handle("alice", open()).unwrap().created.unwrap();
    controller.close("alice", stream).unwrap();
    assert_eq!(
        controller.handle("alice", open()).err(),
        Some("stream_closed")
    );
    controller.rollback_unpublished("alice", stream);
    assert_eq!(
        controller.handle("alice", open()).err(),
        Some("stream_closed")
    );
    assert!(controller.streams.contains_key(&bob));
}
