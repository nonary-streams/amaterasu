use super::*;

fn negotiation() -> Negotiation {
    Negotiation::new(
        TrustedLaunch {
            epoch: 7,
            rtsp_target: "rtspenc://127.0.0.1:48010/launch-1".into(),
            session_token: "opaque-session-1".into(),
            encrypted_rtsp: true,
            audio_port: 49000,
            video_port: 49002,
            control_port: 49004,
            control_connect_data: 123,
            udp_discovery: None,
        },
        Capabilities {
            transport: sdp::TransportSupport {
                hevc: false,
                av1: false,
                stereo_audio: true,
                enet_control: true,
                encrypted_control: true,
                control_v2: true,
                encrypted_remote_input: false,
                encrypted_video: true,
                encrypted_audio: true,
                fec: true,
            },
            supported_colors: vec![sdp::ColorMode::Bt709Limited],
            max_width: 1920,
            max_height: 1080,
            max_fps: 60,
            max_bitrate_kbps: 20000,
            require_video_encryption: false,
            require_audio_encryption: false,
        },
    )
    .unwrap()
}

#[test]
fn hevc_describe_marker_is_ticket_capability_scoped() {
    let default = negotiation();
    assert!(!validation::description(&default.capabilities).contains("sprop-parameter-sets"));
    let mut enabled = negotiation();
    enabled.capabilities.transport.hevc = true;
    let description = validation::description(&enabled.capabilities);
    assert!(description.contains("sprop-parameter-sets=AAAAAU\r\n"));
    assert!(description.contains("H264/90000"));
    assert!(!description.contains("HEVC/90000"));
    assert!(!description.contains("AV1/90000"));
}

#[test]
fn av1_describe_marker_is_ticket_capability_scoped() {
    let mut enabled = negotiation();
    enabled.capabilities.transport.av1 = true;
    let description = validation::description(&enabled.capabilities);
    assert!(description.contains("a=rtpmap:98 AV1/90000\r\n"));
    assert!(!description.contains("sprop-parameter-sets"));
    let video =
        &description[description.find("m=video").unwrap()..description.find("m=audio").unwrap()];
    assert!(
        video.contains("AV1/90000"),
        "marker belongs to the video section"
    );

    enabled.capabilities.transport.hevc = true;
    let both = validation::description(&enabled.capabilities);
    assert!(both.contains("sprop-parameter-sets=AAAAAU\r\n") && both.contains("AV1/90000"));
}

fn request(method: Method, target: &str, cseq: u32, session: bool) -> Request {
    let mut headers = vec![];
    if session {
        headers.push(("session".into(), "opaque-session-1".into()));
    }
    Request {
        method,
        target: target.into(),
        cseq,
        headers,
        body: vec![],
    }
}

fn setup_to_announce(machine: &mut Negotiation) {
    let target = machine.launch.rtsp_target.clone();
    machine
        .handle(7, &request(Method::Options, &target, 1, false))
        .unwrap();
    let mut describe = request(Method::Describe, &target, 2, false);
    describe
        .headers
        .push(("accept".into(), "application/sdp".into()));
    let Step::Reply(reply) = machine.handle(7, &describe).unwrap() else {
        panic!()
    };
    let reply = String::from_utf8(reply).unwrap();
    assert!(reply.contains("H264/90000") && reply.contains("opus/48000/2"));
    assert!(
        !reply.contains("AAAAAU")
            && !reply.contains("AV1")
            && !reply.contains("refPicInvalidation")
    );
    for (cseq, target, port) in [
        (3, "streamid=audio/0/0", 49000),
        (4, "streamid=video/0/0", 49002),
        (5, "streamid=control/13/0", 49004),
    ] {
        let mut setup = request(Method::Setup, target, cseq, cseq > 3);
        setup.headers.push((
            "transport".into(),
            "unicast;X-GS-ClientPort=50000-50001".into(),
        ));
        let Step::Reply(reply) = machine.handle(7, &setup).unwrap() else {
            panic!()
        };
        let reply = String::from_utf8(reply).unwrap();
        assert!(reply.contains(&format!("server_port={port}")));
        assert!(!reply.contains("X-SS-Ping-Payload"));
        if cseq == 5 {
            assert!(reply.contains("X-SS-Connect-Data: 123"));
        }
    }
}

