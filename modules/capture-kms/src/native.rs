use std::{ffi::c_char, ffi::c_void};

pub const ID_CAPACITY: usize = 192;
pub const NAME_CAPACITY: usize = 128;
pub const CARD_CAPACITY: usize = 64;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct NativeInfo {
    pub available: u32,
    pub output_count: u32,
    pub framebuffer_export_supported: u32,
    pub error: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NativeOutput {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub rotation: u32,
    pub attached: u32,
    pub framebuffer_export_supported: u32,
    pub error: u32,
    pub id: [c_char; ID_CAPACITY],
    pub name: [c_char; NAME_CAPACITY],
    pub card: [c_char; CARD_CAPACITY],
}

impl Default for NativeOutput {
    fn default() -> Self {
        Self {
            left: 0,
            top: 0,
            width: 0,
            height: 0,
            rotation: 0,
            attached: 0,
            framebuffer_export_supported: 0,
            error: 0,
            id: [0; ID_CAPACITY],
            name: [0; NAME_CAPACITY],
            card: [0; CARD_CAPACITY],
        }
    }
}

#[cfg(amaterasu_kms)]
unsafe extern "C" {
    pub fn am_kms_probe(output: *mut NativeInfo) -> i32;
    pub fn am_kms_enumerate(outputs: *mut NativeOutput, capacity: usize, count: *mut usize) -> i32;
    pub fn am_kms_open(output_id: *const c_char, error: *mut u32) -> *mut c_void;
    pub fn am_kms_session_geometry(session: *mut c_void, output: *mut NativeOutput) -> i32;
    pub fn am_kms_read(
        session: *mut c_void,
        destination: *mut u8,
        length: usize,
        width: *mut u32,
        height: *mut u32,
    ) -> i32;
    pub fn am_kms_close(session: *mut c_void);
}

#[cfg(all(test, amaterasu_kms))]
mod tests {
    use super::{NativeInfo, NativeOutput};
    use std::ffi::CString;
    use std::ptr;

    #[repr(C)]
    #[derive(Default, Debug)]
    struct CleanupObservation {
        make_current_calls: u32,
        texture_deletions: u32,
        framebuffer_deletions: u32,
        wrong_context_deletions: u32,
        image_deletions: u32,
        plane_queries: u32,
        texture_after: u32,
        framebuffer_after: u32,
        unbind_calls: u32,
        foreign_context_unbinds: u32,
        surface_destructions: u32,
        context_destructions: u32,
        display_terminations: u32,
        current_context_after: u32,
        selected_api_after: u32,
        vg_context_after: u32,
        foreign_display_terminations: u32,
        api_bind_calls: u32,
    }

    unsafe extern "C" {
        fn am_kms_test_image_cleanup(
            make_current_succeeds: u32,
            has_context: u32,
            output: *mut CleanupObservation,
        );
        fn am_kms_test_read_without_current_context(output: *mut CleanupObservation) -> i32;
        fn am_kms_test_read_binding(scenario: u32, output: *mut CleanupObservation) -> i32;
        fn am_kms_test_open_binding(fail_restore: u32, output: *mut CleanupObservation) -> u32;
        fn am_kms_test_close_binding(output: *mut CleanupObservation);
        fn am_kms_test_full_cleanup(
            make_current_succeeds: u32,
            initially_owned: u32,
            selected_vg: u32,
            output: *mut CleanupObservation,
        );
        fn am_kms_test_api_cleanup(scenario: u32, output: *mut CleanupObservation);
    }

    #[test]
    fn failed_teardown_bind_does_not_explicitly_unbind_foreign_context() {
        let mut observed = CleanupObservation::default();
        // SAFETY: every platform operation reached by this fixture is mocked.
        unsafe { am_kms_test_full_cleanup(0, 0, 0, &mut observed) };
        assert_eq!(observed.unbind_calls, 0, "{observed:?}");
        assert_eq!(observed.foreign_context_unbinds, 0, "{observed:?}");
        assert_eq!(observed.foreign_display_terminations, 1, "{observed:?}");
        assert_eq!(observed.texture_deletions, 0, "{observed:?}");
        assert_eq!(observed.framebuffer_deletions, 0, "{observed:?}");
        assert_eq!(observed.image_deletions, 1);
        assert_eq!(observed.surface_destructions, 1);
        assert_eq!(observed.context_destructions, 1);
        assert_eq!(observed.display_terminations, 1);
    }

    #[test]
    fn full_cleanup_unbinds_its_own_context_even_if_rebinding_fails() {
        let mut observed = CleanupObservation::default();
        // SAFETY: this fixture owns its fake handles and mocks platform calls.
        unsafe { am_kms_test_full_cleanup(0, 1, 0, &mut observed) };
        assert_eq!(observed.unbind_calls, 1, "{observed:?}");
        assert_eq!(observed.current_context_after, 0, "{observed:?}");
        assert_eq!(observed.context_destructions, 1);
    }

    #[test]
    fn full_cleanup_unbinds_context_after_successful_binding() {
        let mut observed = CleanupObservation::default();
        // SAFETY: this fixture owns its fake handles and mocks platform calls.
        unsafe { am_kms_test_full_cleanup(1, 0, 0, &mut observed) };
        assert_eq!(observed.unbind_calls, 1, "{observed:?}");
        assert_eq!(observed.foreign_context_unbinds, 0);
        assert_eq!(observed.texture_deletions, 1);
        assert_eq!(observed.framebuffer_deletions, 1);
        assert_eq!(observed.current_context_after, 0, "{observed:?}");
        assert_eq!(observed.context_destructions, 1);
    }

    #[test]
    fn full_cleanup_unbinds_own_gl_context_with_openvg_selected() {
        const EGL_OPENVG_API: u32 = 0x30a1;
        let mut observed = CleanupObservation::default();
        // SAFETY: the fixture tracks separate fake GL and OpenVG contexts.
        unsafe { am_kms_test_full_cleanup(1, 0, 1, &mut observed) };
        assert_eq!(observed.unbind_calls, 1, "{observed:?}");
        assert_eq!(observed.foreign_context_unbinds, 0, "{observed:?}");
        assert_eq!(observed.current_context_after, 0, "{observed:?}");
        assert_eq!(observed.vg_context_after, 6, "{observed:?}");
        assert_eq!(observed.selected_api_after, EGL_OPENVG_API, "{observed:?}");
    }

    #[test]
    fn partial_initialization_does_not_change_the_callers_api() {
        let mut observed = CleanupObservation::default();
        // SAFETY: the fixture uses fake EGL handles and mock entry points.
        unsafe { am_kms_test_api_cleanup(0, &mut observed) };
        assert_eq!(observed.api_bind_calls, 0, "{observed:?}");
        assert_eq!(observed.selected_api_after, 0x30a1, "{observed:?}");
    }

    #[test]
    fn failed_api_switch_does_not_unbind_foreign_context() {
        let mut observed = CleanupObservation::default();
        // SAFETY: the fixture uses fake EGL handles and mock entry points.
        unsafe { am_kms_test_api_cleanup(1, &mut observed) };
        assert_eq!(observed.api_bind_calls, 1, "{observed:?}");
        assert_eq!(observed.unbind_calls, 0, "{observed:?}");
        assert_eq!(observed.selected_api_after, 0x30a1, "{observed:?}");
        assert_eq!(observed.vg_context_after, 6, "{observed:?}");
    }

    #[test]
    fn failed_api_restoration_does_not_claim_the_original_api_was_restored() {
        let mut observed = CleanupObservation::default();
        // SAFETY: the fixture uses fake EGL handles and mock entry points.
        unsafe { am_kms_test_api_cleanup(2, &mut observed) };
        assert_eq!(observed.api_bind_calls, 2, "{observed:?}");
        assert_eq!(observed.selected_api_after, 0x30a0, "{observed:?}");
        assert_eq!(observed.vg_context_after, 6, "{observed:?}");
    }

    #[test]
    fn no_selected_api_is_not_replaced_or_unbound() {
        let mut observed = CleanupObservation::default();
        // SAFETY: the fixture uses fake EGL handles and mock entry points.
        unsafe { am_kms_test_api_cleanup(3, &mut observed) };
        assert_eq!(observed.api_bind_calls, 0, "{observed:?}");
        assert_eq!(observed.unbind_calls, 0, "{observed:?}");
        assert_eq!(observed.selected_api_after, 0x3038, "{observed:?}");
    }

    #[test]
    fn failed_context_binding_never_deletes_another_contexts_gl_objects() {
        let mut observed = CleanupObservation::default();
        // SAFETY: the adapter fixture writes this fixed-layout result and mocks
        // every reached platform call, using no native device or EGL context.
        unsafe { am_kms_test_image_cleanup(0, 1, &mut observed) };
        assert_eq!(observed.make_current_calls, 1);
        assert_eq!(observed.texture_deletions, 0, "{observed:?}");
        assert_eq!(observed.framebuffer_deletions, 0, "{observed:?}");
        assert_eq!(observed.wrong_context_deletions, 0);
        assert_eq!(observed.image_deletions, 1);
        assert_eq!((observed.texture_after, observed.framebuffer_after), (0, 0));
    }

    #[test]
    fn successful_context_binding_deletes_only_owned_gl_objects() {
        let mut observed = CleanupObservation::default();
        // SAFETY: this is the same isolated fixed-layout mock adapter fixture.
        unsafe { am_kms_test_image_cleanup(1, 1, &mut observed) };
        assert_eq!(observed.make_current_calls, 1);
        assert_eq!(observed.texture_deletions, 1);
        assert_eq!(observed.framebuffer_deletions, 1);
        assert_eq!(observed.wrong_context_deletions, 0);
        assert_eq!(observed.image_deletions, 1);
    }

    #[test]
    fn partial_initialization_without_a_context_skips_gl_deletion() {
        let mut observed = CleanupObservation::default();
        // SAFETY: this fixture never opens or uses a platform resource.
        unsafe { am_kms_test_image_cleanup(0, 0, &mut observed) };
        assert_eq!(observed.make_current_calls, 0);
        assert_eq!(observed.texture_deletions, 0);
        assert_eq!(observed.framebuffer_deletions, 0);
        assert_eq!(observed.image_deletions, 1);
    }

    #[test]
    fn read_checks_its_context_before_refreshing_native_framebuffer_ownership() {
        let mut observed = CleanupObservation::default();
        // SAFETY: DRM and EGL entry points reached by this call are mocks. The
        // fixture lends its own initialized frame bytes for the entire call.
        let result = unsafe { am_kms_test_read_without_current_context(&mut observed) };
        assert_eq!(result, 9, "{observed:?}");
        assert_eq!(observed.make_current_calls, 1);
        assert_eq!(observed.plane_queries, 0, "{observed:?}");
        assert_eq!(observed.wrong_context_deletions, 0);
    }

    #[test]
    fn failed_read_releases_its_context_for_another_thread() {
        let mut observed = CleanupObservation::default();
        // SAFETY: the adapter's EGL and DRM calls are mocked by this fixture.
        let result = unsafe { am_kms_test_read_binding(0, &mut observed) };
        assert_ne!(result, 0);
        assert_eq!(observed.plane_queries, 1);
        assert_eq!(observed.unbind_calls, 1);
        assert_eq!(observed.current_context_after, 0);
    }

    #[test]
    fn successful_initial_binding_restores_the_callers_context() {
        let mut observed = CleanupObservation::default();
        // SAFETY: all EGL handles in this fixture are mocked.
        assert_eq!(unsafe { am_kms_test_open_binding(0, &mut observed) }, 1);
        assert_eq!(observed.current_context_after, 5);
        assert_eq!((observed.texture_after, observed.framebuffer_after), (8, 9));
    }

    #[test]
    fn failed_context_restoration_attempts_to_release_our_context() {
        let mut observed = CleanupObservation::default();
        // SAFETY: all EGL handles in this fixture are mocked.
        assert_eq!(unsafe { am_kms_test_open_binding(1, &mut observed) }, 0);
        assert_eq!(observed.current_context_after, 0);
        assert_eq!(observed.unbind_calls, 1);
    }

    #[test]
    fn closing_capture_restores_the_callers_context() {
        let mut observed = CleanupObservation::default();
        // SAFETY: the fixture owns the fake session and mocks all EGL calls.
        unsafe { am_kms_test_close_binding(&mut observed) };
        assert_eq!(observed.current_context_after, 5);
        assert_eq!((observed.texture_after, observed.framebuffer_after), (8, 9));
        assert_eq!(observed.context_destructions, 1);
    }

    #[test]
    fn failed_read_restores_foreign_context_surfaces_and_selected_api() {
        for (scenario, api) in [(1, 0x30a0), (2, 0x30a1)] {
            let mut observed = CleanupObservation::default();
            // SAFETY: all handles are mock-only values owned by the fixture.
            let result = unsafe { am_kms_test_read_binding(scenario, &mut observed) };
            assert_ne!(result, 0);
            assert_eq!(observed.plane_queries, 1);
            assert_eq!(observed.current_context_after, 5, "{observed:?}");
            assert_eq!((observed.texture_after, observed.framebuffer_after), (8, 9));
            assert_eq!(observed.selected_api_after, api);
            assert_eq!(observed.vg_context_after, 6);
            assert_eq!(observed.foreign_context_unbinds, 0);
        }
    }

    #[test]
    fn native_abi_rejects_null_output_records_before_device_access() {
        // SAFETY: a null pointer is deliberately supplied to exercise the C
        // adapter's argument guard, which returns before inspecting DRM devices.
        assert_eq!(unsafe { super::am_kms_probe(ptr::null_mut()) }, 5);
        assert_eq!(
            unsafe { super::am_kms_enumerate(ptr::null_mut(), 1, ptr::null_mut()) },
            5
        );
        let mut count = 0;
        assert_eq!(
            unsafe { super::am_kms_enumerate(ptr::null_mut(), 1, &mut count) },
            5
        );
    }

    #[test]
    fn native_abi_rejects_null_handles_without_touching_pointer_payloads() {
        // SAFETY: each null is covered by an adapter guard before any dereference.
        let mut error = 0;
        assert!(unsafe { super::am_kms_open(ptr::null(), &mut error) }.is_null());
        assert_eq!(error, 5);
        assert_eq!(
            unsafe { super::am_kms_session_geometry(ptr::null_mut(), ptr::null_mut()) },
            5
        );
        assert_eq!(
            unsafe {
                super::am_kms_read(
                    ptr::null_mut(),
                    ptr::null_mut(),
                    0,
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            },
            5
        );
        unsafe { super::am_kms_close(ptr::null_mut()) };
    }

    #[test]
    fn native_abi_rejects_malformed_output_ids_before_opening_devices() {
        for value in [
            "kms:../../etc/hosts/conn=1/crtc=2/plane=3",
            "kms:card0/conn=+1/crtc=2/plane=3",
            "kms:card0/conn=4294967296/crtc=2/plane=3",
            "kms:card00/conn=1/crtc=2/plane=3",
            "kms:card0/conn=01/crtc=2/plane=3",
        ] {
            let malformed = CString::new(value).expect("test output ID has no embedded NUL");
            let mut error = 0;
            // SAFETY: the C string and error slot remain valid for the call;
            // malformed IDs are rejected before any DRM device scan.
            assert!(unsafe { super::am_kms_open(malformed.as_ptr(), &mut error) }.is_null());
            assert_eq!(error, 5, "unexpected error for malformed ID {value}");
        }
    }

    #[test]
    fn native_records_match_c_fixed_width_defaults() {
        let info = NativeInfo::default();
        let output = NativeOutput::default();
        assert_eq!((info.available, info.output_count, info.error), (0, 0, 0));
        assert_eq!((output.width, output.height, output.attached), (0, 0, 0));
        assert!(output.id.iter().all(|byte| *byte == 0));
    }
}

// Provider policy behind the shared bounded frame-resource SDK.
use crate::{backend::Backend, controller::Controller};
use amaterasu_media::*;
use amaterasu_module_sdk::{
    abi::{NativeServiceFactory, Status},
    capture::{
        CpuSourceDescription, CpuSourceFactory, CpuSourceFormat, CpuSourcePresentation,
        CpuSourceProvider,
    },
};
use std::sync::{Arc, Mutex};
type SharedController<B> = Arc<Mutex<Controller<B>>>;
pub struct Factory<B: Backend> {
    pub api: NativeServiceFactory,
    source: Box<CpuSourceFactory<Bridge<B>>>,
}
impl<B: Backend + 'static> Factory<B> {
    pub fn new(controller: SharedController<B>) -> Box<Self> {
        let source = CpuSourceFactory::new(Arc::new(Bridge { controller }));
        Box::new(Self {
            api: source.api,
            source,
        })
    }
    pub fn retirement_ready(&self) -> Result<bool, Status> {
        self.source.retirement_ready()
    }
}
struct Bridge<B: Backend> {
    controller: SharedController<B>,
}
impl<B: Backend> Bridge<B> {
    fn controller(&self) -> Result<std::sync::MutexGuard<'_, Controller<B>>, Status> {
        match self.controller.try_lock() {
            Ok(c) => Ok(c),
            Err(std::sync::TryLockError::WouldBlock) => Err(MEDIA_BUSY),
            Err(_) => Err(MEDIA_BACKEND_ERROR),
        }
    }
}
fn capture_status(error: &crate::model::CaptureError) -> Status {
    match error.code {
        "not_found" | "access_lost" | "device_lost" | "output_unavailable" => MEDIA_CLOSED,
        "unsupported_platform" | "unsupported_pixel_format" | "invalid_geometry" => {
            MEDIA_UNSUPPORTED
        }
        _ => MEDIA_BACKEND_ERROR,
    }
}
fn sdr() -> FrameColorMetadata {
    FrameColorMetadata {
        version: 1,
        size: size_of::<FrameColorMetadata>() as u32,
        primaries: FRAME_PRIMARIES_BT709,
        transfer: FRAME_TRANSFER_SRGB,
        matrix: FRAME_MATRIX_RGB,
        range: FRAME_RANGE_FULL,
    }
}
impl<B: Backend> CpuSourceProvider for Bridge<B> {
    fn describe(&self, caller: &str, stream: u64) -> Result<CpuSourceDescription, Status> {
        let g = self
            .controller()?
            .geometry(caller, stream)
            .map_err(|e| capture_status(&e))?;
        Ok(CpuSourceDescription {
            generation: 1,
            width: g.output.width,
            height: g.output.height,
            formats: vec![CpuSourceFormat {
                pixel_format: PIXEL_BGRA8,
                color: sdr(),
            }],
            hdr: None,
            authoritative_color: false,
        })
    }
    fn read(
        &self,
        caller: &str,
        stream: u64,
        format: &CpuSourceFormat,
    ) -> Result<Option<CpuSourcePresentation>, Status> {
        if format.pixel_format != PIXEL_BGRA8 {
            return Err(MEDIA_UNSUPPORTED);
        }
        let f = self
            .controller()?
            .read(caller, stream)
            .map_err(|e| capture_status(&e))?;
        Ok(Some(CpuSourcePresentation {
            receipt: None,
            width: f.width,
            height: f.height,
            stride: f.stride,
            pixel_format: PIXEL_BGRA8,
            color: sdr(),
            bytes: Arc::new(f.bytes).into(),
            timestamp_us: f.timestamp_us,
            capture_pacing_timestamp_us: f.timestamp_us,
            sequence: f.sequence,
            freshness: CAPTURE_FRAME_FRESH,
        }))
    }
    fn geometry(&self, caller: &str, stream: u64) -> Result<CaptureGeometry, Status> {
        self.controller()?
            .geometry(caller, stream)
            .map_err(|e| capture_status(&e))
    }
}
