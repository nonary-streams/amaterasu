//! Exercise the unchanged RTSP consumer with explicitly selected Network SDK
//! sockets. The downstream fixture opens real, independently owned UDP lanes.
use super::*;
use amaterasu_gamestream::rtsp::Method;
use amaterasu_module_sdk::{
    abi::{self, AbiHeader, BorrowedBytes, HostApi, ReplySink},
    network::UdpSocket,
};
use std::{
    ffi::c_void,
    net::{Ipv4Addr, Ipv6Addr},
};

struct Endpoint {
    network: Network,
    next: u64,
    replies: BTreeMap<u64, Value>,
    lanes: BTreeMap<u64, Vec<UdpSocket>>,
    prepares: Vec<Value>,
    completed: Vec<Value>,
}

impl Endpoint {
    fn respond(&mut self, command: Command) -> Response {
        match command {
            Command::Request { payload, .. } => {
                self.next += 1;
                let id = self.next;
                let response = match payload["action"].as_str() {
                    Some("prepare") => {
                        self.prepares.push(payload.clone());
                        let lanes: Vec<_> = ["audio_bind", "video_bind", "control_bind"]
                            .map(|field| {
                                self.network
                                    .bind_udp(
                                        serde_json::from_value(payload[field].clone()).unwrap(),
                                    )
                                    .unwrap()
                            })
                            .into_iter()
                            .collect();
                        let addresses: Vec<_> = lanes
                            .iter()
                            .map(|lane| lane.local_addr().unwrap())
                            .collect();
                        self.lanes.insert(id, lanes);
                        json!({"ok":true,"state":"prepared","session":id,
                            "audio_local":addresses[0],"video_local":addresses[1],"control_local":addresses[2],
                            "history_activity_id":payload["history_activity_id"],
                            "rtsp_operation_id":payload["rtsp_operation_id"]})
                    }
                    Some("stop") => {
                        let session = payload["session"].as_u64().unwrap();
                        self.lanes.remove(&session).expect("owned session lanes");
                        json!({"ok":true,"session":session,"state":"stopped","cleanup_error":null})
                    }
                    action => panic!("unexpected downstream action {action:?}"),
                };
                self.replies.insert(id, response);
                Response::Accepted { id }
            }
            Command::TakeResult { request } => Response::Result {
                payload: self.replies[&request].clone(),
            },
            Command::Complete { payload, .. } => {
                self.completed.push(payload);
                Response::Ok
            }
            Command::CheckRequest { .. }
            | Command::ReleaseResult { .. }
            | Command::Cancel { .. } => Response::Ok,
            other => panic!("unexpected command {other:?}"),
        }
    }
}

unsafe extern "C" fn invoke(
    context: *mut c_void,
    input: BorrowedBytes,
    reply: ReplySink,
) -> abi::Status {
    // SAFETY: each test retains its boxed endpoint for the synchronous calls.
    let endpoint = unsafe { &mut *context.cast::<Endpoint>() };
    // SAFETY: the runtime lends a complete serialized command through this call.
    let command = serde_json::from_slice(unsafe { input.read() }.unwrap()).unwrap();
    let response = serde_json::to_vec(&endpoint.respond(command)).unwrap();
    // SAFETY: the synchronous sink copies the serialized response before return.
    unsafe { reply.send(&response) };
    abi::STATUS_OK
}

fn fixture(address: IpAddr) -> (Controller, Box<Endpoint>, RuntimeClient) {
    let network = Network::test_provider();
    let settings = serde_json::from_value(json!({"enabled":true,
        "bind":SocketAddr::new(address, 0),"bind_address":address.to_string(),
        "address_family":if address.is_ipv4() {"ipv4"} else {"both"},
        "allowed_principals":["amaterasu.nvhttp"]}))
    .unwrap();
    let mut controller = Controller::with_network(settings, network.clone()).unwrap();
    // The TCP listener remains ephemeral. Find an available configured UDP set
    // for the fixture's preparation without opening any wildcard socket.
    let base = super::tests::free_base_port();
    controller.listener_base_port = Some(base);
    let mut endpoint = Box::new(Endpoint {
        network,
        next: 0,
        replies: BTreeMap::new(),
        lanes: BTreeMap::new(),
        prepares: Vec::new(),
        completed: Vec::new(),
    });
    let api = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (&mut *endpoint as *mut Endpoint).cast(),
        invoke,
    };
    // SAFETY: the returned boxed endpoint outlives all runtime calls in the test.
    let runtime = unsafe { RuntimeClient::from_endpoint(api) };
    (controller, endpoint, runtime)
}

