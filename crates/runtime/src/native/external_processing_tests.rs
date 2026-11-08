//! Opt-in integration test for a separately built SDK processor against the
//! actual authenticated runtime, including contract and grant rejection.
use amaterasu_abi::*;
use amaterasu_contracts::{
    Grants, RuntimeConfig,
    processing_composition::{CapabilityBinding, ProcessingComposition},
};
use amaterasu_module_sdk::{
    RuntimeClient,
    frame_processing::configure_frame_stream,
    media::native_contracts,
    media_abi::{FRAME_PROCESSOR_API_SIZE, FrameProcessorApi},
};
use std::ffi::c_void;

#[test]
#[ignore = "requires EXTERNAL_FRAME_PROCESSOR to point at the independently built SDK example"]
fn independently_built_processor_binds_to_the_real_broker() {
    let path = std::env::var_os("EXTERNAL_FRAME_PROCESSOR").expect("set EXTERNAL_FRAME_PROCESSOR");
    exercise_processor(
        path,
        "example.frame-processor",
        "example.frame-processor/1",
        serde_json::json!({}),
        None,
        false,
    );
}

#[test]
#[ignore = "requires EXTERNAL_RTX_PROCESSOR; does not submit frames or use the GPU"]
fn independently_built_rtx_starts_and_configures_without_other_plugins() {
    let path = std::env::var_os("EXTERNAL_RTX_PROCESSOR").expect("set EXTERNAL_RTX_PROCESSOR");
    let settings = serde_json::json!({"rtx_hdr_enable":true,"rtx_hdr_scope":"global",
        "rtx_hdr_application_enable":false,"rtx_hdr_peak_brightness":1000,
        "rtx_hdr_middle_gray":50,"rtx_hdr_contrast":0,"rtx_hdr_saturation":0,
        "rtx_hdr_debanding":true,"rtx_hdr_sdr_brightness":0,
        "rtx_hdr_foreground_only":false,"rtx_hdr_fullscreen_only":false});
    exercise_processor(
        path,
        "amaterasu.rtx-hdr",
        "amaterasu.rtx-hdr/1",
        settings,
        Some(serde_json::json!({"runtime_directory":std::env::temp_dir()})),
        true,
    );
}

fn exercise_processor(
    path: std::ffi::OsString,
    principal: &str,
    service: &str,
    frame_settings: serde_json::Value,
    private_settings: Option<serde_json::Value>,
    hdr: bool,
) {
    let binding = |principal: &str, service: &str, contract: &str| CapabilityBinding {
        principal: principal.into(),
        service: service.into(),
        contract: contract.into(),
        interface_major: 1,
        interface_minor: 0,
    };
    let processor = binding(principal, service, native_contracts::FRAME_PROCESSOR);
    let consumer = binding(
        "independent.video",
        "independent.video/1",
        amaterasu_module_sdk::video::CONTRACT,
    );
    let mut config = RuntimeConfig::default();
    config.principals.insert(
        processor.principal.clone(),
        Grants {
            provide: [processor.service.clone()].into(),
            native_provide: [processor.service.clone()].into(),
            ..Default::default()
        },
    );
    config.principals.insert(
        consumer.principal.clone(),
        Grants {
            native_provide: [consumer.service.clone()].into(),
            native_use: [processor.service.clone()].into(),
            ..Default::default()
        },
    );
    config
        .principals
        .insert("unapproved".into(), Grants::default());
    if let Some(settings) = private_settings {
        config
            .settings
            .insert(processor.principal.clone(), settings);
    }
    config.processing_compositions.insert(
        processor.principal.clone(),
        ProcessingComposition {
            processor: Some(processor.clone()),
            frame_consumer: Some(consumer.clone()),
            ..Default::default()
        },
    );
    config.processing_compositions.insert(
        consumer.principal.clone(),
        ProcessingComposition {
            processor: Some(processor.clone()),
            frame_consumer: Some(consumer.clone()),
            frame_settings: Some(frame_settings.clone()),
            ..Default::default()
        },
    );
    // SAFETY: the test keeps the runtime, loaded code and all endpoints alive
    // until the consumer binding and module instance have been retired.
    unsafe {
        let runtime_api = &*crate::ffi::amaterasu_runtime_v1();
        let mut runtime = std::ptr::null_mut::<c_void>();
        let json = serde_json::to_vec(&config).unwrap();
        assert_eq!(
            (runtime_api.create)(BorrowedBytes::new(&json), &mut runtime),
            STATUS_OK
        );
        let endpoint = |name: &str| {
            let mut endpoint = std::mem::MaybeUninit::<HostApi>::uninit();
            assert_eq!(
                (runtime_api.endpoint)(
                    runtime,
                    BorrowedBytes::new(name.as_bytes()),
                    endpoint.as_mut_ptr()
                ),
                STATUS_OK
            );
            endpoint.assume_init()
        };
        let provider_endpoint = endpoint(&processor.principal);
        let consumer_client = RuntimeClient::from_endpoint(endpoint(&consumer.principal));
        let unapproved_client = RuntimeClient::from_endpoint(endpoint("unapproved"));
        let library = libloading::Library::new(path).expect("load separately built processor");
        let entry: libloading::Symbol<unsafe extern "C" fn() -> *const ModuleApi> =
            library.get(b"amaterasu_module_v1\0").unwrap();
        let module_api = &*entry();
        let mut module = std::ptr::null_mut();
        assert_eq!(
            (module_api.create)(&provider_endpoint, &mut module),
            STATUS_OK
        );
        let view = consumer_client.processing_composition().unwrap().unwrap();
        assert_eq!(view.processor.as_ref(), Some(&processor));
        assert_eq!(view.frame_settings, Some(frame_settings.clone()));
        assert!(
            unapproved_client
                .bind_native_typed(
                    &processor.service,
                    native_contracts::FRAME_PROCESSOR,
                    1,
                    0,
                    FRAME_PROCESSOR_API_SIZE
                )
                .is_err()
        );
        assert!(
            consumer_client
                .bind_native_typed(
                    &processor.service,
                    "example.wrong-contract/1",
                    1,
                    0,
                    FRAME_PROCESSOR_API_SIZE
                )
                .is_err()
        );
        let native = consumer_client
            .bind_native_typed(
                &processor.service,
                native_contracts::FRAME_PROCESSOR,
                1,
                0,
                FRAME_PROCESSOR_API_SIZE,
            )
            .unwrap();
        let table = native.table::<FrameProcessorApi>().unwrap();
        let encoded = serde_json::to_vec(&frame_settings).unwrap();
        assert_eq!(configure_frame_stream(table, 9, &encoded), Ok(()));
        if hdr {
            let mut metadata = amaterasu_module_sdk::media_abi::HdrMetadata::default();
            assert_eq!(
                (table.hdr_metadata)(table.base.base.base.context, 9, &mut metadata),
                STATUS_OK
            );
            assert_eq!(metadata.enabled, 1);
        }
        (table.base.base.cancel)(table.base.base.base.context, 9);
        drop(native);
        (module_api.destroy)(module);
        drop(library);
        (runtime_api.destroy)(runtime);
    }
}
