use super::*;
use amaterasu_module_sdk::abi::*;
use std::{collections::VecDeque, ffi::c_void, net::UdpSocket};

struct Host {
    typed_network_binds: usize,
    replies: VecDeque<Response>,
    complete: Option<serde_json::Value>,
}

unsafe extern "C" fn invoke(
    context: *mut c_void,
    input: BorrowedBytes,
    output: ReplySink,
) -> Status {
    // SAFETY: the test endpoint context and message allocations outlive invocation.
    let host = unsafe { &mut *context.cast::<Host>() };
    let command: Command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
    match &command {
        Command::NativeBindTyped {
            service, contract, ..
        } => {
            assert_eq!(contract, "amaterasu.network-interface/1");
            assert_eq!(service, "vendor.network/1");
            host.typed_network_binds += 1;
        }
        Command::NativeBind { service, .. }
            if service == "vendor.network/1" || service == "amaterasu.network/1" =>
        {
            panic!("network bindings must declare the typed network contract");
        }
        _ => {}
    }
    if let Command::Complete { payload, .. } = command {
        host.complete = Some(payload);
    }
    let response = host.replies.pop_front().expect("unexpected endpoint call");
    unsafe { output.send(&serde_json::to_vec(&response).unwrap()) };
    STATUS_OK
}

#[test]
fn cancelled_open_releases_udp_ports_and_cannot_leave_a_hidden_sender() {
    for before in [true, false] {
        let ended = Response::Error {
            code: "request_ended".into(),
            message: "ended".into(),
        };
        let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = peer.local_addr().unwrap().port();
        let request = Response::Request {
            id: 1,
            caller: "consumer".into(),
            initiator: "consumer".into(),
            service: SERVICE.into(),
            remaining_ms: 1000,
            payload: json!({"action":"open","peer_ip":"127.0.0.1","video_port":port,"audio_port":port,"audio":{"channels":2,"packet_ms":10,"wire_payload_bytes":4}}),
        };
        let mut host = Host {
            typed_network_binds: 0,
            replies: VecDeque::from([
                Response::Result {
                    payload: json!({"enabled":true,"allowed_principals":["consumer"]}),
                },
                Response::Ok,
                Response::Ok,
                request,
                if before { ended.clone() } else { Response::Ok },
            ]),
            complete: None,
        };
        if !before {
            host.replies.push_back(ended);
        }
        let api = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: (&mut host as *mut Host).cast(),
            invoke,
        };
        // SAFETY: host state outlives module and all runtime endpoint calls.
        let mut module = MediaTransportModule::start_configured(
            unsafe { RuntimeClient::from_endpoint(api) },
            Some(Network::test_provider()),
        )
        .unwrap();
        module.tick().unwrap();
        assert!(host.replies.is_empty());
        let status = module
            .controller
            .lock()
            .unwrap()
            .handle("consumer", Operation::Status { stream: None })
            .unwrap()
            .payload;
        assert_eq!(status["streams"], json!([]));
        if let Some(reply) = &host.complete {
            assert!(UdpSocket::bind(reply["video_local"].as_str().unwrap()).is_ok());
            assert!(UdpSocket::bind(reply["audio_local"].as_str().unwrap()).is_ok());
        } else {
            assert!(before);
        }
    }
}

#[test]
fn startup_rejects_an_unavailable_selected_network_provider() {
    let mut host = Host {
        typed_network_binds: 0,
        replies: VecDeque::from([
            Response::Result {
                payload: json!({"enabled":true,"network_provider":"vendor.network/1","allowed_principals":["consumer"]}),
            },
            Response::Error {
                code: "not_found".into(),
                message: "provider unavailable".into(),
            },
        ]),
        complete: None,
    };
    let api = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (&mut host as *mut Host).cast(),
        invoke,
    };
    // SAFETY: host remains live while startup attempts the selected binding.
    let result = MediaTransportModule::start(unsafe { RuntimeClient::from_endpoint(api) });
    assert!(result.is_err());
    assert_eq!(host.typed_network_binds, 1);
    assert!(host.replies.is_empty());
}