fn request(address: IpAddr, client: u8) -> Open {
    serde_json::from_value(json!({"operation_id":format!("client-{client}"),
        "peer_ip":address,"video_port":50000,"audio_port":50002,
        "key_hex":format!("{client:02x}").repeat(16),"key_id":client,"connect_data":client,
        "capture":{"capture_provider":"dxgi","output":"display","encoder":1,"audio_endpoint":"default"}})).unwrap()
}

fn admit(controller: &mut Controller, runtime: &RuntimeClient, address: IpAddr, client: u8) -> u64 {
    controller.open(
        OpenRequest {
            request: u64::from(client),
            caller: "amaterasu.nvhttp".into(),
            config: request(address, client),
            deadline: 5_000,
            display_preflight_deadline: None,
        },
        0,
        runtime,
        false,
    );
    controller
        .records
        .values()
        .find(|record| record.config.operation_id == format!("client-{client}"))
        .unwrap()
        .id
}

fn settle(controller: &mut Controller, runtime: &RuntimeClient) {
    for now in 1..16 {
        controller.advance_job(runtime, now);
        if controller.job.is_none() {
            return;
        }
    }
    panic!("downstream operation did not settle");
}

fn assert_setup_ports(record: &mut Record, addresses: &[SocketAddr]) {
    let ticket = record.id;
    let url = record.url.clone();
    let token = record.session_token.clone();
    let machine = record.negotiation.as_mut().unwrap();
    let make = |method, target: &str, cseq, session| Request {
        method,
        target: target.into(),
        cseq,
        headers: if session {
            vec![("session".into(), token.clone())]
        } else {
            vec![]
        },
        body: vec![],
    };
    machine
        .handle(ticket, &make(Method::Options, &url, 1, false))
        .unwrap();
    let mut describe = make(Method::Describe, &url, 2, false);
    describe
        .headers
        .push(("accept".into(), "application/sdp".into()));
    machine.handle(ticket, &describe).unwrap();
    for (index, target) in [
        "streamid=audio/0/0",
        "streamid=video/0/0",
        "streamid=control/13/0",
    ]
    .into_iter()
    .enumerate()
    {
        let mut setup = make(Method::Setup, target, 3 + index as u32, index > 0);
        setup.headers.push((
            "transport".into(),
            "unicast;X-GS-ClientPort=50000-50001".into(),
        ));
        let NegotiationStep::Reply(reply) = machine.handle(ticket, &setup).unwrap() else {
            panic!("SETUP reply required")
        };
        assert!(
            String::from_utf8(reply)
                .unwrap()
                .contains(&format!("server_port={}", addresses[index].port()))
        );
    }
}

fn receive(lane: &UdpSocket, expected: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut buffer = [0; 32];
    loop {
        match lane.recv_from(&mut buffer) {
            Ok((length, _)) => {
                assert_eq!(&buffer[..length], expected);
                return;
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::yield_now()
            }
            result => panic!("expected UDP delivery: {result:?}"),
        }
    }
}