#[test]
fn discovery_payloads_are_scoped_to_the_corresponding_encrypted_setup() {
    let mut machine = negotiation();
    machine.launch.udp_discovery = Some(UdpDiscovery {
        audio_ping_payload: "AudioToken_12345".into(),
        video_ping_payload: "VideoToken_12345".into(),
    });
    let target = machine.launch.rtsp_target.clone();
    let Step::Reply(options) = machine
        .handle(7, &request(Method::Options, &target, 1, false))
        .unwrap()
    else {
        panic!()
    };
    let mut describe = request(Method::Describe, &target, 2, false);
    describe
        .headers
        .push(("accept".into(), "application/sdp".into()));
    let Step::Reply(description) = machine.handle(7, &describe).unwrap() else {
        panic!()
    };
    assert!(!options.windows(5).any(|bytes| bytes == b"Token"));
    assert!(!description.windows(5).any(|bytes| bytes == b"Token"));

    for (cseq, target, expected, forbidden) in [
        (
            3,
            "streamid=audio/0/0",
            Some("AudioToken_12345"),
            "VideoToken_12345",
        ),
        (
            4,
            "streamid=video/0/0",
            Some("VideoToken_12345"),
            "AudioToken_12345",
        ),
        (5, "streamid=control/13/0", None, "AudioToken_12345"),
    ] {
        let mut setup = request(Method::Setup, target, cseq, cseq > 3);
        setup.headers.push((
            "transport".into(),
            "unicast;X-GS-ClientPort=50000-50001".into(),
        ));
        let Step::Reply(reply) = machine.handle(7, &setup).unwrap() else {
            panic!()
        };
        let reply = String::from_utf8(reply).unwrap();
        if let Some(payload) = expected {
            assert!(reply.contains(&format!("X-SS-Ping-Payload: {payload}\r\n")));
        } else {
            assert!(!reply.contains("X-SS-Ping-Payload"));
            assert!(!reply.contains("VideoToken_12345"));
        }
        assert!(!reply.contains(forbidden));
    }
}

#[test]
fn discovery_requires_distinct_base64url_payloads_and_encrypted_rtsp() {
    for (audio, video, encrypted) in [
        ("short", "VideoToken_12345", true),
        ("AudioToken+12345", "VideoToken_12345", true),
        ("AudioToken_12345", "VideoToken/12345", true),
        ("AudioToken_12345", "AudioToken_12345", true),
        ("AudioToken_12345", "VideoToken_12345", false),
    ] {
        let mut machine = negotiation();
        machine.launch.udp_discovery = Some(UdpDiscovery {
            audio_ping_payload: audio.into(),
            video_ping_payload: video.into(),
        });
        machine.launch.encrypted_rtsp = encrypted;
        if !encrypted {
            machine.launch.rtsp_target = "rtsp://127.0.0.1:48010/launch-1".into();
        }
        assert!(matches!(
            Negotiation::new(machine.launch, machine.capabilities),
            Err(Error::InvalidLaunch)
        ));
    }
}

fn announce() -> Request {
    let mut request = request(Method::Announce, "streamid=control/13/0", 6, true);
    request
        .headers
        .push(("content-type".into(), "application/sdp".into()));
    request.body = include_str!("../../sdp/fixtures/moonlight-h264.sdp")
        .replace(
            "x-ss-general.encryptionEnabled:0",
            "x-ss-general.encryptionEnabled:1",
        )
        .into_bytes();
    request
}

#[test]
fn primary_modern_order_requires_backend_acceptance_before_play_success() {
    let mut machine = negotiation();
    setup_to_announce(&mut machine);
    let Step::Action {
        ticket,
        action: Action::Announce(value),
    } = machine.handle(7, &announce()).unwrap()
    else {
        panic!()
    };
    assert_eq!(value.video.width, 1920);
    assert_eq!(machine.state(), State::Announce);
    assert_eq!(
        machine.handle(7, &request(Method::Play, "/", 7, true)),
        Err(Error::Pending)
    );
    machine.complete(7, ticket, true).unwrap();
    let Step::Action {
        ticket,
        action: Action::Play,
    } = machine
        .handle(7, &request(Method::Play, "/", 7, true))
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(machine.state(), State::Play);
    machine.complete(7, ticket, true).unwrap();
    assert_eq!(machine.state(), State::Playing);
}

#[test]
fn epoch_session_and_completion_tickets_cannot_adopt_another_launch() {
    let mut machine = negotiation();
    setup_to_announce(&mut machine);
    assert_eq!(machine.handle(8, &announce()), Err(Error::WrongEpoch));
    let mut wrong = announce();
    wrong.headers[0].1 = "other-session".into();
    assert_eq!(machine.handle(7, &wrong), Err(Error::WrongSession));
    let Step::Action { ticket, .. } = machine.handle(7, &announce()).unwrap() else {
        panic!()
    };
    assert_eq!(machine.complete(8, ticket, true), Err(Error::WrongEpoch));
    assert_eq!(
        machine.complete(7, ticket + 1, true),
        Err(Error::WrongTicket)
    );
    machine.complete(7, ticket, true).unwrap();
    assert_eq!(machine.complete(7, ticket, true), Err(Error::WrongTicket));
    assert_eq!(machine.handle(7, &announce()), Err(Error::InvalidSequence));
}

#[test]
fn backend_rejection_never_emits_success_or_arms_play_but_allows_cleanup() {
    let mut machine = negotiation();
    setup_to_announce(&mut machine);
    let Step::Action { ticket, .. } = machine.handle(7, &announce()).unwrap() else {
        panic!()
    };
    let response = machine.complete(7, ticket, false).unwrap();
    assert!(response.starts_with(b"RTSP/1.0 503 "));
    assert_eq!(machine.state(), State::Failed);
    assert_eq!(
        machine.handle(7, &request(Method::Play, "/", 7, true)),
        Err(Error::WrongState)
    );
    let Step::Action {
        ticket,
        action: Action::Teardown,
    } = machine
        .handle(7, &request(Method::Teardown, "/", 8, true))
        .unwrap()
    else {
        panic!()
    };
    machine.complete(7, ticket, true).unwrap();
    assert_eq!(machine.state(), State::Closed);
    assert_eq!(
        machine.handle(7, &request(Method::Teardown, "/", 9, true)),
        Err(Error::WrongState)
    );
}

