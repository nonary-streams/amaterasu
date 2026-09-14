use super::*;
use amaterasu_gamestream::rtsp::Method;
use amaterasu_module_sdk::abi::{self, AbiHeader, BorrowedBytes, HostApi, ReplySink};
use std::ffi::c_void;

#[derive(Default)]
struct Endpoint {
    commands: Vec<Command>,
}

impl Endpoint {
    fn dispatch(&self, command: &Command) -> Response {
        match command {
            Command::CheckRequest { .. } => Response::error("request_ended", "cancelled"),
            Command::Request { .. } => Response::Accepted { id: 71 },
            Command::Cancel { .. } => Response::error("unavailable", "deliberate failure"),
            _ => Response::Ok,
        }
    }
}

unsafe extern "C" fn invoke(
    context: *mut c_void,
    input: BorrowedBytes,
    reply: ReplySink,
) -> abi::Status {
    // SAFETY: the test keeps Endpoint exclusively alive for the client lifetime.
    let endpoint = unsafe { &mut *context.cast::<Endpoint>() };
    let Ok(bytes) = (unsafe { input.read() }) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    let Ok(command) = serde_json::from_slice::<Command>(bytes) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    endpoint.commands.push(command.clone());
    let output = serde_json::to_vec(&endpoint.dispatch(&command)).expect("test response");
    // SAFETY: the synchronous SDK reply sink copies these bytes before return.
    unsafe { reply.send(&output) };
    abi::STATUS_OK
}

fn client(endpoint: &mut Endpoint) -> RuntimeClient {
    let api = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (endpoint as *mut Endpoint).cast(),
        invoke,
    };
    // SAFETY: caller retains endpoint for the synchronous test lifetime.
    unsafe { RuntimeClient::from_endpoint(api) }
}

fn open() -> Open {
    serde_json::from_value(json!({
        "operation_id":"caller-operation",
        "peer_ip":"127.0.0.1",
        "video_port":50000,
        "audio_port":50002,
        "key_hex":"00112233445566778899aabbccddeeff",
        "key_id":1,
        "connect_data":2,
        "capture":{
            "capture_provider":"dxgi",
            "output":"display",
            "encoder":1,
            "audio_endpoint":"default"
        }
    }))
    .expect("valid open")
}

fn record(state: State) -> Record {
    Record {
        id: 5,
        owner: "caller".into(),
        config: open(),
        state,
        session: Some(23),
        negotiation: None,
        url: "rtspenc://127.0.0.1:48010/launch-5".into(),
        session_token: "00112233445566778899aabbccddeeff".into(),
        udp_discovery: None,
        local: "127.0.0.1:48010".parse().expect("literal"),
        expires: 5_000,
        next_status: 5_000,
        failure: None,
        cleanup_error: None,
        uncertain: None,
        uncertain_phase: None,
        needs_lookup: false,
        stream_state: Some("stopping".into()),
        revoke_after: None,
        cleanup_confirmed: false,
    }
}

#[test]
fn cancelled_parent_reaches_cleanup_failed_when_settlement_stays_unavailable() {
    let mut controller = Controller::new(Settings::default()).expect("disabled controller");
    let config = open();
    let record = Record {
        id: 5,
        owner: "caller".into(),
        config,
        state: State::Preparing,
        session: None,
        negotiation: None,
        url: "rtspenc://127.0.0.1:48010/launch-5".into(),
        session_token: "00112233445566778899aabbccddeeff".into(),
        udp_discovery: None,
        local: "127.0.0.1:48010".parse().expect("literal"),
        expires: 5_000,
        next_status: 5_000,
        failure: None,
        cleanup_error: None,
        uncertain: None,
        uncertain_phase: None,
        needs_lookup: true,
        stream_state: None,
        revoke_after: None,
        cleanup_confirmed: false,
    };
    let payload = record.config.prepare(&record.operation(), None);
    controller.records.insert(record.id, record);
    controller.job = Some(Job {
        ticket: 5,
        phase: Phase::Prepare,
        call: Call::new(payload, "prepare-request".into(), Some(17)),
        caller: Some(17),
        network: None,
        deadline: 5_000,
    });
    let mut endpoint = Endpoint::default();
    let runtime = client(&mut endpoint);

    for now in 0..5 {
        controller.advance_job(&runtime, now);
    }

    let record = &controller.records[&5];
    assert!(record.state == State::CleanupFailed);
    assert_eq!(
        record.cleanup_error,
        Some("request_cancellation_unconfirmed")
    );
    assert!(record.uncertain.is_some());
    assert_eq!(
        endpoint
            .commands
            .iter()
            .filter(|command| matches!(command, Command::Request { .. }))
            .count(),
        1,
        "settlement retries must not resubmit an admitted mutation"
    );
}