fn lifecycle_fixture(host: &mut Host) -> MediaTransportModule {
    host.replies.extend([
        Response::Result {
            payload: json!({"enabled":true,"allowed_principals":["owner"]}),
        },
        Response::Ok,
        Response::Ok,
    ]);
    let api = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (host as *mut Host).cast(),
        invoke,
    };
    // SAFETY: each test keeps its host alive through all module operations.
    MediaTransportModule::start_configured(
        unsafe { RuntimeClient::from_endpoint(api) },
        Some(Network::test_provider()),
    )
    .unwrap()
}

#[test]
fn shutdown_announcement_stops_new_streams_without_interrupting_consumers() {
    use crate::stream::tests::{receivers, request, video_packet};
    let mut host = Host {
        typed_network_binds: 0,
        replies: VecDeque::new(),
        complete: None,
    };
    let mut module = lifecycle_fixture(&mut host);
    let (video, audio) = receivers();
    let request = request(&video, &audio, false);
    let opened = module
        .controller
        .lock()
        .unwrap()
        .handle("owner", Operation::Open(request.clone()))
        .unwrap();
    let stream = opened.created.unwrap();
    module.prepare_shutdown(1000).unwrap();
    {
        let mut controller = module.controller.lock().unwrap();
        assert_eq!(
            controller.handle("owner", Operation::Open(request)).err(),
            Some("shutting_down")
        );
        let bytes = [0, 0, 0, 1, 0x65, 1];
        // SAFETY: this immutable access unit lives through native admission.
        assert_eq!(
            unsafe { controller.submit_video("owner", stream, &video_packet(&bytes, 0)) },
            STATUS_OK
        );
        assert_eq!(
            controller
                .handle(
                    "owner",
                    Operation::Status {
                        stream: Some(stream)
                    }
                )
                .unwrap()
                .payload["streams"][0]["state"],
            "sending"
        );
    }
    host.replies.push_back(Response::Pending);
    module.tick().unwrap();
    let mut packet = [0; 2048];
    assert!(video.recv(&mut packet).unwrap() > 0);
    assert!(UdpSocket::bind(opened.payload["video_local"].as_str().unwrap()).is_err());
    assert!(UdpSocket::bind(opened.payload["audio_local"].as_str().unwrap()).is_err());
}

#[test]
fn quiesce_releases_stream_sockets_and_never_readmits_work() {
    use crate::stream::tests::{receivers, request, video_packet};
    let mut host = Host {
        typed_network_binds: 0,
        replies: VecDeque::new(),
        complete: None,
    };
    let mut module = lifecycle_fixture(&mut host);
    let (video, audio) = receivers();
    let request = request(&video, &audio, false);
    let opened = {
        let mut controller = module.controller.lock().unwrap();
        let opened = controller
            .handle("owner", Operation::Open(request.clone()))
            .unwrap();
        let bytes = [0, 0, 0, 1, 0x65, 1];
        // SAFETY: the fixture access unit lives through synchronous admission.
        assert_eq!(
            unsafe {
                controller.submit_video("owner", opened.created.unwrap(), &video_packet(&bytes, 0))
            },
            STATUS_OK
        );
        opened
    };
    assert!(module.quiesce().unwrap());
    assert!(UdpSocket::bind(opened.payload["video_local"].as_str().unwrap()).is_ok());
    assert!(UdpSocket::bind(opened.payload["audio_local"].as_str().unwrap()).is_ok());
    host.replies.push_back(Response::Pending);
    module.tick().unwrap();
    assert!(module.quiesce().unwrap());
    let mut controller = module.controller.lock().unwrap();
    assert_eq!(
        controller
            .handle("owner", Operation::Status { stream: None })
            .unwrap()
            .payload["streams"],
        json!([])
    );
    assert_eq!(
        controller.handle("owner", Operation::Open(request)).err(),
        Some("shutting_down")
    );
    assert_eq!(
        controller.drain("owner", opened.created.unwrap()),
        amaterasu_media::MEDIA_CLOSED
    );
}

