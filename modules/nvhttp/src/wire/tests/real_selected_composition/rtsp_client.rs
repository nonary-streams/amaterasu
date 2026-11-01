//! Real encrypted RTSP client used only by the loopback composition test.
use amaterasu_gamestream::crypto::{Direction, RtspOpener, RtspSealer};
use amaterasu_gamestream::{
    crypto::{ControlProfile, ControlSealer},
    enet::{
        self, CONTROL_CHANNEL, Command, CommandBody, Connect, Datagram, FLAG_ACKNOWLEDGE, Header,
        Limits, UNCONNECTED_PEER,
    },
};
use std::{
    io::{Read, Write},
    net::{TcpStream, UdpSocket},
    time::{Duration, Instant},
};

const SDP: &str =
    include_str!("../../../../../../crates/gamestream/src/sdp/fixtures/moonlight-h264.sdp");

pub(super) fn play(url: &str, mut tick: impl FnMut()) -> UdpSocket {
    let address = url
        .strip_prefix("rtspenc://")
        .expect("encrypted RTSP")
        .split('/')
        .next()
        .unwrap();
    let key = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ];
    let mut sealer = RtspSealer::new(&key, Direction::ClientToHost).unwrap();
    let mut opener = RtspOpener::new(&key, Direction::HostToClient).unwrap();
    let mut send = |request: String| {
        let mut socket = TcpStream::connect(address).unwrap();
        socket
            .write_all(&sealer.seal(request.as_bytes()).unwrap())
            .unwrap();
        socket.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut bytes = Vec::new();
        loop {
            tick();
            let mut data = [0; 8192];
            match socket.read(&mut data) {
                Ok(0) => break,
                Ok(n) => bytes.extend_from_slice(&data[..n]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("RTSP client read failed: {error}"),
            }
            assert!(Instant::now() < deadline, "RTSP transaction timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
        let plaintext = opener
            .open(&bytes)
            .expect("authenticated RTSP response")
            .plaintext;
        let reply = String::from_utf8(plaintext).unwrap();
        assert!(reply.starts_with("RTSP/1.0 200"), "{reply}");
        reply
    };
    send(format!("OPTIONS {url} RTSP/1.0\r\nCSeq: 1\r\n\r\n"));
    send(format!(
        "DESCRIBE {url} RTSP/1.0\r\nCSeq: 2\r\nAccept: application/sdp\r\n\r\n"
    ));
    let mut token = String::new();
    let mut control_endpoint = None;
    for (sequence, target) in [
        (3, "streamid=audio/0/0"),
        (4, "streamid=video/0/0"),
        (5, "streamid=control/13/0"),
    ] {
        let session = if token.is_empty() {
            String::new()
        } else {
            format!("Session: {token}\r\n")
        };
        let reply = send(format!(
            "SETUP {target} RTSP/1.0\r\nCSeq: {sequence}\r\n{session}Transport: unicast;X-GS-ClientPort=50000-50001\r\n\r\n"
        ));
        if token.is_empty() {
            token = reply
                .lines()
                .find_map(|line| line.strip_prefix("Session: "))
                .unwrap()
                .trim()
                .to_owned();
        }
        if target.starts_with("streamid=control") {
            let port: u16 = reply
                .lines()
                .find_map(|line| line.strip_prefix("Transport: unicast;server_port="))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            let hint: u32 = reply
                .lines()
                .find_map(|line| line.strip_prefix("X-SS-Connect-Data: "))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            control_endpoint = Some((port, hint));
        }
    }
    let body = SDP
        .replace(
            "x-ss-general.encryptionEnabled:0",
            "x-ss-general.encryptionEnabled:7",
        )
        .replace(
            "x-nv-video[0].clientViewportWd:1920",
            "x-nv-video[0].clientViewportWd:1280",
        )
        .replace(
            "x-nv-video[0].clientViewportHt:1080",
            "x-nv-video[0].clientViewportHt:720",
        )
        .replace(
            "x-nv-video[0].packetSize:1392",
            "x-nv-video[0].packetSize:1024",
        )
        .replace(
            "x-nv-video[0].initialBitrateKbps:16000",
            "x-nv-video[0].initialBitrateKbps:10000",
        )
        .replace(
            "x-nv-video[0].initialPeakBitrateKbps:16000",
            "x-nv-video[0].initialPeakBitrateKbps:10000",
        )
        .replace(
            "x-nv-vqos[0].bw.minimumBitrateKbps:16000",
            "x-nv-vqos[0].bw.minimumBitrateKbps:10000",
        )
        .replace(
            "x-nv-vqos[0].bw.maximumBitrateKbps:16000",
            "x-nv-vqos[0].bw.maximumBitrateKbps:10000",
        )
        .replace(
            "x-ml-video.configuredBitrateKbps:20000",
            "x-ml-video.configuredBitrateKbps:10000",
        )
        .replace("x-nv-aqos.packetDuration:5", "x-nv-aqos.packetDuration:10");
    send(format!(
        "ANNOUNCE streamid=control/13/0 RTSP/1.0\r\nCSeq: 6\r\nSession: {token}\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    ));
    send(format!(
        "PLAY / RTSP/1.0\r\nCSeq: 7\r\nSession: {token}\r\n\r\n"
    ));
    drop(send);
    let (port, hint) = control_endpoint.unwrap();
    connect_control(port, hint, &key, &mut tick)
}

fn connect_control(port: u16, hint: u32, key: &[u8; 16], tick: &mut impl FnMut()) -> UdpSocket {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.connect(("127.0.0.1", port)).unwrap();
    socket.set_nonblocking(true).unwrap();
    let encode = |header, command| {
        enet::encode(
            &Datagram {
                header,
                commands: vec![command],
            },
            &Limits::default(),
        )
        .unwrap()
    };
    let connect = encode(
        Header {
            peer_id: UNCONNECTED_PEER,
            session_id: 0,
            sent_time: Some(0),
        },
        Command {
            channel_id: CONTROL_CHANNEL,
            reliable_sequence: 1,
            flags: FLAG_ACKNOWLEDGE,
            body: CommandBody::Connect(Connect {
                outgoing_peer_id: 0,
                incoming_session_id: 255,
                outgoing_session_id: 255,
                mtu: 900,
                window_size: 65536,
                channel_count: 48,
                incoming_bandwidth: 0,
                outgoing_bandwidth: 0,
                packet_throttle_interval: 5000,
                packet_throttle_acceleration: 2,
                packet_throttle_deceleration: 2,
                connect_id: [1, 2, 3, 4],
                data: hint,
            }),
        },
    );
    socket.send(&connect).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut buffer = [0; 4096];
    let verify = loop {
        tick();
        match socket.recv(&mut buffer) {
            Ok(size) => {
                let packet = enet::decode(&buffer[..size], &Limits::default()).unwrap();
                if packet
                    .commands
                    .iter()
                    .any(|command| matches!(command.body, CommandBody::VerifyConnect(_)))
                {
                    break packet;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("control handshake: {error}"),
        }
        assert!(Instant::now() < deadline, "control verify timed out");
        std::thread::sleep(Duration::from_millis(1));
    };
    let verified = verify
        .commands
        .iter()
        .find(|command| matches!(command.body, CommandBody::VerifyConnect(_)))
        .unwrap();
    let ack = encode(
        Header {
            peer_id: 0,
            session_id: 0,
            sent_time: Some(1),
        },
        Command {
            channel_id: CONTROL_CHANNEL,
            reliable_sequence: 1,
            flags: 0,
            body: CommandBody::Acknowledge {
                received_sequence: verified.reliable_sequence,
                received_sent_time: verify.header.sent_time.unwrap(),
            },
        },
    );
    socket.send(&ack).unwrap();
    let mut sealer =
        ControlSealer::new(key, ControlProfile::SunshineV2, Direction::ClientToHost).unwrap();
    let ping = sealer.seal(0x0200, &[4, 0, 0, 0, 0, 0, 0, 0]).unwrap();
    let message = encode(
        Header {
            peer_id: 0,
            session_id: 0,
            sent_time: Some(2),
        },
        Command {
            channel_id: 0,
            reliable_sequence: 1,
            flags: FLAG_ACKNOWLEDGE,
            body: CommandBody::SendReliable { data: ping },
        },
    );
    socket.send(&message).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        tick();
        match socket.recv(&mut buffer) {
            Ok(size) => {
                let packet = enet::decode(&buffer[..size], &Limits::default()).unwrap();
                if packet.commands.iter().any(|command| {
                    matches!(
                        command.body,
                        CommandBody::Acknowledge {
                            received_sequence: 1,
                            ..
                        }
                    ) && command.channel_id == 0
                }) {
                    break;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("control authentication: {error}"),
        }
        assert!(
            Instant::now() < deadline,
            "control message acknowledgment timed out"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    socket
}
