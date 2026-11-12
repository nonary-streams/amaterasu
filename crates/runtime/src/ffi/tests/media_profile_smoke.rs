use super::*;
use amaterasu_contracts::media_profile::{FormatId, VideoProfile, VideoProfileSnapshot};
use amaterasu_module_sdk::media_profile::{self, Provider};

const SERVICE: &str = "example.video-profile/1";

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
    }
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
        assert_eq!(offered.profiles, vec![selected_profile]);
        assert!(!offered.supported_by(&VideoProfileSnapshot::default()));
        assert!(offered.supported_by(&offered));
        let mut incompatible = offered.clone();
        incompatible.profiles[0].framing.schema += 1;
        assert!(!offered.supported_by(&incompatible));
        drop(binding);
    }
    unsafe { destroy_runtime(runtime) };
}
#[test]
fn runtime_selected_profiles_require_exact_downstream_support() {
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
    assert!(!malformed.validate());
    let duplicate = VideoProfileSnapshot {
        profiles: vec![profile(3, 99, "A"), profile(3, 100, "B")],
    };
    assert!(!duplicate.validate());
}