#[test]
fn shutdown_defers_contended_native_calls_and_seals_open_admission() {
    use crate::stream::tests::receivers;
    let mut host = Host {
        typed_network_binds: 0,
        replies: VecDeque::new(),
        complete: None,
    };
    let mut module = lifecycle_fixture(&mut host);
    let (video, audio) = receivers();
    let retained = module.controller.clone();
    let native_call = retained.lock().unwrap();
    module.prepare_shutdown(1000).unwrap();
    assert!(!module.quiesce().unwrap());
    host.replies.extend([
        Response::Request {
            id: 1,
            caller: "owner".into(),
            initiator: "owner".into(),
            service: SERVICE.into(),
            remaining_ms: 1000,
            payload: json!({"action":"open","peer_ip":"127.0.0.1","video_port":video.local_addr().unwrap().port(),"audio_port":audio.local_addr().unwrap().port(),"audio":{"channels":2,"packet_ms":10,"wire_payload_bytes":4}}),
        },
        Response::Ok,
        Response::Ok,
    ]);
    module.tick().unwrap();
    assert_eq!(host.replies.len(), 3);
    drop(native_call);
    module.tick().unwrap();
    assert_eq!(host.complete.unwrap()["code"], "shutting_down");
    assert!(module.quiesce().unwrap());
}

#[test]
fn quiesce_releases_sockets_after_controller_panic() {
    use crate::stream::tests::{receivers, request};
    let mut host = Host {
        typed_network_binds: 0,
        replies: VecDeque::new(),
        complete: None,
    };
    let mut module = lifecycle_fixture(&mut host);
    let (video, audio) = receivers();
    let opened = module
        .controller
        .lock()
        .unwrap()
        .handle("owner", Operation::Open(request(&video, &audio, false)))
        .unwrap();
    let retained = module.controller.clone();
    let _ = std::panic::catch_unwind(|| {
        let _guard = retained.lock().unwrap();
        panic!("simulated controller failure");
    });
    assert!(module.quiesce().unwrap());
    assert!(UdpSocket::bind(opened.payload["video_local"].as_str().unwrap()).is_ok());
    assert!(UdpSocket::bind(opened.payload["audio_local"].as_str().unwrap()).is_ok());
}

