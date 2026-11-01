use super::*;
use amaterasu_contracts::{
    SelectedService,
    media_profile::{FormatId, VideoProfile, VideoProfileSnapshot},
};
use amaterasu_gamestream::{
    rtsp::{
        Method, Request,
        negotiation::{Action, Capabilities, Negotiation, Step, TrustedLaunch},
    },
    sdp::{ColorMode, NegotiatedVideoFormat, TransportSupport},
};
use amaterasu_module_sdk::media_profile::{self, Provider};

const SERVICE: &str = "example.video-profile/1";
const FIXTURE: &str = include_str!("../../../../gamestream/src/sdp/fixtures/moonlight-h264.sdp");

struct TestProvider(VideoProfileSnapshot);
struct AlternateProvider(VideoProfileSnapshot);
impl Provider for TestProvider {
    fn snapshot(&self) -> Result<VideoProfileSnapshot, Status> {
        Ok(self.0.clone())
    }
}

impl Provider for AlternateProvider {
    fn snapshot(&self) -> Result<VideoProfileSnapshot, Status> {
        Ok(self.0.clone())
    }
}

fn profile(selector: u32, payload: u8, name: &str) -> VideoProfile {
    VideoProfile {
        codec: FormatId {
            namespace_id: 99,
            value: u64::from(selector),
            schema: 1,
        },
        framing: FormatId {
            namespace_id: 99,
            value: 42,
            schema: 1,
        },
        wire_selector: selector,
        payload_type: payload,
        encoding_name: name.into(),
        media_format: None,
        wire_attributes: Vec::new(),
    }
}

fn request(method: Method, target: &str, cseq: u32, session: bool) -> Request {
    Request {
        method,
        target: target.into(),
        cseq,
        headers: if session {
            vec![("session".into(), "session-1".into())]
        } else {
            vec![]
        },
        body: vec![],
    }
}

fn machine(
    offer: VideoProfileSnapshot,
    downstream: VideoProfileSnapshot,
) -> Result<Negotiation, amaterasu_gamestream::rtsp::negotiation::Error> {
    Negotiation::with_video_profiles(
        TrustedLaunch {
            epoch: 7,
            rtsp_target: "rtspenc://127.0.0.1:48010/launch".into(),
            session_token: "session-1".into(),
            encrypted_rtsp: true,
            audio_port: 49000,
            video_port: 49002,
            control_port: 49004,
            control_connect_data: 123,
            udp_discovery: None,
        },
        Capabilities {
            transport: TransportSupport {
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
            supported_colors: vec![ColorMode::Bt709Limited],
            max_width: 1920,
            max_height: 1080,
            max_fps: 60,
            max_bitrate_kbps: 20000,
            require_video_encryption: false,
            require_audio_encryption: false,
        },
        offer,
        downstream,
    )
}

fn to_announce(machine: &mut Negotiation, marker: &str) {
    let target = "rtspenc://127.0.0.1:48010/launch";
    machine
        .handle(7, &request(Method::Options, target, 1, false))
        .unwrap();
    let mut describe = request(Method::Describe, target, 2, false);
    describe
        .headers
        .push(("accept".into(), "application/sdp".into()));
    let Step::Reply(reply) = machine.handle(7, &describe).unwrap() else {
        panic!()
    };
    assert!(String::from_utf8(reply).unwrap().contains(marker));
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
        machine.handle(7, &setup).unwrap();
    }
}

fn announce(body: String) -> Request {
    let mut request = request(Method::Announce, "streamid=control/13/0", 6, true);
    request
        .headers
        .push(("content-type".into(), "application/sdp".into()));
    request.body = body
        .replace(
            "x-ss-general.encryptionEnabled:0",
            "x-ss-general.encryptionEnabled:1",
        )
        .into_bytes();
    request
}

fn selected(selector: u32) -> String {
    FIXTURE.replace(
        "a=x-nv-vqos[0].bitStreamFormat:0 ",
        &format!("a=x-nv-vqos[0].bitStreamFormat:{selector} "),
    )
}