#[test]
fn cleanup_status_polling_preserves_one_deadline() {
    let mut controller = Controller::new(Settings::default()).expect("disabled controller");
    let mut record = record(State::Closing);
    let mut endpoint = Endpoint::default();
    let runtime = client(&mut endpoint);
    let stopping = json!({
        "ok":true,
        "session":23,
        "state":"stopping",
        "cleanup_error":null
    });
    let job = Job {
        ticket: 5,
        phase: Phase::Status,
        call: Call::new(json!({"action":"status"}), "status-1".into(), None),
        caller: None,
        network: None,
        deadline: 99,
    };

    controller.status_result(&mut record, job, &stopping, 10, &runtime);
    let job = controller.job.take().expect("next status poll");
    assert_eq!(job.deadline, 99);
    controller.status_result(&mut record, job, &stopping, 50, &runtime);
    let job = controller.job.take().expect("next status poll");
    assert_eq!(job.deadline, 99);

    controller.records.insert(record.id, record);
    controller.job = Some(job);
    controller.advance_job(&runtime, 99);
    let record = &controller.records[&5];
    assert!(record.state == State::CleanupFailed);
    assert_eq!(record.cleanup_error, Some("cleanup_deadline"));
    assert!(record.uncertain.is_some());
}

fn request(method: Method, target: &str, cseq: u32, session: bool) -> Request {
    let mut headers = Vec::new();
    if session {
        headers.push(("session".into(), "00112233445566778899aabbccddeeff".into()));
    }
    Request {
        method,
        target: target.into(),
        cseq,
        headers,
        body: Vec::new(),
    }
}

fn pending_play(record: &Record) -> (Negotiation, u64) {
    let mut negotiation = Negotiation::new(
        TrustedLaunch {
            epoch: record.id,
            rtsp_target: record.url.clone(),
            session_token: record.session_token.clone(),
            encrypted_rtsp: true,
            audio_port: 49_000,
            video_port: 49_002,
            control_port: 49_004,
            control_connect_data: record.config.connect_data,
            udp_discovery: None,
        },
        record.capabilities(),
    )
    .expect("valid negotiation");
    let target = record.url.as_str();
    negotiation
        .handle(record.id, &request(Method::Options, target, 1, false))
        .expect("options");
    let mut describe = request(Method::Describe, target, 2, false);
    describe
        .headers
        .push(("accept".into(), "application/sdp".into()));
    negotiation.handle(record.id, &describe).expect("describe");
    for (cseq, target) in [
        (3, "streamid=audio/0/0"),
        (4, "streamid=video/0/0"),
        (5, "streamid=control/13/0"),
    ] {
        let mut setup = request(Method::Setup, target, cseq, cseq > 3);
        setup.headers.push((
            "transport".into(),
            "unicast;X-GS-ClientPort=50000-50001".into(),
        ));
        negotiation.handle(record.id, &setup).expect("setup");
    }
    let mut announce = request(Method::Announce, "streamid=control/13/0", 6, true);
    announce
        .headers
        .push(("content-type".into(), "application/sdp".into()));
    announce.body =
        include_str!("../../../../crates/gamestream/src/sdp/fixtures/moonlight-h264.sdp")
            .replace(
                "x-ss-general.encryptionEnabled:0",
                "x-ss-general.encryptionEnabled:1",
            )
            .into_bytes();
    let NegotiationStep::Action { ticket, .. } =
        negotiation.handle(record.id, &announce).expect("announce")
    else {
        panic!("announce action");
    };
    negotiation
        .complete(record.id, ticket, true)
        .expect("announce accepted");
    let NegotiationStep::Action { ticket, .. } = negotiation
        .handle(record.id, &request(Method::Play, "/", 7, true))
        .expect("play")
    else {
        panic!("play action");
    };
    (negotiation, ticket)
}

#[test]
fn unavailable_play_response_never_marks_the_ticket_playing() {
    let mut controller = Controller::new(Settings::default()).expect("disabled controller");
    let mut record = record(State::Arming);
    record.config.profile.bitrate_kbps = 20_000;
    record.config.profile.packet_size = 1_392;
    record.config.profile.packet_ms = 5;
    record.config.profile.encrypt_video = false;
    record.config.profile.encrypt_audio = false;
    let (negotiation, action_ticket) = pending_play(&record);
    record.negotiation = Some(negotiation);
    let job = Job {
        ticket: record.id,
        phase: Phase::Arm,
        call: Call::new(
            json!({"action":"start_when_authenticated"}),
            "arm".into(),
            None,
        ),
        caller: None,
        network: Some((999, action_ticket)),
        deadline: 5_000,
    };

    controller.armed(
        &mut record,
        job,
        &json!({"ok":true,"session":23,"state":"armed","cleanup_error":null}),
        10,
    );

    assert!(record.state == State::Closing);
    assert_eq!(record.failure, Some("start_rejected"));
}