#[test]
fn held_udp_probe_does_not_block_existing_cleanup_or_disjoint_open() {
    use amaterasu_module_sdk::{network::abi, transport::udp_preflight::ProbeRequest};
    use std::sync::{
        Condvar, Mutex, OnceLock,
        atomic::{AtomicU16, Ordering},
    };

    #[derive(Default)]
    struct ProbeHost {
        requests: VecDeque<Response>,
        completed: std::collections::BTreeMap<u64, serde_json::Value>,
    }

    unsafe extern "C" fn invoke_probe_host(
        context: *mut c_void,
        input: BorrowedBytes,
        output: ReplySink,
    ) -> Status {
        let command: Command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
        let result = if matches!(command, Command::NotifyWorkReady {}) {
            // The worker can notify without accessing the single-threaded fixture.
            Response::Ok
        } else {
            // SAFETY: the host calls all other commands serially and retains this fixture.
            let host = unsafe { &mut *context.cast::<ProbeHost>() };
            match command {
                Command::GetSettings {} => Response::Result {
                    payload: json!({"enabled":true,"allowed_principals":["consumer"]}),
                },
                Command::Poll {} => host.requests.pop_front().unwrap_or(Response::Pending),
                Command::Complete { request, payload } => {
                    host.completed.insert(request, payload);
                    Response::Ok
                }
                _ => Response::Ok,
            }
        };
        unsafe { output.send(&serde_json::to_vec(&result).unwrap()) };
        STATUS_OK
    }

    type Bind = unsafe extern "C" fn(*mut c_void, abi::Address, *mut u64) -> Status;
    static ORIGINAL_BIND: OnceLock<Bind> = OnceLock::new();
    static TEST_PORT: AtomicU16 = AtomicU16::new(0);
    static GATE: (Mutex<(bool, bool)>, Condvar) = (Mutex::new((false, false)), Condvar::new());

    unsafe extern "C" fn hold_probe(
        context: *mut c_void,
        address: abi::Address,
        output: *mut u64,
    ) -> Status {
        if address.port == TEST_PORT.load(Ordering::Acquire) {
            let mut gate = GATE.0.lock().unwrap();
            gate.0 = true;
            GATE.1.notify_all();
            let _ = GATE
                .1
                .wait_timeout_while(gate, Duration::from_secs(2), |gate| !gate.1)
                .unwrap();
        }
        // SAFETY: this wrapper forwards the unchanged ABI arguments to the retained factory.
        unsafe { ORIGINAL_BIND.get().unwrap()(context, address, output) }
    }

    let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
    let peer_port = peer.local_addr().unwrap().port();
    let reserved = UdpSocket::bind("127.0.0.1:0").unwrap();
    let probe_address = reserved.local_addr().unwrap();
    TEST_PORT.store(probe_address.port(), Ordering::Release);
    let factory = amaterasu_network_sockets::Factory::new();
    // SAFETY: the factory and copy of its table live until the module joins its probe.
    let raw = unsafe { amaterasu_network_sockets::bind_standalone(&factory, "media-probe-test") }
        .unwrap();
    let mut table = unsafe { raw.read() };
    ORIGINAL_BIND.set(table.udp_bind).unwrap();
    table.udp_bind = hold_probe;
    let network = unsafe { Network::from_native_table(&table) }.unwrap();

    let request = |id, payload| Response::Request {
        id,
        caller: "consumer".into(),
        initiator: "consumer".into(),
        service: SERVICE.into(),
        remaining_ms: 1_000,
        payload,
    };
    let open = |operation| {
        json!({"action":"open","operation_id":operation,"peer_ip":"127.0.0.1",
        "video_port":peer_port,"audio_port":peer_port,"audio":{"channels":2,"packet_ms":10,"wire_payload_bytes":4}})
    };
    let mut host = ProbeHost::default();
    host.requests.push_back(request(1, open("existing")));
    let endpoint = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (&mut host as *mut ProbeHost).cast(),
        invoke: invoke_probe_host,
    };
    let mut module = MediaTransportModule::start_configured(
        unsafe { RuntimeClient::from_endpoint(endpoint) },
        Some(network),
    )
    .unwrap();
    module.tick().unwrap();
    let old_stream = host.completed[&1]["stream"].as_u64().unwrap();
    host.requests
        .push_back(request(2, ProbeRequest::for_transport(vec![probe_address])));
    module.tick().unwrap();

    let gate = GATE.0.lock().unwrap();
    let (gate, _) = GATE
        .1
        .wait_timeout_while(gate, Duration::from_secs(1), |gate| !gate.0)
        .unwrap();
    assert!(gate.0, "the provider is holding the UDP probe open");
    drop(gate);
    for (id, payload) in [
        (3, json!({"action":"status","stream":old_stream})),
        (4, json!({"action":"close","stream":old_stream})),
        (5, open("disjoint")),
    ] {
        host.requests.push_back(request(id, payload));
        module.tick().unwrap();
        assert_eq!(host.completed[&id]["ok"], true);
    }

    let mut overlapping = open("overlap");
    overlapping["video_bind"] = json!(probe_address);
    host.requests.push_back(request(6, overlapping));
    module.tick().unwrap();
    assert!(
        !host.completed.contains_key(&6),
        "only the conflicting open waits for probe retirement"
    );

    drop(reserved);
    GATE.0.lock().unwrap().1 = true;
    GATE.1.notify_all();
    let until = Instant::now() + Duration::from_secs(1);
    while !host.completed.contains_key(&6) && Instant::now() < until {
        module.tick().unwrap();
        std::thread::yield_now();
    }
    assert_eq!(host.completed[&2]["code"], "udp_probe_cancelled");
    assert_eq!(host.completed[&6]["video_local"], json!(probe_address));
}