#[test]
fn concurrent_clients_negotiate_distinct_provider_ports_and_survive_peer_teardown() {
    let _guard = super::tests::NETWORK_TEST_LOCK.lock().unwrap();
    let ipv4 = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let ipv6 = IpAddr::V6(Ipv6Addr::LOCALHOST);
    for (address, second_peer) in [
        (ipv4, ipv4),
        (ipv4, IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2))),
        (ipv6, ipv6),
    ] {
        let (mut controller, endpoint, runtime) = fixture(address);
        let first = admit(&mut controller, &runtime, address, 1);
        settle(&mut controller, &runtime);
        assert_eq!(controller.records[&first].state, State::Ready);
        let second = admit(&mut controller, &runtime, second_peer, 2);
        settle(&mut controller, &runtime);
        assert_eq!(controller.records[&second].state, State::Ready);
        let first_session = controller.records[&first].session.unwrap();
        let second_session = controller.records[&second].session.unwrap();
        let first_addresses: Vec<_> = endpoint.lanes[&first_session]
            .iter()
            .map(|lane| lane.local_addr().unwrap())
            .collect();
        let second_addresses: Vec<_> = endpoint.lanes[&second_session]
            .iter()
            .map(|lane| lane.local_addr().unwrap())
            .collect();
        for field in ["audio_bind", "video_bind", "control_bind"] {
            let fixed: SocketAddr =
                serde_json::from_value(endpoint.prepares[0][field].clone()).unwrap();
            let additional: SocketAddr =
                serde_json::from_value(endpoint.prepares[1][field].clone()).unwrap();
            assert_eq!(fixed.ip(), address);
            assert_ne!(fixed.port(), 0);
            assert_eq!(additional, SocketAddr::new(address, fixed.port() + 3));
        }
        assert_eq!(
            first_addresses
                .iter()
                .chain(&second_addresses)
                .collect::<BTreeSet<_>>()
                .len(),
            6
        );
        assert_setup_ports(
            controller.records.get_mut(&first).unwrap(),
            &first_addresses,
        );
        assert_setup_ports(
            controller.records.get_mut(&second).unwrap(),
            &second_addresses,
        );
        let sender = endpoint
            .network
            .bind_udp(SocketAddr::new(address, 0))
            .unwrap();
        for (index, destination) in first_addresses.iter().copied().enumerate() {
            sender.send_to(b"first", destination).unwrap();
            receive(&endpoint.lanes[&first_session][index], b"first");
            assert_eq!(
                endpoint.lanes[&second_session][index]
                    .recv_from(&mut [0; 32])
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
        controller.close(9, "amaterasu.nvhttp", first, 20, &runtime);
        settle(&mut controller, &runtime);
        assert!(controller.records[&first].cleanup_confirmed);
        assert!(controller.records[&first].state.terminal());
        assert!(!endpoint.lanes.contains_key(&first_session));
        let reusable = controller.media_binds_for_open(address).unwrap();
        assert_eq!(reusable.video, first_addresses[1]);
        assert_eq!(reusable.audio, first_addresses[0]);
        assert_eq!(reusable.control, first_addresses[2]);
        for (index, destination) in second_addresses.into_iter().enumerate() {
            sender.send_to(b"second survives", destination).unwrap();
            receive(&endpoint.lanes[&second_session][index], b"second survives");
        }
        assert_eq!(controller.records[&second].state, State::Ready);
    }
}

#[test]
fn pending_and_uncertain_transport_reservations_keep_distinct_bounded_ports() {
    let _guard = super::tests::NETWORK_TEST_LOCK.lock().unwrap();
    let address = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let (mut controller, endpoint, runtime) = fixture(address);
    let first = admit(&mut controller, &runtime, address, 1);
    let fixed = controller.records[&first].media_binds.unwrap();
    assert_ne!(fixed.video.port(), 0);
    // This record has not even dispatched its provider request yet.
    assert_eq!(controller.records[&first].state, State::Preparing);
    for state in [State::Preparing, State::CleanupFailed, State::Failed] {
        controller.records.get_mut(&first).unwrap().state = state;
        let additional = controller.media_binds_for_open(address).unwrap();
        assert_eq!(
            [
                additional.video.port(),
                additional.control.port(),
                additional.audio.port()
            ],
            [
                fixed.video.port() + 3,
                fixed.control.port() + 3,
                fixed.audio.port() + 3
            ]
        );
        assert_eq!(additional.video.ip(), address);
    }
    controller.records.get_mut(&first).unwrap().state = State::Preparing;
    let second = admit(&mut controller, &runtime, address, 2);
    assert!(controller.media_binds_for_open(address).is_none());
    controller.open(
        OpenRequest {
            request: 3,
            caller: "amaterasu.nvhttp".into(),
            config: request(address, 3),
            deadline: 5_000,
            display_preflight_deadline: None,
        },
        0,
        &runtime,
        false,
    );
    assert_eq!(
        endpoint.completed.last().unwrap()["code"],
        "capacity_exceeded"
    );
    assert_eq!(controller.records.len(), 2);
    controller.records.get_mut(&first).unwrap().state = State::Failed;
    controller.records.get_mut(&second).unwrap().state = State::Closed;
    let prepare_count = endpoint.prepares.len();
    controller.open(
        OpenRequest {
            request: 4,
            caller: "amaterasu.nvhttp".into(),
            config: request(address, 4),
            deadline: 5_000,
            display_preflight_deadline: None,
        },
        0,
        &runtime,
        false,
    );
    assert_eq!(
        endpoint.completed.last().unwrap()["code"],
        "media_port_capacity_exhausted"
    );
    assert_eq!(controller.records.len(), 2);
    assert_eq!(endpoint.prepares.len(), prepare_count);
    controller
        .records
        .get_mut(&first)
        .unwrap()
        .cleanup_confirmed = true;
    let replacement = admit(&mut controller, &runtime, address, 5);
    assert_eq!(controller.records[&replacement].media_binds, Some(fixed));
    controller
        .records
        .get_mut(&second)
        .unwrap()
        .cleanup_confirmed = true;
    let second_slot = controller.media_binds_for_open(address).unwrap();
    assert_eq!(second_slot.video.port(), fixed.video.port() + 3);
}