#[test]
fn unexpected_targets_sequence_and_transport_extensions_do_not_advance() {
    let mut machine = negotiation();
    assert_eq!(
        machine.handle(7, &request(Method::Setup, "streamid=audio/0/0", 1, false)),
        Err(Error::WrongState)
    );
    assert_eq!(
        machine.handle(7, &request(Method::Options, "*", 1, false)),
        Err(Error::InvalidTarget)
    );
    let target = machine.launch.rtsp_target.clone();
    machine
        .handle(7, &request(Method::Options, &target, 1, false))
        .unwrap();
    let mut describe = request(Method::Describe, &target, 2, false);
    describe
        .headers
        .push(("accept".into(), "application/sdp".into()));
    machine.handle(7, &describe).unwrap();
    for transport in [
        "RTP/AVP/TCP;interleaved=0-1",
        "multicast;X-GS-ClientPort=50000-50001",
        "unicast;X-GS-ClientPort=0-1",
        "unicast;X-GS-ClientPort=50000-50002",
        "unicast;X-GS-ClientPort=50000-50001;destination=192.0.2.1",
    ] {
        let mut setup = request(Method::Setup, "streamid=audio/0/0", 3, false);
        setup.headers.push(("transport".into(), transport.into()));
        assert_eq!(machine.handle(7, &setup), Err(Error::UnsupportedTransport));
        assert_eq!(machine.state(), State::SetupAudio);
    }
}

#[test]
fn unsupported_media_cannot_reach_backend_actions() {
    for (before, after) in [
        ("encryptionEnabled:1", "encryptionEnabled:0"),
        ("encoderCscMode:2", "encoderCscMode:0"),
        ("dynamicRangeMode:0", "dynamicRangeMode:1"),
        ("packetDuration:5", "packetDuration:20"),
    ] {
        let mut machine = negotiation();
        setup_to_announce(&mut machine);
        let mut input = announce();
        let body = String::from_utf8(input.body).unwrap();
        assert!(body.contains(before), "{before}");
        input.body = body.replace(before, after).into_bytes();
        assert_eq!(machine.handle(7, &input), Err(Error::UnsupportedMedia));
        assert_eq!(machine.state(), State::Announce);
    }
}

#[test]
fn announced_color_requires_an_explicit_supported_mode() {
    let mut machine = negotiation();
    let mut announced = sdp::parse_announce(&announce().body).expect("valid ANNOUNCE");
    for mode in [
        sdp::ColorMode::Bt601Limited,
        sdp::ColorMode::Bt601Full,
        sdp::ColorMode::Bt709Limited,
        sdp::ColorMode::Bt709Full,
    ] {
        announced.video.color = mode;
        machine.capabilities.supported_colors = vec![mode];
        assert_eq!(validation::media(&announced, &machine.capabilities), Ok(()));
        machine.capabilities.supported_colors = vec![sdp::ColorMode::Bt709Limited];
        if mode != sdp::ColorMode::Bt709Limited {
            assert_eq!(
                validation::media(&announced, &machine.capabilities),
                Err(Error::UnsupportedMedia)
            );
        }
    }
}

#[test]
fn primary_client_shaped_raw_options_uses_existing_fragmented_decoder() {
    let raw=b"OPTIONS rtspenc://127.0.0.1:48010/launch-1 RTSP/1.0\r\nCSeq: 1\r\nX-GS-ClientVersion: 14\r\n\r\n";
    let mut decoder = super::super::Decoder::default();
    for fragment in raw.chunks(3) {
        decoder.push(fragment).unwrap();
    }
    let input = decoder.next_request().unwrap().unwrap();
    let mut machine = negotiation();
    let Step::Reply(reply) = machine.handle(7, &input).unwrap() else {
        panic!()
    };
    assert!(reply.starts_with(b"RTSP/1.0 200 OK\r\nCSeq: 1\r\n"));
}

#[test]
fn public_request_bounds_and_launch_metadata_are_validated() {
    let mut machine = negotiation();
    let target = machine.launch.rtsp_target.clone();
    let mut input = request(Method::Options, &target, 1, false);
    input.headers = vec![("x-test".into(), "value".into()); 65];
    assert_eq!(machine.handle(7, &input), Err(Error::InvalidHeaders));
    input.headers.clear();
    input.body = vec![0];
    assert_eq!(machine.handle(7, &input), Err(Error::InvalidBody));
    let mut launch = machine.launch;
    launch.session_token = "token\r\ninjected".into();
    assert!(matches!(
        Negotiation::new(launch, machine.capabilities),
        Err(Error::InvalidLaunch)
    ));
}