fn exercise<P: Provider>(provider_impl: P, selected_profile: VideoProfile) {
    let configuration = RuntimeConfig {
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
        service_selections: amaterasu_contracts::ServiceSelections {
            native: [(
                SERVICE.into(),
                Some(SelectedService {
                    provider: "provider".into(),
                    contract: media_profile::CONTRACT.into(),
                }),
            )]
            .into(),
            ..Default::default()
        },
        ..Default::default()
    };
    let bytes = serde_json::to_vec(&configuration).unwrap();
    let mut runtime = std::ptr::null_mut();
    assert_eq!(
        unsafe { create_runtime(BorrowedBytes::new(&bytes), &mut runtime) },
        STATUS_OK
    );
    {
        let provider = unsafe { RuntimeClient::from_endpoint(endpoint(runtime, "provider")) };
        let consumer = unsafe { RuntimeClient::from_endpoint(endpoint(runtime, "consumer")) };
        let factory = media_profile::Factory::new(provider_impl);
        unsafe { media_profile::register(&provider, SERVICE, &factory.api) }.unwrap();
        let intruder = unsafe { RuntimeClient::from_endpoint(endpoint(runtime, "intruder")) };
        assert!(unsafe { media_profile::bind(&intruder, SERVICE) }.is_err());
        let binding = unsafe { media_profile::bind(&consumer, SERVICE) }.unwrap();
        let offered = unsafe { media_profile::read(&binding) }.unwrap();
        assert!(machine(offered.clone(), VideoProfileSnapshot::default()).is_err());
        let mut mismatched = offered.clone();
        mismatched.profiles[0].framing.schema += 1;
        assert!(machine(offered.clone(), mismatched).is_err());
        let mut negotiation = machine(offered.clone(), offered.clone()).unwrap();
        to_announce(
            &mut negotiation,
            &format!(
                "a=rtpmap:{} {}/90000",
                selected_profile.payload_type, selected_profile.encoding_name
            ),
        );
        let Step::Action {
            ticket,
            action: Action::ExtensionAnnounce(result),
        } = negotiation
            .handle(7, &announce(selected(selected_profile.wire_selector)))
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(
            result.video.format,
            NegotiatedVideoFormat::Extension(selected_profile.clone())
        );
        assert!(negotiation.complete(7, ticket, true).is_ok());
        let Step::Action {
            action: Action::Play,
            ..
        } = negotiation
            .handle(7, &request(Method::Play, "/", 7, true))
            .unwrap()
        else {
            panic!()
        };
        for body in [
            selected(777),
            selected(selected_profile.wire_selector)
                .replace("dynamicRangeMode:0", "dynamicRangeMode:1"),
            format!(
                "{}a=x-nv-vqos[0].bitStreamFormat:3\r\n",
                selected(selected_profile.wire_selector)
            ),
        ] {
            let mut negotiation = machine(offered.clone(), offered.clone()).unwrap();
            to_announce(&mut negotiation, "H264/90000");
            assert!(negotiation.handle(7, &announce(body)).is_err());
        }
        let mut legacy = machine(offered.clone(), offered.clone()).unwrap();
        to_announce(&mut legacy, "H264/90000");
        assert!(matches!(
            legacy.handle(7, &announce(FIXTURE.into())).unwrap(),
            Step::Action {
                action: Action::Announce(_),
                ..
            }
        ));
        drop(binding);
    }
    unsafe { destroy_runtime(runtime) };
}

#[test]
fn distinct_runtime_selected_providers_drive_real_describe_and_announce() {
    let first = profile(3, 99, "TESTCODEC");
    let second = profile(4, 100, "OTHERFORMAT");
    exercise(
        TestProvider(VideoProfileSnapshot {
            profiles: vec![first.clone()],
        }),
        first,
    );
    exercise(
        AlternateProvider(VideoProfileSnapshot {
            profiles: vec![second.clone()],
        }),
        second,
    );
}

#[test]
fn invalid_offers_fail_before_advertisement() {
    let mut p = profile(3, 99, "TESTCODEC");
    p.encoding_name = "BAD\r\na=rtpmap:98 injected".into();
    let malformed = VideoProfileSnapshot { profiles: vec![p] };
    assert!(machine(malformed.clone(), malformed).is_err());
    let duplicate = VideoProfileSnapshot {
        profiles: vec![profile(3, 99, "A"), profile(3, 100, "B")],
    };
    assert!(machine(duplicate.clone(), duplicate).is_err());
}

#[test]
fn extension_markers_cannot_claim_disabled_legacy_codecs() {
    for name in [
        "AV1",
        "EXAMPLEAV1",
        "H264",
        "EXAMPLEH264",
        "H265",
        "EXAMPLEH265",
        "HEVC",
        "EXAMPLEHEVC",
        "av1",
        "Av1",
    ] {
        let offer = VideoProfileSnapshot {
            profiles: vec![profile(3, 99, name)],
        };
        assert!(
            machine(offer.clone(), offer).is_err(),
            "accepted marker {name}"
        );
    }
    let offer = VideoProfileSnapshot {
        profiles: vec![profile(3, 99, "AV1-EXAMPLE")],
    };
    let mut negotiation = machine(offer.clone(), offer).unwrap();
    let target = "rtspenc://127.0.0.1:48010/launch";
    negotiation
        .handle(7, &request(Method::Options, target, 1, false))
        .unwrap();
    let mut describe = request(Method::Describe, target, 2, false);
    describe
        .headers
        .push(("accept".into(), "application/sdp".into()));
    let Step::Reply(reply) = negotiation.handle(7, &describe).unwrap() else {
        panic!()
    };
    let description = String::from_utf8(reply).unwrap();
    assert!(description.contains("a=rtpmap:99 AV1-EXAMPLE/90000"));
    assert!(!description.contains("AV1/90000"));
    assert!(!description.contains("sprop-parameter-sets=AAAAAU"));
}
