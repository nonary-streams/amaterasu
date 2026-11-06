//! An unchanged optional-timing consumer reads independently implemented
//! providers through the public SDK adapter and its versioned native contract.
use amaterasu_module_sdk::{abi::*, encode::*, media::provider_encode_timing, media_abi::*};

trait MeasurementFixture: Send + Sync + 'static {
    fn measurement(&self) -> Option<EncodeTimingSnapshotV1>;
}
struct FixedProvider;
impl MeasurementFixture for FixedProvider {
    fn measurement(&self) -> Option<EncodeTimingSnapshotV1> {
        Some(EncodeTimingSnapshotV1 {
            completed_frames: 1,
            total_us: 2000,
            min_us: 2000,
            max_us: 2000,
            ..EncodeTimingSnapshotV1::empty(1)
        })
    }
}
struct SampledReplacement;
impl MeasurementFixture for SampledReplacement {
    fn measurement(&self) -> Option<EncodeTimingSnapshotV1> {
        let mut snapshot = EncodeTimingSnapshotV1::empty(7);
        for duration in [3000, 7000] {
            assert!(snapshot.record(duration));
        }
        Some(snapshot)
    }
}
struct UnmeasuredProvider;
impl MeasurementFixture for UnmeasuredProvider {
    fn measurement(&self) -> Option<EncodeTimingSnapshotV1> {
        None
    }
}

struct FixtureProvider<T>(T);
impl<T: MeasurementFixture> EncoderProvider for FixtureProvider<T> {
    fn probe(&self, _: SessionConfig) -> Result<ControlResponse, Status> {
        Err(MEDIA_UNSUPPORTED)
    }
    fn configure(&self, _: u64, _: SessionConfig) -> Result<(), Status> {
        Err(MEDIA_UNSUPPORTED)
    }
    fn output(&self, _: u64) -> Result<OutputQueue, Status> {
        Err(MEDIA_UNSUPPORTED)
    }
    fn control(&self, _: u64, _: ControlRequest) -> Result<ControlResponse, Status> {
        Err(MEDIA_UNSUPPORTED)
    }
    fn encode_timing(&self, session: u64) -> Result<Option<EncodeTimingSnapshotV1>, Status> {
        if session != 9 {
            return Err(MEDIA_CLOSED);
        }
        Ok(self.0.measurement())
    }
    fn drain(&self, _: u64) -> Result<(), Status> {
        Ok(())
    }
    fn destroy(&self, _: u64) {}
    fn release(&self) -> Result<(), Status> {
        Ok(())
    }
}

unsafe fn unchanged_consumer(api: &EncoderProviderApi) -> Option<f64> {
    // SAFETY: fixture supplies the selected typed provider binding/session and
    // retains it through both synchronous reads. Neither read consumes output.
    let snapshot = unsafe { provider_encode_timing(api, 9) }.unwrap();
    assert_eq!(unsafe { provider_encode_timing(api, 9) }.unwrap(), snapshot);
    assert_eq!(
        unsafe { provider_encode_timing(api, 10) },
        Err(MEDIA_CLOSED)
    );
    snapshot.and_then(|sample| sample.mean_ms_since(EncodeTimingSnapshotV1::empty(sample.epoch)))
}

fn exercise(provider: impl MeasurementFixture) -> Option<f64> {
    let pointer = ProviderAdapter::new(FixtureProvider(provider)).into_binding();
    // SAFETY: the public adapter owns this valid typed table until release.
    unsafe {
        let api = &*pointer;
        let result = unchanged_consumer(api);
        (api.base.release.unwrap())(api.base.context);
        result
    }
}

#[test]
fn one_consumer_uses_two_replacement_providers_and_preserves_unavailable() {
    assert_eq!(exercise(FixedProvider), Some(2.0));
    assert_eq!(exercise(SampledReplacement), Some(5.0));
    assert_eq!(exercise(UnmeasuredProvider), None);
}

#[test]
fn old_provider_prefix_never_reads_the_optional_timing_tail() {
    let pointer = ProviderAdapter::new(FixtureProvider(FixedProvider)).into_binding();
    // SAFETY: fixture exclusively owns this allocation before any borrowing;
    // advertising only the unchanged v1.0 prefix simulates an older provider.
    unsafe {
        (*pointer.cast_mut()).base.header.minor = 0;
        (*pointer.cast_mut()).base.header.size = ENCODER_PROVIDER_API_V1_0_SIZE;
        let api = &*pointer;
        assert_eq!(provider_encode_timing(api, 9), Ok(None));
        (api.base.release.unwrap())(api.base.context);
    }
}
